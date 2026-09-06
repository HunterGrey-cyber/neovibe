//! shell_chrome — feasibility probe for neovibe's P9 ("自定义 Top Bar") test,
//! per docs/neovibe_feasibility_validation.md §12 and the shell-layer split
//! described in docs/neovibe_architecture_summary.md §2.2/§3.2/§4.
//!
//! This proves *shell chrome mechanics only*: custom top bar with working
//! Wayland window controls and drag-to-move, a GtkPaned splitter standing in
//! for the future Neovide|Agent split, a status bar strip, and a CSS theme
//! that suppresses default Adwaita styling. It does not embed Neovide, does
//! not talk to Neovim, and holds no real project/file state.

mod theme;

use gtk4::prelude::*;
use gtk4::{glib, Application};

const APP_ID: &str = "cn.huntergrey.neovibe.shell_chrome";

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let theme = theme::Theme::dark();
    apply_css(&theme.to_css());

    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("neovibe")
        .default_width(1024)
        .default_height(680)
        // No HeaderBar is ever set, and decorated(false) suppresses GTK's
        // own client-side-decoration titlebar entirely — the top bar below
        // is the *only* titlebar, fully custom-drawn.
        .decorated(false)
        .build();
    window.add_css_class("shell-root");

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("shell-root");

    root.append(&build_top_bar(&window));
    root.append(&build_content_area());
    root.append(&build_status_bar());

    window.set_child(Some(&root));
    window.present();
}

/// Custom top bar: app name + project placeholder + window controls, all
/// wrapped in a `WindowHandle` so the bar itself works as a drag-to-move /
/// double-click-to-maximize titlebar surface under Wayland (this is the
/// gtk4-rs idiom for CSD-less custom titlebars — GTK provides the mechanism,
/// we provide the look, per architecture doc §3.2).
fn build_top_bar(window: &gtk4::ApplicationWindow) -> gtk4::Widget {
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

/// Minimize / maximize-restore / close buttons wired to real Wayland window
/// operations (GTK only exposes windowing mechanism here; the icons/spacing
/// are ours).
fn build_window_controls(window: &gtk4::ApplicationWindow) -> gtk4::Widget {
    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    controls.set_valign(gtk4::Align::Center);
    controls.set_margin_end(6);

    let minimize = gtk4::Button::with_label("—");
    minimize.add_css_class("win-btn");
    {
        let window = window.clone();
        minimize.connect_clicked(move |_| window.minimize());
    }

    let maximize = gtk4::Button::with_label("\u{25A1}"); // □
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

    let close = gtk4::Button::with_label("\u{00D7}"); // ×
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

/// Horizontal split standing in for the future Neovide (left) / Agent panel
/// (right) layout. Both sides are just empty placeholder areas here — the
/// point of this probe is proving `GtkPaned` splitter mechanics and layout,
/// not real panes (see feasibility doc §10 P7, which this stands in front of).
fn build_content_area() -> gtk4::Widget {
    let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    paned.add_css_class("content-area");
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_wide_handle(true);

    let left = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    left.add_css_class("pane-placeholder");
    left.add_css_class("left");
    let left_label = gtk4::Label::new(Some("Neovide surface (placeholder)"));
    left_label.set_valign(gtk4::Align::Center);
    left_label.set_halign(gtk4::Align::Center);
    left_label.set_vexpand(true);
    left_label.set_hexpand(true);
    left.append(&left_label);

    let right = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    right.add_css_class("pane-placeholder");
    right.add_css_class("right");
    let right_label = gtk4::Label::new(Some("Agent panel (placeholder)"));
    right_label.set_valign(gtk4::Align::Center);
    right_label.set_halign(gtk4::Align::Center);
    right_label.set_vexpand(true);
    right_label.set_hexpand(true);
    right.append(&right_label);

    paned.set_start_child(Some(&left));
    paned.set_end_child(Some(&right));
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_position(700);

    paned.upcast()
}

/// Bottom status bar strip. Placeholder text matching the target mockup in
/// architecture doc §4 ("NORMAL ... Ln 42 Col 8") — no real editor state.
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
