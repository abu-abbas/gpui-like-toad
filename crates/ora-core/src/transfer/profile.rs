//! Save Profile / Load Profile: the wizard's choices as a JSON file entry.
//! Connections are referenced by profile name; passwords are never part of
//! a profile.

use std::fs;
use std::path::Path;

use crate::store::{parse_json, to_json, Json};

use super::spec::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferTarget {
    Connection(String),
    /// An SQL script file.
    SqlFile(String),
    /// A folder that receives one CSV file per table.
    CsvDir(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProfile {
    pub name: String,
    pub source_connection: String,
    pub target: TransferTarget,
    pub spec: TransferSpec,
}

fn s(v: &str) -> Json {
    Json::Str(v.to_string())
}

fn options_json(o: &TransferOptions) -> Json {
    Json::Obj(vec![
        ("create_tables".into(), Json::Bool(o.create_tables)),
        ("include_indexes".into(), Json::Bool(o.include_indexes)),
        (
            "include_primary_keys".into(),
            Json::Bool(o.include_primary_keys),
        ),
        ("include_uniques".into(), Json::Bool(o.include_uniques)),
        ("include_checks".into(), Json::Bool(o.include_checks)),
        (
            "include_foreign_keys".into(),
            Json::Bool(o.include_foreign_keys),
        ),
        ("include_defaults".into(), Json::Bool(o.include_defaults)),
        ("lock_source".into(), Json::Bool(o.lock_source)),
        ("include_triggers".into(), Json::Bool(o.include_triggers)),
        ("create_records".into(), Json::Bool(o.create_records)),
        ("use_transaction".into(), Json::Bool(o.use_transaction)),
        ("complete_inserts".into(), Json::Bool(o.complete_inserts)),
        ("continue_on_error".into(), Json::Bool(o.continue_on_error)),
        ("drop_target".into(), Json::Bool(o.drop_target)),
        ("empty_target".into(), Json::Bool(o.empty_target)),
        (
            "convert_case".into(),
            match o.convert_case {
                None => Json::Null,
                Some(NameCase::Lower) => s("lower"),
                Some(NameCase::Upper) => s("upper"),
            },
        ),
    ])
}

fn table_json(t: &TableSpec) -> Json {
    let mut pairs = vec![("source".to_string(), s(&t.source))];
    if let Some(target) = &t.target {
        pairs.push(("target".into(), s(target)));
    }
    if let Fields::Custom(map) = &t.fields {
        pairs.push((
            "fields".into(),
            Json::Arr(
                map.iter()
                    .map(|m| {
                        Json::Obj(vec![
                            ("source".into(), s(&m.source)),
                            ("target".into(), s(&m.target)),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    pairs.push(("batch".into(), Json::Num(t.rows.batch() as f64)));
    if let Rows::Custom { conditions, .. } = &t.rows {
        pairs.push((
            "recordsets".into(),
            Json::Arr(conditions.iter().map(|c| s(c)).collect()),
        ));
    }
    Json::Obj(pairs)
}

impl TransferProfile {
    pub fn to_json(&self) -> Json {
        let (kind, target) = match &self.target {
            TransferTarget::Connection(n) => ("connection", n),
            TransferTarget::SqlFile(p) => ("sql_file", p),
            TransferTarget::CsvDir(p) => ("csv_dir", p),
        };
        Json::Obj(vec![
            ("name".into(), s(&self.name)),
            ("source_connection".into(), s(&self.source_connection)),
            ("target_kind".into(), s(kind)),
            ("target".into(), s(target)),
            ("source_schema".into(), s(&self.spec.source_schema)),
            ("target_schema".into(), s(&self.spec.target_schema)),
            ("options".into(), options_json(&self.spec.options)),
            (
                "tables".into(),
                Json::Arr(self.spec.tables.iter().map(table_json).collect()),
            ),
        ])
    }

    pub fn from_json(v: &Json) -> Option<Self> {
        let text = |key: &str| v.get(key).and_then(Json::as_str).map(str::to_string);
        let target = text("target")?;
        let target = match text("target_kind")?.as_str() {
            "connection" => TransferTarget::Connection(target),
            "sql_file" => TransferTarget::SqlFile(target),
            "csv_dir" => TransferTarget::CsvDir(target),
            _ => return None,
        };
        let o = v.get("options")?;
        let flag = |key: &str, default: bool| o.get(key).and_then(Json::as_bool).unwrap_or(default);
        let d = TransferOptions::default();
        let options = TransferOptions {
            create_tables: flag("create_tables", d.create_tables),
            include_indexes: flag("include_indexes", d.include_indexes),
            include_primary_keys: flag("include_primary_keys", d.include_primary_keys),
            include_uniques: flag("include_uniques", d.include_uniques),
            include_checks: flag("include_checks", d.include_checks),
            include_foreign_keys: flag("include_foreign_keys", d.include_foreign_keys),
            include_defaults: flag("include_defaults", d.include_defaults),
            lock_source: flag("lock_source", d.lock_source),
            include_triggers: flag("include_triggers", d.include_triggers),
            create_records: flag("create_records", d.create_records),
            use_transaction: flag("use_transaction", d.use_transaction),
            complete_inserts: flag("complete_inserts", d.complete_inserts),
            continue_on_error: flag("continue_on_error", d.continue_on_error),
            drop_target: flag("drop_target", d.drop_target),
            empty_target: flag("empty_target", d.empty_target),
            convert_case: match o.get("convert_case").and_then(Json::as_str) {
                Some("lower") => Some(NameCase::Lower),
                Some("upper") => Some(NameCase::Upper),
                _ => None,
            },
        };
        let mut tables = Vec::new();
        for t in v.get("tables")?.as_arr()? {
            let source = t.get("source")?.as_str()?.to_string();
            let batch = t
                .get("batch")
                .and_then(Json::as_u64)
                .map_or(DEFAULT_BATCH, |b| b as usize);
            let fields = match t.get("fields").and_then(Json::as_arr) {
                Some(items) => Fields::Custom(
                    items
                        .iter()
                        .filter_map(|m| {
                            Some(FieldMap {
                                source: m.get("source")?.as_str()?.to_string(),
                                target: m.get("target")?.as_str()?.to_string(),
                            })
                        })
                        .collect(),
                ),
                None => Fields::All,
            };
            let rows = match t.get("recordsets").and_then(Json::as_arr) {
                Some(items) => Rows::Custom {
                    batch,
                    conditions: items
                        .iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect(),
                },
                None => Rows::All { batch },
            };
            tables.push(TableSpec {
                source,
                target: t.get("target").and_then(Json::as_str).map(str::to_string),
                fields,
                rows,
            });
        }
        Some(TransferProfile {
            name: text("name")?,
            source_connection: text("source_connection")?,
            target,
            spec: TransferSpec {
                source_schema: text("source_schema")?,
                target_schema: text("target_schema")?,
                tables,
                options,
            },
        })
    }
}

const FILE: &str = "transfer-profiles.json";

/// All saved profiles; a missing or damaged file is an empty list.
pub fn load_profiles_in(dir: &Path) -> Vec<TransferProfile> {
    let Ok(text) = fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    match parse_json(&text) {
        Ok(Json::Arr(items)) => items
            .iter()
            .filter_map(TransferProfile::from_json)
            .collect(),
        _ => Vec::new(),
    }
}

/// Save `profile`, replacing a profile of the same name.
pub fn save_profile_in(dir: &Path, profile: &TransferProfile) -> std::io::Result<()> {
    let mut all = load_profiles_in(dir);
    all.retain(|p| p.name != profile.name);
    all.push(profile.clone());
    all.sort_by_key(|p| p.name.to_lowercase());
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(FILE),
        to_json(&Json::Arr(
            all.iter().map(TransferProfile::to_json).collect(),
        )),
    )
}

pub fn delete_profile_in(dir: &Path, name: &str) -> std::io::Result<()> {
    let mut all = load_profiles_in(dir);
    all.retain(|p| p.name != name);
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(FILE),
        to_json(&Json::Arr(
            all.iter().map(TransferProfile::to_json).collect(),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TransferProfile {
        let mut custom = TableSpec::auto("ORDERS");
        custom.target = Some("ORDERS_COPY".into());
        custom.fields = Fields::Custom(vec![FieldMap {
            source: "ID".into(),
            target: "ORDER_ID".into(),
        }]);
        custom.rows = Rows::Custom {
            batch: 250,
            conditions: vec!["\"ID\" >= 1 AND \"ID\" < 10".into()],
        };
        let options = TransferOptions {
            convert_case: Some(NameCase::Lower),
            continue_on_error: true,
            ..Default::default()
        };
        TransferProfile {
            name: "nightly".into(),
            source_connection: "DEV".into(),
            target: TransferTarget::Connection("STAGING".into()),
            spec: TransferSpec {
                source_schema: "A".into(),
                target_schema: "B".into(),
                tables: vec![TableSpec::auto("T"), custom],
                options,
            },
        }
    }

    #[test]
    fn profile_round_trips_through_a_file() {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("ora-transfer-profile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = sample();
        save_profile_in(&dir, &p).unwrap();
        assert_eq!(load_profiles_in(&dir), vec![p.clone()]);
        // Same name replaces; another name is added.
        let mut q = p.clone();
        q.name = "other".into();
        q.target = TransferTarget::SqlFile("/tmp/x.sql".into());
        save_profile_in(&dir, &q).unwrap();
        save_profile_in(&dir, &p).unwrap();
        assert_eq!(load_profiles_in(&dir).len(), 2);
        delete_profile_in(&dir, "other").unwrap();
        assert_eq!(load_profiles_in(&dir), vec![p]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_holds_no_password_field() {
        let text = to_json(&sample().to_json());
        assert!(!text.to_lowercase().contains("password"));
    }

    #[test]
    fn damaged_files_load_as_empty() {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("ora-transfer-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), "{not json").unwrap();
        assert!(load_profiles_in(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
