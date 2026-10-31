/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Standalone AIFF/AIFC (Apple/SGI audio) → 16-bit little-endian PCM decoder.
//!
//! This is used as a fallback when [`super::symphonia_formats`] cannot probe
//! an AIFF/AIFC file. Symphonia 0.6's RIFF demuxer only implements a subset of
//! the AIFC compression types and fails the whole probe with
//! `Unsupported("aifc: compression type not supported")` for anything else,
//! which leaves games that ship companded audio (e.g. Red Ball 4's
//! `menu.aifc`, IMA 4:1 ADPCM) silent.
//!
//! Supported `AIFC`/`AIFF` compression types:
//! - `NONE`/`twos`: big-endian signed integer PCM (plain AIFF's only type,
//!   and uncompressed AIFC)
//! - `sowt`: little-endian signed integer PCM
//! - `raw `: big-endian unsigned integer PCM
//! - `fl32`/`FL32`, `fl64`/`FL64`: big-endian IEEE 754 float PCM
//! - `ulaw`/`ULAW` (G.711 µ-law) and `alaw`/`ALAW` (G.711 A-law), which are
//!   expanded in software
//! - `ima4`/`IMA4`: Apple QuickTime IMA ADPCM, 34-byte packets per channel
//!   (reuses the CAF decoder's [`super::ima4`] codec)
//!
//! References:
//! - AIFF/AIFC: [`Inside Macintosh: Sound`, "The Format of AIFF-C and AIFF
//!   Audio Files"](https://developer.apple.com/library/archive/documentation/mac/Sound/Sound-268.html)
//!   and Apple's [Audio File Format Description](https://developer.apple.com/library/archive/documentation/MusicAudio/Reference/AudioFileFormat/AudioFileFormats.html)
//! - MultimediaWiki's [Apple QuickTime IMA ADPCM](https://wiki.multimedia.cx/index.php?title=Apple_QuickTime_IMA_ADPCM)
//! - ITU-T Recommendation G.711 (µ-law and A-law expansion tables)

use super::ima4::decode_ima4;
use super::symphonia_formats::SymphoniaDecodedToPcm;

/// Decode an AIFF/AIFC ("FORM" + "AIFC"/"AIFF") file to 16-bit little-endian
/// interleaved PCM. Returns `Err` for anything this module doesn't handle, so
/// the caller can fall back.
#[allow(
    clippy::chunks_exact_to_as_chunks, // byte-slice iteration reads better here
    clippy::needless_range_loop, // the exponent table generator indexes two arrays
)]
pub fn decode_aifc_to_pcm(bytes: &[u8]) -> Result<SymphoniaDecodedToPcm, &'static str> {
    if std::env::var_os("TOUCHHLE_DISABLE_AIFC_DECODER").is_some() {
        return Err("TOUCHHLE_DISABLE_AIFC_DECODER is set");
    }
    if !super::is_aiff_aifc(bytes) {
        return Err("not an AIFF/AIFC file");
    }

    let mut comm: Option<CommChunk> = None;
    let mut ssnd: Option<&[u8]> = None;
    let mut pos = 12usize;
    while bytes.len() - pos >= 8 {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"COMM" => {
                comm = Some(parse_comm(body)?);
            }
            b"SSND" => {
                let (offset, block_size) = {
                    if body.len() < 8 {
                        return Err("SSND chunk too small");
                    }
                    (
                        u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize,
                        u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize,
                    )
                };
                // The sound data starts after the offset into it. A non-zero
                // block size means the data is padded to a block boundary,
                // which only matters for interleaved access — the payload we
                // hand back is flat, so both are consumed the same way.
                let data_start = 8 + offset;
                if data_start > body.len() {
                    return Err("SSND offset past end of chunk");
                }
                let data = if block_size == 0 {
                    &body[data_start..]
                } else {
                    // Trim trailing block padding: total size rounded up to
                    // block_size frames would exceed the chunk, so just take
                    // what's actually there frame by frame later. Apple says
                    // block-aligned readers must skip `block_size`-sized
                    // blocks; we decode the raw payload instead.
                    &body[data_start..]
                };
                ssnd = Some(data);
            }
            // INST, MARK, MIDI, AESD, APPL, FVER, … — ignorable.
            _ => {}
        }
        // IFF chunks are two-byte aligned.
        pos = body_start + size + (size & 1);
    }

    let comm = comm.ok_or("AIFC file had no COMM chunk")?;
    let data = ssnd.ok_or("AIFC file had no SSND chunk")?;
    let channels = comm.channels.max(1) as usize;
    if channels == 0 || comm.frames == 0 {
        return Err("AIFC file has no audio frames");
    }

    let out_pcm = match &comm.compression {
        // IMA 4:1 ADPCM: 34-byte packets (1 state byte + 33 data bytes) per
        // channel, each holding 64 frames. `frames_per_packet` is implied:
        // 64 per packet group.
        b"ima4" | b"IMA4" => {
            let packets = comm.frames.div_ceil(64) as usize;
            let packet_len = 34 * channels;
            if data.len() < packets * packet_len {
                return Err("SSND payload is truncated for IMA4");
            }
            let mut out = Vec::with_capacity(comm.frames as usize * channels * 2);
            for packet in 0..packets {
                // One channel state per channel, but each channel's packets
                // are stored contiguously per frame group.
                let mut decoded = [[0i16; 64]; 2];
                for (channel, slot) in decoded.iter_mut().take(channels).enumerate() {
                    let start = packet * packet_len + channel * 34;
                    let mut chunk = [0u8; 34];
                    chunk.copy_from_slice(&data[start..start + 34]);
                    *slot = decode_ima4(&chunk);
                }
                for frame in 0..64 {
                    for slot in decoded.iter().take(channels) {
                        out.extend_from_slice(&slot[frame].to_le_bytes());
                    }
                }
            }
            out
        }
        comp => {
            let target = match comp {
                b"NONE" | b"none" | b"twos" | b"TWOS" => match comm.sample_size {
                    8 => return Err("8-bit AIFF must be raw (unsigned)"),
                    16 => PcmTarget::Bits16BigEndianSigned,
                    24 => PcmTarget::Bits24BigEndianSigned,
                    32 => PcmTarget::Bits32BigEndianSigned,
                    _ => return Err("unsupported signed PCM sample size"),
                },
                b"sowt" | b"SOWT" => {
                    if comm.sample_size != 16 {
                        return Err("sowt sample size must be 16 bits");
                    }
                    PcmTarget::Bits16LittleEndianSigned
                }
                b"raw " | b"RAW " => {
                    if comm.sample_size != 8 {
                        return Err("raw sample size must be 8 bits");
                    }
                    PcmTarget::Bits8Unsigned
                }
                b"fl32" | b"FL32" => PcmTarget::Bits32BigEndianFloat,
                b"fl64" | b"FL64" => PcmTarget::Bits64BigEndianFloat,
                b"ulaw" | b"ULAW" => PcmTarget::Companded(Companding::ULaw),
                b"alaw" | b"ALAW" => PcmTarget::Companded(Companding::ALaw),
                other => {
                    log!("aifc_decoder: unsupported compression type {:?}", other);
                    return Err("unsupported AIFC compression type");
                }
            };
            let frame_len = sample_bytes(&target) * channels;
            let needed = comm.frames as usize * frame_len;
            if data.len() < needed {
                return Err("SSND payload is truncated");
            }
            let mut out = Vec::with_capacity(comm.frames as usize * channels * 2);
            let payload = &data[..needed];
            match target {
                PcmTarget::Bits16BigEndianSigned => {
                    for sample in payload.chunks_exact(2) {
                        out.extend_from_slice(
                            &i16::from_be_bytes([sample[0], sample[1]]).to_le_bytes(),
                        );
                    }
                }
                PcmTarget::Bits16LittleEndianSigned => {
                    out.extend_from_slice(payload);
                }
                PcmTarget::Bits8Unsigned => {
                    for &sample in payload {
                        out.extend_from_slice(&(((sample as i16) - 128) << 8).to_le_bytes());
                    }
                }
                PcmTarget::Bits24BigEndianSigned => {
                    for sample in payload.chunks_exact(3) {
                        let v = i32::from_be_bytes([0, sample[0], sample[1], sample[2]]);
                        out.extend_from_slice(&((v >> 16) as i16).to_le_bytes());
                    }
                }
                PcmTarget::Bits32BigEndianSigned => {
                    for sample in payload.chunks_exact(4) {
                        let v = i32::from_be_bytes(sample.try_into().unwrap());
                        out.extend_from_slice(&((v >> 16) as i16).to_le_bytes());
                    }
                }
                PcmTarget::Bits32BigEndianFloat => {
                    for sample in payload.chunks_exact(4) {
                        let v = f32::from_be_bytes(sample.try_into().unwrap());
                        out.extend_from_slice(&f32_to_i16(v).to_le_bytes());
                    }
                }
                PcmTarget::Bits64BigEndianFloat => {
                    for sample in payload.chunks_exact(8) {
                        let v = f64::from_be_bytes(sample.try_into().unwrap());
                        out.extend_from_slice(&f32_to_i16(v as f32).to_le_bytes());
                    }
                }
                PcmTarget::Companded(kind) => {
                    for &sample in payload {
                        let v = match kind {
                            Companding::ULaw => ulaw_to_i16(sample),
                            Companding::ALaw => alaw_to_i16(sample),
                        };
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
            out
        }
    };

    if out_pcm.is_empty() {
        return Err("decoded output is empty");
    }
    Ok(SymphoniaDecodedToPcm {
        bytes: out_pcm,
        sample_rate: comm.sample_rate,
        channels: u32::from(comm.channels),
    })
}

/// The `COMM` (Common) chunk: channels, frames, sample size, 80-bit extended
/// sample rate, and the AIFC compression type (with Pascal-string name for
/// `AIFC`; plain `AIFF` has an implied `NONE`).
struct CommChunk {
    channels: u16,
    frames: u32,
    sample_size: u16,
    sample_rate: u32,
    compression: [u8; 4],
}

fn parse_comm(body: &[u8]) -> Result<CommChunk, &'static str> {
    if body.len() < 18 {
        return Err("COMM chunk too small");
    }
    let channels = u16::from_be_bytes(body[0..2].try_into().unwrap());
    let frames = u32::from_be_bytes(body[2..6].try_into().unwrap());
    let sample_size = u16::from_be_bytes(body[6..8].try_into().unwrap());
    let sample_rate = extended_to_u32(&body[8..18]);
    let form_type = if body.len() >= 22 {
        &body[18..22]
    } else {
        b"NONE"
    };
    // AIFC's compression type is followed by a Pascal-style name that we
    // ignore; both `NONE` (big-endian) and `twos` mean the same layout here.
    let compression = match form_type {
        b"NONE" | b"none" => b"NONE",
        b"twos" => b"twos",
        b"sowt" => b"sowt",
        b"raw " => b"raw ",
        b"fl32" => b"fl32",
        b"FL32" => b"FL32",
        b"fl64" => b"fl64",
        b"FL64" => b"FL64",
        b"ulaw" => b"ulaw",
        b"ULAW" => b"ULAW",
        b"alaw" => b"alaw",
        b"ALAW" => b"ALAW",
        b"ima4" => b"ima4",
        b"IMA4" => b"IMA4",
        other => {
            log!("aifc_decoder: unsupported compression type {:?}", other);
            return Err("unsupported AIFC compression type");
        }
    };
    Ok(CommChunk {
        channels,
        frames,
        sample_size,
        sample_rate,
        compression: *compression,
    })
}

/// Convert an 80-bit IEEE 754 extended-precision float (the AIFF sample rate)
/// to an integer sample rate.
fn extended_to_u32(bytes: &[u8]) -> u32 {
    let bytes: [u8; 10] = bytes.try_into().expect("10-byte extended float");
    let exponent = u16::from_be_bytes([bytes[0] & 0x7f, bytes[1]]);
    let mantissa = u64::from_be_bytes(bytes[2..10].try_into().unwrap());
    if exponent == 0 && mantissa == 0 {
        return 0;
    }
    // value = mantissa * 2^(exponent - 16383 - 63). The sample rates we care
    // about are all integers well below 2^31, so a single right shift of the
    // mantissa (with rounding) reconstitutes them exactly.
    let shift = 16_383i32 - 63i32;
    let exp = i32::from(exponent) - shift; // unbiased exponent of the mantissa
    if exp >= 64 {
        return u32::MAX;
    }
    let value = if exp >= 0 {
        (mantissa as u128) << exp
    } else {
        let right = (-exp) as u32;
        if right >= 64 {
            0
        } else {
            let half = 1u64 << (right - 1);
            ((mantissa as u128) + (half as u128)) >> right
        }
    };
    value.min(u128::from(u32::MAX)) as u32
}

enum PcmTarget {
    Bits16BigEndianSigned,
    Bits16LittleEndianSigned,
    Bits8Unsigned,
    Bits24BigEndianSigned,
    Bits32BigEndianSigned,
    Bits32BigEndianFloat,
    Bits64BigEndianFloat,
    Companded(Companding),
}

enum Companding {
    ULaw,
    ALaw,
}

fn sample_bytes(target: &PcmTarget) -> usize {
    match target {
        PcmTarget::Bits16BigEndianSigned | PcmTarget::Bits16LittleEndianSigned => 2,
        PcmTarget::Bits8Unsigned => 1,
        PcmTarget::Bits24BigEndianSigned => 3,
        PcmTarget::Bits32BigEndianSigned | PcmTarget::Bits32BigEndianFloat => 4,
        PcmTarget::Bits64BigEndianFloat => 8,
        PcmTarget::Companded(_) => 1,
    }
}

fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// ITU-T G.711 µ-law expansion, matching the tables Symphonia's own WAV
/// companded decoder uses (`symphonia-codec-pcm`'s `mulaw` table).
fn ulaw_to_i16(byte: u8) -> i16 {
    ULAW_TABLE[byte as usize]
}

/// ITU-T G.711 A-law expansion (the `alaw` table in `symphonia-codec-pcm`).
fn alaw_to_i16(byte: u8) -> i16 {
    ALAW_TABLE[byte as usize]
}

// G.711 expansion tables. Generated from the standard algorithms (see
// https://github.com/ffmpeg/ffmpeg/blob/master/libavcodec/utils.c's
// `libavcodec/pcm.c` tables) rather than hand-typed: µ-law flips its bits
// (0x7F is silence), A-law does not (0x55 is silence).
#[rustfmt::skip]
static ULAW_TABLE: [i16; 256] = {
    let mut table = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        let u = (!(i as u8)) as u16;
        let sign = if u & 0x80 != 0 { -1i32 } else { 1i32 };
        let exponent = ((u >> 4) & 0x07) + 7;
        let mantissa = ((u & 0x0F) as i32) << (exponent - 5);
        let bias = 1i32 << ((exponent - 5) - 1);
        let v = sign * (mantissa + bias - 33);
        table[i] = if v < -32768 {
            -32768
        } else if v > 32767 {
            32767
        } else {
            v as i16
        };
        i += 1;
    }
    table
};

#[rustfmt::skip]
static ALAW_TABLE: [i16; 256] = {
    let mut table = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        let a = i as u8;
        let sign = if a & 0x80 != 0 { -1i32 } else { 1i32 };
        let v = if (a & 0x7F) == 0x55 {
            0
        } else {
            let exponent = (a >> 4) & 0x07;
            let mantissa = (a & 0x0F) as i32;
            let magnitude = if exponent == 0 {
                (mantissa << 4) + 8
            } else {
                ((mantissa | 0x10) << (exponent + 3)) - 128
            };
            sign * magnitude
        };
        table[i] = if v < -32768 {
            -32768
        } else if v > 32767 {
            32767
        } else {
            v as i16
        };
        i += 1;
    }
    table
};
