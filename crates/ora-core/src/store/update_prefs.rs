//! Update-check preferences.

use super::*;

/// Settings for the GitHub Releases update check, kept in `settings.json`
/// next to the theme. Nothing here identifies the user or the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePrefs {
    /// Check automatically at startup (at most once per interval).
    pub auto_check: bool,
    /// Unix seconds of the last request to GitHub (0 = never).
    pub last_checked: u64,
    /// Version the user dismissed the banner for; not announced again.
    pub dismissed_version: Option<String>,
    /// Most recent app version launched on this machine.
    pub last_launched_version: Option<String>,
    /// HTTP(S) proxy URL for update checks and downloads, e.g.
    /// `http://proxy.corp:8080`. `None` = use the proxy environment variables.
    pub proxy: Option<String>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_checked: 0,
            dismissed_version: None,
            last_launched_version: None,
            proxy: None,
        }
    }
}

pub fn load_update_prefs_in(dir: &Path) -> UpdatePrefs {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return UpdatePrefs::default();
    };
    UpdatePrefs {
        auto_check: v
            .get("update_auto_check")
            .and_then(Json::as_bool)
            .unwrap_or(true),
        last_checked: v
            .get("update_last_checked")
            .and_then(Json::as_u64)
            .unwrap_or(0),
        dismissed_version: v
            .get("update_dismissed_version")
            .and_then(Json::as_str)
            .map(str::to_string),
        last_launched_version: v
            .get("update_last_launched_version")
            .and_then(Json::as_str)
            .map(str::to_string),
        proxy: v
            .get("network_proxy")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
    }
}

pub fn save_update_prefs_in(dir: &Path, prefs: &UpdatePrefs) -> std::io::Result<()> {
    // Preserve any other settings keys already in the file.
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert("update_auto_check".into(), Json::Bool(prefs.auto_check));
    pairs.insert(
        "update_last_checked".into(),
        Json::Num(prefs.last_checked as f64),
    );
    match &prefs.dismissed_version {
        Some(v) => pairs.insert("update_dismissed_version".into(), Json::Str(v.clone())),
        None => pairs.remove("update_dismissed_version"),
    };
    match &prefs.last_launched_version {
        Some(v) => pairs.insert("update_last_launched_version".into(), Json::Str(v.clone())),
        None => pairs.remove("update_last_launched_version"),
    };
    match prefs
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(v) => pairs.insert("network_proxy".into(), Json::Str(v.to_string())),
        None => pairs.remove("network_proxy"),
    };
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_update_prefs() -> UpdatePrefs {
    load_update_prefs_in(&config_dir())
}

pub fn save_update_prefs(prefs: &UpdatePrefs) -> std::io::Result<()> {
    save_update_prefs_in(&config_dir(), prefs)
}
