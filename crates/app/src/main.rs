mod completion;
mod keychain;
mod plan;
mod results;
mod schema;
mod theme;
mod workbench;
mod worker;

use std::borrow::Cow;

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, ShowCompletions, Undo};
use gpui_kit::prelude::*;
use gpui_kit::{
    actions, application, assets, init, App, Bounds, KeyBinding, Menu, MenuItem, OsAction, Point,
    Size, SystemMenuType, TitlebarOptions, WindowBounds, WindowOptions,
};

actions!(
    db_explorer_app,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        NewWindow,
        Minimize,
        Zoom,
        ToggleFullScreen
    ]
);

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
    let app = application()
        // AllAssets, NOT Assets: the default bundle embeds only the 106
        // icons in default-icons.txt, so every other Lucide icon used by
        // this app (Database, Clock, Table, House, ChartColumn, ...) silently
        // rendered blank. AllAssets embeds the full catalog.
        .with_assets(assets::AllAssets);
    // macOS keeps the process alive after its last window is closed (red
    // button), with the icon still in the Dock. Without this handler a Dock
    // click did nothing and the only way back was Dock > Quit and relaunch.
    // GPUI calls this when the user clicks the Dock icon (or relaunches the
    // app) while no window is visible: bring back an existing (minimized or
    // hidden) window if there is one, otherwise open a fresh one.
    app.on_reopen(|cx| {
        if let Some(window) = cx.windows().into_iter().next() {
            let _ = window.update(cx, |_, window, _| window.activate_window());
            cx.activate(true);
        } else {
            open_main_window(cx);
        }
    });
    app.run(|cx: &mut App| {
        init(cx);
        load_bundled_fonts(cx);
        // Cmd/Ctrl+Enter and F5 run the active query. "cmd" is the
        // platform modifier (Command on macOS, Super/Win elsewhere);
        // the explicit ctrl-enter binding covers Windows/Linux Ctrl.
        cx.bind_keys([
            KeyBinding::new("cmd-enter", workbench::RunQuery, Some("Workbench")),
            KeyBinding::new("ctrl-enter", workbench::RunQuery, Some("Workbench")),
            KeyBinding::new("f5", workbench::RunQuery, Some("Workbench")),
            // Open the SQL autocomplete popup by hand (it also opens by
            // itself while typing). Only editors with a completion
            // provider react, i.e. the SQL tabs.
            KeyBinding::new("ctrl-space", ShowCompletions, Some("Input")),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        ]);
        // GPUI installs no menu bar by default, so on macOS the top bar
        // showed only the app name and Cmd+Q / Cmd+H did nothing.
        install_menus(cx);
        open_main_window(cx);
        cx.activate(true);
    });
}

/// The standard macOS menu bar: app menu (Hide/Quit), Edit (clipboard and
/// undo, routed to the focused text input), View and Window. On Linux and
/// Windows GPUI ignores `set_menus`, but the actions and key bindings still
/// work there.
fn install_menus(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| open_main_window(cx));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |w| w.toggle_fullscreen()));

    cx.set_menus([
        Menu::new("DB Explorer").items([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide DB Explorer", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit DB Explorer", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("New Window", NewWindow)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([MenuItem::action("Toggle Full Screen", ToggleFullScreen)]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui_kit::Window)) {
    if let Some(handle) = cx.active_window() {
        let _ = handle.update(cx, |_, window, _| f(window));
    }
}

/// Open the main workbench window. Used at launch and again from the macOS
/// reopen handler when every window has been closed.
fn open_main_window(cx: &mut App) {
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
    if let Err(err) = cx.open_window(options, |window, cx| {
        theme::apply_preference(pref, window, cx);
        cx.new(|cx| workbench::Workbench::new(window, cx))
    }) {
        eprintln!("error: failed to open window: {err:#}");
    }
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
