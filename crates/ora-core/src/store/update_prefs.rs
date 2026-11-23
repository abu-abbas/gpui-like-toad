//! Update-check preferences.

use super::*;
use crate::credentials::{CredentialStore, ExposeSecret as _, SecretString};
use crate::redact::{join_proxy_password, proxy_has_user, split_proxy_password};

/// Keychain account (service `kilat`) for the proxy password. Not
/// a profile id: those are `p<hex>-...`.
pub const PROXY_CREDENTIAL_ACCOUNT: &str = "network-proxy";

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
    /// Never holds a password: that lives in the OS keychain under
    /// [`PROXY_CREDENTIAL_ACCOUNT`].
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
            // A password left in an old file is not exposed to the rest of
            // the app; [`resolve_proxy_in`] moves it to the keychain.
            .map(|p| split_proxy_password(p).0),
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
        // Whatever reaches this point, a password never goes to disk.
        Some(v) => pairs.insert("network_proxy".into(), Json::Str(split_proxy_password(v).0)),
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

/// The proxy to use and what happened while finding it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedProxy {
    /// The proxy URL including its password, `None` = use the environment.
    pub url: Option<String>,
    /// Something the user should know, already free of secrets.
    pub warning: Option<String>,
}

/// The proxy URL stored in `settings.json`, exactly as written.
fn stored_proxy_in(dir: &Path) -> Option<String> {
    read_json_file(&dir.join("settings.json"))?
        .get("network_proxy")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// Find the proxy to use at startup. A plaintext password in
/// `settings.json` (older versions) is first saved to the keychain; only
/// after that succeeds is it removed from the file, so a keychain failure
/// loses nothing (the password then stays in the file and the proxy still
/// works). A URL with a user but no password gets its password from the
/// keychain. A URL without a user never touches the keychain.
pub fn resolve_proxy_in(dir: &Path, credentials: &dyn CredentialStore) -> ResolvedProxy {
    let Some(stored) = stored_proxy_in(dir) else {
        return ResolvedProxy::default();
    };
    let (clean, legacy_password) = split_proxy_password(&stored);
    if let Some(password) = legacy_password {
        let migrated = credentials
            .set(
                PROXY_CREDENTIAL_ACCOUNT,
                &SecretString::from(password.clone()),
            )
            .map_err(|e| e.to_string())
            .and_then(|()| {
                let mut prefs = load_update_prefs_in(dir);
                prefs.proxy = Some(clean.clone());
                save_update_prefs_in(dir, &prefs).map_err(|e| e.to_string())
            });
        return ResolvedProxy {
            url: Some(join_proxy_password(&clean, &password)),
            warning: migrated.err().map(|e| {
                format!(
                    "Could not move the proxy password to the system keychain ({}). \
                     It is still stored in settings.json.",
                    crate::redact::scrub_secret(&e, &password)
                )
            }),
        };
    }
    if !proxy_has_user(&clean) {
        return ResolvedProxy {
            url: Some(clean),
            warning: None,
        };
    }
    match credentials.get(PROXY_CREDENTIAL_ACCOUNT) {
        Ok(Some(pw)) => ResolvedProxy {
            url: Some(join_proxy_password(&clean, pw.expose_secret())),
            warning: None,
        },
        Ok(None) => ResolvedProxy {
            url: Some(clean),
            warning: None,
        },
        Err(err) => ResolvedProxy {
            url: Some(clean),
            warning: Some(format!("Could not read the proxy password: {err}")),
        },
    }
}

/// Keep the keychain in step with a proxy URL just typed in Settings:
/// store `password`, or (when the new URL has none but the old one named a
/// user) remove the old password, which belonged to the old URL. Returns a
/// warning, free of secrets, when the keychain refused.
pub fn sync_proxy_password(
    credentials: &dyn CredentialStore,
    old_had_user: bool,
    password: Option<&str>,
) -> Option<String> {
    match password {
        Some(pw) => credentials
            .set(
                PROXY_CREDENTIAL_ACCOUNT,
                &SecretString::from(pw.to_string()),
            )
            .err()
            .map(|err| {
                format!(
                    "Could not save the proxy password to the system keychain ({}). \
                     It is used until you quit; enter it again next time.",
                    crate::redact::scrub_secret(&err.to_string(), pw)
                )
            }),
        None if old_had_user => credentials
            .delete(PROXY_CREDENTIAL_ACCOUNT)
            .err()
            .map(|err| format!("Could not remove the old proxy password: {err}")),
        None => None,
    }
}

/// Save a proxy URL typed in Settings: the URL without its password goes
/// to `settings.json`, the password to the keychain. Returns the URL to
/// use now and the keychain warning, if any.
pub fn save_proxy_in(
    dir: &Path,
    credentials: &dyn CredentialStore,
    typed: Option<&str>,
) -> std::io::Result<ResolvedProxy> {
    let mut prefs = load_update_prefs_in(dir);
    let had_user = prefs.proxy.as_deref().is_some_and(proxy_has_user);
    let typed = typed.map(str::trim).filter(|p| !p.is_empty());
    let (clean, password) = match typed {
        Some(t) => {
            let (clean, pw) = split_proxy_password(t);
            (Some(clean), pw)
        }
        None => (None, None),
    };
    let warning = sync_proxy_password(credentials, had_user, password.as_deref());
    prefs.proxy = clean.clone();
    save_update_prefs_in(dir, &prefs)?;
    Ok(ResolvedProxy {
        url: match (clean, password) {
            (Some(c), Some(p)) => Some(join_proxy_password(&c, &p)),
            (c, _) => c,
        },
        warning,
    })
}
