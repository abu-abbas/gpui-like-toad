//! Connection export/import: the app's own JSON format and Toad login files.

use super::profiles::{profile_from_json, profile_to_json};
use super::*;

/// A portable list of connection settings. Keychain IDs and the saved
/// password flag stay machine-local. The original format identifier keeps
/// these exports compatible with older app versions.
pub fn export_connection_profiles(profiles: &[SavedProfile]) -> String {
    let items = profiles
        .iter()
        .map(profile_to_json)
        .map(|json| {
            let Json::Obj(mut fields) = json else {
                unreachable!("profile is an object");
            };
            fields.retain(|(key, _)| !matches!(key.as_str(), "id" | "save_password"));
            Json::Obj(fields)
        })
        .collect();
    to_json(&Json::Obj(vec![
        ("format".into(), Json::Str("ora-connections-v1".into())),
        ("connections".into(), Json::Arr(items)),
    ]))
}

/// Read the portable format or its older bare connection list. A
/// transferred profile never points to another machine's keychain and
/// must explicitly be opted into SYSDBA after import.
pub fn import_connection_profiles(text: &str) -> Result<Vec<SavedProfile>, String> {
    if looks_like_toad_logins(text) {
        return import_toad_logins(text);
    }
    let value = parse_json(text)?;
    let items = match &value {
        Json::Arr(items) => items.as_slice(),
        _ if value.get("format").and_then(Json::as_str) == Some("ora-connections-v1") => value
            .get("connections")
            .and_then(Json::as_arr)
            .ok_or("Connections must be a JSON array")?,
        _ => {
            return Err(
                "Unsupported connection file. Expected a Kilat connections JSON file.".into(),
            )
        }
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut profile = profile_from_json(item)
                .filter(|p| {
                    !p.name.trim().is_empty()
                        && !p.username.trim().is_empty()
                        && !p.connect_string.trim().is_empty()
                })
                .ok_or_else(|| {
                    format!(
                        "Connection {} needs a name, username, and connect string",
                        index + 1
                    )
                })?;
            profile.id = new_profile_id();
            profile.save_password = false;
            profile.access.sysdba = false;
            Ok(profile)
        })
        .collect()
}

/// Decode a text file that may be UTF-8 or UTF-16 (Toad on Windows writes
/// UTF-16 with a BOM), dropping a leading BOM.
pub fn decode_text_file(bytes: &[u8]) -> Result<String, String> {
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest
            .chunks(2)
            .filter_map(|c| <[u8; 2]>::try_from(c).ok())
            .map(|pair| {
                if le {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|e| e.to_string())
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => {
            String::from_utf8(rest.to_vec()).map_err(|e| e.to_string())
        }
        _ => String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string()),
    }
}

/// True for Toad for Oracle's login list (`toadForOracleLogin_*.txt`,
/// INI sections named `[LOGIN1]`, `[LOGIN2]`...).
pub fn looks_like_toad_logins(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.len() > 6 && l[..6].eq_ignore_ascii_case("[LOGIN"))
}

/// Read the logins of a Toad for Oracle export. Toad does not write the
/// password (or writes it encrypted), so imported profiles ask for it on
/// connect. Everything is grouped in a "Toad" folder; SYSDBA is not carried
/// over, like for Ora's own files.
pub fn import_toad_logins(text: &str) -> Result<Vec<SavedProfile>, String> {
    let mut sections: Vec<BTreeMap<String, String>> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            sections.push(BTreeMap::new());
        } else if let (Some(section), Some((key, value))) =
            (sections.last_mut(), line.split_once('='))
        {
            section.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let profiles: Vec<SavedProfile> = sections
        .iter()
        .filter_map(|fields| {
            let get = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
            let username = get("user");
            if username.is_empty() {
                return None;
            }
            let (host, port) = (get("host"), get("port"));
            let (service, sid) = (get("servicename"), get("sid"));
            let connect_string = if host.is_empty() {
                // Older entries only have the raw `Server` text: a TNS
                // alias or an Easy Connect string.
                get("server").to_string()
            } else if service.is_empty() && !sid.is_empty() {
                let port = if port.is_empty() { "1521" } else { port };
                format!(
                    "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST={host})(PORT={port}))(CONNECT_DATA=(SID={sid})))"
                )
            } else {
                crate::model::compose_easy_connect(host, port, service)
            };
            if connect_string.is_empty() {
                return None;
            }
            let name = match get("alias") {
                "" => format!("{username}@{connect_string}"),
                alias => alias.to_string(),
            };
            Some(SavedProfile {
                id: new_profile_id(),
                name,
                kind: DbKind::Oracle,
                username: username.to_string(),
                connect_string,
                folder: "Toad".to_string(),
                access: ConnectionAccess {
                    read_only: get("sessionreadonly") == "1",
                    ..ConnectionAccess::default()
                },
                save_password: false,
            })
        })
        .collect();
    if profiles.is_empty() {
        return Err("No Toad logins found in this file.".into());
    }
    Ok(profiles)
}
