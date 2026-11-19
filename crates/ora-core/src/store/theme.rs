//! Theme preference, theme names and appearance.

use super::*;

/// Theme preference from the Settings panel. `System` follows the OS
/// appearance; `Light`/`Dark` pin one palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "light" => ThemePreference::Light,
            "dark" => ThemePreference::Dark,
            _ => ThemePreference::System,
        }
    }
}

// ---------------------------------------------------------------------------
// Theme preference
// ---------------------------------------------------------------------------

pub fn load_theme_preference_in(dir: &Path) -> ThemePreference {
    match read_json_file(&dir.join("settings.json")) {
        Some(v) => v
            .get("theme")
            .and_then(Json::as_str)
            .map(ThemePreference::from_name)
            .unwrap_or_default(),
        None => ThemePreference::default(),
    }
}

pub fn save_theme_preference_in(dir: &Path, pref: ThemePreference) -> std::io::Result<()> {
    // Preserve any other settings keys already in the file.
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert("theme".to_string(), Json::Str(pref.as_str().to_string()));
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

/// Names of the color themes picked for light and dark mode. `None` means
/// the app's own palette ("Kilat"). Names match gpui-kit `ThemeRegistry`
/// entries, e.g. "Ayu Light" or "Tokyo Night".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeNames {
    pub light: Option<String>,
    pub dark: Option<String>,
}

pub fn load_theme_names_in(dir: &Path) -> ThemeNames {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return ThemeNames::default();
    };
    let name = |key: &str| {
        v.get(key)
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    ThemeNames {
        light: name("light_theme"),
        dark: name("dark_theme"),
    }
}

pub fn save_theme_names_in(dir: &Path, names: &ThemeNames) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    for (key, value) in [("light_theme", &names.light), ("dark_theme", &names.dark)] {
        match value {
            Some(name) => {
                pairs.insert(key.to_string(), Json::Str(name.clone()));
            }
            None => {
                pairs.remove(key);
            }
        }
    }
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_theme_names() -> ThemeNames {
    load_theme_names_in(&config_dir())
}

pub fn save_theme_names(names: &ThemeNames) -> std::io::Result<()> {
    save_theme_names_in(&config_dir(), names)
}

pub fn load_theme_preference() -> ThemePreference {
    load_theme_preference_in(&config_dir())
}

pub fn save_theme_preference(pref: ThemePreference) -> std::io::Result<()> {
    save_theme_preference_in(&config_dir(), pref)
}

// ---------------------------------------------------------------------------
// Update-check preferences
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AppIcon {
    #[default]
    Folded,
    Bolt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearancePrefs {
    pub app_icon: AppIcon,
    pub accent: String,
    pub ui_font: Option<String>,
    pub ui_font_size: u8,
    pub sql_font_size: u8,
    /// None follows the platform default.
    pub reduce_motion: Option<bool>,
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            app_icon: AppIcon::default(),
            accent: "Violet".into(),
            ui_font: None,
            ui_font_size: 13,
            sql_font_size: 13,
            reduce_motion: None,
        }
    }
}

pub(super) fn read_appearance(dir: &Path) -> AppearancePrefs {
    let default = AppearancePrefs::default();
    let Some(settings) = read_json_file(&dir.join("settings.json")) else {
        return default;
    };
    let size = |key, default| {
        settings
            .get(key)
            .and_then(Json::as_u64)
            .filter(|size| (10..=20).contains(size))
            .map_or(default, |size| size as u8)
    };
    AppearancePrefs {
        app_icon: match settings.get("app_icon").and_then(Json::as_str) {
            Some("bolt") => AppIcon::Bolt,
            _ => AppIcon::Folded,
        },
        accent: settings
            .get("accent")
            .and_then(Json::as_str)
            .map_or(default.accent, str::to_owned),
        ui_font: settings
            .get("ui_font")
            .and_then(Json::as_str)
            .map(str::to_owned),
        ui_font_size: size("ui_font_size", default.ui_font_size),
        sql_font_size: size("sql_font_size", default.sql_font_size),
        reduce_motion: settings.get("reduce_motion").and_then(Json::as_bool),
    }
}

pub fn load_appearance() -> AppearancePrefs {
    read_appearance(&config_dir())
}

pub(super) fn write_appearance(dir: &Path, prefs: &AppearancePrefs) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    if let Some(font) = &prefs.ui_font {
        pairs.insert("ui_font".into(), Json::Str(font.clone()));
    } else {
        pairs.remove("ui_font");
    }
    let app_icon = match prefs.app_icon {
        AppIcon::Folded => "folded",
        AppIcon::Bolt => "bolt",
    };
    pairs.insert("app_icon".into(), Json::Str(app_icon.into()));
    pairs.insert("accent".into(), Json::Str(prefs.accent.clone()));
    pairs.insert("ui_font_size".into(), Json::Num(prefs.ui_font_size.into()));
    pairs.insert(
        "sql_font_size".into(),
        Json::Num(prefs.sql_font_size.into()),
    );
    if let Some(reduce_motion) = prefs.reduce_motion {
        pairs.insert("reduce_motion".into(), Json::Bool(reduce_motion));
    } else {
        pairs.remove("reduce_motion");
    }
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn save_appearance(prefs: &AppearancePrefs) -> std::io::Result<()> {
    write_appearance(&config_dir(), prefs)
}
