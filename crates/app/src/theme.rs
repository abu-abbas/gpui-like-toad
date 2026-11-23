//! DB Explorer design-system theme: violet accent scale, light/dark
//! palettes, density tokens, and the Light/Dark/System preference.
//!
//! The light palette and violet scale follow the v0.1 design-system spec.
//! Kilat Dark uses layered charcoal surfaces and the same semantic accents.
//! Density numbers (§3) are the single
//! source of truth for component heights, spacing, radius, and font sizes
//! used by the workbench layout.
//!
//! The palette is applied through gpui-component's [`Theme`]: first
//! [`Theme::change`] loads the built-in light/dark base, then a second
//! [`Theme::update`] overrides the colors (the order the component docs
//! require, so the Base projection and open windows stay in step).
//!
//! Besides that palette ("Kilat", the default) the user can pick one of the
//! color themes gpui-kit ships in its `themes/` folder, separately for light
//! and dark mode. Those files are bundled under `assets/themes/` and loaded
//! into gpui-kit's [`ThemeRegistry`] at startup; picking one sets
//! `Theme::light_theme`/`dark_theme`, and only fonts and radius are pinned
//! on top so the density stays the same.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::{rgb, App, Global, Hsla, Window};
use ora_core::store::{ThemeNames, ThemePreference};

use crate::MONO_FONT_FAMILY;

// ---------------------------------------------------------------------------
// Density tokens (spec §3) — heights in px
// ---------------------------------------------------------------------------

/// Toolbar height (Run / Stop / Explain / Format row).
pub const TOOLBAR_HEIGHT: f32 = 36.0;
/// Query tab bar height.
pub const TAB_BAR_HEIGHT: f32 = 36.0;
/// Schema tree row height.
pub const TREE_ROW_HEIGHT: f32 = 28.0;
/// Status bar height.
pub const STATUS_BAR_HEIGHT: f32 = 26.0;
/// Width of the compact icon rail.
pub const RAIL_WIDTH: f32 = 44.0;
/// Width of the expanded connections tree.
pub const CONNECTIONS_WIDTH: f32 = 224.0;
/// Default width of the schema and object explorer.
pub const EXPLORER_WIDTH: f32 = 218.0;
/// Height of the Results / Explain Plan / Messages sub-tab row.
pub const SUB_TAB_HEIGHT: f32 = 32.0;
/// Height of the pagination row under the grid.
pub const PAGINATION_HEIGHT: f32 = 36.0;
/// Default width of the explain-plan side panel.
pub const PLAN_PANEL_WIDTH: f32 = 320.0;

// ---------------------------------------------------------------------------
// Spacing scale (spec §3, 8pt)
// ---------------------------------------------------------------------------

pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_6: f32 = 24.0;

// ---------------------------------------------------------------------------
// Radius scale (spec §3)
// ---------------------------------------------------------------------------

pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 8.0;

// ---------------------------------------------------------------------------
// Typography sizes (spec §2). JetBrains Mono everywhere; line heights are
// applied per element where the API allows (see PR notes for the editor,
// whose line height is hardcoded by gpui-component).
// ---------------------------------------------------------------------------

pub const FONT_HEADING_XL: f32 = 24.0;
pub const FONT_HEADING_LG: f32 = 20.0;
pub const FONT_HEADING_MD: f32 = 16.0;
pub const FONT_BODY: f32 = 13.0;
pub const FONT_LABEL: f32 = 12.0;
pub const FONT_CAPTION: f32 = 11.0;
pub const FONT_GRID: f32 = 12.0;

pub const LEADING_HEADING_XL: f32 = 32.0;
pub const LEADING_HEADING_MD: f32 = 24.0;
pub const LEADING_BODY: f32 = 20.0;
pub const LEADING_LABEL: f32 = 18.0;
pub const LEADING_CAPTION: f32 = 16.0;
pub const LEADING_GRID: f32 = 20.0;

// ---------------------------------------------------------------------------
// Violet accent scale (spec §1)
// ---------------------------------------------------------------------------

pub const VIOLET_500: u32 = 0x6D4AFF;
pub const VIOLET_400: u32 = 0x8165FF;
pub const VIOLET_300: u32 = 0xA995FF;
pub const VIOLET_200: u32 = 0xD9CFFF;
pub const VIOLET_100: u32 = 0xEDE9FE;
pub const VIOLET_50: u32 = 0xF5F3FF;

/// Derived darker violet for pressed primary buttons (not in the spec).
const VIOLET_600: u32 = 0x5B3EE0;

static UI_FONT: std::sync::OnceLock<gpui_kit::SharedString> = std::sync::OnceLock::new();

/// Record the platform default font family so the theme can use it.
pub fn remember_ui_font(family: gpui_kit::SharedString) {
    let _ = UI_FONT.set(family);
}

pub fn empty_hint(theme: &Theme) -> Hsla {
    theme.background.blend(theme.muted_foreground.opacity(0.66))
}

/// Backdrop behind every modal (connection dialog, prompts, Quick Open).
/// Kept light so the app stays readable behind the dialog: the old 35 to
/// 45% black turned a light theme into one flat grey, which on Windows
/// (custom title bar included) looked like the whole window was broken.
/// Dark themes need a bit more to separate the card from the background.
pub fn modal_scrim(theme: &Theme) -> Hsla {
    let alpha = if theme.mode.is_dark() { 0.32 } else { 0.16 };
    gpui_kit::hsla(0., 0., 0., alpha)
}

fn hex(value: u32) -> Hsla {
    Hsla::from(rgb(value))
}

// ---------------------------------------------------------------------------
// Bundled gpui-kit themes
// ---------------------------------------------------------------------------

/// Display name of the app's own palette in the theme pickers.
pub const DEFAULT_THEME_LABEL: &str = "Kilat (default)";

/// gpui-kit's `themes/` folder, embedded so the release binary needs no
/// files next to it. Each file is a theme set with one or more themes.
const BUNDLED_THEMES: &[&str] = &[
    include_str!("../assets/themes/adventure.json"),
    include_str!("../assets/themes/alduin.json"),
    include_str!("../assets/themes/asciinema.json"),
    include_str!("../assets/themes/aurora.json"),
    include_str!("../assets/themes/ayu.json"),
    include_str!("../assets/themes/catppuccin.json"),
    include_str!("../assets/themes/everforest.json"),
    include_str!("../assets/themes/fahrenheit.json"),
    include_str!("../assets/themes/flexoki.json"),
    include_str!("../assets/themes/gruvbox.json"),
    include_str!("../assets/themes/harper.json"),
    include_str!("../assets/themes/hybrid.json"),
    include_str!("../assets/themes/jellybeans.json"),
    include_str!("../assets/themes/kibble.json"),
    include_str!("../assets/themes/macos-classic.json"),
    include_str!("../assets/themes/mellifluous.json"),
    include_str!("../assets/themes/molokai.json"),
    include_str!("../assets/themes/solarized.json"),
    include_str!("../assets/themes/spaceduck.json"),
    include_str!("../assets/themes/tokyonight.json"),
    include_str!("../assets/themes/twilight.json"),
];

/// Register the bundled themes in gpui-kit's registry. Call once after
/// `gpui_kit::init`. Returns the errors of files that failed to parse.
pub fn register_bundled_themes(cx: &mut App) -> Vec<String> {
    let registry = ThemeRegistry::global_mut(cx);
    BUNDLED_THEMES
        .iter()
        .filter_map(|json| registry.load_themes_from_str(json).err())
        .map(|err| err.to_string())
        .collect()
}

/// Names of the bundled themes for one mode, sorted, for the pickers. The
/// registry's own default light/dark entries are left out: "Kilat" stands in
/// for them.
pub fn theme_names_for(dark: bool, cx: &App) -> Vec<String> {
    let registry = ThemeRegistry::global(cx);
    let defaults = [
        registry.default_light_theme().name.clone(),
        registry.default_dark_theme().name.clone(),
    ];
    let mut names: Vec<String> = registry
        .themes()
        .values()
        .filter(|t| t.mode.is_dark() == dark && !defaults.contains(&t.name))
        .map(|t| t.name.to_string())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// The registry entry for a saved name, if it still exists and matches the
/// mode it was picked for. A stale or mismatched name falls back to Kilat.
fn lookup(name: Option<&str>, dark: bool, cx: &App) -> Option<Rc<ThemeConfig>> {
    let config = ThemeRegistry::global(cx).themes().get(name?).cloned()?;
    (config.mode.is_dark() == dark).then_some(config)
}

pub const ACCENTS: [(&str, u32, u32); 8] = [
    ("Violet", VIOLET_500, VIOLET_300),
    ("Blue", 0x2563EB, 0x93C5FD),
    ("Green", 0x15803D, 0x86EFAC),
    ("Teal", 0x0F766E, 0x5EEAD4),
    ("Yellow", 0xA16207, 0xFDE047),
    ("Orange", 0xC2410C, 0xFDBA74),
    ("Pink", 0xBE185D, 0xF9A8D4),
    ("Red", 0xB91C1C, 0xFCA5A5),
];

#[derive(Clone)]
struct AppearanceState {
    prefs: ora_core::store::AppearancePrefs,
    system_reduce_motion: bool,
}

impl gpui_kit::Global for AppearanceState {}

pub fn init_appearance(cx: &mut App) {
    #[cfg(target_os = "macos")]
    let system_reduce_motion = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .is_ok_and(|result| {
            result.status.success() && String::from_utf8_lossy(&result.stdout).trim() == "1"
        });
    #[cfg(not(target_os = "macos"))]
    let system_reduce_motion = cx.reduce_motion();
    let prefs = ora_core::store::load_appearance();
    cx.set_global(AppearanceState {
        prefs: prefs.clone(),
        system_reduce_motion,
    });
    set_appearance(prefs, cx);
}

pub fn appearance(cx: &App) -> ora_core::store::AppearancePrefs {
    cx.try_global::<AppearanceState>()
        .map(|state| state.prefs.clone())
        .unwrap_or_default()
}

pub fn set_appearance(prefs: ora_core::store::AppearancePrefs, cx: &mut App) {
    let state = cx.global_mut::<AppearanceState>();
    let reduce_motion = prefs.reduce_motion.unwrap_or(state.system_reduce_motion);
    state.prefs = prefs;
    cx.set_reduce_motion(reduce_motion);
}

fn apply_accent(theme: &mut Theme, name: &str) {
    let Some((_, light, dark)) = ACCENTS.iter().skip(1).find(|(label, _, _)| *label == name) else {
        return;
    };
    let primary = hex(*light);
    let bright = hex(*dark);
    let dark = theme.mode.is_dark();
    let highlight = if dark { bright } else { primary };
    theme.primary = highlight;
    theme.primary_hover = highlight.blend(theme.foreground.opacity(0.12));
    theme.primary_active = highlight.blend(theme.foreground.opacity(0.2));
    theme.primary_foreground = if dark { hex(0x09090B) } else { hex(0xFFFFFF) };
    theme.button_primary = theme.primary;
    theme.button_primary_hover = theme.primary_hover;
    theme.button_primary_active = theme.primary_active;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.accent = theme
        .background
        .blend(highlight.opacity(if dark { 0.24 } else { 0.09 }));
    theme.accent_foreground = if dark {
        bright
    } else {
        primary.blend(hex(0x000000).opacity(0.24))
    };
    theme.ring = highlight;
    theme.link = highlight;
    theme.link_hover = theme.primary_hover;
    theme.link_active = theme.primary_active;
    theme.sidebar_accent = theme.accent;
    theme.sidebar_accent_foreground = theme.accent_foreground;
    theme.sidebar_primary = theme.primary;
    theme.sidebar_primary_foreground = theme.primary_foreground;
    theme.selection = highlight.opacity(0.23);
    theme.table_active = theme.accent;
}

pub fn ui_size(theme: &Theme, base: f32) -> gpui_kit::Pixels {
    gpui_kit::px(base + f32::from(theme.font_size) - FONT_BODY)
}

/// Apply one palette. `change` first (registered theme + mode), then a
/// second `update` with the Kilat colors (when no gpui-kit theme is picked
/// for this mode), fonts, and radius — the order the component theme docs
/// require.
pub fn apply_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    let names = theme_names(cx);
    let appearance = appearance(cx);
    let light = lookup(names.light.as_deref(), false, cx);
    let dark = lookup(names.dark.as_deref(), true, cx);
    let use_ora = if mode.is_dark() {
        dark.is_none()
    } else {
        light.is_none()
    };
    let registry = ThemeRegistry::global(cx);
    let light = light.unwrap_or_else(|| registry.default_light_theme().clone());
    let dark = dark.unwrap_or_else(|| registry.default_dark_theme().clone());
    if cx.has_global::<Theme>() {
        // Swap the configs first; `change` below then loads the one for
        // `mode` even when the mode itself did not change.
        Theme::update(cx, |theme| {
            theme.light_theme = light;
            theme.dark_theme = dark;
        });
    }
    Theme::change(mode, window, cx);
    let ui_font = appearance
        .ui_font
        .as_ref()
        .map(|s| gpui_kit::SharedString::from(s.clone()))
        .or_else(|| UI_FONT.get().cloned())
        .unwrap_or_else(|| ".SystemUIFont".into());
    Theme::update(cx, |theme| {
        if use_ora {
            if mode.is_dark() {
                apply_dark(theme);
            } else {
                apply_light(theme);
            }
        }
        apply_accent(theme, &appearance.accent);
        // Line numbers are muted so they do not compete with the code; the
        // active one stays a step brighter. Derived from the theme's muted
        // foreground so it follows every light/dark theme.
        let muted = theme.muted_foreground;
        let style = &mut std::sync::Arc::make_mut(&mut theme.highlight_theme).style;
        style.editor_line_number = Some(muted.opacity(0.5));
        style.editor_active_line_number = Some(muted.opacity(0.9));
        // UI text keeps the platform font (the Root would otherwise apply
        // the theme font to the whole window); mono is for code only.
        theme.font_family = ui_font.clone();
        theme.mono_font_family = MONO_FONT_FAMILY.into();
        theme.font_size = gpui_kit::px(appearance.ui_font_size.into());
        theme.mono_font_size = gpui_kit::px(appearance.sql_font_size.into());
        theme.radius = gpui_kit::px(RADIUS_MD);
        theme.radius_lg = gpui_kit::px(RADIUS_LG);
        // macOS hides overlay scrollbars until a scroll gesture, so wide
        // tables looked like they had nothing to the right. Keep them shown.
        theme.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
    });
    // Belt and suspenders: the Base theme projection is rebuilt by
    // `update`, but pin its typography tokens to JetBrains Mono directly
    // too, so editor-adjacent surfaces never fall back to a system font.
    let base = gpui_kit::base::Theme::global_mut(cx);
    base.tokens.typography.sans = ui_font.clone();
    base.tokens.typography.mono = MONO_FONT_FAMILY.into();
}

fn apply_light(theme: &mut Theme) {
    theme.background = hex(0xFFFFFF);
    theme.foreground = hex(0x09090B);
    theme.popover = hex(0xFFFFFF);
    theme.popover_foreground = hex(0x09090B);
    theme.primary = hex(VIOLET_500);
    theme.primary_hover = hex(VIOLET_400);
    theme.primary_active = hex(VIOLET_600);
    theme.primary_foreground = hex(0xFFFFFF);
    theme.button_primary = hex(VIOLET_500);
    theme.button_primary_hover = hex(VIOLET_400);
    theme.button_primary_active = hex(VIOLET_600);
    theme.button_primary_foreground = hex(0xFFFFFF);
    theme.button = hex(0xFFFFFF);
    theme.button_hover = hex(0xEFEFF2);
    theme.button_active = hex(0xE4E4E7);
    theme.button_foreground = hex(0x09090B);
    theme.secondary = hex(0xEFEFF2);
    theme.secondary_hover = hex(0xE4E4E7);
    theme.secondary_active = hex(0xD4D4D8);
    theme.secondary_foreground = hex(0x09090B);
    theme.button_secondary = hex(0xEFEFF2);
    theme.button_secondary_hover = hex(0xE4E4E7);
    theme.button_secondary_active = hex(0xD4D4D8);
    theme.button_secondary_foreground = hex(0x09090B);
    theme.muted = hex(0xEFEFF2);
    theme.muted_foreground = hex(0x71717A);
    theme.accent = hex(VIOLET_50);
    theme.accent_foreground = hex(0x4C1D95);
    theme.border = hex(0xE4E4E7);
    theme.input = hex(0xE4E4E7);
    theme.ring = hex(VIOLET_500);
    theme.danger = hex(0xEF4444);
    theme.danger_hover = hex(0xDC2626);
    theme.danger_active = hex(0xB91C1C);
    theme.danger_foreground = hex(0xFFFFFF);
    theme.button_danger = hex(0xEF4444);
    theme.button_danger_hover = hex(0xDC2626);
    theme.button_danger_active = hex(0xB91C1C);
    theme.button_danger_foreground = hex(0xFFFFFF);
    theme.success = hex(0x16A34A);
    theme.success_hover = hex(0x15803D);
    theme.success_active = hex(0x166534);
    theme.success_foreground = hex(0xFFFFFF);
    theme.button_success = hex(0x16A34A);
    theme.button_success_hover = hex(0x15803D);
    theme.button_success_active = hex(0x166534);
    theme.button_success_foreground = hex(0xFFFFFF);
    theme.warning = hex(0xF59E0B);
    theme.warning_hover = hex(0xD97706);
    theme.warning_active = hex(0xB45309);
    theme.warning_foreground = hex(0xFFFFFF);
    theme.button_warning = hex(0xF59E0B);
    theme.button_warning_hover = hex(0xD97706);
    theme.button_warning_active = hex(0xB45309);
    theme.button_warning_foreground = hex(0xFFFFFF);
    theme.info = hex(0x3B82F6);
    theme.info_hover = hex(0x2563EB);
    theme.info_active = hex(0x1D4ED8);
    theme.info_foreground = hex(0xFFFFFF);
    theme.button_info = hex(0x3B82F6);
    theme.button_info_hover = hex(0x2563EB);
    theme.button_info_active = hex(0x1D4ED8);
    theme.button_info_foreground = hex(0xFFFFFF);
    theme.link = hex(VIOLET_500);
    theme.link_hover = hex(VIOLET_400);
    theme.link_active = hex(VIOLET_600);
    // A shade apart from the content, so the sidebar needs no border line.
    theme.sidebar = hex(0xEFEFF2);
    theme.sidebar_foreground = hex(0x09090B);
    theme.sidebar_border = hex(0xE4E4E7);
    theme.sidebar_accent = hex(VIOLET_50);
    theme.sidebar_accent_foreground = hex(0x4C1D95);
    theme.sidebar_primary = hex(VIOLET_500);
    theme.sidebar_primary_foreground = hex(0xFFFFFF);
    theme.tab = hex(0xEFEFF2);
    theme.tab_active = hex(0xFFFFFF);
    theme.tab_active_foreground = hex(0x09090B);
    theme.tab_foreground = hex(0x71717A);
    theme.tab_bar = hex(0xFFFFFF);
    theme.tab_bar_segmented = hex(0xEFEFF2);
    theme.table = hex(0xFFFFFF);
    theme.table_head = hex(0xEFEFF2);
    theme.table_head_foreground = hex(0x09090B);
    theme.table_even = hex(0xFAFAFA);
    theme.table_hover = hex(VIOLET_50);
    theme.table_active = hex(VIOLET_100);
    theme.table_active_border = hex(VIOLET_500);
    // Zebra only: no lines between rows.
    theme.table_row_border = gpui_kit::hsla(0., 0., 0., 0.);
    theme.table_foot = hex(0xEFEFF2);
    theme.table_foot_foreground = hex(0x09090B);
    theme.list_hover = hex(VIOLET_50);
    theme.list_active = hex(VIOLET_100);
    theme.list_active_border = hex(VIOLET_500);
    theme.list_even = hex(0xFAFAFA);
    theme.list_head = hex(0xEFEFF2);
    theme.selection = hex(VIOLET_100);
    theme.caret = hex(VIOLET_500);
    theme.scrollbar = hex(0xE4E4E7);
    theme.scrollbar_thumb = hex(0xD4D4D8);
    theme.scrollbar_thumb_hover = hex(0xA1A1AA);
    theme.title_bar = hex(0xFFFFFF);
    theme.title_bar_border = hex(0xE4E4E7);
    theme.status_bar = hex(0xFFFFFF);
    theme.status_bar_border = hex(0xE4E4E7);
    theme.window_border = hex(0xE4E4E7);
    theme.skeleton = hex(0xEFEFF2);
    theme.drag_border = hex(VIOLET_500);
    theme.drop_target = hex(VIOLET_500);
    theme.group_box = hex(0xFFFFFF);
    theme.group_box_foreground = hex(0x09090B);
    theme.progress_bar = hex(VIOLET_500);
}

fn apply_dark(theme: &mut Theme) {
    let workspace = hex(0x1B1B1B);
    let stripe = hex(0x202020);
    let panel = hex(0x242424);
    let control = hex(0x2D2D2D);
    let hover = hex(0x353535);
    let pressed = hex(0x424242);
    let divider = hex(0x383838);
    let text = hex(0xE9E9E9);
    let subtle_text = hex(0xB2B2B2);

    let editor = &mut std::sync::Arc::make_mut(&mut theme.highlight_theme).style;
    editor.editor_background = Some(hex(0x1E1E1E));
    // A continuous editing surface keeps the first glyph clear of a contrasting gutter edge.
    editor.editor_gutter_background = editor.editor_background;
    editor.editor_foreground = Some(text);
    editor.editor_active_line = Some(hex(0x252525));
    editor.editor_line_number = Some(subtle_text);
    editor.editor_active_line_number = Some(text);

    theme.background = workspace;
    theme.foreground = text;
    theme.popover = control;
    theme.popover_foreground = text;
    theme.primary = hex(VIOLET_400);
    theme.primary_hover = hex(VIOLET_300);
    theme.primary_active = hex(VIOLET_500);
    theme.primary_foreground = hex(0xFFFFFF);
    theme.button_primary = hex(VIOLET_400);
    theme.button_primary_hover = hex(VIOLET_300);
    theme.button_primary_active = hex(VIOLET_500);
    theme.button_primary_foreground = hex(0xFFFFFF);
    theme.button = control;
    theme.button_hover = hover;
    theme.button_active = pressed;
    theme.button_foreground = text;
    theme.secondary = control;
    theme.secondary_hover = hover;
    theme.secondary_active = pressed;
    theme.secondary_foreground = text;
    theme.button_secondary = control;
    theme.button_secondary_hover = hover;
    theme.button_secondary_active = pressed;
    theme.button_secondary_foreground = text;
    theme.muted = control;
    theme.muted_foreground = subtle_text;
    theme.accent = hex(0x30264F);
    theme.accent_foreground = hex(0xE9DDFF);
    theme.border = divider;
    theme.input = divider;
    theme.ring = hex(VIOLET_300);
    theme.danger = hex(0xF87171);
    theme.danger_hover = hex(0xEF4444);
    theme.danger_active = hex(0xDC2626);
    theme.danger_foreground = hex(0xFFFFFF);
    theme.button_danger = hex(0xF87171);
    theme.button_danger_hover = hex(0xEF4444);
    theme.button_danger_active = hex(0xDC2626);
    theme.button_danger_foreground = hex(0xFFFFFF);
    theme.success = hex(0x22C55E);
    theme.success_hover = hex(0x16A34A);
    theme.success_active = hex(0x15803D);
    theme.success_foreground = hex(0xFFFFFF);
    theme.button_success = hex(0x22C55E);
    theme.button_success_hover = hex(0x16A34A);
    theme.button_success_active = hex(0x15803D);
    theme.button_success_foreground = hex(0xFFFFFF);
    theme.warning = hex(0xFBBF24);
    theme.warning_hover = hex(0xF59E0B);
    theme.warning_active = hex(0xD97706);
    theme.warning_foreground = hex(0x09090B);
    theme.button_warning = hex(0xFBBF24);
    theme.button_warning_hover = hex(0xF59E0B);
    theme.button_warning_active = hex(0xD97706);
    theme.button_warning_foreground = hex(0x09090B);
    theme.info = hex(0x60A5FA);
    theme.info_hover = hex(0x3B82F6);
    theme.info_active = hex(0x2563EB);
    theme.info_foreground = hex(0xFFFFFF);
    theme.button_info = hex(0x60A5FA);
    theme.button_info_hover = hex(0x3B82F6);
    theme.button_info_active = hex(0x2563EB);
    theme.button_info_foreground = hex(0xFFFFFF);
    theme.link = hex(VIOLET_300);
    theme.link_hover = hex(VIOLET_200);
    theme.link_active = hex(VIOLET_400);
    theme.sidebar = panel;
    theme.sidebar_foreground = text;
    theme.sidebar_border = divider;
    theme.sidebar_accent = hex(0x30264F);
    theme.sidebar_accent_foreground = hex(0xE9DDFF);
    theme.sidebar_primary = hex(VIOLET_400);
    theme.sidebar_primary_foreground = hex(0xFFFFFF);
    theme.tab = panel;
    theme.tab_active = control;
    theme.tab_active_foreground = text;
    theme.tab_foreground = subtle_text;
    theme.tab_bar = workspace;
    theme.tab_bar_segmented = control;
    theme.table = workspace;
    theme.table_head = panel;
    theme.table_head_foreground = text;
    theme.table_even = stripe;
    theme.table_hover = hover;
    theme.table_active = theme.accent;
    theme.table_active_border = hex(VIOLET_300);
    theme.table_row_border = gpui_kit::hsla(0., 0., 0., 0.);
    theme.table_foot = panel;
    theme.table_foot_foreground = text;
    theme.list_hover = hover;
    theme.list_active = theme.accent;
    theme.list_active_border = hex(VIOLET_300);
    theme.list_even = stripe;
    theme.list_head = panel;
    theme.selection = theme.accent;
    theme.caret = hex(VIOLET_300);
    theme.scrollbar = panel;
    theme.scrollbar_thumb = pressed;
    theme.scrollbar_thumb_hover = hex(0x565656);
    theme.title_bar = control;
    theme.title_bar_border = divider;
    theme.status_bar = panel;
    theme.status_bar_border = divider;
    theme.window_border = divider;
    theme.skeleton = control;
    theme.drag_border = hex(VIOLET_300);
    theme.drop_target = hex(VIOLET_300);
    theme.group_box = panel;
    theme.group_box_foreground = text;
    theme.progress_bar = hex(VIOLET_400);
}

// ---------------------------------------------------------------------------
// Preference state + OS appearance
// ---------------------------------------------------------------------------

/// The persisted Light/Dark/System preference, held as an app global so the
/// window-appearance observer (which only sees `&mut Window` + `&mut App`)
/// can re-resolve System mode when the OS appearance changes.
#[derive(Default)]
pub struct ThemePreferenceState(pub ThemePreference);

impl Global for ThemePreferenceState {}

/// The picked light/dark theme names, as a global for the same reason.
#[derive(Default)]
struct ThemeNamesState(ThemeNames);

impl Global for ThemeNamesState {}

pub fn theme_names(cx: &App) -> ThemeNames {
    cx.try_global::<ThemeNamesState>()
        .map(|state| state.0.clone())
        .unwrap_or_default()
}

pub fn set_theme_names(names: ThemeNames, cx: &mut App) {
    cx.set_global(ThemeNamesState(names));
}

pub fn preference(cx: &App) -> ThemePreference {
    cx.try_global::<ThemePreferenceState>()
        .map(|state| state.0)
        .unwrap_or_default()
}

pub fn set_preference(pref: ThemePreference, cx: &mut App) {
    cx.set_global(ThemePreferenceState(pref));
}

/// Resolve a preference to a concrete mode for one window. System follows
/// the OS appearance reported by GPUI for that window.
pub fn resolve(pref: ThemePreference, window: &Window) -> ThemeMode {
    match pref {
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::Dark => ThemeMode::Dark,
        ThemePreference::System => ThemeMode::from(window.appearance()),
    }
}

/// Apply a preference (resolving System against this window) and remember
/// it in the global. Callers persist the preference to disk separately.
pub fn apply_preference(pref: ThemePreference, window: &mut Window, cx: &mut App) {
    let mode = resolve(pref, window);
    set_preference(pref, cx);
    apply_theme(mode, Some(window), cx);
}

/// Re-apply the current preference when the OS appearance changes. Only
/// System mode reacts; Light/Dark stay pinned.
pub fn sync_system_appearance(window: &mut Window, cx: &mut App) {
    if preference(cx) == ThemePreference::System {
        apply_theme(resolve(ThemePreference::System, window), Some(window), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::BUNDLED_THEMES;
    use gpui_kit::component::ThemeSet;

    /// Every bundled gpui-kit theme file parses with the gpui-kit version we
    /// build against, so none silently drops out of the pickers.
    #[test]
    fn bundled_themes_parse() {
        let mut count = 0;
        for json in BUNDLED_THEMES {
            let set: ThemeSet = serde_json::from_str(json).expect("theme file parses");
            assert!(!set.themes.is_empty());
            count += set.themes.len();
        }
        assert!(count >= 30, "only {count} themes");
    }
}
