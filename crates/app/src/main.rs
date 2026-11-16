mod results;
mod schema;
mod workbench;
mod worker;

use std::borrow::Cow;

use gpui_kit::{application, assets, init, App, WindowOptions};

/// UI font family. Preferred when installed on the system (e.g. alongside a
/// JetBrains IDE); otherwise the platform default is used.
pub const SANS_FONT_FAMILY: &str = "JetBrains Sans";
/// Editor and plan-view font family. Bundled under `assets/fonts/` so the
/// editor renders identically with or without the font installed.
pub const MONO_FONT_FAMILY: &str = "JetBrains Mono";

fn main() {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("gpui-like-toad {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            init(cx);
            load_bundled_fonts(cx);
            let theme = gpui_kit::base::Theme::global_mut(cx);
            theme.tokens.typography.sans = SANS_FONT_FAMILY.into();
            theme.tokens.typography.mono = MONO_FONT_FAMILY.into();
            cx.open_window(WindowOptions::default(), |window, cx| {
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
