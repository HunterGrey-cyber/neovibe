# neovibe

A lightweight IDE built around **Neovide/Neovim as the actual editing
experience**, extended with an AI coding agent panel — not a text editor
rewrite, and not "Neovide with a chat window bolted on." The goal is to
extract Neovide's renderer into an embeddable `EditorSurface` component
hosted inside an independent GTK4 shell that owns layout, panes, window
chrome, and the agent UI, the way Zed owns its workspace — except the center
pane is the real Neovide+Neovim renderer, not a reimplemented editor.

Guiding principle: **component-ize Neovide, don't virtualize it.** The real
input → render path (keyboard/mouse → Neovim → Neovide renderer → Skia → GPU)
is meant to be preserved as-is. No terminal-emulator relay, no WebView relay,
no framebuffer screenshot compositing, no bidirectional buffer sync between a
separate text model and Neovim's buffer.

## Status: pre-release, feasibility validation in progress

**There is no code here yet, and nothing is installable.** The architecture
above is currently being validated against a strict, ordered set of
technical risks (GPU rendering inside GTK4, embedding Neovide's renderer
outside its own window, real Neovim process integration, WebKitGTK
coexistence, HiDPI/IME behavior, upstream-merge maintainability) before any
proof-of-concept code or results are published here.

The two documents below are the plan for that validation, written before
work started:

- **[`docs/neovibe_architecture_summary.md`](docs/neovibe_architecture_summary.md)**
  — the target architecture and design principles.
- **[`docs/neovibe_feasibility_validation.md`](docs/neovibe_feasibility_validation.md)**
  — the ordered Go/No-Go test plan validating it.

Both are also available in their original Chinese under
[`docs/zh/`](docs/zh/).

**Once feasibility validation is complete, this repository will be updated
with the actual proof-of-concept code, measured results, and a real build/run
guide.** Until then, treat this as a public statement of intent and plan
rather than a working project.

## License

MIT — see [`LICENSE`](LICENSE).
