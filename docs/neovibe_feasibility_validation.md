# neovibe pre-development feasibility investigation and Go/No-Go test plan

*[中文原文](zh/neovibe_feasibility_validation.md)*

## 1. Purpose

This document exists to validate, before neovibe's real development starts,
the technical risks most likely to sink the architecture.

The goal is not to test every feature — it's to answer, as fast as possible:

> **"Can the GTK4 + Wayland + Neovide/Skia + WebKitGTK route support an IDE
> shell while preserving Neovide's editing experience?"**

All of this testing should happen before any Claude feature work.

---

# 2. Overall Go/No-Go criteria

Primary route:

```text
GTK4 Shell
├── GtkGLArea
│   └── Skia
│       └── Neovide Renderer
│
└── WebKitGTK
    └── Claude UI
```

If any of the following four turn out to be unfixable, the primary
architecture needs to be reconsidered:

1. Editor-pane frame pacing is noticeably worse than native Neovide
2. IME / input latency is unacceptable
3. 4K + fractional scaling has serious problems
4. WebKitGTK's continuous repainting meaningfully slows the editor pane

---

# 3. P0: GTK4 + GtkGLArea + Skia baseline

## Goal

No Neovide involved yet at all.

Validate whether this stack works reliably:

```text
GTK4
 ↓
GtkGLArea
 ↓
OpenGL
 ↓
Skia Surface
```

## Test content

Build a minimal program:

```text
GtkApplicationWindow
└── GtkGLArea
    └── Skia Canvas
        ├── text
        ├── rectangles
        └── animation
```

Must test:

- 60 Hz
- 120 Hz
- 144 Hz
- 165 Hz
- Resize
- Maximize/restore
- Fractional scaling
- Wayland
- Multi-monitor switching
- GPU usage
- CPU usage

## Key things to watch

- Whether frame pacing is even
- Whether resize causes black frames/flicker
- Whether text stays crisp after scaling
- Whether GPU acceleration is actually engaged
- Whether there's noticeable CPU-side copying

## Go conditions

- Subjectively smooth animation at 165Hz
- No noticeable periodic stutter
- Stable resize
- Fractional scaling works correctly

## No-Go conditions

- GLArea can't reliably drive Skia
- GTK compositing causes visible frame-pacing problems
- Abnormal CPU/GPU overhead at 4K/165Hz

---

# 4. P1: Detaching the Neovide renderer from its own window

## Goal

Prove the Neovide renderer can run without owning a full OS window — instead:

```text
GtkGLArea
   ↓
Skia Canvas
   ↓
Neovide Renderer
```

## Scope

Don't wire up the full Neovim runtime yet.

Priority validation:

- Neovide's font rendering
- Grid rendering
- Cursor rendering
- Animation
- Scroll animation
- Highlighting
- Background

## Required changes

Confirm the renderer:

- Only clears its own viewport
- Doesn't assume canvas == whole window
- Can have its viewport resized
- Doesn't directly depend on a winit `Window`

## Go conditions

Visually essentially matches stock Neovide.

Particular focus on:

- Fonts
- Ligatures
- Cursor
- Smooth scroll
- Animation timing
- Highlighting

## No-Go conditions

If the renderer turns out far more tightly coupled to winit/its own
`SkiaRenderer` than expected — requiring a major rewrite of the low-level
renderer — pause and re-evaluate.

---

# 5. P2: Full Neovim runtime integration

## Goal

Run the real thing:

```text
GtkGLArea
 ↓
Neovide
 ↓
nvim --embed
```

## Test content

Must cover:

- Normal input
- Normal/Insert modes
- Visual mode
- Command mode
- Floating windows
- Popup menus
- Completion
- LazyVim
- fzf-lua/Telescope-style plugins
- Diagnostics
- Large files
- Rapid scrolling
- Rapid key repeat

## Baseline comparison

Must run alongside official Neovide, using the same:

- Neovim config
- Font
- Theme
- Project
- Display

as an A/B test.

## Go conditions

Subjective editing feel close to official Neovide.

Some shell-induced overhead is acceptable, but it must not noticeably affect:

- Typing latency
- Scroll smoothness
- Cursor animation
- Popup responsiveness

---

# 6. P3: Input system

This is a high-risk item.

## Test content

### Keyboard

- Normal English typing
- High-speed key repeat
- Ctrl/Alt/Super
- Function keys
- Leader key
- Key chords
- Key repeat

### Mouse

- Click
- Drag
- Wheel
- Horizontal scroll
- Text selection
- Resize

### Focus

```text
Neovide → Shell
Shell → Claude
Claude → Neovide
```

Test whether focus switches drop keystrokes.

## Go conditions

Neovim's behavior is essentially the same as with official Neovide.

---

# 7. P4: IME

This must be treated as blocking, at the same level as P0/P1.

## Testing

At minimum test:

- Chinese pinyin IME
- Candidate window
- Composition text
- Cursor position
- Insert mode
- Normal → Insert
- Focus switching back from Claude → Neovim

## Checks

The candidate window must appear correctly positioned near the Neovim
cursor.

Specifically test under:

```text
4K
fractional scale
multi-monitor
vertical monitor
```

## No-Go risk

If the GTK Input Method → Neovide → Neovim adaptation causes:

- Lost composition
- Wrong candidate-window position
- Broken IME state after a focus switch
- Noticeable input latency

these must be fixed first — cannot be skipped.

---

# 8. P5: HiDPI / fractional scaling

Especially important for the target environment.

## Test matrix

At minimum:

```text
100%
125%
150%
175%
200%
250%
```

Covering:

- GtkGLArea
- Skia
- Neovide grid
- Mouse coordinates
- Cursor
- IME position
- WebView
- Splitter

## Multi-monitor

Test:

- Normal landscape
- High-DPI landscape
- Portrait
- Dragging between monitors with different scale factors

## Go conditions

No noticeable coordinate drift.

Especially:

```text
mouse pixel
 ↓
Neovim grid
```

must be accurate.

---

# 9. P6: WebKitGTK coexistence stress test

## Goal

Confirm:

> When the Claude WebView on the right is busy, it doesn't affect Neovide on
> the left.

## Test layout

```text
GtkPaned
├── Neovide GtkGLArea
└── WebKitWebView
```

WebView continuously doing:

- Streaming text
- Markdown layout
- Syntax highlighting
- Auto-scroll
- Large code blocks
- Large conversations

While, on the left:

- Fast typing
- Continuous scrolling
- Smooth cursor
- A large buffer

## Measurements

Watch:

- Editor FPS
- Frame time
- Input latency
- UI-thread stalls
- CPU
- GPU
- Memory

## Go conditions

WebView workload must not cause visible editor-pane jitter.

If WebKitGTK does block the GTK main loop, investigate:

- JS update strategy
- Batching
- `requestAnimationFrame`
- Token-chunk batching
- Virtualized lists

---

# 10. P7: Pane resize

Continuously drag the splitter between:

```text
Neovide | Claude
```

Must validate that this chain keeps working correctly:

```text
GtkPaned resize
 ↓
Neovide viewport resize
 ↓
grid size
 ↓
nvim_ui_resize
```

Watch for:

- Neovim flicker
- Grid corruption
- CPU spikes
- Animation glitches

---

# 11. P8: Theme consistency

Not a blocking risk, but best validated early.

Build one unified theme:

```text
Theme Tokens
├── GTK CSS
├── Neovim
└── Web CSS
```

Implement:

- Background
- Surface
- Border
- Text
- Muted text
- Accent

Verify the visuals genuinely read as unified, rather than:

```text
GTK app
+
Neovide
+
browser
```

stitched together.

---

# 12. P9: Custom top bar

Build a minimal:

```text
┌──────────────────────────────────────┐
│ neovibe  project             — □ ×   │
└──────────────────────────────────────┘
```

Using Rust + gtk4-rs.

Requirements:

- No visible default Adwaita styling
- Custom spacing/font/color
- Wayland window controls work
- Window dragging works
- Maximize works

Purpose:

Prove that future IDE-ification won't be constrained by GTK's default look.

---

# 13. P10: Neovide upstream maintainability

This needs long-term, ongoing investigation.

## Approach

After forking, set up:

```text
upstream/neovide
neovibe/main
```

Early on, keep changes concentrated in:

- Surface host
- Geometry
- Input adapter
- Renderer viewport

not in:

- Font renderer
- Grid renderer
- Animation logic
- The Neovim bridge protocol

## Test

After the first surface PoC is done:

1. Pull the latest upstream commits
2. Merge/rebase
3. Record the number of conflicts
4. Judge the ongoing maintenance cost

## Go conditions

Most Neovide renderer/bridge updates can be merged directly.

---

# 14. P11: Performance baseline

Must measure official Neovide first.

Record:

```text
Idle CPU
Idle GPU
Typing CPU/GPU
Fast scroll
Memory
Frame time
Startup
```

Then measure the neovibe PoC.

Don't settle for:

> "feels about the same"

Record at least basic quantitative data.

The goal isn't winning a benchmark — it's:

> neovibe should not meaningfully degrade Neovide's performance advantage.

---

# 15. Recommended test order

Strictly in this order:

```text
01 GtkGLArea + Skia
      ↓
02 Neovide Renderer
      ↓
03 Real Neovim
      ↓
04 Keyboard / Mouse
      ↓
05 IME
      ↓
06 HiDPI / 165Hz
      ↓
07 WebKitGTK coexist
      ↓
08 Pane resize
      ↓
09 Theme / Top Bar
      ↓
10 Upstream merge test
```

Claude API integration comes after all of this.

---

# 16. Suggested phase-1 PoC repository layout

```text
neovibe-poc/
├── src/
│   ├── main.rs
│   ├── shell.rs
│   ├── gl_area.rs
│   └── neovide_surface.rs
│
└── vendor/
    └── neovide/
```

The first version only implements:

```text
┌──────────────────────┬────────────┐
│                      │            │
│       Neovide        │ Empty      │
│                      │            │
└──────────────────────┴────────────┘
```

No Claude yet.

---

# 17. Fallbacks if the primary route fails

## Fallback A: two windows

```text
Neovide Window | Claude Window
```

Simplest to support on Wayland.

Pros:

- Keeps Neovide fully intact
- Claude can use a WebView freely
- Extremely low integration risk

Cons:

- UX isn't as good as a single-window IDE

This is the best fallback.

---

## Fallback B: winit + a native agent UI

```text
winit
├── Neovide
└── native AgentPanel
```

Pros:

- No WebKitGTK dependency
- Simpler on Wayland

Cons:

- Significantly more work for markdown/diff/rich UI

---

## Fallback C: keep Neovide standalone, add only an agent sidecar

Minimal-invasion version:

```text
Neovide
+
Agent sidecar
+
Neovim RPC
```

If shell-ifying turns out too costly, fall back to this.

---

# 18. Final Go decision

Only once ALL of the following pass:

- `GtkGLArea + Skia` works normally at 165Hz
- The Neovide renderer can be stably component-ized
- Neovim input behavior is normal
- Chinese IME works normally
- HiDPI/fractional scaling works normally
- WebKitGTK doesn't noticeably affect the editor pane
- Upstream merge cost is acceptable

does the project formally move into:

```text
Claude Agent
Project Tree
Terminal
Git
IDE features
```

Otherwise, adjust the architecture first.

---

# 19. The single most important sentence

neovibe's biggest early risk isn't the agent, isn't Claude, and isn't
markdown.

What actually decides whether the project can exist at all is:

> **Whether Neovide's high-performance editing experience can survive being
> extracted into an Editor Surface inside a GTK4/Wayland shell.**

So phase 1 has to be built around experiments answering that question, not
around feature count.
