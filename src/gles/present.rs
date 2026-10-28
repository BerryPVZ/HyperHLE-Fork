/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Utilities for presenting frames to the window using an abstract OpenGL ES
//! implementation.

use super::gles11_raw as gles11; // constants and types only
use super::GLES;
use crate::matrix::Matrix;
use std::time::{Duration, Instant};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

const FRAME_HISTORY_SIZE: usize = 48;

pub struct FpsCounter {
    time: Instant,
    last_frame: Instant,
    frames: u32,
}

static LAST_FPS_TEXT: OnceLock<Mutex<String>> = OnceLock::new();
static GLYPH_TEXTURES: OnceLock<Mutex<Option<Vec<u32>>>> = OnceLock::new();
static ONSCREEN_FPS_ENABLED: AtomicBool = AtomicBool::new(false);
static ONSCREEN_FPS_CONFIGURED: AtomicBool = AtomicBool::new(false);
static FRAME_TIME_HISTORY: [AtomicU32; FRAME_HISTORY_SIZE] =
    [const { AtomicU32::new(0) }; FRAME_HISTORY_SIZE];
static FRAME_TIME_HISTORY_INDEX: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_CORNER: AtomicUsize = AtomicUsize::new(0);

// DIAG (TOUCHHLE_STALL_DIAG): epoch-millis of the last presented frame, updated
// at the top of present_frame. The main run loop reads this to detect a guest
// hang (frames stop being presented while the guest keeps burning CPU) and dump
// the spinning thread's stack. 0 = nothing presented yet.
static LAST_PRESENT_MS: AtomicU64 = AtomicU64::new(0);

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Milliseconds since the last presented frame, or `None` if no frame has been
/// presented yet.
pub fn ms_since_last_present() -> Option<u64> {
    let last = LAST_PRESENT_MS.load(Ordering::Relaxed);
    if last == 0 {
        None
    } else {
        Some(epoch_ms().saturating_sub(last))
    }
}

impl FpsCounter {
    pub fn start() -> Self {
        LAST_FPS_TEXT.get_or_init(|| Mutex::new(String::new()));
        GLYPH_TEXTURES.get_or_init(|| Mutex::new(None));
        let now = Instant::now();
        FpsCounter {
            time: now,
            last_frame: now,
            frames: 0,
        }
    }

    pub fn count_frame(&mut self, label: std::fmt::Arguments<'_>, log_output: bool) {
        self.frames += 1;
        let now = Instant::now();
        let frame_duration = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        if onscreen_fps_enabled() {
            let index =
                FRAME_TIME_HISTORY_INDEX.fetch_add(1, Ordering::Relaxed) % FRAME_HISTORY_SIZE;
            FRAME_TIME_HISTORY[index].store(frame_duration.to_bits(), Ordering::Relaxed);
        }
        let duration = now - self.time;
        if duration >= Duration::from_secs(1) {
            self.time = now;
            let fps = std::mem::take(&mut self.frames) as f32 / duration.as_secs_f32();
            #[cfg(target_os = "ios")]
            crate::report_host_fps(fps);
            if log_output {
                echo!("touchHLE: {} FPS: {:.2}", label, fps);
            }
            if onscreen_fps_enabled() {
                let text = format!("FPS {:.1}", fps);
                if let Some(mutex) = LAST_FPS_TEXT.get() {
                    if let Ok(mut s) = mutex.lock() {
                        *s = text;
                    }
                }
            }
        }
    }
}

pub fn onscreen_fps_enabled() -> bool {
    if ONSCREEN_FPS_CONFIGURED.load(Ordering::SeqCst) {
        ONSCREEN_FPS_ENABLED.load(Ordering::SeqCst)
    } else {
        crate::env_flag_cached!("TOUCHHLE_ONSCREEN_FPS")
    }
}

pub fn set_onscreen_fps_enabled(enabled: bool) {
    let was_enabled = onscreen_fps_enabled();
    ONSCREEN_FPS_ENABLED.store(enabled, Ordering::SeqCst);
    ONSCREEN_FPS_CONFIGURED.store(true, Ordering::SeqCst);
    if enabled && !was_enabled {
        for sample in &FRAME_TIME_HISTORY {
            sample.store(0, Ordering::Relaxed);
        }
        FRAME_TIME_HISTORY_INDEX.store(0, Ordering::Relaxed);
        if let Some(text) = LAST_FPS_TEXT.get() {
            if let Ok(mut text) = text.lock() {
                text.clear();
            }
        }
    }
}

pub fn cycle_onscreen_fps_corner() -> &'static str {
    let corner = OVERLAY_CORNER
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1)
        % 4;
    match corner {
        0 => "top-left",
        1 => "top-right",
        2 => "bottom-right",
        _ => "bottom-left",
    }
}

/// Present the the latest frame (e.g. the app's splash screen or rendering
/// output), provided as a texture bound to `GL_TEXTURE_2D`, by drawing it on
/// the window. It may be rotated, scaled and/or letterboxed as necessary. The
/// virtual cursor is also drawn if it should be currently visible.
///
/// The provided context must be current.
pub unsafe fn present_frame(
    gles: &mut dyn GLES,
    viewport: (u32, u32, u32, u32),
    rotation_matrix: Matrix<2>,
    virtual_cursor_visible_at: Option<(f32, f32, bool)>,
) {
    // While this is a generic utility, it is closely tied to
    // crate::frameworks::opengles::eagl::present_renderbuffer, which handles
    // backing up and restoring OpenGL ES state that this function might touch,
    // so these need to be updated in tandem.

    use gles11::types::*;

    // DIAG: record that a frame reached the present path.
    LAST_PRESENT_MS.store(epoch_ms(), Ordering::Relaxed);

    // Draw the quad
    gles.Viewport(
        viewport.0 as _,
        viewport.1 as _,
        viewport.2 as _,
        viewport.3 as _,
    );
    gles.ClearColor(0.0, 0.0, 0.0, 1.0);
    // PERF: only the color buffer needs clearing here. The present quad runs
    // with depth/stencil testing disabled (the caller disables caps before
    // drawing), so clearing DEPTH|STENCIL is a wasted full-viewport pass —
    // noticeable at scale-hack 4x on software rasterizers.
    gles.Clear(gles11::COLOR_BUFFER_BIT);
    // Keep the window's alpha channel at the 1.0 the clear just wrote. The
    // frame texture's alpha is meaningless for an opaque CAEAGLLayer, but a
    // window surface with an alpha channel may be blended by the OS
    // compositor (Android's SurfaceFlinger), which would show through
    // wherever the app left alpha < 1.
    gles.ColorMask(gles11::TRUE, gles11::TRUE, gles11::TRUE, gles11::FALSE);
    gles.BindBuffer(gles11::ARRAY_BUFFER, 0);
    // Stretch the full rendered frame to fill the active host viewport.
    //
    // This does NOT crop or shift the texture. The whole renderbuffer is
    // sampled from normal 0..1 texture coordinates and mapped to a full-screen
    // quad. This is the correct "fill the current window" behavior for
    // PotatoGold-style landscape tests.
    // PERF: cached read-once flag; present_frame runs every frame.
    if crate::env_flag_cached!("TOUCHHLE_PRESENT_STRETCH_TO_VIEWPORT") {
        log_once!(
            "TOUCHHLE_PRESENT_STRETCH_TO_VIEWPORT=1: stretching full rendered frame to the active viewport [this log will only be shown once]"
        );
    }

    let vertices: [f32; 12] = [
        -1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0,
    ];
    gles.EnableClientState(gles11::VERTEX_ARRAY);
    gles.VertexPointer(2, gles11::FLOAT, 0, vertices.as_ptr() as *const GLvoid);
    #[allow(unused_mut)]
    let mut tex_coords: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    #[cfg(target_os = "ios")]
    for coords in tex_coords.chunks_exact_mut(2) {
        let transformed = rotation_matrix.transform([coords[0] - 0.5, coords[1] - 0.5]);
        coords[0] = transformed[0] + 0.5;
        coords[1] = transformed[1] + 0.5;
    }
    gles.EnableClientState(gles11::TEXTURE_COORD_ARRAY);
    gles.TexCoordPointer(2, gles11::FLOAT, 0, tex_coords.as_ptr() as *const GLvoid);
    #[cfg(not(target_os = "ios"))]
    {
    // Apply the device-rotation matrix to the TEXTURE matrix, but rotate
    // around the centre of the tex coord square (0.5, 0.5) instead of the
    // origin. The naive `LoadMatrixf(rotation_matrix)` rotates around (0, 0),
    // which sends standard [0, 1]² UVs out of range — e.g. for a 90° rotation
    // (v, -u) reaches v' = -u ∈ [-1, 0]. On lenient drivers (Mesa, Apple
    // PowerVR) GL_REPEAT wrap quietly maps that back into [0, 1], but
    // strict drivers (Qualcomm Adreno's native ES 1.1 path) treat the
    // resulting sample of an NPOT texture (renderbuffer is typically
    // 320x480) as undefined and produce a mangled / black presented
    // frame. Pre- and post-translating by (0.5, 0.5) keeps tex coords in
    // [0, 1]² for any 90°/180°/270°/identity device rotation while
    // producing the same visual output as before on lenient drivers.
    let r = Matrix::<4>::from(&rotation_matrix);
    let to_center = Matrix::<4>::translate_3d(-0.5, -0.5, 0.0);
    let from_center = Matrix::<4>::translate_3d(0.5, 0.5, 0.0);
    // Note: Matrix::multiply(&other) computes `other · self` in
    // linear-algebra terms (other is applied AFTER self), so to get
    // `from_center · r · to_center` we chain in the order
    // to_center.multiply(&r).multiply(&from_center).
    let centered_rotation = to_center.multiply(&r).multiply(&from_center);
    gles.MatrixMode(gles11::TEXTURE);
    gles.LoadMatrixf(centered_rotation.columns().as_ptr() as *const _);
    }
    #[cfg(target_os = "ios")]
    {
    gles.MatrixMode(gles11::TEXTURE);
    gles.LoadIdentity();
    }
    gles.Enable(gles11::TEXTURE_2D);
    gles.DrawArrays(gles11::TRIANGLES, 0, 6);
    // clean this up so we don't need to worry about it in e.g. Core Animation
    gles.LoadIdentity();

    // Display virtual cursor
    if let Some((x, y, pressed)) = virtual_cursor_visible_at {
        let (vx, vy, vw, vh) = viewport;
        let x = x - vx as f32;
        let y = y - vy as f32;

        gles.DisableClientState(gles11::TEXTURE_COORD_ARRAY);
        gles.Disable(gles11::TEXTURE_2D);

        gles.Enable(gles11::BLEND);
        gles.BlendFunc(gles11::ONE, gles11::ONE_MINUS_SRC_ALPHA);
        gles.Color4f(0.0, 0.0, 0.0, if pressed { 2.0 / 3.0 } else { 1.0 / 3.0 });

        let radius = 10.0;

        let mut vertices = vertices;
        for i in (0..vertices.len()).step_by(2) {
            vertices[i] = (vertices[i] * radius + x) / (vw as f32 / 2.0) - 1.0;
            vertices[i + 1] = 1.0 - (vertices[i + 1] * radius + y) / (vh as f32 / 2.0);
        }
        gles.VertexPointer(2, gles11::FLOAT, 0, vertices.as_ptr() as *const GLvoid);
        gles.DrawArrays(gles11::TRIANGLES, 0, 6);
    }

    if onscreen_fps_enabled() {
        if let Some(mutex) = LAST_FPS_TEXT.get() {
            if let Ok(text) = mutex.lock() {
                if !text.is_empty() {
                    draw_onscreen_panel(gles, viewport, &text);
                }
            }
        }
    }

    // Cheat Engine-style trainer overlay (floating button + panel). Drawn
    // last so it sits on top of everything, including the FPS overlay.
    crate::trainer_ui::draw(gles, viewport);

    gles.ColorMask(gles11::TRUE, gles11::TRUE, gles11::TRUE, gles11::TRUE);
}

// --- Tiny bitmap font & overlay drawing implementation ---
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 8;

const GLYPH_CHARS: &str = "0123456789:.FPSM";
// Each glyph is 8 bytes, each bit is a pixel (MSB left).
const GLYPH_BITMAPS: &[[u8; 8]] = &[
    // 0
    [0x3C, 0x66, 0x6E, 0x7E, 0x76, 0x66, 0x3C, 0x00],
    // 1
    [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
    // 2
    [0x3C, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x7E, 0x00],
    // 3
    [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00],
    // 4
    [0x0C, 0x1C, 0x3C, 0x6C, 0x7E, 0x0C, 0x1E, 0x00],
    // 5
    [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00],
    // 6
    [0x3C, 0x66, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00],
    // 7
    [0x7E, 0x66, 0x0C, 0x18, 0x18, 0x18, 0x18, 0x00],
    // 8
    [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
    // 9
    [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x66, 0x3C, 0x00],
    // : (colon)
    [0x00, 0x18, 0x18, 0x00, 0x00, 0x18, 0x18, 0x00],
    // . (dot)
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
    // F
    [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
    // P
    [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
    // S
    [0x3C, 0x66, 0x30, 0x1C, 0x06, 0x66, 0x3C, 0x00],
    // M
    [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00],
];

fn glyph_index(ch: char) -> Option<usize> {
    GLYPH_CHARS.chars().position(|c| c == ch)
}

unsafe fn ensure_glyph_textures(gles: &mut dyn GLES) -> Option<Vec<u32>> {
    use gles11::types::*;
    let lock = GLYPH_TEXTURES.get().unwrap().lock().unwrap();
    if lock.is_some() {
        return lock.clone();
    }
    drop(lock);

    let mut guard = GLYPH_TEXTURES.get().unwrap().lock().unwrap();
    if guard.is_some() {
        return guard.clone();
    }

    let count = GLYPH_BITMAPS.len();
    let mut texs = Vec::with_capacity(count);
    for i in 0..count {
        let mut tex: GLuint = 0;
        gles.GenTextures(1, &mut tex);
        gles.BindTexture(gles11::TEXTURE_2D, tex);
        // Build RGBA data from bitmap
        let mut data = vec![0u8; (GLYPH_W * GLYPH_H * 4) as usize];
        let bmp = GLYPH_BITMAPS[i];
        for y in 0..GLYPH_H {
            let row = bmp[y as usize];
            for x in 0..GLYPH_W {
                let bit = (row >> (7 - x)) & 1;
                let idx = ((y * GLYPH_W + x) * 4) as usize;
                if bit != 0 {
                    data[idx] = 255; // R
                    data[idx + 1] = 255;
                    data[idx + 2] = 255;
                    data[idx + 3] = 255; // A
                } else {
                    data[idx] = 0;
                    data[idx + 1] = 0;
                    data[idx + 2] = 0;
                    data[idx + 3] = 0;
                }
            }
        }
        gles.TexImage2D(
            gles11::TEXTURE_2D,
            0,
            gles11::RGBA as _,
            GLYPH_W as _,
            GLYPH_H as _,
            0,
            gles11::RGBA,
            gles11::UNSIGNED_BYTE,
            data.as_ptr() as *const _,
        );
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_MIN_FILTER,
            gles11::NEAREST as _,
        );
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_MAG_FILTER,
            gles11::NEAREST as _,
        );
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_WRAP_S,
            gles11::CLAMP_TO_EDGE as _,
        );
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_WRAP_T,
            gles11::CLAMP_TO_EDGE as _,
        );
        texs.push(tex);
    }
    *guard = Some(texs.clone());
    Some(texs)
}

unsafe fn draw_onscreen_panel(gles: &mut dyn GLES, viewport: (u32, u32, u32, u32), fps_text: &str) {
    use gles11::types::*;

    let (_, _, viewport_width, viewport_height) = viewport;
    if viewport_width < 24 || viewport_height < 24 {
        return;
    }

    let scale = if viewport_width >= 240 && viewport_height >= 80 {
        2.0
    } else {
        1.0
    };
    let desired_width: f32 = if scale == 2.0 { 240.0 } else { 180.0 };
    let panel_width = desired_width.min(viewport_width.saturating_sub(16) as f32);
    let panel_height = if scale == 2.0 { 64.0 } else { 42.0 };
    let margin = 8.0;
    let corner = OVERLAY_CORNER.load(Ordering::Relaxed) % 4;
    let panel_x = if corner == 1 || corner == 2 {
        (viewport_width as f32 - panel_width - margin).max(0.0)
    } else {
        margin
    };
    let panel_y = if corner >= 2 {
        (viewport_height as f32 - panel_height - margin).max(0.0)
    } else {
        margin
    };
    let font_scale = scale;
    let glyph_width = GLYPH_W as f32 * font_scale;
    let glyph_height = GLYPH_H as f32 * font_scale;
    let fps = fps_text
        .strip_prefix("FPS ")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0);
    let latest_sample = FRAME_TIME_HISTORY_INDEX.load(Ordering::Relaxed);
    let frame_ms = if latest_sample == 0 {
        0.0
    } else {
        f32::from_bits(
            FRAME_TIME_HISTORY[(latest_sample - 1) % FRAME_HISTORY_SIZE].load(Ordering::Relaxed),
        )
    };
    let color = if fps >= 50.0 {
        [0.28, 0.92, 0.56]
    } else if fps >= 30.0 {
        [1.0, 0.72, 0.28]
    } else {
        [1.0, 0.34, 0.34]
    };

    let mut old_active_texture: GLint = 0;
    gles.GetIntegerv(gles11::ACTIVE_TEXTURE, &mut old_active_texture);
    let mut old_texture: GLint = 0;
    gles.GetIntegerv(gles11::TEXTURE_BINDING_2D, &mut old_texture);
    let mut old_matrix_mode: GLint = 0;
    gles.GetIntegerv(gles11::MATRIX_MODE, &mut old_matrix_mode);
    let mut old_blend_src: GLint = 0;
    let mut old_blend_dst: GLint = 0;
    gles.GetIntegerv(gles11::BLEND_SRC, &mut old_blend_src);
    gles.GetIntegerv(gles11::BLEND_DST, &mut old_blend_dst);
    let old_blend = gles.IsEnabled(gles11::BLEND);
    let old_texture_2d = gles.IsEnabled(gles11::TEXTURE_2D);
    let mut old_vertex_array: GLboolean = gles11::FALSE;
    let mut old_texture_array: GLboolean = gles11::FALSE;
    gles.GetBooleanv(gles11::VERTEX_ARRAY, &mut old_vertex_array);
    gles.GetBooleanv(gles11::TEXTURE_COORD_ARRAY, &mut old_texture_array);

    let Some(textures) = ensure_glyph_textures(gles) else {
        gles.BindTexture(gles11::TEXTURE_2D, old_texture as _);
        gles.ActiveTexture(old_active_texture as _);
        return;
    };

    gles.MatrixMode(gles11::PROJECTION);
    gles.PushMatrix();
    gles.LoadIdentity();
    gles.Orthof(
        0.0,
        viewport_width as _,
        viewport_height as _,
        0.0,
        -1.0,
        1.0,
    );
    gles.MatrixMode(gles11::MODELVIEW);
    gles.PushMatrix();
    gles.LoadIdentity();
    gles.EnableClientState(gles11::VERTEX_ARRAY);
    gles.DisableClientState(gles11::TEXTURE_COORD_ARRAY);
    gles.Disable(gles11::TEXTURE_2D);
    gles.Enable(gles11::BLEND);
    gles.BlendFunc(gles11::SRC_ALPHA, gles11::ONE_MINUS_SRC_ALPHA);

    let panel_vertices = [
        panel_x,
        panel_y,
        panel_x + panel_width,
        panel_y,
        panel_x,
        panel_y + panel_height,
        panel_x + panel_width,
        panel_y + panel_height,
    ];
    gles.VertexPointer(
        2,
        gles11::FLOAT,
        0,
        panel_vertices.as_ptr() as *const GLvoid,
    );
    gles.Color4f(0.025, 0.035, 0.055, 0.88);
    gles.DrawArrays(gles11::TRIANGLE_STRIP, 0, 4);

    let accent_vertices = [
        panel_x,
        panel_y,
        panel_x + 3.0,
        panel_y,
        panel_x,
        panel_y + panel_height,
        panel_x + 3.0,
        panel_y + panel_height,
    ];
    gles.VertexPointer(
        2,
        gles11::FLOAT,
        0,
        accent_vertices.as_ptr() as *const GLvoid,
    );
    gles.Color4f(color[0], color[1], color[2], 1.0);
    gles.DrawArrays(gles11::TRIANGLE_STRIP, 0, 4);

    let graph_width = (panel_width - 122.0).clamp(0.0, 54.0);
    let graph_height = panel_height - 22.0;
    let graph_x = panel_x + panel_width - graph_width - 10.0;
    let graph_y = panel_y + (panel_height - graph_height) / 2.0;
    if graph_width >= 30.0 {
        let graph_bg = [
            graph_x,
            graph_y,
            graph_x + graph_width,
            graph_y,
            graph_x,
            graph_y + graph_height,
            graph_x + graph_width,
            graph_y + graph_height,
        ];
        gles.VertexPointer(2, gles11::FLOAT, 0, graph_bg.as_ptr() as *const GLvoid);
        gles.Color4f(0.12, 0.15, 0.19, 0.9);
        gles.DrawArrays(gles11::TRIANGLE_STRIP, 0, 4);

        let mut bars = [0.0f32; FRAME_HISTORY_SIZE * 12];
        let mut vertices = 0usize;
        let next_sample = FRAME_TIME_HISTORY_INDEX.load(Ordering::Relaxed);
        let bar_width = graph_width / FRAME_HISTORY_SIZE as f32;
        for column in 0..FRAME_HISTORY_SIZE {
            let sample = f32::from_bits(
                FRAME_TIME_HISTORY[(next_sample + column) % FRAME_HISTORY_SIZE]
                    .load(Ordering::Relaxed),
            );
            if sample <= 0.0 {
                continue;
            }
            let height = (sample / 40.0).clamp(0.06, 1.0) * (graph_height - 2.0);
            let left = graph_x + column as f32 * bar_width;
            let right = left + (bar_width - 0.35).max(0.5);
            let bottom = graph_y + graph_height - 1.0;
            let top = bottom - height;
            bars[vertices..vertices + 12].copy_from_slice(&[
                left, top, left, bottom, right, top, right, top, left, bottom, right, bottom,
            ]);
            vertices += 12;
        }
        if vertices > 0 {
            gles.VertexPointer(2, gles11::FLOAT, 0, bars.as_ptr() as *const GLvoid);
            gles.Color4f(color[0], color[1], color[2], 0.95);
            gles.DrawArrays(gles11::TRIANGLES, 0, (vertices / 2) as _);
        }
    }

    gles.Enable(gles11::TEXTURE_2D);
    gles.EnableClientState(gles11::TEXTURE_COORD_ARRAY);
    let texture_coords: [f32; 8] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
    let mut draw_text = |text: &str, x: f32, y: f32, tint: [f32; 4]| {
        gles.Color4f(tint[0], tint[1], tint[2], tint[3]);
        let mut x_px = x;
        for ch in text.chars() {
            if let Some(index) = glyph_index(ch) {
                gles.BindTexture(gles11::TEXTURE_2D, textures[index]);
                let x1 = x_px + glyph_width;
                let y1 = y + glyph_height;
                let vertices = [x_px, y, x_px, y1, x1, y, x1, y1];
                gles.VertexPointer(2, gles11::FLOAT, 0, vertices.as_ptr() as *const GLvoid);
                gles.TexCoordPointer(
                    2,
                    gles11::FLOAT,
                    0,
                    texture_coords.as_ptr() as *const GLvoid,
                );
                gles.DrawArrays(gles11::TRIANGLE_STRIP, 0, 4);
                x_px += glyph_width + 2.0;
            } else {
                x_px += glyph_width / 2.0;
            }
        }
    };

    draw_text(
        fps_text,
        panel_x + 10.0,
        panel_y + if scale == 2.0 { 8.0 } else { 5.0 },
        [color[0], color[1], color[2], 1.0],
    );
    draw_text(
        &format!("{frame_ms:.1} MS"),
        panel_x + 10.0,
        panel_y + if scale == 2.0 { 34.0 } else { 21.0 },
        [0.78, 0.82, 0.88, 1.0],
    );
    drop(draw_text);

    gles.BindTexture(gles11::TEXTURE_2D, old_texture as _);
    gles.ActiveTexture(old_active_texture as _);
    if old_blend != gles11::TRUE {
        gles.Disable(gles11::BLEND);
    }
    if old_texture_2d != gles11::TRUE {
        gles.Disable(gles11::TEXTURE_2D);
    }
    if old_vertex_array != gles11::TRUE {
        gles.DisableClientState(gles11::VERTEX_ARRAY);
    }
    if old_texture_array != gles11::TRUE {
        gles.DisableClientState(gles11::TEXTURE_COORD_ARRAY);
    }
    gles.BlendFunc(old_blend_src as _, old_blend_dst as _);
    gles.MatrixMode(gles11::MODELVIEW);
    gles.PopMatrix();
    gles.MatrixMode(gles11::PROJECTION);
    gles.PopMatrix();
    gles.MatrixMode(old_matrix_mode as _);
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn overlay_corner_cycles_in_clockwise_order() {
        let start = OVERLAY_CORNER.load(Ordering::SeqCst);
        OVERLAY_CORNER.store(0, Ordering::SeqCst);
        assert_eq!(cycle_onscreen_fps_corner(), "top-right");
        assert_eq!(cycle_onscreen_fps_corner(), "bottom-right");
        assert_eq!(cycle_onscreen_fps_corner(), "bottom-left");
        assert_eq!(cycle_onscreen_fps_corner(), "top-left");
        OVERLAY_CORNER.store(start, Ordering::SeqCst);
    }
}
