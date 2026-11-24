//! Data tab preferences kept in `settings.json`.

use super::*;

/// Whether a data tab counts all rows by itself after loading a page.
/// Off by default: a `COUNT(*)` over a big table can be slow, and the
/// connection runs one statement at a time.
pub fn load_auto_count_in(dir: &Path) -> bool {
    read_json_file(&dir.join("settings.json"))
        .and_then(|v| v.get("data_auto_count").and_then(Json::as_bool))
        .unwrap_or(false)
}

pub fn save_auto_count_in(dir: &Path, on: bool) -> std::io::Result<()> {
    // Preserve any other settings keys already in the file.
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert("data_auto_count".into(), Json::Bool(on));
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_auto_count() -> bool {
    load_auto_count_in(&config_dir())
}

pub fn save_auto_count(on: bool) -> std::io::Result<()> {
    save_auto_count_in(&config_dir(), on)
}
