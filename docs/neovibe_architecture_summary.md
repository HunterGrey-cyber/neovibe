# neovibe project summary and starting baseline

*[中文原文](zh/neovibe_architecture_summary.md)*

## 1. Project positioning

**neovibe** is a lightweight IDE built around Neovide/Neovim as its core
editing experience, extended for AI coding / agent workflows.

The goal is not to reimplement a text editor, and not to bolt a chat window
onto Neovide as an afterthought. Instead:

> Progressively extract Neovide from a "complete application" into an
> embeddable, high-performance `EditorSurface`, hosted by an independent IDE
> shell that owns layout, panes, window management, and the agent UI.

The end state can be compared to Zed's workspace layout:

```text
┌──────────────────────────────────────────────────┐
│                    neovibe                       │
├──────────┬────────────────────────┬──────────────┤
│ Project  │                        │ Claude       │
│ Git      │      Neovide Editor    │ Agent        │
│          │      + Neovim          │ Tasks        │
│          │                        │ Diff         │
├──────────┴────────────────────────┴──────────────┤
│ Terminal / Problems / Logs                       │
└──────────────────────────────────────────────────┘
```

But the center editing area is real Neovide + Neovim, not a reimplementation
of Vim behavior.

---

## 2. Core design principles

### 2.1 Neovide keeps its native performance path

The editor's core path stays:

```text
Keyboard / Mouse
      ↓
   Neovim
      ↓
Neovide Renderer
      ↓
    Skia
      ↓
     GPU
```

Not introduced:

- A terminal-emulator relay
- A WebView relay
- Framebuffer screenshot compositing
- Bidirectional sync between a Zed-style buffer and the Neovim buffer

Principle:

> **Component-ize Neovide, don't virtualize it.**

---

### 2.2 The shell only owns IDE-layer capabilities

The shell owns:

- The top-level window
- Layout / dock / pane management
- Splitters
- Focus
- Top bar
- Sidebar
- Status bar
- The Claude / agent panel
- Workspace lifecycle
- Later: terminal / git / problems / preview

The shell does NOT own:

- The Neovim buffer
- Vim motions
- LSP
- Completion
- Treesitter
- The Neovim plugin system
- Editor rendering

These stay Neovim's/Neovide's responsibility.

---

### 2.3 Neovide as an `EditorSurface`

The ideal boundary:

```rust
trait EditorSurface {
    fn resize(&mut self, rect: Rect);
    fn focus(&mut self);
    fn handle_input(&mut self, event: InputEvent);
    fn render(&mut self, ctx: &mut RenderContext);
}
```

First implementation:

```text
EditorSurface
    ↓
NeovideEditor
    ↓
Neovim
```

Extensible later to:

```text
EditorSurface
├── NeovideEditor
├── DiffView
├── Preview
└── TerminalItem
```

V1 does not need a dynamic plugin system — just clean crate/trait
boundaries.

---

## 3. Recommended tech stack

### 3.1 Linux/Wayland as the primary route

```text
GTK4 Shell
├── Custom Rust UI
│   ├── Top Bar
│   ├── Sidebar
│   ├── Status Bar
│   └── Splitter
│
├── NeovideSurface
│   └── GtkGLArea
│       └── Skia
│           └── Neovide Renderer
│               └── Neovim
│
└── AgentPanel
    └── WebKitGTK
        └── HTML / CSS / TS
```

Rationale:

- GTK4 is mature, first-class Wayland support
- WebKitGTK is the natural way to embed web content on Linux
- GTK only handles windowing, layout, focus, IME, and container mechanics
- Neovide keeps its own Skia/GPU rendering path
- The Claude UI can use the mature web ecosystem for markdown / diff / code
  highlighting

---

### 3.2 GTK's role

GTK is not neovibe's "visual style."

GTK only provides:

- Windowing
- Layout
- Widget mechanics
- Focus
- IME
- Drag & drop
- Accessibility
- A WebKitGTK container

The visual layer is built separately:

```text
neovibe-ui
├── Theme Tokens
├── TopBar
├── Tab
├── IconButton
├── Sidebar
├── StatusBar
└── Split
```

Written in Rust + gtk4-rs, hiding GTK's default visuals via custom CSS/classes.

Principle:

> **GTK provides the mechanism; neovibe defines its own design language.**

---

## 4. A unified visual system

A shared theme:

```rust
struct Theme {
    background: Color,
    surface: Color,
    elevated: Color,
    border: Color,

    text: Color,
    text_muted: Color,

    accent: Color,

    radius: f32,
    spacing: f32,
}
```

Synced across:

```text
Theme
├── GTK CSS
├── Neovide / Neovim theme bridge
└── WebView CSS variables
```

Target look:

```text
┌──────────────────────────────────────────────┐
│ neovibe       main.go                 — □ ×  │
├─────────┬───────────────────────┬────────────┤
│ Project │                       │ Claude     │
│         │      Neovide          │            │
│         │                       │            │
├─────────┴───────────────────────┴────────────┤
│ NORMAL  main.go                  Ln 42 Col 8 │
└──────────────────────────────────────────────┘
```

It should read as one unified product, not GTK + Neovide + a browser stitched
together.

---

## 5. Claude / agent architecture

### 5.1 Agent UI

The agent UI is a WebView:

```text
WebKitGTK
└── React / Vue / TS
    ├── Markdown
    ├── Syntax highlighting
    ├── Diff
    ├── Tool calls
    ├── Permissions
    ├── Plan
    ├── Tasks
    └── Composer
```

Why:

- The markdown/code-block/table/diff ecosystem is mature
- High UI development velocity
- No demanding refresh-rate or ultra-low-latency requirements
- Doesn't touch Neovide's native editing performance path

---

### 5.2 Agent state should not live in the WebView as the sole source of truth

Recommended:

```text
             Rust Host
                │
        AgentSessionState
           /                  Claude         WebView
```

Rust holds:

- session id
- messages
- tool calls
- permissions
- cwd
- task
- status

The WebView is a pure view layer.

This way, a WebView reload or crash can recover state.

---

### 5.3 Decoupling the agent from the editor

A unified capability trait:

```rust
trait EditorContext {
    async fn current_file(&self) -> Option<PathBuf>;
    async fn cursor(&self) -> Option<Position>;
    async fn selection(&self) -> Option<Selection>;
    async fn buffer_text(&self) -> Option<String>;
    async fn diagnostics(&self) -> Vec<Diagnostic>;
}
```

Implemented by Neovide as:

```text
EditorContext
      ↓
Neovim RPC
```

Claude/agent code should never depend on Neovide's internals directly.

---

## 6. Key boundaries Neovide needs refactored

### 6.1 Don't turn Neovide into a general-purpose GUI library in one shot

Minimize the split in the fork first.

Target:

```text
Neovide App
    ↓
NeovideEditor / NeovideSurface
```

Mostly kept intact:

- Renderer
- Bridge / Neovim runtime
- Input
- Font
- Animation
- Grid state

Taken over by the shell:

- OS window
- Layout
- Surface allocation
- Focus
- IME host
- Presentation

---

### 6.2 The renderer only paints its own viewport

Neovide originally assumes it owns the whole window.

Needs to become:

```text
Shell Canvas / GL Area
├── editor_rect
└── other UI
```

The Neovide renderer may only clear/paint `editor_rect`, never the whole
host surface.

---

### 6.3 Geometry becomes viewport-based

Previously:

```text
Window Size
  ↓
Grid Size
```

Becomes:

```text
Editor Viewport Size
  ↓
Grid Size
  ↓
nvim_ui_resize
```

This lets the shell freely resize the editor region.

---

### 6.4 Input needs an adapter layer

Previously:

```text
winit WindowEvent
    ↓
Neovide Input
```

Going forward:

```text
GTK Event
    ↓
NeovideInputAdapter
    ↓
KeyboardManager / MouseManager
    ↓
Neovim
```

---

### 6.5 Window dependencies abstracted behind a Host trait

Something like:

```rust
trait SurfaceHost {
    fn request_redraw(&self);
    fn set_ime_enabled(&self, enabled: bool);
    fn set_ime_cursor_area(&self, rect: Rect);
    fn set_cursor(&self, cursor: Cursor);
    fn set_title(&self, title: &str);
}
```

`NeovideSurface` never touches a whole GTK/winit `Window` directly.

---

## 7. Recommended repository layout

Early on:

```text
neovibe/
├── shell/
│   ├── window
│   ├── layout
│   ├── focus
│   └── theme
│
├── neovide-editor/
│   ├── renderer
│   ├── input
│   ├── runtime
│   ├── surface
│   └── bridge
│
├── agent/
│   ├── backend
│   ├── session
│   ├── context
│   └── events
│
└── agent-ui/
    └── web
```

V1 does not do `.so`/`.dll` dynamic plugins.

Modularity comes entirely from:

- The Cargo workspace
- Crates
- Traits
- A command/event bus

---

## 8. Explicitly out of scope for phase 1

To keep the project from spiraling, phase 1 does not build:

- A custom text editor
- A custom Vim mode
- A custom LSP
- Custom completion
- A custom Treesitter integration
- A dynamic plugin ABI
- Multi-agent support
- A complex project tree
- Git IDE features
- Remote SSH
- Terminal tabs
- A custom Skia markdown renderer
- Multiple editor backends

The only goal is proving the core route is viable.

---

## 9. Recommended development phases

### Phase 0 — Surface PoC

Goal:

```text
GTK Window
├── NeovideSurface
└── Empty Pane
```

Validate that Neovide can exist as a component at all.

---

### Phase 1 — Shell

Implement:

- A custom top bar
- Horizontal split
- Resize
- Focus
- Theme tokens

The right-hand pane can still be empty.

---

### Phase 2 — Web agent UI

```text
NeovideSurface | WebKitGTK
```

Initially just:

- An input box
- Plain markdown
- Streaming text

---

### Phase 3 — Neovim context bridge

Support:

- Current file
- Cursor
- Selection
- Current buffer
- cwd
- Diagnostics

---

### Phase 4 — Claude agent

Support:

- Send
- Interrupt
- Resume
- Tool calls
- Permissions
- Session

---

### Phase 5 — Becoming an IDE

Then consider:

- Project tree
- Git
- Terminal
- Problems
- Diff review
- Task / plan
- Multi-agent
- Remote

---

## 10. Current recommended conclusion

**Recommended primary architecture:**

```text
GTK4
  ↓
neovibe Shell
├── NeovideSurface
│   └── Skia/GPU + Neovim
│
└── AgentPanel
    └── WebKitGTK
```

Core principle:

> Neovide is the editor engine, GTK is the host, WebKitGTK is the agent's
> presentation layer, and the Rust host coordinates data and lifecycle
> between the two sides.

The most important thing in phase 1 is not wiring up Claude — it's
validating:

> **Once Neovide's renderer is extracted into a surface under a GTK4/Wayland
> shell, does performance and input feel stay close enough to native
> Neovide?**

As long as that holds, neovibe's overall architecture can be considered
settled.
