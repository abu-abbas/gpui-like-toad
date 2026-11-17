//! DB Explorer design-system theme: violet accent scale, light/dark
//! palettes, density tokens, and the Light/Dark/System preference.
//!
//! All color values come verbatim from the v0.1 design-system spec
//! (§1). Hover/active shades not listed in the spec are derived from the
//! violet scale and noted as such. Density numbers (§3) are the single
//! source of truth for component heights, spacing, radius, and font sizes
//! used by the workbench layout.
//!
//! The palette is applied through gpui-component's [`Theme`]: first
//! [`Theme::change`] loads the built-in light/dark base, then a second
//! [`Theme::update`] overrides the colors (the order the component docs
//! require, so the Base projection and open windows stay in step).
//!
//! Besides that palette ("Ora", the default) the user can pick one of the
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
/// Data grid row height (also the DataTable `Small` row height).
pub const GRID_ROW_HEIGHT: f32 = 30.0;
/// Status bar height.
pub const STATUS_BAR_HEIGHT: f32 = 26.0;
/// Primary icon rail width. Wider than the 64px token so the longest
/// rail label fits on one line: "Connections" is 11 JetBrains Mono chars
/// at 0.6em advance, i.e. 66px at [`FONT_RAIL`], inside a 70px button
/// (`RAIL_WIDTH - 6`).
pub const RAIL_WIDTH: f32 = 76.0;
/// Height of one rail button (icon + label).
pub const RAIL_BUTTON_HEIGHT: f32 = 52.0;
/// Height of the Results / Explain Plan / Messages sub-tab row.
pub const SUB_TAB_HEIGHT: f32 = 32.0;
/// Height of the pagination row under the grid.
pub const PAGINATION_HEIGHT: f32 = 36.0;
/// Default width of the connections/schema side panel.
pub const SIDE_PANEL_WIDTH: f32 = 300.0;
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
pub const SPACE_8: f32 = 32.0;

// ---------------------------------------------------------------------------
// Radius scale (spec §3)
// ---------------------------------------------------------------------------

pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 8.0;
pub const RADIUS_XL: f32 = 12.0;

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
pub const FONT_EDITOR: f32 = 13.0;
pub const FONT_GRID: f32 = 12.0;
/// Icon rail label size: 10px keeps "Connections" (11 chars x 0.6em =
/// 66px) on one line inside the 70px rail button.
pub const FONT_RAIL: f32 = 10.0;

pub const LEADING_HEADING_XL: f32 = 32.0;
pub const LEADING_HEADING_LG: f32 = 28.0;
pub const LEADING_HEADING_MD: f32 = 24.0;
pub const LEADING_BODY: f32 = 20.0;
pub const LEADING_LABEL: f32 = 18.0;
pub const LEADING_CAPTION: f32 = 16.0;
/// Icon rail label line height: icon 20 + gap 4 + label 14 = 38px, inside
/// the 52px rail button.
pub const LEADING_RAIL: f32 = 14.0;
pub const LEADING_EDITOR: f32 = 22.0;
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

fn hex(value: u32) -> Hsla {
    Hsla::from(rgb(value))
}

// ---------------------------------------------------------------------------
// Bundled gpui-kit themes
// ---------------------------------------------------------------------------

/// Display name of the app's own palette in the theme pickers.
pub const ORA_THEME_LABEL: &str = "Ora (default)";

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
/// registry's own default light/dark entries are left out: "Ora" stands in
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
/// mode it was picked for. A stale or mismatched name falls back to Ora.
fn lookup(name: Option<&str>, dark: bool, cx: &App) -> Option<Rc<ThemeConfig>> {
    let config = ThemeRegistry::global(cx).themes().get(name?).cloned()?;
    (config.mode.is_dark() == dark).then_some(config)
}

/// Apply one palette. `change` first (registered theme + mode), then a
/// second `update` with the Ora colors (when no gpui-kit theme is picked
/// for this mode), fonts, and radius — the order the component theme docs
/// require.
pub fn apply_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    let names = theme_names(cx);
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
    Theme::update(cx, |theme| {
        if use_ora {
            if mode.is_dark() {
                apply_dark(theme);
            } else {
                apply_light(theme);
            }
        }
        // JetBrains Mono for ALL UI text: both the sans and mono tokens.
        theme.font_family = MONO_FONT_FAMILY.into();
        theme.mono_font_family = MONO_FONT_FAMILY.into();
        theme.font_size = gpui_kit::px(FONT_BODY);
        theme.mono_font_size = gpui_kit::px(FONT_EDITOR);
        theme.radius = gpui_kit::px(RADIUS_MD);
        theme.radius_lg = gpui_kit::px(RADIUS_LG);
    });
    // Belt and suspenders: the Base theme projection is rebuilt by
    // `update`, but pin its typography tokens to JetBrains Mono directly
    // too, so editor-adjacent surfaces never fall back to a system font.
    let base = gpui_kit::base::Theme::global_mut(cx);
    base.tokens.typography.sans = MONO_FONT_FAMILY.into();
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
    theme.button_hover = hex(0xF4F4F5);
    theme.button_active = hex(0xE4E4E7);
    theme.button_foreground = hex(0x09090B);
    theme.secondary = hex(0xF4F4F5);
    theme.secondary_hover = hex(0xE4E4E7);
    theme.secondary_active = hex(0xD4D4D8);
    theme.secondary_foreground = hex(0x09090B);
    theme.button_secondary = hex(0xF4F4F5);
    theme.button_secondary_hover = hex(0xE4E4E7);
    theme.button_secondary_active = hex(0xD4D4D8);
    theme.button_secondary_foreground = hex(0x09090B);
    theme.muted = hex(0xF4F4F5);
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
    theme.sidebar = hex(0xFFFFFF);
    theme.sidebar_foreground = hex(0x09090B);
    theme.sidebar_border = hex(0xE4E4E7);
    theme.sidebar_accent = hex(VIOLET_50);
    theme.sidebar_accent_foreground = hex(0x4C1D95);
    theme.sidebar_primary = hex(VIOLET_500);
    theme.sidebar_primary_foreground = hex(0xFFFFFF);
    theme.tab = hex(0xF4F4F5);
    theme.tab_active = hex(0xFFFFFF);
    theme.tab_active_foreground = hex(0x09090B);
    theme.tab_foreground = hex(0x71717A);
    theme.tab_bar = hex(0xFFFFFF);
    theme.tab_bar_segmented = hex(0xF4F4F5);
    theme.table = hex(0xFFFFFF);
    theme.table_head = hex(0xF4F4F5);
    theme.table_head_foreground = hex(0x09090B);
    theme.table_even = hex(0xFAFAFA);
    theme.table_hover = hex(VIOLET_50);
    theme.table_active = hex(VIOLET_100);
    theme.table_active_border = hex(VIOLET_500);
    // Zebra only: no lines between rows.
    theme.table_row_border = gpui_kit::hsla(0., 0., 0., 0.);
    theme.table_foot = hex(0xF4F4F5);
    theme.table_foot_foreground = hex(0x09090B);
    theme.list_hover = hex(VIOLET_50);
    theme.list_active = hex(VIOLET_100);
    theme.list_active_border = hex(VIOLET_500);
    theme.list_even = hex(0xFAFAFA);
    theme.list_head = hex(0xF4F4F5);
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
    theme.skeleton = hex(0xF4F4F5);
    theme.drag_border = hex(VIOLET_500);
    theme.drop_target = hex(VIOLET_500);
    theme.group_box = hex(0xFFFFFF);
    theme.group_box_foreground = hex(0x09090B);
    theme.progress_bar = hex(VIOLET_500);
}

fn apply_dark(theme: &mut Theme) {
    theme.background = hex(0x09090B);
    theme.foreground = hex(0xFAFAFA);
    theme.popover = hex(0x18181B);
    theme.popover_foreground = hex(0xFAFAFA);
    theme.primary = hex(VIOLET_400);
    theme.primary_hover = hex(VIOLET_300);
    theme.primary_active = hex(VIOLET_500);
    theme.primary_foreground = hex(0xFFFFFF);
    theme.button_primary = hex(VIOLET_400);
    theme.button_primary_hover = hex(VIOLET_300);
    theme.button_primary_active = hex(VIOLET_500);
    theme.button_primary_foreground = hex(0xFFFFFF);
    theme.button = hex(0x18181B);
    theme.button_hover = hex(0x27272A);
    theme.button_active = hex(0x3F3F46);
    theme.button_foreground = hex(0xFAFAFA);
    theme.secondary = hex(0x27272A);
    theme.secondary_hover = hex(0x3F3F46);
    theme.secondary_active = hex(0x52525B);
    theme.secondary_foreground = hex(0xFAFAFA);
    theme.button_secondary = hex(0x27272A);
    theme.button_secondary_hover = hex(0x3F3F46);
    theme.button_secondary_active = hex(0x52525B);
    theme.button_secondary_foreground = hex(0xFAFAFA);
    theme.muted = hex(0x27272A);
    theme.muted_foreground = hex(0xA1A1AA);
    theme.accent = hex(0x30264F);
    theme.accent_foreground = hex(0xE9DDFF);
    theme.border = hex(0x3F3F46);
    theme.input = hex(0x3F3F46);
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
    theme.sidebar = hex(0x09090B);
    theme.sidebar_foreground = hex(0xFAFAFA);
    theme.sidebar_border = hex(0x3F3F46);
    theme.sidebar_accent = hex(0x30264F);
    theme.sidebar_accent_foreground = hex(0xE9DDFF);
    theme.sidebar_primary = hex(VIOLET_400);
    theme.sidebar_primary_foreground = hex(0xFFFFFF);
    theme.tab = hex(0x27272A);
    theme.tab_active = hex(0x18181B);
    theme.tab_active_foreground = hex(0xFAFAFA);
    theme.tab_foreground = hex(0xA1A1AA);
    theme.tab_bar = hex(0x09090B);
    theme.tab_bar_segmented = hex(0x27272A);
    theme.table = hex(0x09090B);
    theme.table_head = hex(0x27272A);
    theme.table_head_foreground = hex(0xFAFAFA);
    theme.table_even = hex(0x18181B);
    theme.table_hover = hex(0x30264F);
    theme.table_active = hex(0x30264F);
    theme.table_active_border = hex(VIOLET_300);
    theme.table_row_border = gpui_kit::hsla(0., 0., 0., 0.);
    theme.table_foot = hex(0x27272A);
    theme.table_foot_foreground = hex(0xFAFAFA);
    theme.list_hover = hex(0x30264F);
    theme.list_active = hex(0x30264F);
    theme.list_active_border = hex(VIOLET_300);
    theme.list_even = hex(0x18181B);
    theme.list_head = hex(0x27272A);
    theme.selection = hex(0x30264F);
    theme.caret = hex(VIOLET_300);
    theme.scrollbar = hex(0x27272A);
    theme.scrollbar_thumb = hex(0x3F3F46);
    theme.scrollbar_thumb_hover = hex(0x52525B);
    theme.title_bar = hex(0x09090B);
    theme.title_bar_border = hex(0x3F3F46);
    theme.status_bar = hex(0x09090B);
    theme.status_bar_border = hex(0x3F3F46);
    theme.window_border = hex(0x3F3F46);
    theme.skeleton = hex(0x27272A);
    theme.drag_border = hex(VIOLET_300);
    theme.drop_target = hex(VIOLET_300);
    theme.group_box = hex(0x18181B);
    theme.group_box_foreground = hex(0xFAFAFA);
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
