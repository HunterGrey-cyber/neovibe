//! P2 feasibility probe (see docs/neovibe_feasibility_validation.md §5):
//!
//!     GtkApplicationWindow
//!     └── GtkGLArea (focusable, receives keyboard input)
//!         └── Skia Surface
//!             └── neovide::live_harness::LiveHarness
//!                 (patched Neovide fork, neovibe-integration branch)
//!                 └── real `nvim --embed` child process
//!
//! Direct sibling of `poc/neovide_embed` (P1): same GTK4 + `GtkGLArea` + Skia-wrapping-the-FBO
//! plumbing (`SkiaState`, `resolve_gl_proc`, `make_gl_interface`, `current_bound_framebuffer`,
//! `compute_content_region`'s resizable-viewport handling, the frame-pacing log) copied over
//! essentially unchanged -- see that crate's own doc comments for the full rationale on the
//! GL/Skia interop, which is not repeated here. The only architectural addition is real input:
//! this crate wires `GtkEventControllerKey` into `LiveHarness::send_text_input`, so a real
//! `nvim --embed` connection receives real keystrokes typed into the GTK window.
//!
//! ## What's new vs. P1
//!
//! - `neovide::demo_harness::DemoHarness` (fabricated content, no nvim) is replaced by
//!   `neovide::live_harness::LiveHarness` (real `nvim --embed` connection) per the P2 surgery
//!   phase's report.
//! - `GtkEventControllerKey` is attached to the `GLArea` (which is made focusable and grabs focus
//!   on startup) and forwards basic printable-character input --
//!   `gdk::Key::to_unicode()` plus a handful of named keys (Escape/Return/BackSpace/Tab) common
//!   enough that a human can actually drive nvim (enter insert mode, type, get back out, correct
//!   typos) -- through to `LiveHarness::send_text_input`. This is deliberately *not* full key-
//!   code/modifier translation fidelity (that is a later, dedicated input-system phase): any
//!   non-Shift modifier (Ctrl/Alt/Super) held down is treated as "not plain text" and ignored
//!   rather than forwarded, to avoid silently sending e.g. bare `c` for Ctrl+C.
//! - `LiveHarness::with_options` performs a real, synchronous, *blocking* call into
//!   `NeovimRuntime::launch` (spawns the child process and waits for the msgpack-rpc session to
//!   be established) -- unlike `DemoHarness::new`, which does no I/O at all. Per this phase's own
//!   task background (citing `poc/pump_events_spike/FINDINGS.md`'s "nothing on the shared GLib
//!   thread may block" constraint), that call is *not* hidden inside the same `render()` callback
//!   that first needs it: a "starting nvim..." placeholder frame is painted and presented first,
//!   and the actual (blocking) construction happens on the *next* callback, so the freeze -- which
//!   is real and was observed, see this crate's own `MANUAL_VERIFICATION.md` -- at least happens
//!   after the user sees an explanatory frame rather than a silently-frozen blank/garbage window.
//!   `LiveHarness::render_frame`'s own internal per-frame pump is separately confirmed
//!   non-blocking (`NON_BLOCKING = Duration::ZERO` in the harness's own source) and needs no such
//!   mitigation.
//! - On window close, `LiveHarness::shutdown()` is called and its return value logged, so a real
//!   `nvim --embed` child is never left orphaned behind this probe (see the `connect_close_request`
//!   handler below, and this crate's `MANUAL_VERIFICATION.md` for the `pgrep` diff used to confirm
//!   it empirically).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::gdk::{Key, ModifierType};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, EventControllerKey, GLArea};

use skia_safe::gpu::gl::{Format as GlFormat, FramebufferInfo, Interface as GlInterface};
use skia_safe::gpu::{backend_render_targets, direct_contexts, surfaces, DirectContext, SurfaceOrigin};
use skia_safe::{Canvas, Color4f, Paint, PaintStyle, Rect, Surface};

use neovide::live_harness::{LiveHarness, LiveHarnessOptions};
use neovide::units::PixelRect;

const APP_ID: &str = "cn.huntergrey.neovibe.neovide_embed_live";

/// Log a frame-pacing line every N frames instead of spamming stdout every frame.
const LOG_EVERY_N_FRAMES: u64 = 60;

/// How many `add_tick_callback` invocations between `[tick]` summary log lines (see `TickStats`).
/// The tick callback fires once per display frame (~144-165Hz on the dev machine this was
/// measured on per `poc/p11_measurements/PHASE_REPORT.md`), so 300 ticks is roughly a 2s window --
/// frequent enough to see idle-vs-active behavior change within a couple of seconds in the log,
/// without spamming stdout at display refresh rate.
const TICK_LOG_EVERY_N_TICKS: u64 = 300;

/// Inset (device pixels) between the GtkGLArea's own framebuffer edge and the rect handed to
/// `LiveHarness::render_frame` as `content_region`. Same rationale/value as P1's
/// `neovide_embed::CONTENT_MARGIN` -- a non-zero margin makes a viewport-containment bleed (or a
/// misplaced region) immediately visible instead of silently passing.
const CONTENT_MARGIN: f32 = 40.0;

/// Painted by us into the full framebuffer before each `LiveHarness::render_frame` call. Same
/// role as P1's `OUTSIDE_COLOR`: a color the renderer would never itself produce, so a viewport-
/// clear regression bleeding past `CONTENT_MARGIN` is visible at a glance.
const OUTSIDE_COLOR: Color4f = Color4f::new(0.55, 0.15, 0.55, 1.0);
const BORDER_COLOR: Color4f = Color4f::new(0.95, 0.85, 0.25, 1.0);
/// Painted across the whole `content_region` while `LiveHarness` is being constructed (the one
/// real, observed blocking call in this crate -- see this file's module doc) -- distinct from
/// both `OUTSIDE_COLOR` and anything the real renderer would draw, so it's obvious on screen
/// which phase is showing.
const STARTING_COLOR: Color4f = Color4f::new(0.12, 0.12, 0.16, 1.0);
/// Painted across `content_region` if `LiveHarness::with_options` itself returned `Err` (e.g. no
/// `nvim` on `$PATH`) -- distinct from every other state color here.
const FAILED_COLOR: Color4f = Color4f::new(0.5, 0.05, 0.05, 1.0);

/// GL-context-bound Skia state. Identical in spirit/implementation to
/// `neovide_embed::SkiaState` -- see that crate's own doc comment for the full rationale; nothing
/// about Skia/GL wrapping changes for a live nvim connection vs. the demo harness.
struct SkiaState {
    gr_context: DirectContext,
    surface: Option<Surface>,
    fb_width: i32,
    fb_height: i32,
}

impl SkiaState {
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

/// Carried over verbatim from `neovide_embed::current_bound_framebuffer` (itself carried over from
/// `gl_skia_test`) -- see that crate's doc comment for the full nm -D-verified explanation of the
/// local libepoxy quirk this works around.
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

/// Carried over verbatim from `neovide_embed::make_gl_interface`.
fn make_gl_interface() -> GlInterface {
    unsafe {
        let lib = libloading::os::unix::Library::this();
        GlInterface::new_load_with(move |name: &str| resolve_gl_proc(&lib, name))
            .expect("failed to assemble Skia GL interface via process-wide symbol lookup")
    }
}

/// Carried over verbatim from `neovide_embed::resolve_gl_proc`.
unsafe fn resolve_gl_proc(
    lib: &libloading::os::unix::Library,
    name: &str,
) -> *const std::ffi::c_void {
    unsafe {
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
        if let Ok(cname) = std::ffi::CString::new(name) {
            if let Ok(sym) = lib.get::<*const std::ffi::c_void>(cname.as_bytes_with_nul()) {
                return *sym;
            }
        }
        std::ptr::null()
    }
}

/// The pixel rect (within the GLArea's own framebuffer) handed to `LiveHarness::render_frame` as
/// `content_region`, inset from the framebuffer edges by `CONTENT_MARGIN` on every side (falling
/// back to the full framebuffer if it's too small for that margin to make sense). Identical to
/// `neovide_embed::compute_content_region`.
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

fn fill_content_region(canvas: &Canvas, content_region: &PixelRect<f32>, color: Color4f) {
    let mut paint = Paint::default();
    paint.set_color4f(color, None);
    canvas.draw_rect(
        Rect::from_ltrb(
            content_region.min.x,
            content_region.min.y,
            content_region.max.x,
            content_region.max.y,
        ),
        &paint,
    );
}

/// Lifecycle of the `LiveHarness` this window drives, kept explicit (rather than a bare
/// `Option<LiveHarness>`) so the render callback can paint a "starting nvim..." placeholder frame
/// *before* the one real, observed blocking call in this crate (`LiveHarness::with_options`) runs
/// -- see this file's module doc for why that ordering matters. `NotStarted` -> one placeholder
/// frame painted+presented -> `Starting` -> next render callback performs the blocking
/// construction -> `Ready`/`Failed`.
enum LiveState {
    NotStarted,
    Starting,
    Ready(Box<LiveSession>),
    Failed(String),
}

struct LiveSession {
    harness: LiveHarness,
    start: Instant,
    last_frame: Instant,
    frame_count: u64,
    logged_ready: bool,
    /// `render_frame`'s own returned `animating` value, set after every render callback
    /// invocation below. This is the P11-report-identified signal the tick callback was
    /// previously ignoring -- see `poc/p11_measurements/PHASE_REPORT.md`'s "headline finding".
    /// Starts `true` so the tick callback keeps rendering continuously through the first few
    /// Ready-state frames, until a real `render_frame` call has actually reported a real value --
    /// erring toward "render" rather than "skip" whenever this value hasn't been established yet.
    last_animating: Cell<bool>,
    /// `harness.redraw_batches_seen()` as of the last time either the render callback or the tick
    /// callback looked at it. The tick callback calls `LiveHarness::pump` every tick specifically
    /// so a change here is visible at full display-refresh-rate latency even on ticks that don't
    /// render -- this is the "did nvim actually send anything new" half of the fix, independent of
    /// `last_animating`.
    last_seen_batches: Cell<u64>,
    /// Set by the resize handler and the keyboard input handler: both are real external events
    /// that deserve a guaranteed next frame regardless of what `last_animating`/
    /// `last_seen_batches` currently say (a resize needs its own frame at the new size even if
    /// nvim sent nothing new; a keypress deserves a same-tick-latency render rather than waiting
    /// on nvim's async redraw round-trip to eventually move `last_seen_batches`). Read-and-cleared
    /// by the tick callback every tick.
    wants_frame: Cell<bool>,
}

impl LiveSession {
    fn new(harness: LiveHarness) -> Self {
        let now = Instant::now();
        Self {
            harness,
            start: now,
            last_frame: now,
            frame_count: 0,
            logged_ready: false,
            last_animating: Cell::new(true),
            last_seen_batches: Cell::new(0),
            wants_frame: Cell::new(false),
        }
    }

    /// Advance real elapsed time and return (dt, instantaneous_fps), matching
    /// `neovide_embed::DemoState::tick`'s own semantics exactly.
    fn tick(&mut self) -> (f32, f32) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.frame_count += 1;
        let fps = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        (dt, fps)
    }
}

/// Counts, over rolling windows of `TICK_LOG_EVERY_N_TICKS` tick-callback invocations, how many
/// ticks actually issued a `queue_render()` vs. how many skipped it because nothing needed another
/// frame -- this is the fix's own before/after evidence: an idle window should show `skipped`
/// dominating, while a typing/scrolling/animating window should show `issued` dominating. See
/// `poc/p11_measurements/PHASE_REPORT.md` for the bug this directly addresses.
struct TickStats {
    ticks: Cell<u64>,
    issued: Cell<u64>,
    skipped: Cell<u64>,
    issued_total: Cell<u64>,
    skipped_total: Cell<u64>,
}

impl TickStats {
    fn new() -> Self {
        Self {
            ticks: Cell::new(0),
            issued: Cell::new(0),
            skipped: Cell::new(0),
            issued_total: Cell::new(0),
            skipped_total: Cell::new(0),
        }
    }

    /// Record one tick's outcome; every `TICK_LOG_EVERY_N_TICKS` ticks, print a `[tick]` summary
    /// of the just-finished window and reset the windowed counters (the `_total` counters keep
    /// accumulating for the life of the process).
    fn record(&self, issued_this_tick: bool) {
        self.ticks.set(self.ticks.get() + 1);
        if issued_this_tick {
            self.issued.set(self.issued.get() + 1);
            self.issued_total.set(self.issued_total.get() + 1);
        } else {
            self.skipped.set(self.skipped.get() + 1);
            self.skipped_total.set(self.skipped_total.get() + 1);
        }

        if self.ticks.get() >= TICK_LOG_EVERY_N_TICKS {
            let ticks = self.ticks.get();
            let issued = self.issued.get();
            let skipped = self.skipped.get();
            println!(
                "[tick] last {ticks} ticks: issued={issued} skipped={skipped} \
                 skip_ratio={:.1}% (cumulative issued={} skipped={})",
                (skipped as f64 / ticks as f64) * 100.0,
                self.issued_total.get(),
                self.skipped_total.get(),
            );
            self.ticks.set(0);
            self.issued.set(0);
            self.skipped.set(0);
        }
    }
}

fn main() -> glib::ExitCode {
    // Passthrough convenience for manual verification runs, mirroring the fork's own
    // `examples/live_harness_offscreen.rs` choice to opt into `--clean` for determinism -- *not*
    // a change to `LiveHarnessOptions`'s own default, which stays "a real embedding host's actual
    // nvim config" per the surgery report. Pass `--clean` on this binary's own command line
    // (before GTK/glib get a chance to see it) to launch nvim with `--clean` instead.
    let want_clean = std::env::args().any(|arg| arg == "--clean");

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| build_ui(app, want_clean));
    app.run_with_args::<&str>(&[])
}

fn build_ui(app: &Application, want_clean: bool) {
    let gl_area = GLArea::builder()
        .hexpand(true)
        .vexpand(true)
        .has_stencil_buffer(true)
        .auto_render(true)
        .focusable(true)
        .can_focus(true)
        .build();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("neovibe P2: real nvim --embed (LiveHarness) in GtkGLArea")
        .default_width(1000)
        .default_height(700)
        .child(&gl_area)
        .build();

    let skia_state: Rc<RefCell<Option<SkiaState>>> = Rc::new(RefCell::new(None));
    let live_state: Rc<RefCell<LiveState>> = Rc::new(RefCell::new(LiveState::NotStarted));

    // --- resize: same rationale as neovide_embed's own resize handler -- GtkGLArea's FBO can be
    // resized/recreated under us, so drop the cached Surface and let the next render() rebuild it
    // against the new framebuffer dimensions (device pixels, not logical widget units). This is
    // also what feeds a fresh `content_region` into LiveHarness::render_frame on the very next
    // call -- P1's viewport-containment point, now proven against real nvim redraw traffic.
    {
        let skia_state = skia_state.clone();
        let live_state = live_state.clone();
        gl_area.connect_resize(move |widget, width, height| {
            // A resize is a real event that deserves a guaranteed next frame at the new size --
            // flag it for the tick callback regardless of whether nvim itself has anything new to
            // say (see `LiveSession::wants_frame`'s own doc).
            let live = live_state.borrow();
            let forced_next_frame = if let LiveState::Ready(session) = &*live {
                session.wants_frame.set(true);
                true
            } else {
                false
            };
            drop(live);
            println!(
                "[resize] fb={}x{}px scale_factor={} logical={}x{} forced_next_frame={}",
                width,
                height,
                widget.scale_factor(),
                widget.width(),
                widget.height(),
                forced_next_frame,
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

    // --- keyboard input: GtkEventControllerKey attached directly to the GLArea (made focusable
    // above, grab_focus()'d below once the window is shown). Deliberately not full key-code/
    // modifier translation fidelity -- see this file's module doc -- just enough plain-text input
    // to drive a real nvim buffer: printable characters via `Key::to_unicode()`, plus a handful of
    // named keys common enough to actually use nvim with (Escape to leave insert mode, Return,
    // BackSpace, Tab). Any held Ctrl/Alt/Super is treated as "not plain text" and ignored, rather
    // than silently forwarding e.g. bare `c` for what the user meant as Ctrl+C.
    {
        let live_state = live_state.clone();
        let key_controller = EventControllerKey::new();
        key_controller.connect_key_pressed(move |_controller, key, _keycode, state| {
            if state.intersects(
                ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::SUPER_MASK,
            ) {
                return glib::Propagation::Proceed;
            }

            let text: Option<&str> = match key {
                Key::Escape => Some("<Esc>"),
                Key::Return | Key::KP_Enter => Some("<CR>"),
                Key::BackSpace => Some("<BS>"),
                Key::Tab => Some("<Tab>"),
                _ => None,
            };

            let owned_char;
            let text = if let Some(text) = text {
                Some(text)
            } else if let Some(ch) = key.to_unicode() {
                if ch.is_control() {
                    None
                } else {
                    owned_char = ch.to_string();
                    Some(owned_char.as_str())
                }
            } else {
                None
            };

            let Some(text) = text else {
                return glib::Propagation::Proceed;
            };

            let mut live = live_state.borrow_mut();
            if let LiveState::Ready(session) = &mut *live {
                if !session.harness.has_neovim_exited() {
                    session.harness.send_text_input(text);
                    // A keypress deserves a same-tick-latency render rather than waiting on
                    // nvim's async redraw round-trip to eventually move `last_seen_batches`.
                    session.wants_frame.set(true);
                }
            }
            // Handled either way (even pre-Ready/post-exit) -- there is nothing else on this
            // single-widget window that should react to a keypress instead.
            glib::Propagation::Stop
        });
        gl_area.add_controller(key_controller);
    }

    // --- render: build (lazily) and drive one LiveHarness frame every tick. See the `LiveState`
    // doc for why construction is deliberately split across two render callbacks instead of
    // happening inline here.
    {
        let skia_state = skia_state.clone();
        let live_state = live_state.clone();
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
            let content_region = compute_content_region(fb_w, fb_h);
            let canvas = surface.canvas();

            // Paint the *entire* framebuffer a color the renderer would never itself produce,
            // then hand only the inset `content_region` to whatever's actually drawing this
            // frame -- same viewport-containment check as P1, now against real nvim traffic.
            canvas.clear(OUTSIDE_COLOR);

            let mut live = live_state.borrow_mut();
            match &mut *live {
                LiveState::NotStarted => {
                    // Paint+present a placeholder frame *before* the blocking
                    // LiveHarness::with_options call below ever runs (that call happens on the
                    // *next* render callback, once this state transition has actually been
                    // presented to the compositor) -- see this file's module doc.
                    println!(
                        "[live] showing 'starting nvim...' placeholder; LiveHarness::with_options \
                         will run (and block this thread) on the next frame"
                    );
                    fill_content_region(canvas, &content_region, STARTING_COLOR);
                    *live = LiveState::Starting;
                }
                LiveState::Starting => {
                    fill_content_region(canvas, &content_region, STARTING_COLOR);

                    let os_scale_factor = widget.scale_factor() as f64;
                    let options = LiveHarnessOptions {
                        os_scale_factor,
                        extra_nvim_args: if want_clean {
                            vec!["--clean".to_string()]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    };
                    println!(
                        "[live] constructing LiveHarness::with_options(os_scale_factor={os_scale_factor}, \
                         clean={want_clean}) -- this performs a real, synchronous nvim launch and \
                         WILL block the GTK main loop until it returns"
                    );
                    let t0 = Instant::now();
                    match LiveHarness::with_options(options) {
                        Ok(harness) => {
                            let elapsed = t0.elapsed();
                            println!(
                                "[live] LiveHarness::with_options returned after {elapsed:?} \
                                 (blocked the GTK main loop for that long)"
                            );
                            *live = LiveState::Ready(Box::new(LiveSession::new(harness)));
                        }
                        Err(err) => {
                            let elapsed = t0.elapsed();
                            let message = format!("{err:#}");
                            println!(
                                "[live] LiveHarness::with_options failed after {elapsed:?}: {message}"
                            );
                            *live = LiveState::Failed(message);
                        }
                    }
                }
                LiveState::Ready(session) => {
                    let (dt, fps) = session.tick();
                    let animating =
                        session.harness.render_frame(canvas, Some(&content_region), dt);
                    // Share this frame's "do we still need more frames" signals with the tick
                    // callback -- the fix this crate exists to validate (see PHASE_REPORT.md).
                    session.last_animating.set(animating);
                    session.last_seen_batches.set(session.harness.redraw_batches_seen());

                    if !session.logged_ready && session.harness.is_ready() {
                        session.logged_ready = true;
                        println!(
                            "[live] LiveHarness reports is_ready()=true after {:?} \
                             ({} redraw batch(es) applied) -- first real nvim content should be \
                             visible now",
                            session.start.elapsed(),
                            session.harness.redraw_batches_seen()
                        );
                    }

                    if session.frame_count.is_multiple_of(LOG_EVERY_N_FRAMES) {
                        let elapsed = session.start.elapsed().as_secs_f32();
                        println!(
                            "[frame {:>6}] t={:>7.2}s dt={:>6.2}ms instant_fps={:>6.1} avg_fps={:>6.1} \
                             fb={}x{} region={}x{}@({},{}) animating={} ready={} batches={} \
                             nvim_exited={}",
                            session.frame_count,
                            elapsed,
                            dt * 1000.0,
                            fps,
                            session.frame_count as f32 / elapsed.max(0.0001),
                            fb_w,
                            fb_h,
                            (content_region.max.x - content_region.min.x) as i32,
                            (content_region.max.y - content_region.min.y) as i32,
                            content_region.min.x as i32,
                            content_region.min.y as i32,
                            animating,
                            session.harness.is_ready(),
                            session.harness.redraw_batches_seen(),
                            session.harness.has_neovim_exited(),
                        );
                    }
                }
                LiveState::Failed(message) => {
                    fill_content_region(canvas, &content_region, FAILED_COLOR);
                    let _ = message; // already logged once when the transition happened
                }
            }
            drop(live);

            // Border traces exactly where `content_region` is, so a bleed (or a misplaced region)
            // is visible at a glance, in every LiveState -- same role as in neovide_embed.
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

            state.gr_context.flush_and_submit();

            glib::Propagation::Stop
        });
    }

    // --- drive redraws off the display's frame clock, same tick-callback plumbing as
    // neovide_embed/gl_skia_test (ties frame pacing to actual vsync-reported timing rather than a
    // fixed timer) -- but, per the P11 fix, only actually calls `queue_render()` when something
    // genuinely needs another frame. Previously this unconditionally requested a render on every
    // single tick (~144-165Hz on the P11 dev machine) forever, which is the entire root cause of
    // that phase's idle CPU/GPU finding (see `poc/p11_measurements/PHASE_REPORT.md`) -- `nvim`
    // itself was already idling at 0.0% CPU; only this host-shell tick loop was hot.
    {
        let live_state = live_state.clone();
        let gl_area_for_tick = gl_area.clone();
        let tick_stats = Rc::new(TickStats::new());
        gl_area.add_tick_callback(move |_widget, _clock| {
            let mut live = live_state.borrow_mut();
            let issued = match &mut *live {
                LiveState::Ready(session) => {
                    // Cheap, non-blocking drain of any nvim redraw traffic that arrived since the
                    // last tick. This does not touch the GL context or the Skia surface (that's
                    // `render_frame`'s job, called only from the render callback) -- it's safe,
                    // and per `LiveHarness::pump`'s own doc harmless, to call every display-frame
                    // tick regardless of whether this tick ends up rendering. Without this, a
                    // redraw batch nvim sent while we were otherwise idle would sit unapplied
                    // until something else happened to trigger a render.
                    session.harness.pump(Duration::ZERO);
                    let batches_now = session.harness.redraw_batches_seen();
                    let new_content = batches_now != session.last_seen_batches.get();
                    if new_content {
                        session.last_seen_batches.set(batches_now);
                    }

                    // Read-and-clear: a resize or a keypress since the last tick each force
                    // exactly one more frame, on top of the ongoing-animation and new-content
                    // signals above.
                    let wants_frame = session.wants_frame.replace(false);

                    session.last_animating.get() || new_content || wants_frame
                }
                // NotStarted/Starting: the placeholder-frame dance and the one blocking
                // `LiveHarness::with_options` call both happen *inside* the render callback and
                // only run when a render is actually requested -- keep rendering continuously
                // here so that state machine can advance (see `LiveState`'s own doc). Failed: a
                // rare terminal state; keep rendering rather than risk the one remaining
                // Starting->Failed state-transition frame never actually getting painted (that
                // transition sets the enum variant but doesn't itself paint FAILED_COLOR -- the
                // *next* render call does, in the `Failed` match arm).
                LiveState::NotStarted | LiveState::Starting | LiveState::Failed(_) => true,
            };
            drop(live);

            if issued {
                gl_area_for_tick.queue_render();
            }
            tick_stats.record(issued);

            glib::ControlFlow::Continue
        });
    }

    // --- shutdown: on window close, ask LiveHarness to cleanly quit its real nvim connection and
    // log whether a real NeovimExited was actually observed (LiveHarness::shutdown's own return
    // value) before letting the window actually close. This blocks for up to 5s in the worst case
    // (LiveHarness::shutdown's own documented wait) -- acceptable on the way out, unlike the
    // startup block this file's module doc discusses, since the app is exiting either way. Only
    // matters when LiveState::Ready was ever reached; NotStarted/Starting/Failed have no live
    // nvim connection to shut down.
    {
        let live_state = live_state.clone();
        window.connect_close_request(move |_window| {
            let mut live = live_state.borrow_mut();
            if let LiveState::Ready(session) = &mut *live {
                println!("[live] window closing: calling LiveHarness::shutdown()...");
                let exited_cleanly = session.harness.shutdown();
                println!(
                    "[live] LiveHarness::shutdown() returned {exited_cleanly} \
                     ({})",
                    if exited_cleanly {
                        "real NeovimExited observed"
                    } else {
                        "timed out waiting for NeovimExited -- nvim child may be orphaned, see \
                         LiveHarness::shutdown's own doc"
                    }
                );
            }
            glib::Propagation::Proceed
        });
    }

    window.present();
    gl_area.grab_focus();
    println!(
        "neovibe P2 probe running (LiveHarness/real nvim --embed in GtkGLArea). \
         initial window scale_factor={} clean={}",
        window.scale_factor(),
        want_clean
    );
}
