#[test]
fn toad_logins_become_profiles() {
    let text = "\u{feff}[LOGIN1]\r\nUser=WEBKE\r\nServer=x\r\nSavePassword=0\r\nSessionReadOnly=1\r\nAlias=\r\nHost=dbm.example\r\nServiceName=PDL\r\nSID=\r\nPort=1521\r\nConnectAs=SYSDBA\r\n\r\n[LOGIN2]\r\nUser=SIM\r\nServer=OLDTNS\r\nAlias=Simpeg\r\nHost=\r\nSID=\r\n\r\n[LOGIN3]\r\nUser=\r\nHost=h\r\n\r\n[LOGIN4]\r\nUser=U4\r\nHost=h4\r\nSID=ORCL\r\n";
    let text = text.trim_start_matches('\u{feff}');
    assert!(looks_like_toad_logins(text));
    let profiles = import_connection_profiles(text).unwrap();
    assert_eq!(profiles.len(), 3);
    assert_eq!(profiles[0].name, "WEBKE@dbm.example:1521/PDL");
    assert_eq!(profiles[0].connect_string, "dbm.example:1521/PDL");
    assert!(profiles[0].access.read_only);
    assert!(!profiles[0].access.sysdba);
    assert_eq!(profiles[0].folder, "Toad");
    assert_eq!(profiles[1].name, "Simpeg");
    assert_eq!(profiles[1].connect_string, "OLDTNS");
    assert!(profiles[2].connect_string.contains("(SID=ORCL)"));
    assert_ne!(profiles[0].id, profiles[1].id);
}

#[test]
fn decode_text_file_handles_utf16_and_bom() {
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("[LOGIN1]".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    assert_eq!(decode_text_file(&utf16).unwrap(), "[LOGIN1]");
    assert_eq!(decode_text_file(b"\xEF\xBB\xBFabc").unwrap(), "abc");
}
#[test]
fn folder_paths_rebase() {
    use super::{folder_in, folder_rebase};
    assert!(folder_in("a/b", "a") && !folder_in("ab", "a") && folder_in("a", "a"));
    assert_eq!(folder_rebase("a/b/c", "a/b", "x").as_deref(), Some("x/c"));
    assert_eq!(folder_rebase("a/b", "a/b", "x").as_deref(), Some("x"));
    assert_eq!(folder_rebase("a/b", "a/b", "").as_deref(), Some(""));
    assert_eq!(folder_rebase("a/b/c", "a/b", "").as_deref(), Some("c"));
    assert_eq!(folder_rebase("ab", "a", "x"), None);
}

use super::theme::{read_appearance, write_appearance};
use super::*;

fn scratch(name: &str) -> PathBuf {
    // Test-only scratch dir, unique per process id and test name.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    let dir = env::temp_dir().join(format!(
        "gpui-like-toad-store-test-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn import_preserves_environment_without_reusing_credentials() {
    let original = SavedProfile {
        kind: Default::default(),
        id: "machine-keychain-id".into(),
        name: "Production".into(),
        username: "REPORTS".into(),
        connect_string: "host:1521/prod".into(),
        folder: "Oracle".into(),
        access: ConnectionAccess {
            read_only: true,
            protect_production: true,
            sysdba: true,
            environment: ConnectionEnvironment::Production,
            ..Default::default()
        },
        save_password: true,
    };
    let portable = export_connection_profiles(std::slice::from_ref(&original));
    assert!(!portable.contains(&original.id));
    assert!(!portable.contains("save_password"));
    let imported = &import_connection_profiles(&portable).unwrap()[0];
    assert_ne!(imported.id, original.id);
    assert_eq!(imported.folder, original.folder);
    assert!(imported.access.read_only);
    assert!(imported.access.protect_production);
    assert_eq!(
        imported.access.environment,
        ConnectionEnvironment::Production
    );
    assert!(!imported.access.sysdba);
    assert!(!imported.save_password);
    assert!(import_connection_profiles(&"[".repeat(80)).is_err());
}

#[test]
fn appearance_round_trips_alongside_theme_preference() {
    let dir = scratch("appearance");
    let prefs = AppearancePrefs {
        app_icon: AppIcon::Bolt,
        accent: "Teal".into(),
        ui_font: Some("Arial".into()),
        ui_font_size: 15,
        sql_font_size: 17,
        reduce_motion: Some(true),
    };
    assert_eq!(read_appearance(&dir), AppearancePrefs::default());
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    write_appearance(&dir, &prefs).unwrap();
    assert_eq!(read_appearance(&dir), prefs);
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn json_round_trip() {
    let value = Json::Obj(vec![
        ("name".to_string(), Json::Str("HR \"prod\"".to_string())),
        ("n".to_string(), Json::Num(42.0)),
        ("f".to_string(), Json::Num(1.5)),
        ("ok".to_string(), Json::Bool(true)),
        ("nothing".to_string(), Json::Null),
        (
            "list".to_string(),
            Json::Arr(vec![
                Json::Str("a\nb\t\\".to_string()),
                Json::Str("é😀".to_string()),
                Json::Num(-3.0),
            ]),
        ),
    ]);
    let text = to_json(&value);
    assert_eq!(parse_json(&text).unwrap(), value);
    assert!(text.contains("\\n"));
    assert!(text.contains("é😀"));
}

#[test]
fn json_rejects_malformed_input() {
    assert!(parse_json("{").is_err());
    assert!(parse_json("[1,]").is_err());
    assert!(parse_json("{\"a\": }").is_err());
    assert!(parse_json("\"unterminated").is_err());
    assert!(parse_json("tru").is_err());
    assert!(parse_json("{} extra").is_err());
    assert!(parse_json("").is_err());
    assert_eq!(
        parse_json("  { \"a\" : [1, 2.5, true, null] }  ").unwrap(),
        Json::Obj(vec![(
            "a".to_string(),
            Json::Arr(vec![
                Json::Num(1.0),
                Json::Num(2.5),
                Json::Bool(true),
                Json::Null
            ])
        )])
    );
}

#[test]
fn json_unicode_escapes() {
    assert_eq!(
        parse_json("\"\\u0041\\u00e9\"").unwrap(),
        Json::Str("Aé".to_string())
    );
    // Surrogate pair for U+1F600.
    assert_eq!(
        parse_json("\"\\uD83D\\uDE00\"").unwrap(),
        Json::Str("😀".to_string())
    );
}

#[test]
fn profiles_round_trip_and_fail_soft() {
    let dir = scratch("profiles");
    assert!(load_profiles_in(&dir).is_empty());
    let profiles = vec![
        SavedProfile {
            kind: Default::default(),
            id: "p1".to_string(),
            name: "HR Local".to_string(),
            username: "hr".to_string(),
            connect_string: "localhost:1521/FREEPDB1".to_string(),
            folder: "Work".into(),
            access: Default::default(),
            save_password: true,
        },
        SavedProfile {
            kind: DbKind::Sqlite,
            id: "p2".to_string(),
            name: "Prod \"A\"".to_string(),
            username: "app".to_string(),
            connect_string: "db:1521/PROD".to_string(),
            folder: String::new(),
            access: Default::default(),
            save_password: false,
        },
    ];
    save_profiles_in(&dir, &profiles).unwrap();
    assert_eq!(load_profiles_in(&dir), profiles);
    // Corrupt file -> empty list, not a crash.
    fs::write(dir.join("connections.json"), "{not json").unwrap();
    assert!(load_profiles_in(&dir).is_empty());
    // Entries missing a field are skipped, valid ones kept.
    fs::write(
        dir.join("connections.json"),
        r#"[{"name":"x"},{"name":"y","username":"u","connect_string":"c"}]"#,
    )
    .unwrap();
    let legacy = load_profiles_in(&dir);
    assert_eq!(legacy.len(), 1);
    // Files from before the database kind existed are Oracle profiles.
    assert_eq!(legacy[0].kind, DbKind::Oracle);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn profiles_file_never_holds_a_password() {
    let dir = scratch("profiles-nopw");
    let profiles = vec![SavedProfile {
        kind: Default::default(),
        id: "p1".to_string(),
        name: "HR".to_string(),
        username: "hr".to_string(),
        connect_string: "db:1521/X".to_string(),
        folder: String::new(),
        access: Default::default(),
        save_password: true,
    }];
    save_profiles_in(&dir, &profiles).unwrap();
    let text = fs::read_to_string(dir.join("connections.json")).unwrap();
    assert!(!text.contains("\"password\""), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn old_profiles_get_stable_ids() {
    let dir = scratch("profiles-legacy");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("connections.json"),
        r#"[{"name":"a","username":"u","connect_string":"c"},
           {"id":"dup","name":"b","username":"u","connect_string":"c"},
           {"id":"dup","name":"c","username":"u","connect_string":"c"}]"#,
    )
    .unwrap();
    let first = load_profiles_in(&dir);
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|p| !p.id.is_empty() && !p.save_password));
    assert_eq!(first[1].id, "dup");
    assert_ne!(first[2].id, "dup");
    assert_ne!(first[0].id, first[2].id);
    // Ids were written back, so a second load sees the same ones.
    assert_eq!(load_profiles_in(&dir), first);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn profile_ids_are_unique() {
    let ids: std::collections::HashSet<_> = (0..1000).map(|_| new_profile_id()).collect();
    assert_eq!(ids.len(), 1000);
}

#[test]
fn history_round_trip_dedupe_and_cap() {
    let dir = scratch("history");
    let mut entries = Vec::new();
    record_history(&mut entries, "SELECT 1", 100);
    record_history(&mut entries, "SELECT 2", 101);
    record_history(&mut entries, "SELECT 1", 102);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sql, "SELECT 1");
    assert_eq!(entries[0].saved_at, 102);
    record_history(&mut entries, "   ", 103);
    assert_eq!(entries.len(), 2);
    entries[1].favorite = true;
    record_history(&mut entries, "SELECT 2", 104);
    assert!(entries[0].favorite, "re-run keeps favorite flag");
    for i in 0..60 {
        record_history(&mut entries, &format!("SELECT {i}"), 200 + i);
    }
    assert_eq!(entries.len(), MAX_HISTORY_ENTRIES);
    save_history_in(&dir, &entries).unwrap();
    assert_eq!(load_history_in(&dir), entries);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn theme_preference_round_trip() {
    let dir = scratch("theme");
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::System);
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    save_theme_preference_in(&dir, ThemePreference::Light).unwrap();
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Light);
    fs::write(dir.join("settings.json"), r#"{"theme":"bogus"}"#).unwrap();
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::System);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn panel_layout_round_trip_keeps_other_keys() {
    let dir = scratch("panel-layout");
    assert_eq!(load_panel_layout_in(&dir), PanelLayout::default());
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    let layout = PanelLayout {
        info_collapsed: true,
        plan_collapsed: false,
        sidebar_width: Some(300),
        explorer_width: None,
    };
    save_panel_layout_in(&dir, layout).unwrap();
    assert_eq!(load_panel_layout_in(&dir), layout);
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn theme_names_round_trip_keep_other_keys() {
    let dir = scratch("theme-names");
    assert_eq!(load_theme_names_in(&dir), ThemeNames::default());
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    let names = ThemeNames {
        light: Some("Ayu Light".into()),
        dark: Some("Tokyo Night".into()),
    };
    save_theme_names_in(&dir, &names).unwrap();
    assert_eq!(load_theme_names_in(&dir), names);
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    // Back to the app palette removes the key again.
    save_theme_names_in(
        &dir,
        &ThemeNames {
            light: None,
            ..names.clone()
        },
    )
    .unwrap();
    assert_eq!(load_theme_names_in(&dir).light, None);
    let text = fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(!text.contains("light_theme"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn config_dir_is_app_specific() {
    let dir = config_dir();
    assert!(dir.ends_with("Kilat"));
    assert!(legacy_config_dir().ends_with("gpui-like-toad"));
    assert!(dir.is_absolute() || dir.starts_with("."));
}

#[test]
fn update_prefs_roundtrip_keeps_theme() {
    let dir = scratch("update-prefs");
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    assert_eq!(load_update_prefs_in(&dir), UpdatePrefs::default());
    let prefs = UpdatePrefs {
        auto_check: false,
        last_checked: 1_700_000_000,
        dismissed_version: Some("0.2.0".into()),
        last_launched_version: Some("0.1.0".into()),
        proxy: Some("http://proxy.corp:8080".into()),
    };
    save_update_prefs_in(&dir, &prefs).unwrap();
    assert_eq!(load_update_prefs_in(&dir), prefs);
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
}

fn write_settings(dir: &Path, proxy: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        format!("{{\"theme\":\"x\",\"network_proxy\":\"{proxy}\"}}"),
    )
    .unwrap();
}

#[test]
fn legacy_plaintext_proxy_password_moves_to_the_keychain() {
    use crate::credentials::{CredentialStore, ExposeSecret as _, MemoryCredentialStore};
    let dir = scratch("proxy-legacy");
    write_settings(&dir, "http://bob:s3cret@proxy:8080");
    let keychain = MemoryCredentialStore::default();
    let resolved = resolve_proxy_in(&dir, &keychain);
    assert_eq!(
        resolved.url.as_deref(),
        Some("http://bob:s3cret@proxy:8080")
    );
    assert_eq!(resolved.warning, None);
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(!text.contains("s3cret"), "{text}");
    assert!(text.contains("http://bob@proxy:8080") && text.contains("\"theme\""));
    let saved = keychain.get(PROXY_CREDENTIAL_ACCOUNT).unwrap().unwrap();
    assert_eq!(saved.expose_secret(), "s3cret");
    // Second start: password comes from the keychain, file stays clean.
    let again = resolve_proxy_in(&dir, &keychain);
    assert_eq!(again.url.as_deref(), Some("http://bob:s3cret@proxy:8080"));
    assert_eq!(
        load_update_prefs_in(&dir).proxy.as_deref(),
        Some("http://bob@proxy:8080")
    );
}

#[test]
fn failed_keychain_keeps_the_legacy_password_and_warns_without_leaking() {
    use crate::credentials::{CredentialError, CredentialStore, SecretString};
    struct Broken;
    impl CredentialStore for Broken {
        fn get(&self, _: &str) -> Result<Option<SecretString>, CredentialError> {
            Err(CredentialError("locked".into()))
        }
        fn set(&self, _: &str, _: &SecretString) -> Result<(), CredentialError> {
            Err(CredentialError("locked s3cret".into()))
        }
        fn delete(&self, _: &str) -> Result<(), CredentialError> {
            Ok(())
        }
    }
    let dir = scratch("proxy-broken");
    write_settings(&dir, "http://bob:s3cret@proxy:8080");
    let resolved = resolve_proxy_in(&dir, &Broken);
    assert_eq!(
        resolved.url.as_deref(),
        Some("http://bob:s3cret@proxy:8080")
    );
    let warning = resolved.warning.unwrap();
    assert!(
        warning.contains("still stored") && !warning.contains("s3cret"),
        "{warning}"
    );
    // Nothing lost: the file still has it, and saving prefs never rewrites it back.
    assert!(std::fs::read_to_string(dir.join("settings.json"))
        .unwrap()
        .contains("s3cret"));
    let prefs = load_update_prefs_in(&dir);
    assert_eq!(prefs.proxy.as_deref(), Some("http://bob@proxy:8080"));
}

#[test]
fn settings_proxy_without_user_never_touches_the_keychain() {
    use crate::credentials::MemoryCredentialStore;
    let dir = scratch("proxy-nouser");
    write_settings(&dir, "http://proxy:8080");
    let keychain = MemoryCredentialStore::default();
    let resolved = resolve_proxy_in(&dir, &keychain);
    assert_eq!(resolved.url.as_deref(), Some("http://proxy:8080"));
    assert!(keychain.is_empty());
}

#[test]
fn saving_a_proxy_keeps_the_password_out_of_settings() {
    use crate::credentials::{CredentialStore, ExposeSecret as _, MemoryCredentialStore};
    let dir = scratch("proxy-save");
    let keychain = MemoryCredentialStore::default();
    let out = save_proxy_in(&dir, &keychain, Some("http://bob:pw1@proxy:3128")).unwrap();
    assert_eq!(out.url.as_deref(), Some("http://bob:pw1@proxy:3128"));
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(!text.contains("pw1"), "{text}");
    assert_eq!(
        keychain
            .get(PROXY_CREDENTIAL_ACCOUNT)
            .unwrap()
            .unwrap()
            .expose_secret(),
        "pw1"
    );
    // Changing to a URL without a password drops the old one.
    let out = save_proxy_in(&dir, &keychain, Some("http://bob@other:3128")).unwrap();
    assert_eq!(out.url.as_deref(), Some("http://bob@other:3128"));
    assert!(keychain.get(PROXY_CREDENTIAL_ACCOUNT).unwrap().is_none());
    // Clearing the field removes the setting.
    save_proxy_in(&dir, &keychain, None).unwrap();
    assert_eq!(load_update_prefs_in(&dir).proxy, None);
}

#[test]
fn history_option_roundtrips_and_defaults_to_recording() {
    let dir = scratch("history-opt");
    let mut profile = SavedProfile {
        id: "p1".into(),
        name: "n".into(),
        kind: Default::default(),
        username: "u".into(),
        connect_string: "h:1521/s".into(),
        folder: String::new(),
        access: ConnectionAccess::default(),
        save_password: false,
    };
    assert!(!profile.access.skip_history);
    let mut off = profile.clone();
    off.id = "p2".into();
    off.access.skip_history = true;
    save_profiles_in(&dir, &[profile.clone(), off]).unwrap();
    let loaded = load_profiles_in(&dir);
    assert!(!loaded[0].access.skip_history);
    assert!(loaded[1].access.skip_history);
    // A file from before the option has no key and keeps recording.
    profile.id = "p3".into();
    let text = std::fs::read_to_string(dir.join("connections.json")).unwrap();
    assert!(text.contains("record_history"));
    let legacy = text.replace("\"record_history\"", "\"x\"");
    std::fs::write(dir.join("connections.json"), legacy).unwrap();
    assert!(load_profiles_in(&dir)
        .iter()
        .all(|p| !p.access.skip_history));
}

#[test]
fn migrate_renames_old_dir_with_all_data() {
    let root = scratch("migrate-rename");
    let (old, new) = (root.join("gpui-like-toad"), root.join("Kilat"));
    fs::create_dir_all(old.join("server-logs/developer-tools")).unwrap();
    fs::write(old.join("connections.json"), "c").unwrap();
    fs::write(old.join("timing-on"), "").unwrap();
    fs::write(
        old.join("server-logs/developer-tools/running.pids"),
        "1 redis\n",
    )
    .unwrap();
    assert_eq!(migrate_dir(&old, &new), Migration::Renamed);
    assert!(!old.exists());
    assert_eq!(
        fs::read_to_string(new.join("connections.json")).unwrap(),
        "c"
    );
    assert!(new.join("timing-on").exists());
    assert!(new
        .join("server-logs/developer-tools/running.pids")
        .exists());
    // second run does nothing
    assert_eq!(migrate_dir(&old, &new), Migration::NotNeeded);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migrate_never_touches_existing_new_dir() {
    let root = scratch("migrate-keep");
    let (old, new) = (root.join("gpui-like-toad"), root.join("Kilat"));
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    fs::write(old.join("settings.json"), "old").unwrap();
    fs::write(new.join("settings.json"), "new").unwrap();
    assert_eq!(migrate_dir(&old, &new), Migration::NotNeeded);
    assert_eq!(
        fs::read_to_string(new.join("settings.json")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::read_to_string(old.join("settings.json")).unwrap(),
        "old"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migrate_without_old_dir_is_noop() {
    let root = scratch("migrate-none");
    let new = root.join("Kilat");
    assert_eq!(
        migrate_dir(&root.join("gpui-like-toad"), &new),
        Migration::NotNeeded
    );
    assert!(!new.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migrate_copy_fallback_keeps_old_and_nests() {
    let root = scratch("migrate-copy");
    let old = root.join("gpui-like-toad");
    fs::create_dir_all(old.join("a/b")).unwrap();
    fs::write(old.join("a/b/f.txt"), "x").unwrap();
    let tmp = root.join("Kilat.migrating");
    migrate::copy_tree(&old, &tmp).unwrap();
    assert_eq!(fs::read_to_string(tmp.join("a/b/f.txt")).unwrap(), "x");
    assert!(old.join("a/b/f.txt").exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn auto_count_defaults_off_and_keeps_other_settings() {
    let dir = scratch("auto-count");
    assert!(!load_auto_count_in(&dir));
    save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
    save_auto_count_in(&dir, true).unwrap();
    assert!(load_auto_count_in(&dir));
    assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    save_auto_count_in(&dir, false).unwrap();
    assert!(!load_auto_count_in(&dir));
}

// ---------------------------------------------------------------------------
// Settings Sync
// ---------------------------------------------------------------------------

fn sync_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("kilat-sync-test-{tag}-{}", new_profile_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sync_profile(id: &str, name: &str, env: ConnectionEnvironment) -> SavedProfile {
    SavedProfile {
        id: id.into(),
        name: name.into(),
        kind: DbKind::from_key("oracle"),
        username: "u".into(),
        connect_string: "h:1521/S".into(),
        folder: String::new(),
        access: ConnectionAccess {
            environment: env,
            ..Default::default()
        },
        save_password: true,
    }
}

const PASS: &str = "test passphrase";

fn sync_plain(dir: &Path, choices: &BTreeMap<String, Choice>) -> Result<SyncOutcome, String> {
    let store = crate::credentials::MemoryCredentialStore::default();
    sync_with(
        dir,
        choices,
        &SyncSecrets {
            store: &store,
            passphrase: PASS,
        },
    )
}

/// What the sync file holds once decrypted, as JSON text.
fn shared_text(shared: &Path) -> String {
    let items = read_sync_file(&shared.join(SYNC_FILE), Some(PASS))
        .unwrap()
        .unwrap()
        .0;
    to_json(&Json::Obj(items.into_iter().collect()))
}

fn enable_sync(dir: &Path, shared: &Path, name: &str) {
    let mut c = load_sync_config_in(dir);
    c.enabled = true;
    c.folder = shared.to_string_lossy().into();
    c.device_name = name.into();
    save_sync_config_in(dir, &c).unwrap();
}

#[test]
fn sync_moves_connections_between_computers_without_prod_or_passwords() {
    let (a, b, shared) = (sync_dir("a"), sync_dir("b"), sync_dir("shared"));
    save_profiles_in(
        &a,
        &[
            sync_profile("1", "dev", ConnectionEnvironment::Development),
            sync_profile("2", "prod", ConnectionEnvironment::Production),
        ],
    )
    .unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    assert!(matches!(
        sync_plain(&a, &BTreeMap::new()),
        Ok(SyncOutcome::Synced { pushed: 1.., .. })
    ));
    let text = shared_text(&shared);
    assert!(text.contains("\"dev\"") && !text.contains("\"prod\""));
    assert!(!text.to_lowercase().contains("password\":\""));

    sync_plain(&b, &BTreeMap::new()).unwrap();
    let on_b = load_profiles_in(&b);
    assert_eq!(on_b.len(), 1);
    assert_eq!(on_b[0].name, "dev");

    // Deleting on B reaches A; A's local PROD connection is untouched.
    save_profiles_in(&b, &[]).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();
    let on_a = load_profiles_in(&a);
    assert_eq!(on_a.len(), 1);
    assert_eq!(on_a[0].name, "prod");
}

#[test]
fn approved_prod_connection_syncs() {
    let (a, b, shared) = (sync_dir("pa"), sync_dir("pb"), sync_dir("pshared"));
    save_profiles_in(
        &a,
        &[sync_profile("2", "prod", ConnectionEnvironment::Production)],
    )
    .unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    let mut c = load_sync_config_in(&a);
    c.prod_approved = vec!["2".into()];
    save_sync_config_in(&a, &c).unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();
    assert_eq!(load_profiles_in(&b)[0].name, "prod");
    // Still there after another round on A (the base keeps it approved).
    sync_plain(&a, &BTreeMap::new()).unwrap();
    assert!(read_sync_file(&shared.join(SYNC_FILE), Some(PASS))
        .unwrap()
        .unwrap()
        .0
        .contains_key("conn:2"));
}

#[test]
fn change_on_both_sides_is_a_conflict_until_chosen() {
    let (a, b, shared) = (sync_dir("ca"), sync_dir("cb"), sync_dir("cshared"));
    save_profiles_in(&a, &[sync_profile("1", "dev", ConnectionEnvironment::None)]).unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    sync_plain(&a, &BTreeMap::new()).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();

    save_profiles_in(
        &a,
        &[sync_profile("1", "from-a", ConnectionEnvironment::None)],
    )
    .unwrap();
    save_profiles_in(
        &b,
        &[sync_profile("1", "from-b", ConnectionEnvironment::None)],
    )
    .unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();

    let Ok(SyncOutcome::Conflicts(conflicts)) = sync_plain(&b, &BTreeMap::new()) else {
        panic!("expected a conflict");
    };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].key, "conn:1");
    // Nothing changed on B while the conflict is open.
    assert_eq!(load_profiles_in(&b)[0].name, "from-b");

    let choices = BTreeMap::from([("conn:1".to_string(), Choice::Remote)]);
    sync_plain(&b, &choices).unwrap();
    assert_eq!(load_profiles_in(&b)[0].name, "from-a");
}

#[test]
fn first_sync_merges_both_sides_and_keeps_old_versions() {
    let (a, b, shared) = (sync_dir("ma"), sync_dir("mb"), sync_dir("mshared"));
    save_profiles_in(&a, &[sync_profile("1", "one", ConnectionEnvironment::None)]).unwrap();
    save_profiles_in(&b, &[sync_profile("2", "two", ConnectionEnvironment::None)]).unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    sync_plain(&a, &BTreeMap::new()).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();
    assert_eq!(load_profiles_in(&a).len(), 2);
    assert_eq!(load_profiles_in(&b).len(), 2);
    assert!(shared.join("versions").is_dir());
}

#[test]
fn favorites_and_appearance_sync_but_plain_history_does_not() {
    let (a, b, shared) = (sync_dir("fa"), sync_dir("fb"), sync_dir("fshared"));
    let mut hist = Vec::new();
    record_history(&mut hist, "select 1 from secret_table", 10);
    record_history(&mut hist, "select 2 from dual", 20);
    hist[0].favorite = true;
    save_history_in(&a, &hist).unwrap();
    save_theme_names_in(
        &a,
        &ThemeNames {
            light: Some("Latte".into()),
            dark: None,
        },
    )
    .unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    sync_plain(&a, &BTreeMap::new()).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();
    let on_b = load_history_in(&b);
    assert_eq!(on_b.len(), 1);
    assert_eq!(on_b[0].sql, "select 2 from dual");
    assert_eq!(load_theme_names_in(&b).light.as_deref(), Some("Latte"));
}

#[test]
fn kilat_file_export_and_import_never_delete() {
    let (a, b) = (sync_dir("ea"), sync_dir("eb"));
    save_profiles_in(&a, &[sync_profile("1", "one", ConnectionEnvironment::None)]).unwrap();
    save_profiles_in(
        &b,
        &[sync_profile("9", "mine", ConnectionEnvironment::None)],
    )
    .unwrap();
    let file = a.join("backup.kilat");
    export_kilat_file(
        &a,
        &file,
        SyncCategories::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    import_kilat_file(&b, &file, SyncCategories::default(), &BTreeMap::new(), None).unwrap();
    assert_eq!(load_profiles_in(&b).len(), 2);
    assert!(read_sync_file(&a.join("nope.kilat"), None)
        .unwrap()
        .is_none());
    fs::write(a.join("bad.kilat"), "{}").unwrap();
    assert!(read_sync_file(&a.join("bad.kilat"), None).is_err());
}

#[test]
fn tools_and_grid_sync_but_never_the_start_command() {
    let (a, b, shared) = (sync_dir("ta"), sync_dir("tb"), sync_dir("tshared"));
    let mut redis = ToolInstance::new(ToolKind::Redis);
    redis.name = "Cache".into();
    redis.start_server = true;
    redis.server_command = "redis-server --port 1".into();
    let mut prod = ToolInstance::new(ToolKind::Redis);
    prod.name = "Prod cache".into();
    prod.environment = ConnectionEnvironment::Production;
    save_tools_in(&a, &[redis.clone(), prod]).unwrap();
    save_auto_count_in(&a, true).unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    sync_plain(&a, &BTreeMap::new()).unwrap();

    let text = shared_text(&shared);
    assert!(text.contains("Cache") && !text.contains("Prod cache"));
    assert!(!text.contains("server_command") && !text.contains("redis-server"));

    sync_plain(&b, &BTreeMap::new()).unwrap();
    let on_b = load_tools_in(&b);
    assert_eq!(on_b.len(), 1);
    assert_eq!(on_b[0].name, "Cache");
    assert!(!on_b[0].start_server && on_b[0].server_command.is_empty());
    assert!(load_auto_count_in(&b));

    // A hand-edited file cannot plant a start command either.
    let mut v = read_sync_file(&shared.join(SYNC_FILE), Some(PASS))
        .unwrap()
        .unwrap()
        .0;
    let key = v.keys().find(|k| k.starts_with("tool:")).unwrap().clone();
    if let Some(Json::Obj(pairs)) = v.get_mut(&key) {
        pairs.push(("server_command".into(), Json::Str("rm -rf /".into())));
        pairs.push(("start_server".into(), Json::Bool(true)));
    }
    let file = a.join("planted.kilat");
    fs::write(&file, to_json(&sync_file_json(&v, "x"))).unwrap();
    import_kilat_file(&b, &file, SyncCategories::default(), &BTreeMap::new(), None).unwrap();
    let on_b = load_tools_in(&b);
    assert!(on_b
        .iter()
        .all(|t| !t.start_server && t.server_command.is_empty()));
}

#[test]
fn passwords_sync_only_encrypted_and_only_with_the_right_passphrase() {
    use crate::credentials::{CredentialStore, MemoryCredentialStore, SecretString};
    let (a, b, shared) = (sync_dir("sa"), sync_dir("sb"), sync_dir("sshared"));
    let (store_a, store_b) = (
        MemoryCredentialStore::default(),
        MemoryCredentialStore::default(),
    );
    let mut p = sync_profile("1", "dev", ConnectionEnvironment::None);
    p.save_password = true;
    let mut prod = sync_profile("2", "prod", ConnectionEnvironment::Production);
    prod.save_password = true;
    save_profiles_in(&a, &[p, prod]).unwrap();
    store_a
        .set("1", &SecretString::from("hunter2".to_string()))
        .unwrap();
    store_a
        .set("2", &SecretString::from("prod-pw".to_string()))
        .unwrap();
    for (dir, name) in [(&a, "A"), (&b, "B")] {
        enable_sync(dir, &shared, name);
        let mut c = load_sync_config_in(dir);
        c.categories.passwords = true;
        save_sync_config_in(dir, &c).unwrap();
    }
    let secrets_a = SyncSecrets {
        store: &store_a,
        passphrase: PASS,
    };
    let secrets_b = SyncSecrets {
        store: &store_b,
        passphrase: PASS,
    };
    assert_eq!(check_passphrase(&shared, "anything"), Ok(false));
    sync_with(&a, &BTreeMap::new(), &secrets_a).unwrap();

    let text = shared_text(&shared);
    let raw = fs::read_to_string(shared.join(SYNC_FILE)).unwrap();
    assert!(raw.contains("\"crypto\"") && !raw.contains("secret:1"));
    assert!(text.contains("secret:1"));
    assert!(!text.contains("hunter2") && !text.contains("prod-pw") && !text.contains("secret:2"));
    assert_eq!(check_passphrase(&shared, PASS), Ok(true));
    assert!(check_passphrase(&shared, "wrong").is_err());

    // Wrong passphrase on B: refused, B's keychain untouched.
    let wrong = SyncSecrets {
        store: &store_b,
        passphrase: "wrong",
    };
    assert!(sync_with(&b, &BTreeMap::new(), &wrong).is_err());
    assert!(store_b.is_empty());

    sync_with(&b, &BTreeMap::new(), &secrets_b).unwrap();
    assert_eq!(
        store_b
            .get("1")
            .unwrap()
            .map(|s| crate::credentials::ExposeSecret::expose_secret(&s).to_string()),
        Some("hunter2".to_string())
    );

    // B clears its keychain entry: the password stays in the file for A.
    store_b.delete("1").unwrap();
    sync_with(&b, &BTreeMap::new(), &secrets_b).unwrap();
    assert!(read_sync_file(&shared.join(SYNC_FILE), Some(PASS))
        .unwrap()
        .unwrap()
        .0
        .contains_key("secret:1"));
}

#[test]
fn other_computers_extensions_are_only_suggested() {
    let (a, b, shared) = (sync_dir("xa"), sync_dir("xb"), sync_dir("xshared"));
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    let mut ext = load_extensions_in(&a);
    ext.install("redis");
    save_extensions_in(&a, &ext).unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();
    sync_plain(&b, &BTreeMap::new()).unwrap();
    sync_plain(&a, &BTreeMap::new()).unwrap();
    let suggestions = extension_suggestions(&b);
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].0, "A");
    assert_eq!(suggestions[0].1[0].id, "redis");
    assert!(!load_extensions_in(&b).is_installed("redis"));
    assert!(extension_suggestions(&a).is_empty());
}

#[test]
fn sync_config_keeps_every_toggle() {
    let dir = sync_dir("cfg");
    let mut c = load_sync_config_in(&dir);
    c.categories = SyncCategories {
        connections: false,
        favorites: false,
        appearance: false,
        tools: false,
        grid: false,
        passwords: true,
        extensions: false,
    };
    c.prod_approved = vec!["x".into()];
    save_sync_config_in(&dir, &c).unwrap();
    let back = load_sync_config_in(&dir);
    assert_eq!(back.categories, c.categories);
    assert_eq!(back.prod_approved, c.prod_approved);
}

#[test]
fn the_sync_file_is_sealed_and_a_plain_one_is_upgraded() {
    let (a, b, shared) = (sync_dir("ea1"), sync_dir("eb1"), sync_dir("eshared1"));
    save_profiles_in(
        &a,
        &[sync_profile(
            "1",
            "secret-host-name",
            ConnectionEnvironment::None,
        )],
    )
    .unwrap();
    enable_sync(&a, &shared, "A");
    enable_sync(&b, &shared, "B");
    sync_plain(&a, &BTreeMap::new()).unwrap();
    let raw = fs::read_to_string(shared.join(SYNC_FILE)).unwrap();
    assert!(raw.contains("\"version\":2") || raw.contains("\"version\": 2"));
    assert!(!raw.contains("secret-host-name") && !raw.contains("h:1521"));
    // Without the passphrase the file stays shut.
    assert_eq!(
        read_sync_file(&shared.join(SYNC_FILE), None).unwrap_err(),
        ENCRYPTED_FILE
    );
    assert!(read_sync_file(&shared.join(SYNC_FILE), Some("nope")).is_err());
    sync_plain(&b, &BTreeMap::new()).unwrap();
    assert_eq!(load_profiles_in(&b).len(), 1);

    // A plain file from an older Kilat is read, then replaced by a sealed
    // one; its plain backups are removed.
    let (c, old) = (sync_dir("ec1"), sync_dir("eold1"));
    let items: Items = [("conn:9".to_string(), Json::Str("legacy-item".into()))]
        .into_iter()
        .collect();
    fs::write(old.join(SYNC_FILE), to_json(&sync_file_json(&items, "old"))).unwrap();
    fs::create_dir_all(old.join("versions")).unwrap();
    fs::write(old.join("versions").join("kilat-sync-1.json"), "plain").unwrap();
    enable_sync(&c, &old, "C");
    sync_plain(&c, &BTreeMap::new()).unwrap();
    let raw = fs::read_to_string(old.join(SYNC_FILE)).unwrap();
    assert!(!raw.contains("legacy-item") && raw.contains("payload"));
    assert!(!old.join("versions").exists());
    assert!(read_sync_file(&old.join(SYNC_FILE), Some(PASS))
        .unwrap()
        .unwrap()
        .0
        .contains_key("conn:9"));
}

#[test]
fn a_sealed_export_needs_the_passphrase_to_import() {
    let (a, b) = (sync_dir("ea2"), sync_dir("eb2"));
    save_profiles_in(&a, &[sync_profile("1", "one", ConnectionEnvironment::None)]).unwrap();
    let file = a.join("sealed.kilat");
    export_kilat_file(
        &a,
        &file,
        SyncCategories::default(),
        &Default::default(),
        Some(PASS),
    )
    .unwrap();
    assert!(!fs::read_to_string(&file).unwrap().contains("h:1521"));
    let none = BTreeMap::new();
    assert_eq!(
        import_kilat_file(&b, &file, SyncCategories::default(), &none, None).unwrap_err(),
        ENCRYPTED_FILE
    );
    assert!(import_kilat_file(&b, &file, SyncCategories::default(), &none, Some("x")).is_err());
    assert!(load_profiles_in(&b).is_empty());
    import_kilat_file(&b, &file, SyncCategories::default(), &none, Some(PASS)).unwrap();
    assert_eq!(load_profiles_in(&b).len(), 1);
}
