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

## Status: pre-alpha, feasibility validation in progress

**Early, experimental code is available under `poc/`** — one scoped
proof-of-concept crate per technical risk being validated (GPU rendering
inside GTK4, embedding Neovide's renderer outside its own window, real
Neovim process integration, WebKitGTK coexistence, pane resize). This is not
a usable IDE yet: treat everything here as pre-alpha.

The two documents below are the plan behind that validation work, written
before it started:

- **[`docs/neovibe_architecture_summary.md`](docs/neovibe_architecture_summary.md)**
  — the target architecture and design principles.
- **[`docs/neovibe_feasibility_validation.md`](docs/neovibe_feasibility_validation.md)**
  — the ordered Go/No-Go test plan validating it.

Both are also available in their original Chinese under
[`docs/zh/`](docs/zh/).

**The full validation write-up (measured results, per-crate verification
checklists, dev-process docs) is still being finished and will be published
once feasibility validation wraps up.** What's here now is real, buildable
code — see [Building](#building) — just not yet the complete picture of
what's been proven.

## Building

This depends on a fork of upstream [neovide/neovide](https://github.com/neovide/neovide)
vendored in as a git submodule at `neovide/`:

```sh
git clone --recurse-submodules https://github.com/HunterGrey-cyber/neovibe.git
cd neovibe
# if you cloned without --recurse-submodules:
#   git submodule update --init --recursive

cargo build --manifest-path poc/Cargo.toml            # whole workspace
cargo build --manifest-path poc/Cargo.toml -p <crate> # one crate
cargo run   --manifest-path poc/Cargo.toml -p shell_composed
```

Building anything under `poc/` that opens a window needs a real Wayland
session (`WAYLAND_DISPLAY` set) and `nvim` on `PATH`. `poc/shell_composed`
(chrome + editor pane + agent WebView composed into one window) is the
closest thing here to a working prototype today.

## License

MIT — see [`LICENSE`](LICENSE).
