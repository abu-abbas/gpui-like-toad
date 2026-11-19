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
    assert!(dir.ends_with("gpui-like-toad"));
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
