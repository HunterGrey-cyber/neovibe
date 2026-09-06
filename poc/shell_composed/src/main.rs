//! shell_composed — combines all three prior neovibe PoC crates into the single-window shell
//! described in `docs/neovibe_architecture_summary.md`, as the harness the next phase (P6/P7 per
//! `docs/neovibe_feasibility_validation.md` §9/§10) will stress-test:
//!
//! ```text
//! ApplicationWindow (undecorated, custom chrome)
//! └── vertical Box
//!     ├── top bar (WindowHandle: drag-to-move, minimize/maximize/close)      <- from shell_chrome
//!     ├── GtkPaned (horizontal, wide handle)                                  <- from shell_chrome
//!     │   ├── GtkGLArea + Skia + neovide::live_harness::LiveHarness           <- from neovide_embed_live
//!     │   └── webkit6::WebView (streaming agent-chat simulation)              <- from webkit_pane
//!     └── status bar strip                                                   <- from shell_chrome
//! ```
//!
//! Each source crate's own module doc has the full rationale for the piece it contributes; this
//! file only repeats what's needed to follow the composition, plus documents what's genuinely new
//! here (none of the three source crates had it):
//!
//! - **Programmatic pane-resize sweep** (`install_auto_resize_sweep`, gated by the
//!   `SHELL_COMPOSED_AUTO_RESIZE_SWEEP` env var): moves the `GtkPaned` divider back and forth on a
//!   timer, standing in for a human drag gesture, since this sandbox has no reliable way to
//!   synthesize real pointer-drag input (a known limitation carried forward from prior phases —
//!   see `poc/pump_events_spike`). Off by default; see that function's own doc for the full env-var
//!   surface.
//! - **Resize-event-triggered frame-pacing detail**: `neovide_embed_live`'s frame-pacing log only
//!   fired every `LOG_EVERY_N_FRAMES` frames on the render path. Here, every `GtkGLArea` resize
//!   event *also* logs framebuffer size, the recomputed `content_region`, and how long it's been
//!   since the last rendered frame — the P7 resize-glitch signal the next phase needs, decoupled
//!   from the periodic render-loop cadence.
//!
//! Everything else — the GL/Skia interop plumbing, the `LiveHarness` lifecycle state machine, the
//! keyboard-forwarding controller, the WebView's HTML/CSS/JS payload, the top bar/theme/status bar
//! — is carried over essentially unchanged from its source crate; see that crate's own doc comments
//! for why any of it is shaped the way it is.

mod theme;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::gdk::{Key, ModifierType};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, EventControllerKey, GLArea, Paned};

use skia_safe::gpu::gl::{Format as GlFormat, FramebufferInfo, Interface as GlInterface};
use skia_safe::gpu::{backend_render_targets, direct_contexts, surfaces, DirectContext, SurfaceOrigin};
use skia_safe::{Canvas, Color4f, Paint, PaintStyle, Rect, Surface};

use neovide::live_harness::{LiveHarness, LiveHarnessOptions};
use neovide::units::PixelRect;

use webkit6::prelude::*;
use webkit6::WebView;

const APP_ID: &str = "cn.huntergrey.neovibe.shell_composed";

/// Log a periodic frame-pacing line every N frames on the render path (same cadence/rationale as
/// `neovide_embed_live`) — distinct from the *per-resize-event* log this crate adds separately.
const LOG_EVERY_N_FRAMES: u64 = 60;

/// Verification instrumentation added for the P6/P7 measurement pass (feasibility doc §9/§10):
/// overrides `LOG_EVERY_N_FRAMES` via `SHELL_COMPOSED_LOG_EVERY_N_FRAMES` so a resize-sweep run can
/// set it to `1` and get a per-frame dt/fps line for *every* rendered frame instead of only every
/// 60th. This matters specifically for correlating dt with resize events: both the `[resize #N]`
/// log line and the periodic `[frame N]` line are emitted from the same single-threaded GTK main
/// loop, so their order in stdout is the true chronological order — with per-frame logging enabled,
/// the `[frame N]` line immediately following a `[resize #N]` line is that resize's very next
/// rendered frame, letting dt-immediately-after-resize be read directly off the log rather than
/// inferred from timestamps. Defaults to the original cadence (60) when unset/invalid, so normal
/// (non-verification) runs are unaffected.
fn log_every_n_frames() -> u64 {
    std::env::var("SHELL_COMPOSED_LOG_EVERY_N_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(LOG_EVERY_N_FRAMES)
}

/// How many `add_tick_callback` invocations between `[tick]` summary log lines (see `TickStats`).
/// The tick callback fires once per display frame (~144-165Hz on the dev machine this was
/// measured on per `poc/p11_measurements/PHASE_REPORT.md`), so 300 ticks is roughly a 2s window --
/// frequent enough to see idle-vs-active behavior change within a couple of seconds in the log,
/// without spamming stdout at display refresh rate.
const TICK_LOG_EVERY_N_TICKS: u64 = 300;

/// Inset (device pixels) between the GtkGLArea's own framebuffer edge and the rect handed to
/// `LiveHarness::render_frame` as `content_region`. Same value/rationale as
/// `neovide_embed_live::CONTENT_MARGIN`.
const CONTENT_MARGIN: f32 = 40.0;

const OUTSIDE_COLOR: Color4f = Color4f::new(0.55, 0.15, 0.55, 1.0);
const BORDER_COLOR: Color4f = Color4f::new(0.95, 0.85, 0.25, 1.0);
const STARTING_COLOR: Color4f = Color4f::new(0.12, 0.12, 0.16, 1.0);
const FAILED_COLOR: Color4f = Color4f::new(0.5, 0.05, 0.05, 1.0);

/// Self-contained HTML/CSS/JS payload simulating a busy agent chat pane, copied verbatim from
/// `poc/webkit_pane/src/main.rs` — see that crate for the full rationale of every piece (streaming
/// token reveal, re-highlight-on-every-chunk, autoscroll, on-screen frame counter/stall indicator).
/// This is exactly the signal the P6 coexistence test (feasibility doc §9) watches on the WebView
/// side, so it's kept intact rather than simplified.
const PAYLOAD_HTML: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body {
    margin: 0; padding: 0; height: 100%;
    background: #1e1e24; color: #d8dde6;
    font-family: -apple-system, "Segoe UI", sans-serif;
  }
  #hud {
    position: fixed; top: 0; left: 0; right: 0;
    display: flex; gap: 1.5rem; align-items: baseline;
    padding: 8px 14px; background: #14141a; border-bottom: 1px solid #33333d;
    font-family: ui-monospace, monospace; font-size: 13px; z-index: 10;
  }
  #hud b { color: #7ee787; }
  #hud .stall { color: #ff7b72; }
  #feed {
    position: absolute; top: 40px; bottom: 0; left: 0; right: 0;
    overflow-y: auto; padding: 12px 16px 40px;
  }
  .msg { margin-bottom: 18px; border-left: 2px solid #3a3a46; padding-left: 10px; }
  .msg h3 { margin: 0 0 4px; font-size: 13px; color: #8ab4f8; }
  .msg p { margin: 4px 0; line-height: 1.45; font-size: 14px; }
  pre {
    background: #12121a; border: 1px solid #2c2c36; border-radius: 6px;
    padding: 10px; overflow-x: auto; font-size: 12.5px; line-height: 1.4;
  }
  code { font-family: ui-monospace, "SF Mono", monospace; }
  .tok-kw { color: #ff7b72; }
  .tok-str { color: #a5d6ff; }
  .tok-fn { color: #d2a8ff; }
  .tok-com { color: #6e7681; font-style: italic; }
  .cursor { display: inline-block; width: 8px; background: #7ee787; animation: blink 1s steps(1) infinite; }
  @keyframes blink { 50% { opacity: 0; } }
</style>
</head>
<body>
  <div id="hud">
    <span>frame <b id="frame-count">0</b></span>
    <span>t+<b id="elapsed">0.0s</b></span>
    <span>fps <b id="fps">--</b></span>
    <span id="stall-indicator"></span>
  </div>
  <div id="feed"></div>

<script>
(function () {
  "use strict";

  const feed = document.getElementById("feed");
  const frameCountEl = document.getElementById("frame-count");
  const elapsedEl = document.getElementById("elapsed");
  const fpsEl = document.getElementById("fps");
  const stallEl = document.getElementById("stall-indicator");

  // ---- rAF loop: on-screen "is the WebView's own loop smooth" indicator ----
  const start = performance.now();
  let frame = 0;
  let lastFpsSample = start;
  let framesSinceSample = 0;
  let lastFrameTime = start;

  function tick(now) {
    frame += 1;
    framesSinceSample += 1;

    const dt = now - lastFrameTime;
    lastFrameTime = now;
    // A single JS frame taking >50ms is a visible stall/jank at this
    // workload's scale (well below one dropped 60Hz frame's ~16.7ms, but a
    // generous threshold so occasional GC pauses don't spam the indicator).
    if (dt > 50) {
      stallEl.textContent = "stall " + dt.toFixed(0) + "ms";
      stallEl.className = "stall";
    } else if (now - lastFpsSample > 400) {
      stallEl.textContent = "";
      stallEl.className = "";
    }

    if (now - lastFpsSample >= 500) {
      const fps = (framesSinceSample * 1000) / (now - lastFpsSample);
      fpsEl.textContent = fps.toFixed(0);
      framesSinceSample = 0;
      lastFpsSample = now;
    }

    frameCountEl.textContent = String(frame);
    elapsedEl.textContent = ((now - start) / 1000).toFixed(1) + "s";

    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);

  // ---- Streaming "agent response" simulator ----
  // Markdown-ish message blocks made of prose + a fenced code block,
  // "typed" in token-sized chunks like an LLM stream, one chunk per
  // interval tick (~60/s), with a fake syntax-highlight pass re-run on
  // every chunk (the thing that makes a real markdown/code-highlight
  // renderer expensive: repeated DOM class churn on growing content).

  const LINES = [
    { h: "Reviewing webkit_pane::main", kind: "prose",
      text: "Streaming this reply token by token to simulate an LLM response " +
            "arriving over SSE, the same shape a real Claude Code agent panel " +
            "would render while a human watches it grow." },
    { h: "Proposed change", kind: "code",
      code: [
        "// re-apply highlight classes on every appended token,",
        "// same as a naive markdown renderer would.",
        "function highlight(el) {",
        "  const kw = /\\b(fn|let|const|return|if|else)\\b/g;",
        "  el.innerHTML = el.textContent",
        "    .replace(kw, '<span class=\"tok-kw\">$1</span>');",
        "}",
      ] },
    { h: "Notes", kind: "prose",
      text: "Auto-scroll keeps the viewport pinned to the newest chunk unless " +
            "you scroll up, exactly like a chat UI. Large code blocks and long " +
            "conversations accumulate DOM nodes the same way this pane does." },
  ];

  let lineIdx = 0;
  let charIdx = 0;
  let currentMsg = null;
  let currentBody = null;
  let userScrolledUp = false;

  feed.addEventListener("scroll", () => {
    const atBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 24;
    userScrolledUp = !atBottom;
  });

  function startMessage(spec) {
    currentMsg = document.createElement("div");
    currentMsg.className = "msg";
    const h = document.createElement("h3");
    h.textContent = spec.h;
    currentMsg.appendChild(h);

    if (spec.kind === "code") {
      const pre = document.createElement("pre");
      const code = document.createElement("code");
      pre.appendChild(code);
      currentMsg.appendChild(pre);
      currentBody = code;
      currentBody.dataset.full = spec.code.join("\n");
    } else {
      const p = document.createElement("p");
      currentMsg.appendChild(p);
      currentBody = p;
      currentBody.dataset.full = spec.text;
    }
    const cursor = document.createElement("span");
    cursor.className = "cursor";
    cursor.textContent = " ";
    currentMsg.appendChild(cursor);
    currentMsg._cursor = cursor;

    feed.appendChild(currentMsg);
    charIdx = 0;
  }

  // Cheap stand-in for "syntax highlighting": re-scan the fully-revealed
  // text so far and wrap keyword-ish tokens in spans. Deliberately redone
  // on every tick (not memoized) to mimic a naive re-highlight-on-change
  // renderer, which is the realistic worst case this probe is for.
  function reHighlight(el) {
    const revealed = el.dataset.full.slice(0, charIdx);
    const escaped = revealed
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    const highlighted = escaped
      .replace(/\b(fn|let|const|return|if|else|function)\b/g, '<span class="tok-kw">$1</span>')
      .replace(/(\/\/.*)$/gm, '<span class="tok-com">$1</span>')
      .replace(/"([^"]*)"/g, '<span class="tok-str">"$1"</span>');
    el.innerHTML = highlighted;
  }

  function step() {
    if (!currentMsg) {
      startMessage(LINES[lineIdx]);
    }
    const full = currentBody.dataset.full;
    if (charIdx < full.length) {
      // Reveal a few characters per tick, like token-chunk streaming.
      charIdx = Math.min(full.length, charIdx + 3);
      reHighlight(currentBody);
      currentMsg.appendChild(currentMsg._cursor);
    } else {
      currentMsg._cursor.remove();
      lineIdx = (lineIdx + 1) % LINES.length;
      currentMsg = null;
      currentBody = null;
    }

    if (!userScrolledUp) {
      feed.scrollTop = feed.scrollHeight;
    }
  }

  setInterval(step, 16); // ~60 appends/sec: deliberately aggressive vs. a real LLM's token rate
})();
</script>
</body>
</html>
"##;

/// GL-context-bound Skia state, identical in spirit/implementation to
/// `neovide_embed_live::SkiaState` — see that crate for the full rationale.
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
            0,
            8,
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

/// Carried over verbatim from `neovide_embed_live::current_bound_framebuffer` (itself carried over
/// from `gl_skia_test`/`neovide_embed`) — see those crates' doc comments for the full
/// nm -D-verified explanation of the local libepoxy quirk this works around.
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

/// Carried over verbatim from `neovide_embed_live::make_gl_interface`.
fn make_gl_interface() -> GlInterface {
    unsafe {
        let lib = libloading::os::unix::Library::this();
        GlInterface::new_load_with(move |name: &str| resolve_gl_proc(&lib, name))
            .expect("failed to assemble Skia GL interface via process-wide symbol lookup")
    }
}

/// Carried over verbatim from `neovide_embed_live::resolve_gl_proc`.
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

/// Identical to `neovide_embed_live::compute_content_region`: the pixel rect (within the GLArea's
/// own framebuffer) handed to `LiveHarness::render_frame` as `content_region`, inset from the
/// framebuffer edges by `CONTENT_MARGIN` on every side.
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

/// Lifecycle of the `LiveHarness` this window drives — identical shape/rationale to
/// `neovide_embed_live::LiveState` (see that crate's module doc for why construction is split
/// across two render callbacks instead of happening inline).
enum LiveState {
    NotStarted,
    Starting,
    Ready(Box<LiveSession>),
    Failed(String),
}

impl LiveState {
    /// Short tag for log lines (used by the resize-event log this crate adds).
    fn tag(&self) -> &'static str {
        match self {
            LiveState::NotStarted => "not_started",
            LiveState::Starting => "starting",
            LiveState::Ready(_) => "ready",
            LiveState::Failed(_) => "failed",
        }
    }
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
    /// `neovide_embed_live::LiveSession::tick`'s own semantics exactly.
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
/// dominating, while a typing/scrolling/animating/resize-sweep window should show `issued`
/// dominating. See `poc/p11_measurements/PHASE_REPORT.md` for the bug this directly addresses.
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
    // Same passthrough convenience as neovide_embed_live: pass `--clean` on this binary's own
    // command line to launch nvim with `--clean` instead of a real embedding host's actual config.
    let want_clean = std::env::args().any(|arg| arg == "--clean");

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| build_ui(app, want_clean));
    app.run_with_args::<&str>(&[])
}

fn build_ui(app: &Application, want_clean: bool) {
    let theme = theme::Theme::dark();
    apply_css(&theme.to_css());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("neovibe")
        .default_width(1280)
        .default_height(760)
        // Same reasoning as shell_chrome: no HeaderBar, decorated(false) suppresses GTK's own
        // CSD titlebar entirely -- the custom top bar built below is the only titlebar.
        .decorated(false)
        .build();
    window.add_css_class("shell-root");

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("shell-root");

    let (editor_widget, live_state, gl_area) = build_editor_pane(want_clean);
    let agent_widget = build_agent_pane();
    let (content_widget, paned) = build_content_area(&editor_widget, &agent_widget);

    root.append(&build_top_bar(&window));
    root.append(&content_widget);
    root.append(&build_status_bar());

    window.set_child(Some(&root));

    // --- shutdown: reuse neovide_embed_live's connect_close_request -> LiveHarness::shutdown()
    // pattern verbatim, so a real nvim --embed child is never left orphaned behind this shell --
    // this is the exact regression the task background calls out as unacceptable to reintroduce.
    {
        let live_state = live_state.clone();
        window.connect_close_request(move |_window| {
            let mut live = live_state.borrow_mut();
            if let LiveState::Ready(session) = &mut *live {
                println!("[live] window closing: calling LiveHarness::shutdown()...");
                let exited_cleanly = session.harness.shutdown();
                println!(
                    "[live] LiveHarness::shutdown() returned {exited_cleanly} ({})",
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

    install_auto_resize_sweep(&paned);

    window.present();
    gl_area.grab_focus();
    println!(
        "shell_composed running: chrome+editor+agent-panel single window. \
         initial window scale_factor={} clean={} auto_resize_sweep={}",
        window.scale_factor(),
        want_clean,
        std::env::var("SHELL_COMPOSED_AUTO_RESIZE_SWEEP").as_deref() == Ok("1"),
    );
}

/// Custom top bar, carried over from `shell_chrome::build_top_bar` unchanged (see that crate's
/// doc comment for why a `WindowHandle` is the right idiom for a CSD-less draggable titlebar).
fn build_top_bar(window: &ApplicationWindow) -> gtk4::Widget {
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    bar.add_css_class("topbar");
    bar.set_valign(gtk4::Align::Fill);

    let app_name = gtk4::Label::new(Some("neovibe"));
    app_name.add_css_class("topbar-app-name");

    let project_name = gtk4::Label::new(Some("project"));
    project_name.add_css_class("topbar-project-name");
    project_name.set_hexpand(true);
    project_name.set_halign(gtk4::Align::Start);

    bar.append(&app_name);
    bar.append(&project_name);
    bar.append(&build_window_controls(window));

    let handle = gtk4::WindowHandle::new();
    handle.set_child(Some(&bar));
    handle.upcast()
}

/// Carried over from `shell_chrome::build_window_controls` unchanged.
fn build_window_controls(window: &ApplicationWindow) -> gtk4::Widget {
    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    controls.set_valign(gtk4::Align::Center);
    controls.set_margin_end(6);

    let minimize = gtk4::Button::with_label("—");
    minimize.add_css_class("win-btn");
    {
        let window = window.clone();
        minimize.connect_clicked(move |_| window.minimize());
    }

    let maximize = gtk4::Button::with_label("\u{25A1}");
    maximize.add_css_class("win-btn");
    {
        let window = window.clone();
        maximize.connect_clicked(move |_| {
            if window.is_maximized() {
                window.unmaximize();
            } else {
                window.maximize();
            }
        });
    }

    let close = gtk4::Button::with_label("\u{00D7}");
    close.add_css_class("win-btn");
    close.add_css_class("close");
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }

    controls.append(&minimize);
    controls.append(&maximize);
    controls.append(&close);
    controls.upcast()
}

/// Horizontal split, carried over from `shell_chrome::build_content_area`'s `GtkPaned` setup, with
/// the two placeholder panes replaced by the real editor/agent widgets. Returns the wrapping
/// widget plus the `Paned` itself (needed by `install_auto_resize_sweep`).
fn build_content_area(editor: &gtk4::Widget, agent: &gtk4::Widget) -> (gtk4::Widget, Paned) {
    let paned = Paned::new(gtk4::Orientation::Horizontal);
    paned.add_css_class("content-area");
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_wide_handle(true);

    paned.set_start_child(Some(editor));
    paned.set_end_child(Some(agent));
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_position(760);

    let widget = paned.clone().upcast();
    (widget, paned)
}

/// Bottom status bar strip, carried over from `shell_chrome::build_status_bar` unchanged.
fn build_status_bar() -> gtk4::Widget {
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    bar.add_css_class("statusbar");

    let mode = gtk4::Label::new(Some("NORMAL"));
    mode.add_css_class("statusbar-accent");

    let sep = gtk4::Label::new(Some("  \u{2014}  Ln 1, Col 1"));

    bar.append(&mode);
    bar.append(&sep);

    bar.upcast()
}

/// Left pane: the real editor, carried over from `neovide_embed_live::build_ui`'s `GLArea`
/// construction/render/input wiring almost unchanged -- the only differences are (1) it returns
/// the pieces the caller (this file's `build_ui`) needs to wire into the shared window/paned
/// instead of owning its own `ApplicationWindow`, and (2) the resize handler additionally logs
/// `content_region` + time-since-last-frame (see this file's own module doc on why that's new).
fn build_editor_pane(
    want_clean: bool,
) -> (gtk4::Widget, Rc<RefCell<LiveState>>, GLArea) {
    let gl_area = GLArea::builder()
        .hexpand(true)
        .vexpand(true)
        .has_stencil_buffer(true)
        .auto_render(true)
        .focusable(true)
        .can_focus(true)
        .build();

    let skia_state: Rc<RefCell<Option<SkiaState>>> = Rc::new(RefCell::new(None));
    let live_state: Rc<RefCell<LiveState>> = Rc::new(RefCell::new(LiveState::NotStarted));
    let resize_events: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let log_every_n_frames = log_every_n_frames();

    // --- resize: same rationale as neovide_embed_live's own resize handler (drop the cached
    // Surface so render() rebuilds it against the new framebuffer dimensions), PLUS this crate's
    // own addition: log content_region and time-since-last-rendered-frame on every event, which
    // is the P7 resize-glitch-detection signal the task background asks for (independent of the
    // periodic every-N-frames log on the render path below).
    {
        let skia_state = skia_state.clone();
        let live_state = live_state.clone();
        let resize_events = resize_events.clone();
        gl_area.connect_resize(move |widget, width, height| {
            let event_no = resize_events.get() + 1;
            resize_events.set(event_no);

            let content_region = compute_content_region(width, height);
            let live = live_state.borrow();
            // A resize is a real event that deserves a guaranteed next frame at the new size --
            // flag it for the tick callback regardless of whether nvim itself has anything new to
            // say (see `LiveSession::wants_frame`'s own doc).
            let (since_last_frame, frame_count, forced_next_frame) = match &*live {
                LiveState::Ready(session) => {
                    session.wants_frame.set(true);
                    (Some(session.last_frame.elapsed()), Some(session.frame_count), true)
                }
                _ => (None, None, false),
            };
            println!(
                "[resize #{event_no}] fb={}x{}px scale_factor={} logical={}x{} \
                 content_region={}x{}@({},{}) live_state={} since_last_frame={} frame_count={} \
                 forced_next_frame={forced_next_frame}",
                width,
                height,
                widget.scale_factor(),
                widget.width(),
                widget.height(),
                (content_region.max.x - content_region.min.x) as i32,
                (content_region.max.y - content_region.min.y) as i32,
                content_region.min.x as i32,
                content_region.min.y as i32,
                live.tag(),
                match since_last_frame {
                    Some(d) => format!("{:.2}ms", d.as_secs_f64() * 1000.0),
                    None => "n/a".to_string(),
                },
                frame_count.map(|c| c.to_string()).unwrap_or_else(|| "n/a".to_string()),
            );
            drop(live);

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

    // --- keyboard input: identical to neovide_embed_live's EventControllerKey wiring.
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
            glib::Propagation::Stop
        });
        gl_area.add_controller(key_controller);
    }

    // --- render: identical lifecycle/logging to neovide_embed_live's connect_render.
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

            canvas.clear(OUTSIDE_COLOR);

            let mut live = live_state.borrow_mut();
            match &mut *live {
                LiveState::NotStarted => {
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

                    if session.frame_count.is_multiple_of(log_every_n_frames) {
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
                    let _ = message;
                }
            }
            drop(live);

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
    // neovide_embed_live/neovide_embed/gl_skia_test -- but, per the P11 fix, only actually calls
    // `queue_render()` when something genuinely needs another frame. Previously this
    // unconditionally requested a render on every single tick (~144-165Hz on the P11 dev machine)
    // forever, which is the entire root cause of that phase's idle CPU/GPU finding (see
    // `poc/p11_measurements/PHASE_REPORT.md`) -- `nvim` itself was already idling at 0.0% CPU;
    // only this host-shell tick loop was hot.
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

    let widget: gtk4::Widget = gl_area.clone().upcast();
    (widget, live_state, gl_area)
}

/// Right pane: the real agent panel, carried over from `webkit_pane::main`'s `WebView`
/// construction unchanged -- same inline HTML/CSS/JS payload, same on-screen frame counter/stall
/// indicator (kept intact per this task's own instruction: it's the signal the next phase watches
/// for "does the WebView's own JS loop stay smooth").
fn build_agent_pane() -> gtk4::Widget {
    let webview = WebView::new();
    webview.load_html(PAYLOAD_HTML, None);
    webview.set_hexpand(true);
    webview.set_vexpand(true);

    install_webview_stall_poll(&webview);

    webview.upcast()
}

/// New in this crate: periodically reads the WebView's own on-screen frame counter/fps/stall
/// indicator (the `#frame-count`/`#fps`/`#elapsed`/`#stall-indicator` DOM text `PAYLOAD_HTML`'s own
/// rAF loop already maintains) back out via `evaluate_javascript` and logs it -- this is the P6
/// measurement task's own ask ("report the WebView's own stall-indicator readings alongside the
/// editor's frame numbers from the same time window"), and specifically the only way to get that
/// number out of the sandboxed WebProcess without a working screenshot path (see this crate's
/// module doc / the task background on why screen capture isn't available here). Polls every 2s;
/// each read is one async JS round-trip through the WebView's own event loop, so a slow/blocked
/// WebProcess would itself show up here as a late or missing `[webview-poll]` line, not just a
/// stale on-screen value.
fn install_webview_stall_poll(webview: &WebView) {
    const POLL_SCRIPT: &str = "JSON.stringify({\
        frame: document.getElementById('frame-count')?.textContent ?? null,\
        elapsed: document.getElementById('elapsed')?.textContent ?? null,\
        fps: document.getElementById('fps')?.textContent ?? null,\
        stall: document.getElementById('stall-indicator')?.textContent ?? null\
    })";

    let webview = webview.clone();
    glib::timeout_add_local(Duration::from_millis(2000), move || {
        let poll_sent_at = Instant::now();
        webview.evaluate_javascript(
            POLL_SCRIPT,
            None,
            None,
            None::<&gtk4::gio::Cancellable>,
            move |result| {
                let round_trip = poll_sent_at.elapsed();
                match result {
                    Ok(value) => {
                        println!(
                            "[webview-poll] round_trip={:.2}ms result={}",
                            round_trip.as_secs_f64() * 1000.0,
                            value.to_str()
                        );
                    }
                    Err(err) => {
                        println!(
                            "[webview-poll] round_trip={:.2}ms evaluate_javascript failed: {err}",
                            round_trip.as_secs_f64() * 1000.0
                        );
                    }
                }
            },
        );
        glib::ControlFlow::Continue
    });
}

/// New in this crate: programmatically sweeps the `GtkPaned` divider back and forth over time,
/// standing in for a human drag gesture on the splitter (feasibility doc §10, P7) -- this sandbox
/// has no reliable way to synthesize real pointer-drag input (a known limitation already
/// documented by prior phases, e.g. `poc/pump_events_spike`'s own findings on synthetic input).
///
/// Off by default; enabled by setting `SHELL_COMPOSED_AUTO_RESIZE_SWEEP=1`. Two more env vars tune
/// it, both optional:
/// - `SHELL_COMPOSED_AUTO_RESIZE_SWEEP_INTERVAL_MS` (default 16, i.e. ~60Hz): how often the
///   divider moves.
/// - `SHELL_COMPOSED_AUTO_RESIZE_SWEEP_STEP_PX` (default 6): how many pixels it moves per tick.
///
/// Each tick nudges `paned.position()` by `step` pixels in the current direction, bouncing between
/// 15% and 85% of the paned's current allocated width (recomputed every tick, so it stays correct
/// across window resizes) -- every position change goes through the same `Paned::set_position`
/// call a real drag would end up calling, so it exercises the exact
/// `GtkPaned resize -> Neovide viewport resize -> content_region` path the P7 test cares about.
fn install_auto_resize_sweep(paned: &Paned) {
    let enabled = std::env::var("SHELL_COMPOSED_AUTO_RESIZE_SWEEP").as_deref() == Ok("1");
    if !enabled {
        println!(
            "[resize-sweep] disabled (set SHELL_COMPOSED_AUTO_RESIZE_SWEEP=1 to enable; \
             tunable via SHELL_COMPOSED_AUTO_RESIZE_SWEEP_INTERVAL_MS / \
             SHELL_COMPOSED_AUTO_RESIZE_SWEEP_STEP_PX)"
        );
        return;
    }

    let interval_ms: u64 = std::env::var("SHELL_COMPOSED_AUTO_RESIZE_SWEEP_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16);
    let step_px: i32 = std::env::var("SHELL_COMPOSED_AUTO_RESIZE_SWEEP_STEP_PX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    println!(
        "[resize-sweep] enabled: interval={interval_ms}ms step={step_px}px \
         (bouncing paned.position() between 15%-85% of current width)"
    );

    let direction: Rc<Cell<i32>> = Rc::new(Cell::new(1));
    let tick_count: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let paned = paned.clone();
    glib::timeout_add_local(Duration::from_millis(interval_ms), move || {
        let width = paned.width();
        if width <= 0 {
            // Not yet allocated (e.g. before first present()) -- try again next tick.
            return glib::ControlFlow::Continue;
        }

        let min_pos = (width as f64 * 0.15) as i32;
        let max_pos = (width as f64 * 0.85) as i32;

        let mut pos = paned.position() + direction.get() * step_px;
        if pos >= max_pos {
            pos = max_pos;
            direction.set(-1);
        } else if pos <= min_pos {
            pos = min_pos;
            direction.set(1);
        }
        paned.set_position(pos);

        let n = tick_count.get() + 1;
        tick_count.set(n);
        if n.is_multiple_of(30) {
            println!(
                "[resize-sweep] tick {n}: paned.position()={pos} bounds=[{min_pos},{max_pos}] width={width}"
            );
        }

        glib::ControlFlow::Continue
    });
}

fn apply_css(css: &str) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(css);

    let display = gtk4::gdk::Display::default().expect("no default GDK display");
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
