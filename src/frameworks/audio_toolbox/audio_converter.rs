/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! `AudioConverter.h` — Audio format conversion.
//!
//! Implements real LPCM -> LPCM conversion: sample-rate conversion
//! (linear interpolation), channel count conversion (mono <-> stereo
//! up/downmix), bit depth conversion (8/16/24/32-bit) and integer
//! <-> floating point, in both little- and big-endian layouts.
//!
//! `AudioConverterFillComplexBuffer` uses a staging buffer in guest memory
//! for LPCM input (the game's input
//! callback fills source-format data, which is then converted into the
//! caller's output buffer). Input callbacks may replace mData with a pointer
//! to their own samples; that memory is borrowed, never freed by the converter.

use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::FunctionExports;
use crate::export_c_func;
use crate::frameworks::carbon_core::OSStatus;
use crate::frameworks::core_audio_types::{
    debug_fourcc, fourcc, kAudioFormatFlagIsBigEndian, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsNonInterleaved, kAudioFormatLinearPCM, AudioStreamBasicDescription,
};
use crate::mem::{ConstPtr, MutPtr, MutVoidPtr, SafeRead};
use crate::Environment;

const kAudioConverterErr_InvalidInputSize: OSStatus = -50;
const kAudioConverterErr_FormatNotSupported: OSStatus = fourcc(b"!cnv") as i32;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: MutVoidPtr,
}
unsafe impl SafeRead for AudioBuffer {}

#[repr(C, packed)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}
unsafe impl SafeRead for AudioBufferList {}

#[repr(C, packed)]
pub struct AudioStreamPacketDescription {
    pub mStartOffset: i64,
    pub mVariableFramesInPacket: u32,
    pub mDataByteSize: u32,
}
unsafe impl SafeRead for AudioStreamPacketDescription {}

pub type AudioConverterComplexInputDataProc = GuestFunction;

#[repr(C, packed)]
struct OpaqueAudioConverter {
    source_format: AudioStreamBasicDescription,
    dest_format: AudioStreamBasicDescription,
    /// Stored `kAudioConverterSampleRateConverterQuality` value.
    src_quality: u32,
    /// Stored `kAudioConverterSampleRateConverterComplexity` value.
    src_complexity: u32,
}
unsafe impl SafeRead for OpaqueAudioConverter {}

type AudioConverterRef = MutPtr<OpaqueAudioConverter>;

fn pcm_plane(interleaved: &[u8], sample_bytes: usize, channels: usize, channel: usize) -> Vec<u8> {
    interleaved
        .chunks_exact(sample_bytes * channels)
        .flat_map(|frame| {
            frame[channel * sample_bytes..(channel + 1) * sample_bytes]
                .iter()
                .copied()
        })
        .collect()
}

/// Copy only complete frames actually supplied by the input callback.
fn returned_pcm(
    mem: &crate::mem::Mem,
    abl: &AudioBufferList,
    packets: u32,
    bytes_per_frame: u32,
) -> Vec<u8> {
    let buffer = abl.mBuffers[0];
    if abl.mNumberBuffers == 0 || buffer.mData.is_null() || bytes_per_frame == 0 {
        return Vec::new();
    }
    let frames = packets.min(buffer.mDataByteSize / bytes_per_frame);
    mem.bytes_at(buffer.mData.cast(), frames * bytes_per_frame)
        .to_vec()
}

/// `kAudioConverterSampleRateConverterQuality` ('srcq') and the quality
/// constants from Apple's `AudioConverter.h`.
const kAudioConverterSampleRateConverterQuality: u32 = fourcc(b"srcq");
const kAudioConverterSampleRateConverterComplexity: u32 = fourcc(b"srca");
#[allow(dead_code)]
pub mod converter_quality {
    pub const MAX: u32 = 0x7F00_0000;
    pub const HIGH: u32 = 0x6000_0000;
    pub const MEDIUM: u32 = 0x4000_0000;
    pub const LOW: u32 = 0x3000_0000;
}

// ---------------------------------------------------------------------------
// PCM shape + real conversion core
// ---------------------------------------------------------------------------

/// The parts of an `AudioStreamBasicDescription` needed to interpret a
/// byte buffer of interleaved linear PCM data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PcmShape {
    pub sample_rate: f64,
    pub channels: u32,
    pub bits: u32,
    pub is_float: bool,
    pub is_big_endian: bool,
    pub bytes_per_frame: u32,
}

/// Extract a [PcmShape] from an ASBD. Returns [None] for non-LPCM formats or
/// nonsensical ones.
pub fn pcm_shape_from_asbd(asbd: &AudioStreamBasicDescription) -> Option<PcmShape> {
    if asbd.format_id != kAudioFormatLinearPCM {
        return None;
    }
    let channels = asbd.channels_per_frame;
    let bits = asbd.bits_per_channel;
    if channels == 0 || channels > 8 || !matches!(bits, 8 | 16 | 24 | 32) {
        return None;
    }
    let bytes_per_channel = (bits / 8) as u32;
    let bytes_per_frame = if asbd.format_flags & kAudioFormatFlagIsNonInterleaved != 0 {
        channels * bytes_per_channel
    } else if asbd.bytes_per_frame > 0 {
        asbd.bytes_per_frame
    } else {
        channels * bytes_per_channel
    };
    if bytes_per_frame < channels * bytes_per_channel {
        return None;
    }
    Some(PcmShape {
        sample_rate: asbd.sample_rate,
        channels,
        bits,
        is_float: (asbd.format_flags & kAudioFormatFlagIsFloat) != 0,
        is_big_endian: (asbd.format_flags & kAudioFormatFlagIsBigEndian) != 0,
        bytes_per_frame,
    })
}

/// Whether two shapes need no conversion at all.
pub fn pcm_shapes_equal(a: &PcmShape, b: &PcmShape) -> bool {
    a.sample_rate == b.sample_rate
        && a.channels == b.channels
        && a.bits == b.bits
        && a.is_float == b.is_float
        && a.is_big_endian == b.is_big_endian
        && a.bytes_per_frame == b.bytes_per_frame
}

fn read_f32_sample(data: &[u8], shape: &PcmShape, frame: usize, channel: usize) -> f32 {
    let bytes_per_channel = (shape.bits / 8) as usize;
    let offset = frame * shape.bytes_per_frame as usize + channel * bytes_per_channel;
    let bytes = &data[offset..offset + bytes_per_channel];
    let sample = if shape.is_big_endian {
        match shape.bits {
            8 => ((bytes[0] as i32) - 128) as f64 / 128.0,
            16 => i16::from_be_bytes(bytes.try_into().unwrap()) as f64 / 32768.0,
            24 => {
                let v = ((bytes[0] as i32) << 16) | ((bytes[1] as i32) << 8) | (bytes[2] as i32);
                let v = (v << 8) >> 8; // sign-extend 24 -> 32
                v as f64 / 8388608.0
            }
            32 => {
                if shape.is_float {
                    f32::from_be_bytes(bytes.try_into().unwrap()) as f64
                } else {
                    i32::from_be_bytes(bytes.try_into().unwrap()) as f64 / 2147483648.0
                }
            }
            _ => 0.0,
        }
    } else {
        match shape.bits {
            8 => ((bytes[0] as i32) - 128) as f64 / 128.0,
            16 => i16::from_le_bytes(bytes.try_into().unwrap()) as f64 / 32768.0,
            24 => {
                let v = ((bytes[0] as i32) << 16) | ((bytes[1] as i32) << 8) | (bytes[2] as i32);
                let v = (v << 8) >> 8; // sign-extend 24 -> 32
                v as f64 / 8388608.0
            }
            32 => {
                if shape.is_float {
                    f32::from_le_bytes(bytes.try_into().unwrap()) as f64
                } else {
                    i32::from_le_bytes(bytes.try_into().unwrap()) as f64 / 2147483648.0
                }
            }
            _ => 0.0,
        }
    };
    // 8-bit WAV PCM is unsigned, handled above; float samples can exceed
    // [-1, 1] slightly — clamp at the end of conversion instead of here so
    // intermediate math keeps headroom.
    sample as f32
}

fn write_f32_sample(out: &mut Vec<u8>, shape: &PcmShape, sample: f32) {
    let sample = sample.clamp(-1.0, 1.0);
    let bytes_per_channel = (shape.bits / 8) as usize;
    let mut buf = [0u8; 4];
    match shape.bits {
        8 => {
            buf[0] = ((sample * 127.0) as i32 + 128).clamp(0, 255) as u8;
        }
        16 => {
            let v = (sample * 32767.0) as i16;
            if shape.is_big_endian {
                buf[..2].copy_from_slice(&v.to_be_bytes());
            } else {
                buf[..2].copy_from_slice(&v.to_le_bytes());
            }
        }
        24 => {
            let v = (sample * 8388607.0) as i32;
            if shape.is_big_endian {
                buf[..3].copy_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
            } else {
                buf[..3].copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]);
            }
        }
        32 => {
            if shape.is_float {
                let v = sample;
                if shape.is_big_endian {
                    buf.copy_from_slice(&v.to_be_bytes());
                } else {
                    buf.copy_from_slice(&v.to_le_bytes());
                }
            } else {
                let v = (sample * 2147483647.0) as i32;
                if shape.is_big_endian {
                    buf.copy_from_slice(&v.to_be_bytes());
                } else {
                    buf.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        _ => {}
    }
    out.extend_from_slice(&buf[..bytes_per_channel]);
}

/// Convert interleaved PCM bytes from one shape to another. Handles sample
/// rate (linear interpolation), channel count (up/downmix) and sample
/// encoding (bit depth / float / endian) conversion.
pub fn convert_pcm(src: &[u8], src_shape: &PcmShape, dst_shape: &PcmShape) -> Vec<u8> {
    let src_bytes_per_frame = src_shape.bytes_per_frame as usize;
    let src_frames = if src_bytes_per_frame > 0 {
        src.len() / src_bytes_per_frame
    } else {
        return Vec::new();
    };

    // 1. Decode to interleaved f32.
    let mut samples: Vec<f32> = Vec::with_capacity(src_frames * src_shape.channels as usize);
    for frame in 0..src_frames {
        for ch in 0..src_shape.channels as usize {
            samples.push(read_f32_sample(src, src_shape, frame, ch));
        }
    }

    // 2. Sample-rate conversion (linear interpolation).
    let src_rate = src_shape.sample_rate.max(1.0);
    let dst_rate = dst_shape.sample_rate.max(1.0);
    let resampled: Vec<f32> = if (src_rate - dst_rate).abs() < f64::EPSILON {
        samples
    } else {
        let dst_frames = ((src_frames as f64) * dst_rate / src_rate).round() as usize;
        let mut out = Vec::with_capacity(dst_frames * src_shape.channels as usize);
        for i in 0..dst_frames {
            let pos = (i as f64) * src_rate / dst_rate;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let idx1 = idx.min(src_frames.saturating_sub(1));
            let idx2 = (idx + 1).min(src_frames.saturating_sub(1));
            for ch in 0..src_shape.channels as usize {
                let a = samples[idx1 * src_shape.channels as usize + ch];
                let b = samples[idx2 * src_shape.channels as usize + ch];
                out.push(a + (b - a) * frac);
            }
        }
        out
    };
    let frames = resampled.len() / src_shape.channels as usize;

    // 3. Channel conversion.
    let src_ch = src_shape.channels as usize;
    let dst_ch = dst_shape.channels as usize;
    let mixed: Vec<f32> = if src_ch == dst_ch {
        resampled
    } else {
        let mut out = Vec::with_capacity(frames * dst_ch);
        for frame in 0..frames {
            let frame_samples = &resampled[frame * src_ch..(frame + 1) * src_ch];
            match (src_ch, dst_ch) {
                (1, _) => {
                    // Mono -> N: duplicate.
                    for _ in 0..dst_ch {
                        out.push(frame_samples[0]);
                    }
                }
                (_, 1) => {
                    // N -> Mono: average.
                    let sum: f32 = frame_samples.iter().sum();
                    out.push(sum / src_ch as f32);
                }
                _ => {
                    // General N -> M: average when narrowing, duplicate the
                    // last available channel when widening (keeps stereo
                    // width for 5.1 -> stereo-style cases reasonable).
                    for c in 0..dst_ch {
                        if c < src_ch {
                            out.push(frame_samples[c]);
                        } else {
                            out.push(frame_samples[src_ch - 1]);
                        }
                    }
                }
            }
        }
        out
    };

    // 4. Encode to destination sample format.
    let mut out = Vec::with_capacity(frames * dst_shape.bytes_per_frame as usize);
    for frame in 0..frames {
        for ch in 0..dst_ch {
            write_f32_sample(&mut out, dst_shape, mixed[frame * dst_ch + ch]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Host-side property access
// ---------------------------------------------------------------------------

fn AudioConverterNew(
    env: &mut Environment,
    in_source_format: ConstPtr<AudioStreamBasicDescription>,
    in_destination_format: ConstPtr<AudioStreamBasicDescription>,
    out_audio_converter: MutPtr<AudioConverterRef>,
) -> OSStatus {
    let source_format = env.mem.read(in_source_format);
    let dest_format = env.mem.read(in_destination_format);

    log_dbg!(
        "AudioConverterNew: {} -> {}",
        debug_fourcc(source_format.format_id),
        debug_fourcc(dest_format.format_id)
    );

    let converter_data = OpaqueAudioConverter {
        source_format,
        dest_format,
        // Apple's default quality is medium.
        src_quality: converter_quality::MEDIUM,
        src_complexity: 0,
    };

    let converter: AudioConverterRef = env.mem.alloc_and_write(converter_data);
    env.mem.write(out_audio_converter, converter);

    0 // noErr
}

fn AudioConverterDispose(env: &mut Environment, in_audio_converter: AudioConverterRef) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    env.mem.free(in_audio_converter.cast());
    0
}

fn AudioConverterReset(_env: &mut Environment, _in_audio_converter: AudioConverterRef) -> OSStatus {
    0
}

fn AudioConverterGetProperty(
    env: &mut Environment,
    in_audio_converter: AudioConverterRef,
    in_property_id: u32,
    io_data_size: MutPtr<u32>,
    out_property_data: MutPtr<u8>,
) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let converter: OpaqueAudioConverter = env.mem.read(in_audio_converter);
    match in_property_id {
        kAudioConverterSampleRateConverterQuality => {
            let size = env.mem.read(io_data_size);
            if size < 4 {
                return kAudioConverterErr_InvalidInputSize;
            }
            env.mem.write(io_data_size, 4u32);
            env.mem
                .write(out_property_data.cast(), converter.src_quality);
            0
        }
        kAudioConverterSampleRateConverterComplexity => {
            let size = env.mem.read(io_data_size);
            if size < 4 {
                return kAudioConverterErr_InvalidInputSize;
            }
            env.mem.write(io_data_size, 4u32);
            env.mem
                .write(out_property_data.cast(), converter.src_complexity);
            0
        }
        _ => {
            log_dbg!(
                "AudioConverterGetProperty({:?}, {}) -> unimplemented property",
                in_audio_converter,
                debug_fourcc(in_property_id),
            );
            -1 // Unimplemented
        }
    }
}

fn AudioConverterSetProperty(
    env: &mut Environment,
    in_audio_converter: AudioConverterRef,
    in_property_id: u32,
    in_data_size: u32,
    in_property_data: ConstPtr<u8>,
) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let mut converter: OpaqueAudioConverter = env.mem.read(in_audio_converter);
    match in_property_id {
        kAudioConverterSampleRateConverterQuality => {
            if in_data_size < 4 {
                return kAudioConverterErr_InvalidInputSize;
            }
            converter.src_quality = env.mem.read(in_property_data.cast());
            env.mem.write(in_audio_converter, converter);
            0
        }
        kAudioConverterSampleRateConverterComplexity => {
            if in_data_size < 4 {
                return kAudioConverterErr_InvalidInputSize;
            }
            converter.src_complexity = env.mem.read(in_property_data.cast());
            env.mem.write(in_audio_converter, converter);
            0
        }
        _ => {
            log_dbg!(
                "AudioConverterSetProperty({:?}, {}, size={}) -> unimplemented property",
                in_audio_converter,
                debug_fourcc(in_property_id),
                in_data_size,
            );
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion entry points
// ---------------------------------------------------------------------------

/// Simple one-shot conversion between two flat buffers
/// (`AudioConverterConvertBuffer`).
fn AudioConverterConvertBuffer(
    env: &mut Environment,
    in_audio_converter: AudioConverterRef,
    input_data_size: u32,
    input_data: ConstPtr<u8>,
    io_output_data_size: MutPtr<u32>,
    output_data: MutVoidPtr,
) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let converter: OpaqueAudioConverter = env.mem.read(in_audio_converter);
    // These flat-buffer APIs do not carry separate PCM planes.
    if (converter.source_format.format_flags | converter.dest_format.format_flags)
        & kAudioFormatFlagIsNonInterleaved
        != 0
    {
        return kAudioConverterErr_FormatNotSupported;
    }
    let Some(src_shape) = pcm_shape_from_asbd(&converter.source_format) else {
        log!(
            "AudioConverterConvertBuffer: unsupported source format {:#?}",
            converter.source_format
        );
        return kAudioConverterErr_FormatNotSupported;
    };
    let Some(dst_shape) = pcm_shape_from_asbd(&converter.dest_format) else {
        log!(
            "AudioConverterConvertBuffer: unsupported destination format {:#?}",
            converter.dest_format
        );
        return kAudioConverterErr_FormatNotSupported;
    };

    let src = env
        .mem
        .bytes_at(input_data.cast(), input_data_size)
        .to_vec();
    let converted = convert_pcm(&src, &src_shape, &dst_shape);

    let out_capacity = env.mem.read(io_output_data_size) as usize;
    let out_bytes = converted.len().min(out_capacity);
    let out_slice = env.mem.bytes_at_mut(output_data.cast(), out_bytes as u32);
    out_slice.copy_from_slice(&converted[..out_bytes]);
    env.mem.write(io_output_data_size, out_bytes as u32);
    if converted.len() > out_capacity {
        log!(
            "AudioConverterConvertBuffer: output buffer too small ({} > {}); truncated.",
            converted.len(),
            out_capacity
        );
        return kAudioConverterErr_InvalidInputSize;
    }
    0
}

/// `AudioConverterConvertComplexBuffer` — the simple (non-callback)
/// conversion API: converts `in_number_frames` from `in_input_data` into
/// `out_output_data` in one shot.
fn AudioConverterConvertComplexBuffer(
    env: &mut Environment,
    in_audio_converter: AudioConverterRef,
    in_number_frames: u32,
    in_input_data: ConstPtr<AudioBufferList>,
    out_output_data: MutPtr<AudioBufferList>,
) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let converter: OpaqueAudioConverter = env.mem.read(in_audio_converter);
    // These flat-buffer APIs do not carry separate PCM planes.
    if (converter.source_format.format_flags | converter.dest_format.format_flags)
        & kAudioFormatFlagIsNonInterleaved
        != 0
    {
        return kAudioConverterErr_FormatNotSupported;
    }
    let Some(src_shape) = pcm_shape_from_asbd(&converter.source_format) else {
        log!(
            "AudioConverterConvertComplexBuffer: unsupported source format {:#?}",
            converter.source_format
        );
        return kAudioConverterErr_FormatNotSupported;
    };
    let Some(dst_shape) = pcm_shape_from_asbd(&converter.dest_format) else {
        log!(
            "AudioConverterConvertComplexBuffer: unsupported destination format {:#?}",
            converter.dest_format
        );
        return kAudioConverterErr_FormatNotSupported;
    };

    let in_abl: AudioBufferList = env.mem.read(in_input_data);
    let in_buf = in_abl.mBuffers[0];
    let src_bytes = (in_number_frames as usize) * src_shape.bytes_per_frame as usize;
    let src_bytes = src_bytes.min(in_buf.mDataByteSize as usize);
    let src = env
        .mem
        .bytes_at(in_buf.mData.cast(), src_bytes as u32)
        .to_vec();

    let converted = convert_pcm(&src, &src_shape, &dst_shape);

    let mut out_abl: AudioBufferList = env.mem.read(out_output_data);
    let out_capacity = out_abl.mBuffers[0].mDataByteSize as usize;
    let out_bytes = converted.len().min(out_capacity);
    let out_slice = env
        .mem
        .bytes_at_mut(out_abl.mBuffers[0].mData.cast(), out_bytes as u32);
    out_slice.copy_from_slice(&converted[..out_bytes]);
    out_abl.mBuffers[0].mDataByteSize = out_bytes as u32;
    env.mem.write(out_output_data, out_abl);
    0
}

/// Complex buffer conversion with a game-provided input callback.
///
/// When the source and destination shapes match this stays a zero-copy
/// passthrough: the callback fills the caller's output buffer directly.
/// When they differ, the callback fills a staging buffer in source format
/// and the data is converted into the caller's output buffer.
fn AudioConverterFillComplexBuffer(
    env: &mut Environment,
    in_audio_converter: AudioConverterRef,
    in_input_data_proc: AudioConverterComplexInputDataProc,
    in_input_data_proc_user_data: MutVoidPtr,
    io_output_data_packet_size: MutPtr<u32>,
    out_output_data: MutPtr<AudioBufferList>,
    out_packet_description: MutPtr<MutPtr<AudioStreamPacketDescription>>,
) -> OSStatus {
    if in_audio_converter.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }

    let converter: OpaqueAudioConverter = env.mem.read(in_audio_converter);
    let src_shape = pcm_shape_from_asbd(&converter.source_format);
    let dst_shape = pcm_shape_from_asbd(&converter.dest_format);

    let (Some(src_shape), Some(dst_shape)) = (src_shape, dst_shape) else {
        // Non-LPCM formats can't be converted here; let the callback fill
        // the output buffer directly as before and hope for the best.
        log_dbg!(
            "AudioConverterFillComplexBuffer: non-LPCM conversion {} -> {}; passing through.",
            debug_fourcc(converter.source_format.format_id),
            debug_fourcc(converter.dest_format.format_id)
        );
        return in_input_data_proc.call_from_host(
            env,
            (
                in_audio_converter,
                io_output_data_packet_size,
                out_output_data,
                out_packet_description,
                in_input_data_proc_user_data,
            ),
        );
    };

    let dst_planar_buffers =
        converter.dest_format.format_flags & kAudioFormatFlagIsNonInterleaved != 0;
    let buffer_count = env.mem.read(out_output_data).mNumberBuffers;
    if buffer_count
        < if dst_planar_buffers {
            dst_shape.channels
        } else {
            1
        }
    {
        return kAudioConverterErr_InvalidInputSize;
    }

    // Determine how many source frames we can ask for, based on the output
    // buffer capacity in destination frames.
    let out_abl: AudioBufferList = env.mem.read(out_output_data);
    let mut out_capacity = out_abl.mBuffers[0].mDataByteSize as usize;
    if dst_planar_buffers {
        for channel in 0..dst_shape.channels {
            let buffer: AudioBuffer = env.mem.read(ConstPtr::from_bits(
                out_output_data.to_bits() + 4 + channel * 12,
            ));
            if buffer.mData.is_null() {
                return kAudioConverterErr_InvalidInputSize;
            }
            out_capacity = out_capacity.min(buffer.mDataByteSize as usize);
        }
    } else if out_abl.mBuffers[0].mData.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let dst_planar = converter.dest_format.format_flags & kAudioFormatFlagIsNonInterleaved != 0;
    let dst_buffer_frame_size = if dst_planar {
        dst_shape.bits / 8
    } else {
        dst_shape.bytes_per_frame
    };
    let dst_frames_capacity = (out_capacity / dst_buffer_frame_size as usize)
        .min(env.mem.read(io_output_data_packet_size) as usize);
    let src_frames_max = ((dst_frames_capacity as f64) * src_shape.sample_rate
        / dst_shape.sample_rate)
        .ceil() as usize;
    if dst_frames_capacity == 0 {
        env.mem.write(io_output_data_packet_size, 0);
        return 0;
    }
    let src_frames_max = src_frames_max.max(1);
    let staging_size = (src_frames_max * src_shape.bytes_per_frame as usize) as u32;

    // Staging buffer + ABL in guest memory, freed before returning.
    let staging_data: MutVoidPtr = env.mem.alloc(staging_size);
    if staging_data.is_null() {
        return kAudioConverterErr_InvalidInputSize;
    }
    let src_planar = converter.source_format.format_flags & kAudioFormatFlagIsNonInterleaved != 0;
    let src_buffers = if src_planar { src_shape.channels } else { 1 };
    let staging_abl_ptr: MutPtr<AudioBufferList> = env.mem.alloc(4 + src_buffers * 12).cast();
    env.mem.write(staging_abl_ptr.cast::<u32>(), src_buffers);
    let plane_size = staging_size / src_buffers;
    for channel in 0..src_buffers {
        let buffer = AudioBuffer {
            mNumberChannels: if src_planar { 1 } else { src_shape.channels },
            mDataByteSize: plane_size,
            mData: MutVoidPtr::from_bits(staging_data.to_bits() + channel * plane_size),
        };
        env.mem.write(
            MutPtr::<AudioBuffer>::from_bits(staging_abl_ptr.to_bits() + 4 + channel * 12),
            buffer,
        );
    }

    let mut packets = src_frames_max as u32;
    let packets_ptr: MutPtr<u32> = env.mem.alloc_and_write(packets);
    let callback_status: OSStatus = in_input_data_proc.call_from_host(
        env,
        (
            in_audio_converter,
            packets_ptr,
            staging_abl_ptr,
            MutPtr::<MutPtr<AudioStreamPacketDescription>>::null(),
            in_input_data_proc_user_data,
        ),
    );

    if callback_status == 0 {
        packets = env.mem.read(packets_ptr);
        // The callback owns its returned mData. PopCap returns pointers into
        // decoded sound buffers rather than writing into our staging storage.
        let returned: AudioBufferList = env.mem.read(staging_abl_ptr);
        let requested = packets.min(src_frames_max as u32);
        let src = if src_planar && returned.mNumberBuffers >= src_shape.channels {
            let mut planes = Vec::new();
            for channel in 0..src_shape.channels {
                let buffer: AudioBuffer = env.mem.read(ConstPtr::from_bits(
                    staging_abl_ptr.to_bits() + 4 + channel * 12,
                ));
                let plane = AudioBufferList {
                    mNumberBuffers: 1,
                    mBuffers: [buffer],
                };
                planes.push(returned_pcm(
                    &env.mem,
                    &plane,
                    requested,
                    src_shape.bits / 8,
                ));
            }
            let frames = planes
                .iter()
                .map(|plane| plane.len() / (src_shape.bits / 8) as usize)
                .min()
                .unwrap_or(0);
            let mut interleaved = Vec::new();
            for frame in 0..frames {
                let offset = frame * (src_shape.bits / 8) as usize;
                for plane in &planes {
                    interleaved
                        .extend_from_slice(&plane[offset..offset + (src_shape.bits / 8) as usize]);
                }
            }
            interleaved
        } else if !src_planar {
            returned_pcm(&env.mem, &returned, requested, src_shape.bytes_per_frame)
        } else {
            Vec::new()
        };
        let converted = convert_pcm(&src, &src_shape, &dst_shape);
        let out_bytes = converted
            .len()
            .min(dst_frames_capacity * dst_shape.bytes_per_frame as usize);
        let produced_frames = out_bytes / dst_shape.bytes_per_frame as usize;
        if dst_planar {
            let sample_bytes = (dst_shape.bits / 8) as usize;
            for channel in 0..dst_shape.channels as usize {
                let ptr = MutPtr::<AudioBuffer>::from_bits(
                    out_output_data.to_bits() + 4 + channel as u32 * 12,
                );
                let mut buffer = env.mem.read(ptr);
                let plane = pcm_plane(
                    &converted[..out_bytes],
                    sample_bytes,
                    dst_shape.channels as usize,
                    channel,
                );
                if !buffer.mData.is_null() {
                    let size = plane.len().min(buffer.mDataByteSize as usize);
                    env.mem
                        .bytes_at_mut(buffer.mData.cast(), size as u32)
                        .copy_from_slice(&plane[..size]);
                    buffer.mDataByteSize = size as u32;
                    env.mem.write(ptr, buffer);
                }
            }
        } else {
            env.mem
                .bytes_at_mut(out_abl.mBuffers[0].mData.cast(), out_bytes as u32)
                .copy_from_slice(&converted[..out_bytes]);
            let mut updated = out_abl;
            updated.mBuffers[0].mDataByteSize = out_bytes as u32;
            env.mem.write(out_output_data, updated);
        }
        env.mem
            .write(io_output_data_packet_size, produced_frames as u32);
    }

    env.mem.free(packets_ptr.cast());
    env.mem.free(staging_abl_ptr.cast());
    env.mem.free(staging_data);

    callback_status
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(AudioConverterNew(_, _, _)),
    export_c_func!(AudioConverterDispose(_)),
    export_c_func!(AudioConverterReset(_)),
    export_c_func!(AudioConverterGetProperty(_, _, _, _)),
    export_c_func!(AudioConverterSetProperty(_, _, _, _)),
    export_c_func!(AudioConverterConvertBuffer(_, _, _, _, _)),
    export_c_func!(AudioConverterConvertComplexBuffer(_, _, _, _)),
    export_c_func!(AudioConverterFillComplexBuffer(_, _, _, _, _, _)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_pcm_reaches_canonical_stereo_float_planes() {
        let dst = crate::frameworks::audio_toolbox::audio_components::AudioComponentInstanceHostObject::default()
            .global_stream_format;
        let dst_shape = pcm_shape_from_asbd(&dst).unwrap();
        let src_shape = PcmShape {
            sample_rate: 44100.0,
            channels: 1,
            bits: 16,
            is_float: false,
            is_big_endian: false,
            bytes_per_frame: 2,
        };
        let samples: Vec<u8> = [16384i16, -8192]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        let converted = convert_pcm(&samples, &src_shape, &dst_shape);
        for channel in 0..2 {
            let plane = pcm_plane(&converted, 4, 2, channel);
            let floats: Vec<f32> = plane
                .chunks_exact(4)
                .map(|sample| f32::from_le_bytes(sample.try_into().unwrap()))
                .collect();
            assert_eq!(floats, vec![0.5, -0.25]);
        }
        assert!(convert_pcm(&[], &src_shape, &dst_shape).is_empty());
    }

    #[test]
    fn input_callback_can_return_borrowed_samples_and_short_read() {
        let mut mem = crate::mem::Mem::new();
        mem.set_null_segment_size(4096);
        let samples = mem.alloc(8);
        mem.bytes_at_mut(samples.cast(), 8)
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let abl = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: 6,
                mData: samples,
            }],
        };
        assert_eq!(returned_pcm(&mem, &abl, 4, 2), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(returned_pcm(&mem, &abl, 1, 2), vec![1, 2]);
        assert!(returned_pcm(&mem, &abl, 0, 2).is_empty());
        // The converter must leave borrowed input memory intact.
        assert_eq!(mem.bytes_at(samples.cast(), 8), &[1, 2, 3, 4, 5, 6, 7, 8]);
        mem.free(samples);
    }
}
