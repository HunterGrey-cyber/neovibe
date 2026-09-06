//! pump_events_spike
//!
//! Throwaway feasibility spike. Answers ONE question:
//!
//!   Can a `winit` + GL window, driven via `EventLoopExtPumpEvents::pump_app_events`
//!   instead of `run_app`, coexist inside a GTK4 application's own GLib main loop
//!   (`gtk::Application::run()`), side by side with real GTK4 widgets, without
//!   stalling either loop, losing input, or breaking resize/redraw?
//!
//! See ../FINDINGS.md for the verdict. This is not production code: no Skia, no
//! real Neovide, minimal error handling, single file.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::prelude::*;
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;

use gtk4::prelude::*;
use gtk4::{glib, Application, ApplicationWindow, Box as GtkBox, Button, Label, Orientation, Paned};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowId};

const APP_ID: &str = "cn.huntergrey.neovibe.pump-events-spike";

/// The winit-owned side: a real OS window with a GL context, animated
/// independently of GTK, pumped from a GLib timeout source instead of
/// `run_app`.
struct GlApp {
    window: Option<Window>,
    gl_context: Option<PossiblyCurrentContext>,
    gl_surface: Option<Surface<WindowSurface>>,
    start: Instant,
    frame_count: u64,
    last_fps_report: Instant,
}

impl GlApp {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            window: None,
            gl_context: None,
            gl_surface: None,
            start: now,
            frame_count: 0,
            last_fps_report: now,
        }
    }

    fn draw(&mut self) {
        let (Some(gl_context), Some(gl_surface), Some(window)) =
            (&self.gl_context, &self.gl_surface, &self.window)
        else {
            return;
        };

        let elapsed = self.start.elapsed().as_secs_f32();
        let r = elapsed.sin() * 0.5 + 0.5;
        let g = (elapsed * 0.7).sin() * 0.5 + 0.5;
        let b = (elapsed * 1.3).sin() * 0.5 + 0.5;

        let size = window.inner_size();
        let (w, h) = (size.width as i32, size.height as i32);

        unsafe {
            gl::Viewport(0, 0, w, h);
            gl::ClearColor(r, g, b, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // "moving rectangle" without any shader/VBO plumbing: scissor-clear
            // a sub-region to white and slide it left->right over time. This is
            // purely about event-loop plumbing, not rendering quality.
            if w > 0 && h > 0 {
                let rect_w = (w / 8).max(20);
                let period = w + rect_w;
                let t = ((elapsed * 220.0) as i32).rem_euclid(period.max(1));
                let x = t - rect_w;
                let visible_x = x.clamp(0, w);
                let visible_w = (x + rect_w).clamp(0, w) - visible_x;
                if visible_w > 0 {
                    gl::Enable(gl::SCISSOR_TEST);
                    gl::Scissor(visible_x, 0, visible_w, h);
                    gl::ClearColor(1.0, 1.0, 1.0, 1.0);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                    gl::Disable(gl::SCISSOR_TEST);
                }
            }
        }

        gl_surface
            .swap_buffers(gl_context)
            .expect("failed to swap GL buffers");

        self.frame_count += 1;
        if self.last_fps_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "[winit/gl] ~{} fps over last {:.2}s (frame {})",
                self.frame_count,
                self.last_fps_report.elapsed().as_secs_f32(),
                self.frame_count
            );
            self.frame_count = 0;
            self.last_fps_report = Instant::now();
        }
    }
}

impl ApplicationHandler for GlApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        println!("[winit] resumed: creating GL window (separate OS window, NOT run_app)");

        let window_attributes = Window::default_attributes()
            .with_title("pump_events_spike: winit+GL side (pumped, not run_app)")
            .with_inner_size(winit::dpi::LogicalSize::new(480.0, 360.0));

        let template = ConfigTemplateBuilder::new().with_alpha_size(8);
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(window_attributes));

        let (window, gl_config) = display_builder
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|accum, cfg| {
                        if cfg.num_samples() > accum.num_samples() {
                            cfg
                        } else {
                            accum
                        }
                    })
                    .expect("no GL configs returned by display builder")
            })
            .expect("failed to build winit window + GL config");
        let window = window.expect("DisplayBuilder did not produce a window");

        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());
        let gl_display = gl_config.display();

        let context_attributes = ContextAttributesBuilder::new().build(raw_window_handle);
        let fallback_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(None))
            .build(raw_window_handle);

        let not_current_context = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .unwrap_or_else(|_| {
                    gl_display
                        .create_context(&gl_config, &fallback_attributes)
                        .expect("failed to create GL context (core and GLES fallback both failed)")
                })
        };

        let attrs = window
            .build_surface_attributes(Default::default())
            .expect("failed to build surface attributes");
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &attrs)
                .expect("failed to create GL window surface")
        };

        let gl_context = not_current_context
            .make_current(&gl_surface)
            .expect("failed to make GL context current");

        // IMPORTANT (see FINDINGS.md): we tried `SwapInterval::Wait(1)` first.
        // That made `swap_buffers` block synchronously on the compositor's
        // per-surface frame-done callback -- and since this whole draw call
        // happens inside a GLib timeout source on the *same* thread as GTK's
        // own main loop, a slow/absent frame callback (observed for several
        // seconds right after this second window was first mapped, before
        // Mutter settled into steady presentation for it) stalled the entire
        // process: GTK heartbeats and button-click signal delivery froze too.
        // Switching to `DontWait` and self-pacing frames via the ~16ms GLib
        // timer instead removed the stall entirely and produced smooth,
        // independent 500ms/16ms cadences on both sides. This is the load-
        // bearing lesson of this spike, not a stylistic choice.
        gl_surface
            .set_swap_interval(&gl_context, SwapInterval::DontWait)
            .ok();

        gl::load_with(|symbol| {
            let symbol = std::ffi::CString::new(symbol).unwrap();
            gl_display.get_proc_address(symbol.as_c_str()).cast()
        });

        println!("[winit] GL context ready: {}", gl_display.version_string());

        self.window = Some(window);
        self.gl_context = Some(gl_context);
        self.gl_surface = Some(gl_surface);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                println!("[winit] close requested -> event_loop.exit()");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let (Some(gl_context), Some(gl_surface)) = (&self.gl_context, &self.gl_surface) {
                    if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                        gl_surface.resize(gl_context, w, h);
                        println!("[winit] resized -> {}x{}", size.width, size.height);
                        self.draw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Keep the animation running continuously: ask for another frame every
        // pump. In a real embedding this would instead be gated by dirty state.
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("neovibe pump_events_spike")
        .default_width(900)
        .default_height(520)
        .build();

    let paned = Paned::new(Orientation::Horizontal);
    paned.set_wide_handle(true);

    // Left pane: a genuinely interactive, plain GTK4 widget tree. If this
    // stays responsive while the winit/GL window animates, GTK's own GLib
    // main loop is not being starved by the pump timer or by the winit side.
    let left = GtkBox::new(Orientation::Vertical, 8);
    left.set_margin_top(16);
    left.set_margin_bottom(16);
    left.set_margin_start(16);
    left.set_margin_end(16);

    let label = Label::new(Some(
        "GTK4 native widget side.\n\
         Click the button and drag the splitter while the\n\
         separate winit/GL window (see console + other window)\n\
         is animating, to check the GLib main loop stays live.",
    ));
    label.set_wrap(true);

    let click_count = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let button = Button::with_label("Click me (GTK main-loop liveness probe)");
    {
        let click_count = click_count.clone();
        button.connect_clicked(move |_| {
            click_count.set(click_count.get() + 1);
            println!("[gtk] button clicked (count = {})", click_count.get());
        });
    }

    left.append(&label);
    left.append(&button);
    paned.set_start_child(Some(&left));

    // Right pane: explains why this is a separate OS window rather than an
    // embedded widget (see FINDINGS.md), and hosts a paned-position probe.
    let right = GtkBox::new(Orientation::Vertical, 8);
    right.set_margin_top(16);
    right.set_margin_bottom(16);
    right.set_margin_start(16);
    right.set_margin_end(16);
    let note = Label::new(Some(
        "The winit+GL animated content lives in a SEPARATE real OS window,\n\
         not embedded as a widget here. True in-process embedding of a\n\
         foreign client's wl_surface is not supported by stock Wayland\n\
         (no XEmbed equivalent) -- see FINDINGS.md.\n\n\
         Dragging this splitter fires GTK signals; watch stdout for\n\
         interleaved '[gtk] paned position' and '[winit/gl] ~N fps' lines.",
    ));
    note.set_wrap(true);
    right.append(&note);
    paned.set_end_child(Some(&right));

    paned.connect_notify_local(Some("position"), |paned, _| {
        println!("[gtk] paned position changed -> {}", paned.position());
    });

    window.set_child(Some(&paned));

    // GTK-side heartbeat, independent of the winit pump timer, so the log
    // shows two independently-scheduled GLib sources both making progress.
    glib::timeout_add_local(Duration::from_millis(500), || {
        println!("[gtk] heartbeat (main loop alive)");
        glib::ControlFlow::Continue
    });

    spawn_winit_pump();

    window.present();
}

/// Build a winit EventLoop the "embedded" way (no `run_app`), and drive it
/// from a recurring GLib timeout source instead. This is the crux of the
/// spike: winit's docs sanction `pump_app_events` for exactly this, but
/// nobody had verified it against a real GTK4/GLib main loop on Wayland.
fn spawn_winit_pump() {
    let mut event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("failed to build winit EventLoop");
    let mut app = GlApp::new();

    // ~60Hz poll. We tried to instead register the winit/Wayland connection's
    // fd directly as a glib::unix_fd_add_local source so pumping would be
    // reactive rather than timer-polled; winit does not expose that fd (or
    // the underlying wayland-client Connection) through public API in 0.30,
    // so a busy/idle poll timer is what's left short of forking winit. See
    // FINDINGS.md.
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let status = event_loop.pump_app_events(Some(Duration::ZERO), &mut app);
        match status {
            PumpStatus::Continue => glib::ControlFlow::Continue,
            PumpStatus::Exit(code) => {
                println!("[winit] pump loop exited with code {code}; stopping GLib timer");
                glib::ControlFlow::Break
            }
        }
    });
}

fn main() {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}
