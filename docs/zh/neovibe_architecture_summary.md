# neovibe 项目总结与开工基线

*[English translation](../neovibe_architecture_summary.md)*

## 1. 项目定位

**neovibe** 是一个以 Neovide / Neovim 为核心编辑体验、面向 AI Coding / Agent 工作流扩展的轻量 IDE。

目标不是重新实现文本编辑器，也不是给 Neovide 简单外挂一个聊天窗口，而是：

> 将 Neovide 从“完整应用”逐步抽成可嵌入的高性能 `EditorSurface`，由一个独立的 IDE Shell 负责布局、面板、窗口管理和 Agent UI。

最终形态可以类比 Zed 的工作区结构：

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

但中央编辑区是真正的 Neovide + Neovim，而不是重新实现 Vim 行为。

---

## 2. 核心设计原则

### 2.1 Neovide 保持原生性能路径

编辑器核心路径保持：

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

不引入：

- Terminal emulator 中转
- WebView 中转
- framebuffer 截图再合成
- Zed Buffer ↔ Neovim Buffer 双向同步

原则：

> **Component 化 Neovide，而不是虚拟化 Neovide。**

---

### 2.2 Shell 只负责 IDE 层能力

Shell 负责：

- 顶层窗口
- Layout / Dock / Pane
- Splitter
- Focus
- Top bar
- Sidebar
- Status bar
- Claude / Agent 面板
- Workspace 生命周期
- 后续 Terminal / Git / Problems / Preview

Shell 不负责：

- Neovim buffer
- Vim motions
- LSP
- Completion
- Treesitter
- Neovim plugin system
- Editor rendering

这些继续由 Neovim / Neovide 负责。

---

### 2.3 Neovide 作为 `EditorSurface`

理想边界：

```rust
trait EditorSurface {
    fn resize(&mut self, rect: Rect);
    fn focus(&mut self);
    fn handle_input(&mut self, event: InputEvent);
    fn render(&mut self, ctx: &mut RenderContext);
}
```

第一实现：

```text
EditorSurface
    ↓
NeovideEditor
    ↓
Neovim
```

未来可以扩展：

```text
EditorSurface
├── NeovideEditor
├── DiffView
├── Preview
└── TerminalItem
```

V1 不需要动态插件系统，只需要 crate / trait 边界清晰。

---

## 3. 推荐技术栈

### 3.1 Linux / Wayland 主路线

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

理由：

- GTK4 对 Wayland 是成熟的一等支持
- WebKitGTK 是 Linux 上嵌入 Web 内容的天然路径
- GTK 只承担窗口、布局、Focus、IME、容器能力
- Neovide 仍然走自己的 Skia/GPU 渲染
- Claude UI 可以使用成熟的 Web Markdown / Diff / Code Highlight 生态

---

### 3.2 GTK 的角色

GTK 不是 neovibe 的“视觉风格”。

GTK 只提供：

- Windowing
- Layout
- Widget mechanics
- Focus
- IME
- Drag & Drop
- Accessibility
- WebKitGTK 容器

视觉层自己做：

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

使用 Rust + gtk4-rs 编写，并通过自定义 CSS / class 隐藏 GTK 默认视觉。

原则：

> **GTK 提供机制，neovibe 自己定义设计语言。**

---

## 4. 统一视觉系统

建立统一 Theme：

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

同步给：

```text
Theme
├── GTK CSS
├── Neovide / Neovim theme bridge
└── WebView CSS variables
```

目标：

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

看起来是一个统一产品，而不是 GTK + Neovide + 浏览器拼接。

---

## 5. Claude / Agent 架构

### 5.1 Agent UI

Agent UI 推荐 WebView：

```text
WebKitGTK
└── React / Vue / TS
    ├── Markdown
    ├── Syntax Highlight
    ├── Diff
    ├── Tool Calls
    ├── Permission
    ├── Plan
    ├── Tasks
    └── Composer
```

原因：

- Markdown / Code Block / Table / Diff 生态成熟
- UI 开发效率高
- 对刷新率和极低延迟要求不高
- 不影响 Neovide 的原生编辑性能路径

---

### 5.2 Agent 状态不要放在 WebView 作为唯一真相

推荐：

```text
             Rust Host
                │
        AgentSessionState
           /                  Claude         WebView
```

Rust 保存：

- session id
- messages
- tool calls
- permissions
- cwd
- task
- status

WebView 只是 View。

这样 WebView reload / crash 后可以恢复。

---

### 5.3 Agent 与 Editor 解耦

定义统一能力：

```rust
trait EditorContext {
    async fn current_file(&self) -> Option<PathBuf>;
    async fn cursor(&self) -> Option<Position>;
    async fn selection(&self) -> Option<Selection>;
    async fn buffer_text(&self) -> Option<String>;
    async fn diagnostics(&self) -> Vec<Diagnostic>;
}
```

Neovide 实现：

```text
EditorContext
      ↓
Neovim RPC
```

Claude 不应直接依赖 Neovide 内部结构。

---

## 6. Neovide 需要重构的关键边界

### 6.1 不要一次性把 Neovide 改成通用 GUI library

先在 fork 中最小化拆分。

目标：

```text
Neovide App
    ↓
NeovideEditor / NeovideSurface
```

主要保留：

- Renderer
- Bridge / Neovim runtime
- Input
- Font
- Animation
- Grid state

Shell 接管：

- OS Window
- Layout
- Surface allocation
- Focus
- IME host
- Presentation

---

### 6.2 Renderer 只画自己的 viewport

Neovide 原本默认拥有整个窗口。

要改成：

```text
Shell Canvas / GL Area
├── editor_rect
└── other UI
```

Neovide renderer 只允许清理 / 绘制 `editor_rect`，不能 clear 整个宿主 surface。

---

### 6.3 Geometry 改成基于 viewport

原来：

```text
Window Size
  ↓
Grid Size
```

改成：

```text
Editor Viewport Size
  ↓
Grid Size
  ↓
nvim_ui_resize
```

这样 Shell 可以任意改变 editor 区域大小。

---

### 6.4 Input 需要适配层

原来：

```text
winit WindowEvent
    ↓
Neovide Input
```

未来：

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

### 6.5 Window 依赖抽象成 Host

类似：

```rust
trait SurfaceHost {
    fn request_redraw(&self);
    fn set_ime_enabled(&self, enabled: bool);
    fn set_ime_cursor_area(&self, rect: Rect);
    fn set_cursor(&self, cursor: Cursor);
    fn set_title(&self, title: &str);
}
```

NeovideSurface 不直接依赖整个 GTK / winit Window。

---

## 7. 推荐仓库结构

前期：

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

V1 不做 `.so/.dll` 动态插件。

模块化只通过：

- Cargo workspace
- crate
- trait
- command/event bus

即可。

---

## 8. 第一阶段明确不做

为了避免项目失控，前期不做：

- 自己实现文本编辑器
- 自己实现 Vim mode
- 自己实现 LSP
- 自己实现 completion
- 自己实现 Treesitter
- 动态插件 ABI
- 多 Agent
- 复杂 Project Tree
- Git IDE
- Remote SSH
- Terminal tabs
- 自定义 Skia Markdown renderer
- 多种 Editor backend

只证明核心路线可行。

---

## 9. 推荐开发阶段

### Phase 0 — Surface PoC

目标：

```text
GTK Window
├── NeovideSurface
└── Empty Pane
```

验证 Neovide 能否作为组件存在。

---

### Phase 1 — Shell

实现：

- 自定义 top bar
- horizontal split
- resize
- focus
- theme tokens

右侧仍然可以是空 pane。

---

### Phase 2 — Web Agent UI

```text
NeovideSurface | WebKitGTK
```

先只显示：

- 输入框
- 普通 Markdown
- streaming text

---

### Phase 3 — Neovim Context Bridge

支持：

- current file
- cursor
- selection
- current buffer
- cwd
- diagnostics

---

### Phase 4 — Claude Agent

支持：

- send
- interrupt
- resume
- tool calls
- permissions
- session

---

### Phase 5 — IDE 化

再考虑：

- Project Tree
- Git
- Terminal
- Problems
- Diff review
- Task / Plan
- multi-agent
- remote

---

## 10. 当前推荐结论

**推荐主架构：**

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

核心原则：

> Neovide 是 Editor Engine，GTK 是 Host，WebKitGTK 是 Agent Presentation，Rust Host 是两边的数据和生命周期协调器。

第一阶段最重要的事情不是接 Claude，而是验证：

> **GTK4 / Wayland 下，Neovide 的 renderer 被抽成 surface 后，性能和输入体验是否仍然足够接近原生 Neovide。**

只要这一点成立，neovibe 的整体架构就可以正式确定。
