/// Display name shown to people: window title, macOS app menu, Home and
/// About. A macro (not a const) so it can be used inside `concat!`.
/// Crate/binary name, config directory, bundle identifier and keychain
/// service intentionally stay `gpui-like-toad` so existing profiles and
/// saved passwords keep working.
macro_rules! app_name {
    () => {
        "Ora The Explorer"
    };
}

mod completion;
mod crash_log;
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
        println!(concat!(app_name!(), " {}"), env!("CARGO_PKG_VERSION"));
        return;
    }
    crash_log::install();
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
        // gpui-kit's bundled color themes (Settings > Light/Dark theme).
        for err in theme::register_bundled_themes(cx) {
            eprintln!("warning: bundled theme failed to load: {err}");
        }
        theme::set_theme_names(ora_core::store::load_theme_names(), cx);
        // Cmd/Ctrl+Enter and F5 run the active query. "cmd" is the
        // platform modifier (Command on macOS, Super/Win elsewhere);
        // the explicit ctrl-enter binding covers Windows/Linux Ctrl.
        cx.bind_keys([
            KeyBinding::new("cmd-enter", workbench::RunQuery, Some("Workbench")),
            KeyBinding::new("ctrl-enter", workbench::RunQuery, Some("Workbench")),
            KeyBinding::new("f5", workbench::RunQuery, Some("Workbench")),
            KeyBinding::new("cmd-p", workbench::QuickOpenTable, Some("Workbench")),
            KeyBinding::new("ctrl-p", workbench::QuickOpenTable, Some("Workbench")),
            // Open the SQL autocomplete popup by hand (it also opens by
            // itself while typing). Only editors with a completion
            // provider react, i.e. the SQL tabs.
            KeyBinding::new("ctrl-space", ShowCompletions, Some("Input")),
            KeyBinding::new("cmd-,", workbench::OpenSettings, None),
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
        Menu::new(app_name!()).items([
            // "About" opens Settings, whose About section shows the version.
            MenuItem::action(concat!("About ", app_name!()), workbench::OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Settings…", workbench::OpenSettings),
            MenuItem::action("Check for Updates…", workbench::CheckForUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(concat!("Hide ", app_name!()), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(concat!("Quit ", app_name!()), Quit),
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
        Menu::new("Connection").items([
            MenuItem::action("New Connection…", workbench::NewConnection),
            MenuItem::separator(),
            MenuItem::action("Connect", workbench::ConnectLast),
            MenuItem::action("Disconnect", workbench::DisconnectSession),
            MenuItem::action("Edit Connection…", workbench::EditConnection),
            MenuItem::separator(),
            MenuItem::action("Refresh Schema", workbench::RefreshSchema),
        ]),
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
    // Below this the rail, the side panel's minimum width and the
    // toolbar no longer fit side by side, and content gets clipped.
    options.window_min_size = Some(Size {
        width: gpui_kit::px(820.),
        height: gpui_kit::px(520.),
    });
    // Linux: Wayland/X11 match this against `gpui-like-toad.desktop`
    // (installed by the release tarball's install.sh) for the launcher and
    // taskbar icon. Same id as the binary and config dir on purpose.
    options.app_id = Some("gpui-like-toad".into());
    // X11 also takes the window icon directly, so it shows even without
    // the .desktop file installed (Wayland ignores this).
    #[cfg(target_os = "linux")]
    {
        options.icon = linux_window_icon();
    }
    options.titlebar = Some(TitlebarOptions {
        title: Some(app_name!().into()),
        appears_transparent: false,
        traffic_light_position: None,
    });
    // `gpui_kit::open_window` wraps the content in a Root, which owns the
    // tooltip layer; with a bare `cx.open_window` no tooltip ever shows.
    if let Err(err) = gpui_kit::open_window(options, cx, |window, cx| {
        // Remember the platform's default UI font before the Root applies
        // the theme font over it.
        theme::remember_ui_font(window.text_style().font_family);
        theme::apply_preference(pref, window, cx);
        let view = cx.new(|cx| workbench::Workbench::new(window, cx));
        // Background, rate-limited, silent on failure.
        view.update(cx, |this, cx| this.auto_check_updates(cx));
        if let Some(panic) = crash_log::take_previous() {
            view.update(cx, |this, _| {
                this.push_message(format!(
                    "Sesi sebelumnya crash: {panic}. Detail lengkap ada di {}",
                    crash_log::log_path().display()
                ))
            });
        }
        view
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

/// 256 px app icon for the X11 window (`_NET_WM_ICON`).
#[cfg(target_os = "linux")]
fn linux_window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    let png = include_bytes!("../assets/icon/AppIcon.iconset/icon_256x256.png");
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()
        .map(|img| std::sync::Arc::new(img.into_rgba8()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn linux_window_icon_decodes() {
        let icon = super::linux_window_icon().expect("embedded PNG decodes");
        assert_eq!((icon.width(), icon.height()), (256, 256));
    }
}
