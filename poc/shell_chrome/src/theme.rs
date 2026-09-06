//! Minimal placeholder implementation of the `Theme` struct described in
//! `docs/neovibe_architecture_summary.md` §4 ("统一视觉系统").
//!
//! This is a throwaway feasibility-probe palette, not a real design system.
//! The same `Theme` shape is meant to eventually also drive the Neovim theme
//! bridge and the WebView CSS variables; here we only implement the GTK CSS
//! side (turning the token set into a `CssProvider`-loadable stylesheet).

/// Plain RGBA color, kept simple (no external color-crate dependency) since
/// all we need is to format it into CSS `rgb()`/`rgba()` literals.
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 1.0 }
    }

    #[allow(dead_code)] // part of the Theme token API even where unused by the current placeholder palette
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
        Color { r, g, b, a }
    }

    /// Render as a CSS color literal, e.g. `rgba(30, 32, 38, 1)`.
    pub fn to_css(self) -> String {
        format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, self.a)
    }
}

/// Unified visual token set (architecture doc §4). Fields are plain values,
/// not a builder/theming-engine — this is intentionally a placeholder.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub elevated: Color,
    pub border: Color,

    pub text: Color,
    pub text_muted: Color,

    pub accent: Color,

    pub radius: f32,
    pub spacing: f32,
}

impl Theme {
    /// One reasonable dark-mode placeholder palette.
    pub fn dark() -> Self {
        Theme {
            background: Color::rgb(0x1a, 0x1b, 0x1f),
            surface: Color::rgb(0x22, 0x23, 0x29),
            elevated: Color::rgb(0x2b, 0x2c, 0x34),
            border: Color::rgb(0x38, 0x3a, 0x44),

            text: Color::rgb(0xe6, 0xe6, 0xea),
            text_muted: Color::rgb(0x8b, 0x8d, 0x98),

            accent: Color::rgb(0x7c, 0xa9, 0xff),

            radius: 6.0,
            spacing: 8.0,
        }
    }

    /// Turn the theme tokens into a GTK CSS stylesheet string, suitable for
    /// loading into a `gtk4::CssProvider` and applying to the whole display.
    ///
    /// This deliberately overrides GTK/Adwaita defaults on `window`, `paned`,
    /// `button`, etc. so none of the default theme leaks through — the shell
    /// chrome is meant to have its own visual identity, not GTK's.
    pub fn to_css(&self) -> String {
        let bg = self.background.to_css();
        let surface = self.surface.to_css();
        let elevated = self.elevated.to_css();
        let border = self.border.to_css();
        let text = self.text.to_css();
        let text_muted = self.text_muted.to_css();
        let accent = self.accent.to_css();
        let radius = self.radius;
        let spacing = self.spacing;

        format!(
            r#"
window {{
    background-color: {bg};
    color: {text};
    font-family: sans-serif;
}}

.shell-root {{
    background-color: {bg};
}}

.topbar {{
    background-color: {surface};
    border-bottom: 1px solid {border};
    padding: 0 {spacing}px;
    min-height: 38px;
}}

.topbar-app-name {{
    color: {text};
    font-weight: 700;
    font-size: 13px;
    margin-right: {spacing}px;
}}

.topbar-project-name {{
    color: {text_muted};
    font-size: 12px;
}}

.win-btn {{
    background-color: transparent;
    background-image: none;
    border: none;
    box-shadow: none;
    color: {text_muted};
    min-width: 28px;
    min-height: 28px;
    padding: 0;
    border-radius: {radius}px;
}}

.win-btn:hover {{
    background-color: {elevated};
    color: {text};
}}

.win-btn.close:hover {{
    background-color: #e34b4b;
    color: #ffffff;
}}

.content-area {{
    background-color: {bg};
}}

paned.content-area > separator {{
    background-color: {border};
    min-width: 1px;
    min-height: 1px;
}}

.pane-placeholder {{
    background-color: {surface};
    color: {text_muted};
    font-size: 12px;
}}

.pane-placeholder.left {{
    background-color: {bg};
}}

.statusbar {{
    background-color: {surface};
    border-top: 1px solid {border};
    padding: 0 {spacing}px;
    min-height: 24px;
    color: {text_muted};
    font-size: 11px;
}}

.statusbar-accent {{
    color: {accent};
}}
"#
        )
    }
}
