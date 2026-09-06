//! P1 feasibility probe (see docs/neovibe_feasibility_validation.md §4):
//!
//!     GtkApplicationWindow
//!     └── GtkGLArea
//!         └── Skia Surface
//!             └── neovide::demo_harness::DemoHarness
//!                 (patched Neovide fork, neovibe-integration branch)
//!
//! Proves the patched Neovide renderer can be driven frame-by-frame inside a `GtkGLArea`-hosted
//! Skia surface with no winit window/nvim connection of its own, and that it can be handed an
//! arbitrary, *resizable* sub-rect of that surface (not just "the whole canvas") — the actual
//! point of P1. No real Neovim runtime involved (that's P2+); rendering is driven entirely by
//! `DemoHarness`'s hand-built fabricated content.
//!
//! The GTK4/Skia/GL-interop plumbing (`SkiaState`, `resolve_gl_proc`, `make_gl_interface`,
//! `current_bound_framebuffer`) is carried over from `poc/gl_skia_test/src/main.rs` essentially
//! unchanged — that crate already solved wrapping a `GtkGLArea`'s own FBO as a Skia GPU `Surface`
//! on this machine, including the local libepoxy quirk (only `epoxy_glFoo` *data* symbols are
//! exported, not plain callable `glFoo` ones — see `resolve_gl_proc`'s doc comment below for the
//! full explanation). See that file/its MANUAL_VERIFICATION.md for the underlying proof; this
//! file only adds the demo-harness wiring on top.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, GLArea};

use skia_safe::gpu::gl::{Format as GlFormat, FramebufferInfo, Interface as GlInterface};
use skia_safe::gpu::{backend_render_targets, direct_contexts, surfaces, DirectContext, SurfaceOrigin};
use skia_safe::{Color4f, Paint, PaintStyle, Rect, Surface};

use neovide::demo_harness::DemoHarness;
use neovide::units::PixelRect;

const APP_ID: &str = "cn.huntergrey.neovibe.neovide_embed";

/// Log a frame-pacing line every N frames instead of spamming stdout every frame.
const LOG_EVERY_N_FRAMES: u64 = 60;

/// Inset (device pixels) between the GtkGLArea's own framebuffer edge and the rect handed to
/// `DemoHarness::render_frame` as `content_region`. Deliberately non-zero: passing a
/// `content_region` that does *not* start at pixel (0, 0) is exactly the case the surgery-phase
/// report flagged (`RenderedWindow::get_target_position` offsets by `grid_rect.min`, so a naive
/// implementation could paint the demo content at the canvas's absolute origin instead of inside
/// the caller's viewport) and exactly the case the P1 spec calls out ("an arbitrary, resizable
/// viewport instead of assuming it owns a whole window"). The margin area is left painted in
/// `OUTSIDE_COLOR` (by our own code, *not* by the harness) and bordered, so a bleed is
/// immediately visible on screen instead of silently passing.
const CONTENT_MARGIN: f32 = 40.0;

/// Painted by us into the full framebuffer before each `DemoHarness::render_frame` call. If the
/// P1 viewport-clear fix (`Renderer::draw_frame` clearing only its own clipped region) ever
/// regresses, this bleeds through past `CONTENT_MARGIN` and becomes visible immediately instead
/// of silently passing — deliberately not a color Neovide's own default background would produce.
const OUTSIDE_COLOR: Color4f = Color4f::new(0.55, 0.15, 0.55, 1.0);
const BORDER_COLOR: Color4f = Color4f::new(0.95, 0.85, 0.25, 1.0);

/// GL-context-bound Skia state. Created lazily on the first `render` callback (the GL context is
/// only guaranteed current inside GTK's render/resize signals), and its `surface` is torn down
/// and rebuilt whenever the GtkGLArea's framebuffer size changes. Identical in spirit to
/// `gl_skia_test::SkiaState` — see that file for the full rationale.
struct SkiaState {
    gr_context: DirectContext,
    surface: Option<Surface>,
    fb_width: i32,
    fb_height: i32,
}

impl SkiaState {
    /// (Re)build the Skia Surface wrapping the GL area's *current* draw framebuffer at the
    /// *current* size. Must be called with the GL context current (true inside GtkGLArea's
    /// "render" signal) because it queries GL_FRAMEBUFFER_BINDING to find the FBO id GtkGLArea is
    /// actually using -- GtkGLArea renders into its own internally managed FBO, not FBO 0, and
    /// that FBO's id is not guaranteed stable across resizes.
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

/// Look up `glGetIntegerv` via the same process-wide epoxy dispatch-pointer resolution as
/// `resolve_gl_proc` (see its doc comment for why a plain `dlsym("glGetIntegerv")` does not work
/// against this libepoxy build) and use it to read GL_FRAMEBUFFER_BINDING. Carried over verbatim
/// from `gl_skia_test::current_bound_framebuffer`.
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

/// Build Skia's GL function-pointer table by resolving each name against the process's already-
/// loaded dynamic symbols, rather than via `GlInterface::new_native()`. Carried over verbatim from
/// `gl_skia_test::make_gl_interface` — see that file's doc comment for why `new_native()` fails on
/// this Wayland/EGL session and why libepoxy needs the extra pointer indirection below.
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
        // Preferred: libepoxy's `epoxy_<name>` dispatch-pointer *variable*. See
        // gl_skia_test::resolve_gl_proc's doc comment for the full nm -D-verified explanation of
        // why this needs one extra manual pointer dereference beyond a plain dlsym.
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
        // Fallback: some loaders (or a differently-built libepoxy) export the plain name directly
        // as a callable function symbol.
        if let Ok(cname) = std::ffi::CString::new(name) {
            if let Ok(sym) = lib.get::<*const std::ffi::c_void>(cname.as_bytes_with_nul()) {
                return *sym;
            }
        }
        std::ptr::null()
    }
}

/// Frame-pacing bookkeeping for the demo harness, independent of the GL/Skia state. `dt` is real
/// wall-clock elapsed time since the previous frame, matching `DemoHarness::render_frame`'s doc
/// ("real elapsed wall-time is the natural choice for a live GTK render loop").
struct DemoState {
    harness: DemoHarness,
    start: Instant,
    last_frame: Instant,
    frame_count: u64,
}

impl DemoState {
    fn new(os_scale_factor: f64) -> Self {
        let now = Instant::now();
        Self {
            harness: DemoHarness::new(os_scale_factor),
            start: now,
            last_frame: now,
            frame_count: 0,
        }
    }

    /// Advance real elapsed time and return (dt, instantaneous_fps).
    fn tick(&mut self) -> (f32, f32) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.frame_count += 1;
        let fps = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        (dt, fps)
    }
}

/// The pixel rect (within the GLArea's own framebuffer) handed to `DemoHarness::render_frame` as
/// `content_region`, inset from the framebuffer edges by `CONTENT_MARGIN` on every side (falling
/// back to the full framebuffer if it's too small for that margin to make sense).
fn compute_content_region(fb_width: i32, fb_height: i32) -> PixelRect<f32> {
    let (w, h) = (fb_width as f32, fb_height as f32);
    if w > CONTENT_MARGIN * 2.0 + 20.0 && h > CONTENT_MARGIN * 2.0 + 20.0 {
        PixelRect::from_min_max(
            (CONTENT_MARGIN, CONTENT_MARGIN),
            (w - CONTENT_MARGIN, h - CONTENT_MARGIN),
        )
    } else {
        PixelRect::from_min_max((0.0, 0.0), (w.max(1.0), h.max(1.0)))
    }
}

fn main() -> glib::ExitCode {
    env_logger::init();
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
        .title("neovibe P1: Neovide DemoHarness in GtkGLArea")
        .default_width(1000)
        .default_height(700)
        .child(&gl_area)
        .build();

    let skia_state: Rc<RefCell<Option<SkiaState>>> = Rc::new(RefCell::new(None));
    let demo_state: Rc<RefCell<Option<DemoState>>> = Rc::new(RefCell::new(None));

    // --- resize: GtkGLArea's own FBO can be resized/recreated under us, so never assume a fixed
    // canvas size -- drop the cached Surface and let the next render() call rebuild it against
    // the new framebuffer dimensions. `width`/`height` here are already in device pixels (the GL
    // framebuffer size), not logical/DPI-independent widget units. This is also the resize path
    // that feeds a fresh `content_region` (via `compute_content_region`) into the demo harness on
    // the very next `render_frame` call below -- the actual thing P1 is trying to prove.
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
                    // GrContext not created yet; render() will pick up the GLArea's current size
                    // directly.
                }
            }
        });
    }

    // --- render: advance and draw one DemoHarness frame every tick.
    {
        let skia_state = skia_state.clone();
        let demo_state = demo_state.clone();
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
                *state_slot = Some(SkiaState { gr_context, surface: None, fb_width: width, fb_height: height });
            }

            let state = state_slot.as_mut().unwrap();
            state.ensure_surface();

            let Some(surface) = state.surface.as_mut() else {
                return glib::Propagation::Stop;
            };

            let (fb_w, fb_h) = (state.fb_width, state.fb_height);

            // DemoHarness::new spins up a full Renderer (fonts, settings, ...); GTK4 only reports
            // an integer widget scale factor, so it's forwarded straight through as
            // os_scale_factor. Built lazily here (first render callback) for the same reason
            // SkiaState is: it's the first point a real GL-area size is known, though DemoHarness
            // itself needs no GL context at all -- see the surgery-phase report.
            let mut demo_slot = demo_state.borrow_mut();
            if demo_slot.is_none() {
                let os_scale_factor = widget.scale_factor() as f64;
                println!("[init] DemoHarness::new(os_scale_factor={os_scale_factor})");
                *demo_slot = Some(DemoState::new(os_scale_factor));
            }
            let demo = demo_slot.as_mut().unwrap();

            let content_region = compute_content_region(fb_w, fb_h);
            let canvas = surface.canvas();

            // Paint the *entire* framebuffer a color the harness/renderer would never itself
            // produce, then hand the harness only the inset `content_region`. If the P1
            // viewport-clear fix ever regressed, the renderer would clear/paint the whole canvas
            // and this color would vanish -- instead it should persist untouched in the
            // `CONTENT_MARGIN`-wide border every frame.
            canvas.clear(OUTSIDE_COLOR);

            let (dt, fps) = demo.tick();
            let animating = demo.harness.render_frame(canvas, Some(&content_region), dt);

            // Border traces exactly where `content_region` is, so a bleed (or a misplaced region)
            // is visible at a glance rather than requiring pixel-probing like the upstream
            // offscreen example does.
            let mut border_paint = Paint::default();
            border_paint.set_anti_alias(true);
            border_paint.set_style(PaintStyle::Stroke);
            border_paint.set_stroke_width(2.0);
            border_paint.set_color4f(BORDER_COLOR, None);
            canvas.draw_rect(
                Rect::from_ltrb(
                    content_region.min.x,
                    content_region.min.y,
                    content_region.max.x,
                    content_region.max.y,
                ),
                &border_paint,
            );

            if demo.frame_count.is_multiple_of(LOG_EVERY_N_FRAMES) {
                let elapsed = demo.start.elapsed().as_secs_f32();
                println!(
                    "[frame {:>6}] t={:>7.2}s dt={:>6.2}ms instant_fps={:>6.1} avg_fps={:>6.1} fb={}x{} region={}x{}@({},{}) animating={}",
                    demo.frame_count,
                    elapsed,
                    dt * 1000.0,
                    fps,
                    demo.frame_count as f32 / elapsed.max(0.0001),
                    fb_w,
                    fb_h,
                    (content_region.max.x - content_region.min.x) as i32,
                    (content_region.max.y - content_region.min.y) as i32,
                    content_region.min.x as i32,
                    content_region.min.y as i32,
                    animating,
                );
            }

            state.gr_context.flush_and_submit();

            glib::Propagation::Stop
        });
    }

    // --- drive a continuous redraw off the display's frame clock (ties frame pacing to actual
    // vsync-reported timing at whatever the monitor's real refresh rate is -- 60/120/144/165Hz --
    // rather than a fixed timer), same pattern as gl_skia_test.
    {
        let gl_area_for_tick = gl_area.clone();
        gl_area.add_tick_callback(move |_widget, _clock| {
            gl_area_for_tick.queue_render();
            glib::ControlFlow::Continue
        });
    }

    window.present();
    println!(
        "neovibe P1 probe running (DemoHarness in GtkGLArea). initial window scale_factor={}",
        window.scale_factor()
    );
}
