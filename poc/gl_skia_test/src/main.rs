//! P0 feasibility probe (see docs/neovibe_feasibility_validation.md §3):
//!
//!     GtkApplicationWindow
//!     └── GtkGLArea
//!         └── Skia Canvas
//!             ├── text
//!             ├── rectangles
//!             └── animation
//!
//! This is a throwaway test program, not product code: it exists only to prove
//! `GTK4 -> GtkGLArea -> OpenGL -> Skia Surface` can render smoothly and survive
//! resize/HiDPI. No Neovide/Neovim involved (that's P1+).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, GLArea};

use skia_safe::gpu::gl::{Format as GlFormat, FramebufferInfo, Interface as GlInterface};
use skia_safe::gpu::{backend_render_targets, direct_contexts, surfaces, DirectContext, SurfaceOrigin};
use skia_safe::{Color4f, Font, Paint, PaintStyle, Point, Rect, Surface};

const APP_ID: &str = "cn.huntergrey.neovibe.gl_skia_test";

/// Log a frame-pacing line every N frames instead of spamming stdout every frame.
const LOG_EVERY_N_FRAMES: u64 = 60;

/// GL-context-bound Skia state. Created lazily on the first `render` callback
/// (the GL context is only guaranteed current inside GTK's render/resize
/// signals), and its `surface` is torn down and rebuilt whenever the
/// GtkGLArea's framebuffer size changes.
struct SkiaState {
    gr_context: DirectContext,
    surface: Option<Surface>,
    fb_width: i32,
    fb_height: i32,
}

impl SkiaState {
    /// (Re)build the Skia Surface wrapping the GL area's *current* draw
    /// framebuffer at the *current* size. Must be called with the GL context
    /// current (true inside GtkGLArea's "render" signal) because it queries
    /// GL_FRAMEBUFFER_BINDING to find the FBO id GtkGLArea is actually using
    /// -- GtkGLArea renders into its own internally managed FBO, not FBO 0,
    /// and that FBO's id is not guaranteed stable across resizes.
    fn ensure_surface(&mut self) {
        if self.surface.is_some() {
            return;
        }
        if self.fb_width <= 0 || self.fb_height <= 0 {
            return;
        }

        let fboid = current_bound_framebuffer();
        let fb_info = FramebufferInfo {
            fboid: fboid as u32,
            format: GlFormat::RGBA8.into(),
            ..Default::default()
        };

        let render_target = backend_render_targets::make_gl(
            (self.fb_width, self.fb_height),
            0, // sample_count: no explicit MSAA, GtkGLArea isn't configured for it
            8, // stencil_bits: GLArea is built with has_stencil_buffer(true)
            fb_info,
        );

        let surface = surfaces::wrap_backend_render_target(
            &mut self.gr_context,
            &render_target,
            SurfaceOrigin::BottomLeft,
            skia_safe::ColorType::RGBA8888,
            None,
            None,
        )
        .expect("failed to wrap GtkGLArea framebuffer as a Skia Surface");

        self.surface = Some(surface);
    }
}

/// Look up `glGetIntegerv` via the same process-wide epoxy dispatch-pointer
/// resolution as `resolve_gl_proc` (see its doc comment for why a plain
/// `dlsym("glGetIntegerv")` does not work against this libepoxy build) and
/// use it to read GL_FRAMEBUFFER_BINDING. This avoids depending on
/// skia-safe's internal GL interface (which does not expose a generic
/// proc-address getter to Rust callers) just to answer one query GtkGLArea
/// itself doesn't expose through gtk4-rs.
fn current_bound_framebuffer() -> i32 {
    const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    unsafe {
        let lib = libloading::os::unix::Library::this();
        let proc_addr = resolve_gl_proc(&lib, "glGetIntegerv");
        if proc_addr.is_null() {
            return 0;
        }
        let get_integerv: unsafe extern "C" fn(u32, *mut i32) = std::mem::transmute(proc_addr);
        let mut fbo: i32 = 0;
        get_integerv(GL_FRAMEBUFFER_BINDING, &mut fbo as *mut i32);
        fbo
    }
}

/// Build Skia's GL function-pointer table by resolving each name against the
/// process's already-loaded dynamic symbols, rather than via
/// `GlInterface::new_native()`.
///
/// `new_native()` guesses a *platform* binding (GLX on X11, WGL on Windows,
/// etc.) and on this Wayland/EGL session that guess fails outright (verified
/// against this environment: it returned `None`, see MANUAL_VERIFICATION.md
/// notes / run log). GDK's own GL context is created via libepoxy, which is
/// already loaded process-wide by the time GtkGLArea's "render" signal
/// fires -- but this libepoxy build does *not* export plain callable symbols
/// like `glClear`; `nm -D libepoxy.so.0` shows only `D` (data) symbols named
/// `epoxy_glClear` etc. -- these are the function-pointer *variables* that
/// `<epoxy/gl.h>`'s `#define glClear epoxy_glClear` macro expands calls
/// through. So resolution here does the same thing C code compiled against
/// epoxy's headers does: dlsym the `epoxy_<name>` variable, then read the
/// function pointer value stored in it (one level of indirection) -- rather
/// than treating the dlsym'd address itself as the function, which is what
/// broke the first attempt at this (see git history on this file).
fn make_gl_interface() -> GlInterface {
    unsafe {
        let lib = libloading::os::unix::Library::this();
        GlInterface::new_load_with(move |name: &str| resolve_gl_proc(&lib, name))
            .expect("failed to assemble Skia GL interface via process-wide symbol lookup")
    }
}

unsafe fn resolve_gl_proc(
    lib: &libloading::os::unix::Library,
    name: &str,
) -> *const std::ffi::c_void {
    unsafe {
        // Preferred: libepoxy's `epoxy_<name>` dispatch-pointer *variable*.
        //
        // `libloading::Symbol<T>::deref` reinterprets the raw address dlsym
        // returns as a value of type T -- correct for ordinary function
        // symbols, where the symbol's address already *is* the callable
        // entry point. `epoxy_<name>` is a *data* symbol though (confirmed
        // via `nm -D libepoxy.so.0`: it's listed `D`, and the `epoxy_glFoo`
        // symbols sit at consecutive addresses 8 bytes apart, i.e. an array
        // of pointer-sized slots) -- dlsym on it gives the address of the
        // slot, not the function pointer stored inside it. So getting it as
        // `Symbol<*const c_void>` and calling `*sym` only recovers that slot
        // address; an extra plain-Rust pointer read is needed to actually
        // load the function pointer value out of the slot. Skipping this
        // step was the cause of an earlier SIGSEGV here (jumping into the
        // slot's raw pointer bytes as if they were code -- see git history).
        if let Ok(epoxy_name) = std::ffi::CString::new(format!("epoxy_{name}")) {
            if let Ok(sym) = lib.get::<*const std::ffi::c_void>(epoxy_name.as_bytes_with_nul()) {
                let slot: *const *const std::ffi::c_void = *sym as *const *const std::ffi::c_void;
                if !slot.is_null() {
                    let fn_ptr = *slot;
                    if !fn_ptr.is_null() {
                        return fn_ptr;
                    }
                }
            }
        }
        // Fallback: some loaders (or a differently-built libepoxy) export the
        // plain name directly as a callable function symbol.
        if let Ok(cname) = std::ffi::CString::new(name) {
            if let Ok(sym) = lib.get::<*const std::ffi::c_void>(cname.as_bytes_with_nul()) {
                return *sym;
            }
        }
        std::ptr::null()
    }
}

/// Frame-pacing / animation bookkeeping, independent of the GL/Skia state.
struct AnimState {
    start: Instant,
    last_frame: Instant,
    frame_count: u64,
    angle_deg: f32,
    bounce_x: f32,
    bounce_dir: f32,
}

impl AnimState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last_frame: now,
            frame_count: 0,
            angle_deg: 0.0,
            bounce_x: 0.0,
            bounce_dir: 1.0,
        }
    }

    /// Advance animation state by `dt` seconds and return (dt, instantaneous_fps).
    fn tick(&mut self, bounce_track_width: f32) -> (f32, f32) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.frame_count += 1;

        // Rotation: 90 deg/sec, wraps at 360.
        self.angle_deg = (self.angle_deg + dt * 90.0) % 360.0;

        // Bounce: ping-pongs across the available track width at 220 px/sec.
        let speed = 220.0;
        self.bounce_x += self.bounce_dir * speed * dt;
        if self.bounce_x > bounce_track_width {
            self.bounce_x = bounce_track_width;
            self.bounce_dir = -1.0;
        } else if self.bounce_x < 0.0 {
            self.bounce_x = 0.0;
            self.bounce_dir = 1.0;
        }

        let fps = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        (dt, fps)
    }
}

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let gl_area = GLArea::builder()
        .hexpand(true)
        .vexpand(true)
        .has_stencil_buffer(true)
        .auto_render(true)
        .build();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("neovibe P0: GtkGLArea + Skia probe")
        .default_width(1000)
        .default_height(700)
        .child(&gl_area)
        .build();

    let skia_state: Rc<RefCell<Option<SkiaState>>> = Rc::new(RefCell::new(None));
    let anim_state = Rc::new(RefCell::new(AnimState::new()));

    // --- resize: GtkGLArea's own FBO can be resized/recreated under us, so
    // never assume a fixed canvas size -- drop the cached Surface and let the
    // next render() call rebuild it against the new framebuffer dimensions.
    // `width`/`height` here are already in device pixels (the GL framebuffer
    // size), not logical/DPI-independent widget units.
    {
        let skia_state = skia_state.clone();
        gl_area.connect_resize(move |widget, width, height| {
            println!(
                "[resize] fb={}x{}px scale_factor={} logical={}x{}",
                width,
                height,
                widget.scale_factor(),
                widget.width(),
                widget.height(),
            );
            let mut state = skia_state.borrow_mut();
            match state.as_mut() {
                Some(state) => {
                    state.fb_width = width;
                    state.fb_height = height;
                    state.surface = None; // force rebuild on next render
                }
                None => {
                    // GrContext not created yet; render() will pick up the
                    // GLArea's current size directly.
                }
            }
        });
    }

    // --- render: draw text + rectangles + a running animation every frame.
    {
        let skia_state = skia_state.clone();
        let anim_state = anim_state.clone();
        gl_area.connect_render(move |widget, _gl_ctx| {
            let mut state_slot = skia_state.borrow_mut();

            if state_slot.is_none() {
                let interface = make_gl_interface();
                let gr_context = direct_contexts::make_gl(interface, None)
                    .expect("failed to create Skia GL DirectContext");
                let width = widget.width() * widget.scale_factor();
                let height = widget.height() * widget.scale_factor();
                println!(
                    "[init] Skia DirectContext created; initial fb={}x{}px scale_factor={}",
                    width,
                    height,
                    widget.scale_factor()
                );
                *state_slot = Some(SkiaState {
                    gr_context,
                    surface: None,
                    fb_width: width,
                    fb_height: height,
                });
            }

            let state = state_slot.as_mut().unwrap();
            state.ensure_surface();

            let Some(surface) = state.surface.as_mut() else {
                return glib::Propagation::Stop;
            };

            let (fb_w, fb_h) = (state.fb_width as f32, state.fb_height as f32);
            let canvas = surface.canvas();

            canvas.clear(Color4f::new(0.09, 0.10, 0.13, 1.0));

            // --- text ---
            let mut font = Font::default();
            font.set_size(28.0);
            let mut text_paint = Paint::default();
            text_paint.set_color4f(Color4f::new(0.92, 0.93, 0.95, 1.0), None);
            text_paint.set_anti_alias(true);
            canvas.draw_str(
                "neovibe P0: GtkGLArea + Skia",
                Point::new(24.0, 40.0),
                &font,
                &text_paint,
            );

            let mut anim = anim_state.borrow_mut();
            let bounce_track_width = (fb_w - 120.0).max(0.0);
            let (dt, fps) = anim.tick(bounce_track_width);

            let mut stats_font = Font::default();
            stats_font.set_size(18.0);
            let stats_text = format!(
                "frame {:>6}  dt={:>6.2}ms  fps={:>6.1}  fb={}x{}  scale={}",
                anim.frame_count,
                dt * 1000.0,
                fps,
                state.fb_width,
                state.fb_height,
                widget.scale_factor(),
            );
            canvas.draw_str(&stats_text, Point::new(24.0, 70.0), &stats_font, &text_paint);

            // --- rectangles ---
            let mut rect_paint = Paint::default();
            rect_paint.set_anti_alias(true);

            rect_paint.set_color4f(Color4f::new(0.86, 0.36, 0.36, 1.0), None);
            rect_paint.set_style(PaintStyle::Fill);
            canvas.draw_rect(Rect::from_xywh(24.0, 100.0, 160.0, 90.0), &rect_paint);

            rect_paint.set_color4f(Color4f::new(0.36, 0.72, 0.86, 1.0), None);
            canvas.draw_rect(Rect::from_xywh(210.0, 100.0, 160.0, 90.0), &rect_paint);

            rect_paint.set_color4f(Color4f::new(0.45, 0.86, 0.45, 1.0), None);
            rect_paint.set_style(PaintStyle::Stroke);
            rect_paint.set_stroke_width(4.0);
            canvas.draw_rect(Rect::from_xywh(396.0, 100.0, 160.0, 90.0), &rect_paint);

            // --- animation: a rotating square ---
            let cx = fb_w * 0.5;
            let cy = 300.0f32.min((fb_h - 40.0).max(40.0));
            canvas.save();
            canvas.translate((cx, cy));
            canvas.rotate(anim.angle_deg, None);
            let mut rot_paint = Paint::default();
            rot_paint.set_anti_alias(true);
            rot_paint.set_color4f(Color4f::new(0.95, 0.75, 0.25, 1.0), None);
            canvas.draw_rect(Rect::from_xywh(-40.0, -40.0, 80.0, 80.0), &rot_paint);
            canvas.restore();

            // --- animation: a bouncing circle ---
            let bounce_y = (cy + 140.0).min((fb_h - 30.0).max(30.0));
            let mut circle_paint = Paint::default();
            circle_paint.set_anti_alias(true);
            circle_paint.set_color4f(Color4f::new(0.68, 0.52, 0.95, 1.0), None);
            canvas.draw_circle(Point::new(24.0 + anim.bounce_x + 30.0, bounce_y), 24.0, &circle_paint);

            if anim.frame_count.is_multiple_of(LOG_EVERY_N_FRAMES) {
                let elapsed = anim.start.elapsed().as_secs_f32();
                println!(
                    "[frame {:>6}] t={:>7.2}s dt={:>6.2}ms instant_fps={:>6.1} avg_fps={:>6.1}",
                    anim.frame_count,
                    elapsed,
                    dt * 1000.0,
                    fps,
                    anim.frame_count as f32 / elapsed.max(0.0001),
                );
            }
            drop(anim);

            state.gr_context.flush_and_submit();

            glib::Propagation::Stop
        });
    }

    // --- drive a continuous redraw off the display's frame clock (ties our
    // animation to actual vsync-reported timing at whatever the monitor's
    // real refresh rate is -- 60/120/144/165Hz -- rather than a fixed timer).
    {
        let gl_area_for_tick = gl_area.clone();
        gl_area.add_tick_callback(move |_widget, _clock| {
            gl_area_for_tick.queue_render();
            glib::ControlFlow::Continue
        });
    }

    window.present();
    println!(
        "neovibe P0 probe running. initial window scale_factor={}",
        window.scale_factor()
    );
}
