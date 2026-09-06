//! webkit_pane — P6 feasibility probe (see docs/neovibe_feasibility_validation.md §9).
//!
//! Opens a bare GTK4 window containing a single WebKitGTK `WebView` and loads
//! an inline HTML/CSS/JS payload that fakes a busy Claude agent chat pane:
//! streaming text chunks, markdown-ish DOM growth, class-reapplication to
//! simulate syntax highlighting, and autoscroll. A visible on-screen frame
//! counter driven by `requestAnimationFrame` lets a human eyeball whether the
//! WebView's own JS/paint loop stays smooth in isolation.
//!
//! This crate does NOT yet exercise the actual coexistence test (there is no
//! Neovide/GL editor pane in this workspace crate) — it only establishes the
//! WebView-side half described in the feasibility doc, so a later step can
//! place this next to a real `GtkGLArea` editor surface and measure whether
//! this workload stalls the shared GTK main loop.

use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow};
use webkit6::prelude::*;
use webkit6::WebView;

const APP_ID: &str = "cn.huntergrey.neovibe.webkit-pane-poc";

/// Self-contained HTML/CSS/JS payload simulating a busy agent chat pane.
/// No external network requests, no build step — loaded directly via
/// `WebView::load_html`.
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
    cursor.textContent = " ";
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

fn main() -> gtk4::glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();

    app.connect_activate(|app| {
        let webview = WebView::new();
        webview.load_html(PAYLOAD_HTML, None);
        webview.set_hexpand(true);
        webview.set_vexpand(true);

        let window = ApplicationWindow::builder()
            .application(app)
            .title("webkit_pane — P6 probe")
            .default_width(900)
            .default_height(650)
            .child(&webview)
            .build();

        window.present();
    });

    app.run()
}
