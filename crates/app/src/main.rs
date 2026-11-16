mod plan;
mod results;
mod schema;
mod theme;
mod workbench;
mod worker;

use std::borrow::Cow;

use gpui_kit::prelude::*;
use gpui_kit::{
    application, assets, init, App, Bounds, KeyBinding, Point, Size, TitlebarOptions, WindowBounds,
    WindowOptions,
};

/// Font family for ALL UI text, per the design system (spec §2): JetBrains
/// Mono everywhere, including toolbar, tree, grid, and status bar. The TTFs
/// are bundled under `assets/fonts/` so the UI renders identically with or
/// without the font installed.
pub const MONO_FONT_FAMILY: &str = "JetBrains Mono";

fn main() {
    // Checking for --version/-V is not a security operation: argv[0] is never read.
    // nosemgrep: rust.lang.security.args.args
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("gpui-like-toad {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            init(cx);
            load_bundled_fonts(cx);
            // Cmd/Ctrl+Enter and F5 run the active query. "cmd" is the
            // platform modifier (Command on macOS, Super/Win elsewhere);
            // the explicit ctrl-enter binding covers Windows/Linux Ctrl.
            cx.bind_keys([
                KeyBinding::new("cmd-enter", workbench::RunQuery, Some("Workbench")),
                KeyBinding::new("ctrl-enter", workbench::RunQuery, Some("Workbench")),
                KeyBinding::new("f5", workbench::RunQuery, Some("Workbench")),
            ]);
            let pref = ora_core::store::load_theme_preference();
            let mut options = WindowOptions::default();
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::new(
                Point::default(),
                Size {
                    width: gpui_kit::px(1280.),
                    height: gpui_kit::px(800.),
                },
            )));
            options.titlebar = Some(TitlebarOptions {
                title: Some("DB Explorer".into()),
                appears_transparent: false,
                traffic_light_position: None,
            });
            cx.open_window(options, |window, cx| {
                theme::apply_preference(pref, window, cx);
                cx.new(|cx| workbench::Workbench::new(window, cx))
            })
            .expect("failed to open window");
        });
}

/// Install the bundled JetBrains Mono faces into the text system. Failure
/// is non-fatal: shaping falls back to an installed monospace font.
///
/// Code ligatures (`->`, `=>`, `!=`, ...) render joined because the text
/// system leaves the `calt` OpenType feature on by default; nothing in the
/// editor path disables it.
fn load_bundled_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")),
        Cow::Borrowed(include_bytes!(
            "../assets/fonts/JetBrainsMono-BoldItalic.ttf"
        )),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("warning: bundled fonts failed to load: {err}");
    }
}
