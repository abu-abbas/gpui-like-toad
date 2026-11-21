//! Oracle Recycle Bin: the SQL for Flashback Table and the PURGE family,
//! and the small text format that carries a recycle bin row through the
//! generic [`SchemaObject`](crate::SchemaObject) list.
//!
//! A dropped object keeps a system name like `BIN$q7m5w==$0`. That name
//! contains `$` and `=`, so it is always double-quoted. Only names that
//! start with `BIN$` are accepted, so these helpers can never be pointed at
//! a regular table by mistake.

use crate::dialect::Dialect;

/// Prefix Oracle gives every object in the recycle bin.
const BIN_PREFIX: &str = "BIN$";

/// `PURGE RECYCLEBIN`: empties the current user's recycle bin.
pub const PURGE_RECYCLEBIN_SQL: &str = "PURGE RECYCLEBIN";

/// `PURGE DBA_RECYCLEBIN`: empties every user's recycle bin. Needs the
/// SYSDBA privilege; without it Oracle answers ORA-38301.
pub const PURGE_DBA_RECYCLEBIN_SQL: &str = "PURGE DBA_RECYCLEBIN";

/// What the explorer needs to know about one recycle bin row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleInfo {
    /// `USER_RECYCLEBIN.TYPE`: TABLE, INDEX, TRIGGER, ...
    pub kind: String,
    /// `DROP` or `TRUNCATE`.
    pub operation: String,
    /// `DROPTIME` as Oracle prints it, `YYYY-MM-DD:HH24:MI:SS`, shown as
    /// `YYYY-MM-DD HH24:MI:SS`.
    pub dropped: String,
    pub can_undrop: bool,
    pub can_purge: bool,
}

const SEP: &str = " · ";

impl RecycleInfo {
    /// Text for the explorer row's `detail`; [`Self::parse`] reads it back.
    pub fn to_detail(&self) -> String {
        let flags = format!(
            "{}{}",
            if self.can_undrop { "U" } else { "-" },
            if self.can_purge { "P" } else { "-" }
        );
        [
            self.kind.as_str(),
            self.operation.as_str(),
            self.dropped.as_str(),
            flags.as_str(),
        ]
        .join(SEP)
    }

    pub fn parse(detail: &str) -> Option<Self> {
        let mut parts = detail.split(SEP);
        let (kind, operation, dropped, flags) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || flags.len() != 2 {
            return None;
        }
        Some(Self {
            kind: kind.to_string(),
            operation: operation.to_string(),
            dropped: dropped.to_string(),
            can_undrop: flags.starts_with('U'),
            can_purge: flags.ends_with('P'),
        })
    }

    /// Short label under the name, e.g. `TABLE · dropped 2026-11-21 10:20:30`.
    pub fn summary(&self) -> String {
        let verb = if self.operation.eq_ignore_ascii_case("TRUNCATE") {
            "truncated"
        } else {
            "dropped"
        };
        format!("{} · {verb} {}", self.kind, self.dropped)
    }

    /// Flashback Table works on tables that can still be undropped.
    pub fn can_flashback(&self) -> bool {
        self.kind.eq_ignore_ascii_case("TABLE") && self.can_undrop
    }

    /// PURGE TABLE / PURGE INDEX exist; other kinds leave with their table.
    pub fn can_purge_object(&self) -> bool {
        self.can_purge
            && (self.kind.eq_ignore_ascii_case("TABLE") || self.kind.eq_ignore_ascii_case("INDEX"))
    }
}

/// `YYYY-MM-DD:HH24:MI:SS` (Oracle's DROPTIME) as `YYYY-MM-DD HH24:MI:SS`.
pub fn pretty_droptime(raw: &str) -> String {
    let raw = raw.trim();
    match raw.split_once(':') {
        Some((date, time)) if date.len() == 10 => format!("{date} {time}"),
        _ => raw.to_string(),
    }
}

fn check_bin_name(name: &str) -> Result<(), String> {
    if !name.starts_with(BIN_PREFIX) || name.len() <= BIN_PREFIX.len() {
        return Err("That is not a recycle bin object.".to_string());
    }
    if name.chars().any(char::is_control) || name.len() > 128 {
        return Err("That is not a valid recycle bin name.".to_string());
    }
    Ok(())
}

/// `FLASHBACK TABLE "OWNER"."BIN$..." TO BEFORE DROP [RENAME TO "NEW"]`.
/// `rename_to` is for the case where a table with the original name exists
/// again (ORA-38312); blank means "restore under the original name".
pub fn flashback_table_sql(
    dialect: &dyn Dialect,
    owner: &str,
    bin_name: &str,
    rename_to: &str,
) -> Result<String, String> {
    check_bin_name(bin_name)?;
    let mut sql = format!(
        "FLASHBACK TABLE {} TO BEFORE DROP",
        dialect.table_ref(owner, bin_name)
    );
    let new = rename_to.trim();
    if !new.is_empty() {
        if new.starts_with(BIN_PREFIX) || new.chars().any(char::is_control) || new.len() > 128 {
            return Err("That is not a valid table name.".to_string());
        }
        sql.push_str(&format!(" RENAME TO {}", dialect.quote_ident(new)));
    }
    Ok(sql)
}

/// `PURGE TABLE "BIN$..."` or `PURGE INDEX "BIN$..."` for one recycle bin
/// object. `PURGE` takes the name without a schema.
pub fn purge_object_sql(
    dialect: &dyn Dialect,
    bin_name: &str,
    info: &RecycleInfo,
) -> Result<String, String> {
    check_bin_name(bin_name)?;
    if !info.can_purge_object() {
        return Err(format!(
            "A {} cannot be purged on its own; it goes when its table is purged.",
            info.kind.to_lowercase()
        ));
    }
    Ok(format!(
        "PURGE {} {}",
        info.kind.to_ascii_uppercase(),
        dialect.quote_ident(bin_name)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::OracleDialect;

    fn info(kind: &str) -> RecycleInfo {
        RecycleInfo {
            kind: kind.to_string(),
            operation: "DROP".to_string(),
            dropped: "2026-11-21 10:20:30".to_string(),
            can_undrop: true,
            can_purge: true,
        }
    }

    #[test]
    fn detail_round_trips() {
        let i = info("TABLE");
        assert_eq!(RecycleInfo::parse(&i.to_detail()), Some(i));
        let mut no = info("INDEX");
        no.can_undrop = false;
        assert_eq!(RecycleInfo::parse(&no.to_detail()), Some(no));
        assert_eq!(RecycleInfo::parse("PRIMARY KEY, ENABLED"), None);
    }

    #[test]
    fn droptime_is_made_readable() {
        assert_eq!(
            pretty_droptime("2026-11-21:10:20:30"),
            "2026-11-21 10:20:30"
        );
        assert_eq!(pretty_droptime("odd"), "odd");
    }

    #[test]
    fn flashback_quotes_the_system_name() {
        let sql = flashback_table_sql(&OracleDialect, "HR", "BIN$q7m5w==$0", "").unwrap();
        assert_eq!(
            sql,
            "FLASHBACK TABLE \"HR\".\"BIN$q7m5w==$0\" TO BEFORE DROP"
        );
    }

    #[test]
    fn flashback_with_rename_quotes_and_doubles() {
        let sql = flashback_table_sql(&OracleDialect, "HR", "BIN$a$0", " My\"T ").unwrap();
        assert_eq!(
            sql,
            "FLASHBACK TABLE \"HR\".\"BIN$a$0\" TO BEFORE DROP RENAME TO \"My\"\"T\""
        );
    }

    #[test]
    fn flashback_refuses_regular_tables_and_bad_names() {
        assert!(flashback_table_sql(&OracleDialect, "HR", "EMPLOYEES", "").is_err());
        assert!(flashback_table_sql(&OracleDialect, "HR", "BIN$", "").is_err());
        assert!(flashback_table_sql(&OracleDialect, "HR", "BIN$a$0", "BIN$x").is_err());
        assert!(flashback_table_sql(&OracleDialect, "HR", "BIN$a$0", "a\nb").is_err());
    }

    #[test]
    fn purge_object_by_type() {
        let t = purge_object_sql(&OracleDialect, "BIN$a==$0", &info("TABLE")).unwrap();
        assert_eq!(t, "PURGE TABLE \"BIN$a==$0\"");
        let i = purge_object_sql(&OracleDialect, "BIN$b==$0", &info("INDEX")).unwrap();
        assert_eq!(i, "PURGE INDEX \"BIN$b==$0\"");
        assert!(purge_object_sql(&OracleDialect, "BIN$c==$0", &info("TRIGGER")).is_err());
        assert!(purge_object_sql(&OracleDialect, "EMPLOYEES", &info("TABLE")).is_err());
        let mut locked = info("TABLE");
        locked.can_purge = false;
        assert!(purge_object_sql(&OracleDialect, "BIN$a==$0", &locked).is_err());
    }

    #[test]
    fn flashback_needs_a_table_that_can_be_undropped() {
        assert!(info("TABLE").can_flashback());
        assert!(!info("INDEX").can_flashback());
        let mut gone = info("TABLE");
        gone.can_undrop = false;
        assert!(!gone.can_flashback());
    }
}
