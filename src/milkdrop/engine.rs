//! projectM rendering inside the MilkDrop window.
//!
//! Rendering runs inside egui's paint callback, where the OpenGL context is
//! current. libprojectM 4.1 draws into the bottom-left of the window framebuffer,
//! so the result is copied into the display area before egui draws the frame.
//! The engine is dropped from eframe's exit hook while the context is current.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::Arc;

use eframe::glow::{self, HasContext};
use projectm_sys as pm;

use super::CROSSFADE_SECONDS;

/// One running projectM and the framebuffer its frames are copied
/// through.
pub struct Engine {
    handle: pm::projectm_handle,
    gl: Arc<glow::Context>,
    copy: Option<Copy>,
    seconds: u32,
}

/// A framebuffer the picture's size, with the texture it draws into.
struct Copy {
    framebuffer: glow::Framebuffer,
    texture: glow::Texture,
    width: u32,
    height: u32,
}

// SAFETY: only ever touched from the thread that owns the window's context,
// which is where the paint callback runs; the mutex it sits in wants Send.
unsafe impl Send for Engine {}

unsafe extern "C" fn switch_failed(
    preset_filename: *const c_char,
    message: *const c_char,
    _user_data: *mut c_void,
) {
    let text = |pointer: *const c_char| {
        if pointer.is_null() {
            String::new()
        } else {
            // SAFETY: projectM hands over C strings that live for the call.
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    };
    log::warn!(
        "MilkDrop could not load {}: {}",
        text(preset_filename),
        text(message)
    );
}

#[cfg(windows)]
unsafe extern "C" {
    /// GLEW, which libprojectM reads OpenGL through on Windows, has to be
    /// pointed at the context first; the library leaves that to whoever
    /// made the context.
    fn glewInit() -> u32;
}

impl Engine {
    /// Starts projectM in the current context, looking for presets'
    /// textures in `texture_dirs`. `None` when it cannot start, which means
    /// the context is short of OpenGL 3.3.
    pub fn new(gl: Arc<glow::Context>, texture_dirs: &[&Path], seconds: u32) -> Option<Self> {
        #[cfg(windows)]
        {
            static GLEW: std::sync::Once = std::sync::Once::new();
            // SAFETY: the context is current, which is all GLEW asks.
            GLEW.call_once(|| unsafe {
                glewInit();
            });
        }
        // SAFETY: the context is current; a null handle is the documented
        // way projectM says it cannot work in it.
        let handle = unsafe { pm::projectm_create() };
        if handle.is_null() {
            log::warn!("projectM could not start: MilkDrop needs OpenGL 3.3");
            return None;
        }
        // SAFETY: a fresh handle.
        unsafe {
            pm::projectm_set_preset_switch_failed_event_callback(
                handle,
                Some(switch_failed),
                std::ptr::null_mut(),
            );
            // `Presets::advance` decides when a preset ends. This is what
            // a preset reads as its `progress`, kept exact: left alone,
            // projectM varies each preset's length at random.
            pm::projectm_set_preset_duration(handle, f64::from(seconds));
            pm::projectm_set_easter_egg(handle, 0.0);
            pm::projectm_set_soft_cut_duration(handle, CROSSFADE_SECONDS);
            // Beat-driven cuts are off, as MilkDrop shipped them.
            pm::projectm_set_hard_cut_enabled(handle, false);
            pm::projectm_set_aspect_correction(handle, true);
            // MilkDrop's own mesh, on which the per-vertex equations run.
            pm::projectm_set_mesh_size(handle, 48, 36);
            pm::projectm_set_fps(handle, 60);
        }
        let engine = Self {
            handle,
            gl,
            copy: None,
            seconds,
        };
        engine.set_texture_dirs(texture_dirs);
        Some(engine)
    }

    fn set_texture_dirs(&self, dirs: &[&Path]) {
        let owned: Vec<CString> = dirs
            .iter()
            .filter_map(|dir| CString::new(dir.to_string_lossy().as_bytes()).ok())
            .collect();
        let mut pointers: Vec<*const c_char> = owned.iter().map(|dir| dir.as_ptr()).collect();
        // SAFETY: the strings live for the call, which copies them.
        unsafe {
            pm::projectm_set_texture_search_paths(
                self.handle,
                pointers.as_mut_ptr(),
                pointers.len(),
            );
        }
    }

    pub fn set_seconds(&mut self, seconds: u32) {
        if self.seconds != seconds {
            self.seconds = seconds;
            // SAFETY: a live handle.
            unsafe { pm::projectm_set_preset_duration(self.handle, f64::from(seconds)) };
        }
    }

    /// Loads a preset, fading into it or cutting straight to it. One that
    /// cannot be read is reported through the failure callback and the
    /// one playing stays.
    pub fn load(&mut self, path: &Path, smooth: bool) {
        let Ok(path) = CString::new(path.to_string_lossy().as_bytes()) else {
            return;
        };
        // SAFETY: a live handle and a string that lives for the call.
        unsafe { pm::projectm_load_preset_file(self.handle, path.as_ptr(), smooth) };
    }

    /// Feeds projectM stereo frames (LRLR pairs), from the shared ring.
    pub fn feed_frames(&mut self, frames: &[[f32; 2]]) {
        if frames.is_empty() {
            return;
        }
        // SAFETY: `frames` is `[f32; 2]` pairs laid out as LRLR, which is
        // what projectM reads `count` pairs of.
        unsafe {
            pm::projectm_pcm_add_float(
                self.handle,
                frames.as_ptr().cast::<f32>(),
                frames.len() as std::os::raw::c_uint,
                pm::projectm_channels_PROJECTM_STEREO,
            );
        }
    }

    /// Draws a frame into the picture: `left` and `bottom` from the
    /// window's bottom-left corner, all in pixels. `scale` divides the
    /// inner resolution: the picture is drawn small and stretched back up
    /// with hard pixels, easing a slower GPU.
    pub fn render(&mut self, left: i32, bottom: i32, width: u32, height: u32, scale: u32) {
        // projectM halves its viewport for the blur and warp passes, so an
        // odd width or height leaves a column or row those passes never
        // wrote, which its feedback then smears into a seam. Kept even, the
        // passes tile the picture cleanly.
        let width = width & !1;
        let height = height & !1;
        if width == 0 || height == 0 {
            return;
        }
        let scale = scale.max(1);
        let inner_w = (width / scale).max(2) & !1;
        let inner_h = (height / scale).max(2) & !1;
        if self
            .copy
            .as_ref()
            .is_none_or(|copy| (copy.width, copy.height) != (inner_w, inner_h))
        {
            self.resize(inner_w, inner_h);
        }
        let Some(copy) = &self.copy else {
            return;
        };
        let gl = &self.gl;
        let (w, h) = (inner_w as i32, inner_h as i32);
        let (full_w, full_h) = (width as i32, height as i32);
        // SAFETY: the context is current; every call is a plain GL call on
        // objects this engine made or the window owns.
        unsafe {
            // The picture lands in the window's own framebuffer, always.
            // Asking GL what is bound instead read back one of projectM's
            // internal framebuffers, left bound from the frame before, and
            // the stretched picture went into projectM's guts while the
            // window kept only the small corner projectM draws directly.
            let target = None;
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, target);
            gl.disable(glow::SCISSOR_TEST);
            gl.viewport(0, 0, w, h);
            pm::projectm_opengl_render_frame(self.handle);
            // projectM leaves its own scissor set to the picture it just
            // drew; left on, it clips the stretch below to that corner.
            gl.disable(glow::SCISSOR_TEST);
            // The frame is in the window's corner; the picture overlaps it,
            // and a framebuffer copied onto itself is anyone's guess, so it
            // goes through a copy.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(copy.framebuffer));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(copy.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, target);
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                left,
                bottom,
                left + full_w,
                bottom + full_h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            // The window is see-through wherever its alpha says so; the
            // picture is not, whatever a preset wrote there.
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(left, bottom, full_w, full_h);
            gl.color_mask(false, false, false, true);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.color_mask(true, true, true, true);
            // egui's painter puts the rest of its state back itself.
        }
    }

    /// Draws a frame off-screen and reads it into `pixels`, RGBA with the
    /// bottom row first. `width` and `height` must be even and `pixels` that
    /// many pixels long; the child's frame sizes come from `frames::fit`.
    pub fn render_into(&mut self, width: u32, height: u32, pixels: &mut [u8]) {
        if self
            .copy
            .as_ref()
            .is_none_or(|copy| (copy.width, copy.height) != (width, height))
        {
            self.resize(width, height);
        }
        let Some(copy) = &self.copy else {
            return;
        };
        let gl = &self.gl;
        let (w, h) = (width as i32, height as i32);
        // SAFETY: the context is current; every call is a plain GL call on
        // objects this engine made, and `pixels` holds what is read.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(copy.framebuffer));
            gl.disable(glow::SCISSOR_TEST);
            gl.viewport(0, 0, w, h);
            pm::projectm_opengl_render_frame_fbo(self.handle, copy.framebuffer.0.get());
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(copy.framebuffer));
            gl.disable(glow::SCISSOR_TEST);
            // The picture is opaque, whatever a preset wrote in its alpha.
            gl.color_mask(false, false, false, true);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.color_mask(true, true, true, true);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(pixels)),
            );
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        let gl = Arc::clone(&self.gl);
        // SAFETY: the context is current; the old objects are this
        // engine's own.
        unsafe {
            pm::projectm_set_window_size(self.handle, width as usize, height as usize);
            if let Some(old) = self.copy.take() {
                gl.delete_framebuffer(old.framebuffer);
                gl.delete_texture(old.texture);
            }
            let (Ok(texture), Ok(framebuffer)) = (gl.create_texture(), gl.create_framebuffer())
            else {
                log::warn!("MilkDrop could not make a framebuffer for its picture");
                return;
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            self.copy = Some(Copy {
                framebuffer,
                texture,
                width,
                height,
            });
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: the context is current (see the module's note), the
        // handle is live, and the objects are this engine's own.
        unsafe {
            pm::projectm_destroy(self.handle);
            if let Some(copy) = self.copy.take() {
                self.gl.delete_framebuffer(copy.framebuffer);
                self.gl.delete_texture(copy.texture);
            }
        }
    }
}
