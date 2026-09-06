# neovibe 前期可行性调查与 Go / No-Go 测试计划

*[English translation](../neovibe_feasibility_validation.md)*

## 1. 目的

这份文档用于在正式开发 neovibe 之前，优先验证最可能导致架构失败的技术风险。

目标不是测试全部功能，而是尽快回答：

> **“GTK4 + Wayland + Neovide/Skia + WebKitGTK 这条路线，能否在保留 Neovide 编辑体验的前提下支撑一个 IDE Shell？”**

所有测试都应先于 Claude 功能开发完成。

---

# 2. 总体 Go / No-Go 标准

主路线：

```text
GTK4 Shell
├── GtkGLArea
│   └── Skia
│       └── Neovide Renderer
│
└── WebKitGTK
    └── Claude UI
```

只要以下四项中出现无法修复的问题，就需要重新评估主架构：

1. 编辑区帧 pacing 明显差于原生 Neovide
2. IME / 输入延迟不可接受
3. 4K + fractional scaling 存在严重问题
4. WebKitGTK 持续刷新明显拖慢编辑区

---

# 3. P0：GTK4 + GtkGLArea + Skia 基础验证

## 目标

先完全不引入 Neovide。

验证：

```text
GTK4
 ↓
GtkGLArea
 ↓
OpenGL
 ↓
Skia Surface
```

能否稳定工作。

## 测试内容

实现最小程序：

```text
GtkApplicationWindow
└── GtkGLArea
    └── Skia Canvas
        ├── text
        ├── rectangles
        └── animation
```

必须测试：

- 60 Hz
- 120 Hz
- 144 Hz
- 165 Hz
- resize
- maximize / restore
- fractional scaling
- Wayland
- 多屏切换
- GPU 占用
- CPU 占用

## 重点观察

- frame pacing 是否均匀
- resize 是否有黑帧 / 闪烁
- scaling 后文字是否清晰
- GPU 是否正常加速
- 是否出现明显 CPU copy

## Go 条件

- 165Hz 下动画主观流畅
- 无明显周期性卡顿
- resize 稳定
- fractional scaling 正常

## No-Go 条件

- GLArea 无法稳定驱动 Skia
- GTK composition 带来明显 frame pacing 问题
- 4K/165Hz 下 CPU/GPU 开销异常

---

# 4. P1：Neovide Renderer 脱离原 Window

## 目标

证明 Neovide renderer 可以不拥有完整 OS Window，而是：

```text
GtkGLArea
   ↓
Skia Canvas
   ↓
Neovide Renderer
```

## 工作内容

先不要接完整 Neovim runtime。

优先验证：

- Neovide Font
- Grid rendering
- Cursor rendering
- Animation
- Scroll animation
- Highlight
- Background

## 必须修改

确认 renderer：

- 只 clear 自己 viewport
- 不假设 canvas == whole window
- viewport 可以 resize
- 不直接依赖 winit Window

## Go 条件

视觉上与原版 Neovide 基本一致。

重点包括：

- 字体
- ligature
- cursor
- smooth scroll
- animation timing
- highlight

## No-Go 条件

如果 renderer 与 winit / SkiaRenderer 耦合程度远高于预期，导致需要大量重写底层 renderer，应暂停并重新评估。

---

# 5. P2：完整 Neovim Runtime 接入

## 目标

跑真正的：

```text
GtkGLArea
 ↓
Neovide
 ↓
nvim --embed
```

## 测试内容

必须覆盖：

- 普通输入
- Normal / Insert
- Visual
- command mode
- floating window
- popup menu
- completion
- LazyVim
- fzf-lua / Telescope 类插件
- diagnostics
- large file
- rapid scrolling
- rapid key repeat

## 对比基线

必须同时运行官方 Neovide。

同一个：

- Neovim config
- font
- theme
- project
- display

进行 A/B 测试。

## Go 条件

主观编辑体验与官方 Neovide接近。

可以接受 Shell 带来少量开销，但不能明显影响：

- typing latency
- scroll smoothness
- cursor animation
- popup response

---

# 6. P3：输入系统

这是高风险项。

## 测试内容

### Keyboard

- 普通英文输入
- 高速重复按键
- Ctrl / Alt / Super
- function keys
- leader key
- key chord
- key repeat

### Mouse

- click
- drag
- wheel
- horizontal scroll
- text selection
- resize

### Focus

```text
Neovide → Shell
Shell → Claude
Claude → Neovide
```

测试 Focus 切换是否丢键。

## Go 条件

Neovim 行为与官方 Neovide基本一致。

---

# 7. P4：IME

这是必须单独作为 P0/P1 级别对待的验证。

## 测试

至少测试：

- 中文拼音 IME
- candidate window
- composition text
- cursor position
- Insert mode
- Normal → Insert
- Claude → Neovim focus 回切

## 检查

候选框必须出现在正确的 Neovim cursor 附近。

特别测试：

```text
4K
fractional scale
multi-monitor
vertical monitor
```

## No-Go 风险

如果 GTK Input Method → Neovide → Neovim 的适配导致：

- composition 丢失
- candidate window 位置错误
- focus 后 IME 状态异常
- 输入有明显延迟

必须优先解决，不能先跳过。

---

# 8. P5：HiDPI / Fractional Scaling

目标环境特别重要。

## 测试矩阵

至少：

```text
100%
125%
150%
175%
200%
250%
```

覆盖：

- GtkGLArea
- Skia
- Neovide grid
- mouse coordinate
- cursor
- IME position
- WebView
- splitter

## 多屏

测试：

- 普通横屏
- 高 DPI 横屏
- 竖屏
- 不同 scale monitor 之间拖动

## Go 条件

坐标系统没有明显漂移。

尤其：

```text
mouse pixel
 ↓
Neovim grid
```

必须准确。

---

# 9. P6：WebKitGTK 共存压力测试

## 目标

确认：

> 右边 Claude WebView 很忙时，不影响左边 Neovide。

## 测试布局

```text
GtkPaned
├── Neovide GtkGLArea
└── WebKitWebView
```

WebView 持续：

- streaming text
- Markdown layout
- syntax highlighting
- auto scroll
- large code blocks
- large conversation

同时左侧：

- 快速输入
- continuous scrolling
- smooth cursor
- large buffer

## 测量

观察：

- editor FPS
- frame time
- input latency
- UI thread stalls
- CPU
- GPU
- memory

## Go 条件

WebView workload 不应造成肉眼明显的编辑区 jitter。

如果 WebKitGTK 会阻塞 GTK main loop，必须调查：

- JS 更新策略
- batching
- requestAnimationFrame
- token chunk batching
- virtualized list

---

# 10. P7：Pane Resize

不断拖动：

```text
Neovide | Claude
```

之间的 splitter。

必须验证：

```text
GtkPaned resize
 ↓
Neovide viewport resize
 ↓
grid size
 ↓
nvim_ui_resize
```

连续工作正常。

观察：

- Neovim flicker
- grid corruption
- CPU spike
- animation glitches

---

# 11. P8：Theme 一致性

不是阻断性风险，但最好前期验证。

做一个统一主题：

```text
Theme Tokens
├── GTK CSS
├── Neovim
└── Web CSS
```

实现：

- background
- surface
- border
- text
- muted text
- accent

验证视觉是否真的能做到统一，而不是：

```text
GTK app
+
Neovide
+
browser
```

三块拼接感。

---

# 12. P9：自定义 Top Bar

实现一个最小：

```text
┌──────────────────────────────────────┐
│ neovibe  project             — □ ×   │
└──────────────────────────────────────┘
```

使用 Rust + gtk4-rs。

要求：

- 不使用明显 Adwaita 默认视觉
- 自定义 spacing / font / color
- Wayland window controls 正常
- drag window 正常
- maximize 正常

目的：

证明未来 IDE 化不会被 GTK 的默认外观限制。

---

# 13. P10：Neovide Upstream 可维护性

这是长期必须调查的。

## 做法

fork 后建立：

```text
upstream/neovide
neovibe/main
```

前期尽量把修改集中在：

- surface host
- geometry
- input adapter
- renderer viewport

而不是修改：

- font renderer
- grid renderer
- animation logic
- Neovim bridge protocol

## 测试

完成第一次 surface PoC 后：

1. 拉取 upstream 最新提交
2. merge / rebase
3. 记录冲突数
4. 判断维护成本

## Go 条件

大部分 Neovide renderer / bridge 更新可以直接合并。

---

# 14. P11：性能基线

必须先测官方 Neovide。

记录：

```text
Idle CPU
Idle GPU
Typing CPU/GPU
Fast scroll
Memory
Frame time
Startup
```

然后测试 neovibe PoC。

不要只凭：

> “感觉差不多”

至少记录简单定量数据。

目标不是 benchmark 冠军，而是：

> neovibe 不应明显破坏 Neovide 的体验优势。

---

# 15. 建议测试顺序

严格按以下顺序：

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

Claude API 接入放在这些之后。

---

# 16. 建议的第一阶段 PoC 仓库

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

第一版只实现：

```text
┌──────────────────────┬────────────┐
│                      │            │
│       Neovide        │ Empty      │
│                      │            │
└──────────────────────┴────────────┘
```

不要 Claude。

---

# 17. 主方案失败时的 fallback

## Fallback A：双窗口

```text
Neovide Window | Claude Window
```

Wayland 支持最简单。

优点：

- 保留完整 Neovide
- Claude 随便用 WebView
- 极低集成风险

缺点：

- UX 不如单窗口 IDE

这是最佳保底。

---

## Fallback B：winit + Native Agent UI

```text
winit
├── Neovide
└── native AgentPanel
```

优点：

- 不依赖 WebKitGTK
- Wayland 简洁

缺点：

- Markdown / Diff / Rich UI 工作量明显增加

---

## Fallback C：保持 Neovide App，只做 Agent Sidecar

最小侵入：

```text
Neovide
+
Agent sidecar
+
Neovim RPC
```

如果 Shell 化成本过高，可以退回这个版本。

---

# 18. 最终 Go 决策

只有当以下全部通过：

- `GtkGLArea + Skia` 165Hz 正常
- Neovide renderer 可稳定 component 化
- Neovim 输入行为正常
- 中文 IME 正常
- HiDPI / fractional scaling 正常
- WebKitGTK 不明显影响编辑区
- upstream merge 成本可接受

才正式进入：

```text
Claude Agent
Project Tree
Terminal
Git
IDE features
```

否则先调整架构。

---

# 19. 最重要的一句话

neovibe 前期最危险的不是 Agent，不是 Claude，也不是 Markdown。

真正决定项目能不能成立的是：

> **Neovide 的高性能编辑体验，能否在 GTK4 / Wayland Shell 中作为一个 Editor Surface 被保留下来。**

所以第一阶段必须围绕这个问题做实验，而不是围绕功能数量做开发。
