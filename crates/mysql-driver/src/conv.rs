//! Values and errors between MySQL's wire format and Kilat's model.

use mysql::consts::{ColumnFlags, ColumnType};
use mysql::{Column, Value as My};
use ora_core::{DbError, RowCollector, Value};

/// The message of a server error as the user should see it.
pub(crate) fn db_err(err: mysql::Error) -> DbError {
    match &err {
        mysql::Error::MySqlError(e) => {
            DbError::new(format!("MySQL: {} [{}]", e.message.trim(), e.code))
        }
        other => DbError::new(format!("MySQL: {other}")),
    }
}

/// SQL string literal, safe whatever `NO_BACKSLASH_ESCAPES` says is not
/// needed: the session never sets it, and backslash and quote are escaped.
pub(crate) fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

pub(crate) fn quote(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

/// A column the data is binary in (BLOB, BINARY, not text in a charset).
fn is_binary(col: &Column) -> bool {
    col.character_set() == 63
        && matches!(
            col.column_type(),
            ColumnType::MYSQL_TYPE_TINY_BLOB
                | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
                | ColumnType::MYSQL_TYPE_LONG_BLOB
                | ColumnType::MYSQL_TYPE_BLOB
                | ColumnType::MYSQL_TYPE_VAR_STRING
                | ColumnType::MYSQL_TYPE_STRING
                | ColumnType::MYSQL_TYPE_VARCHAR
        )
}

/// Type name shown in the grid header, in the words the catalog uses.
pub(crate) fn type_label(col: &Column) -> String {
    use ColumnType::*;
    let unsigned = col.flags().contains(ColumnFlags::UNSIGNED_FLAG);
    let name = match col.column_type() {
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if is_binary(col) {
                "VARBINARY"
            } else {
                "VARCHAR"
            }
        }
        MYSQL_TYPE_STRING => {
            if is_binary(col) {
                "BINARY"
            } else {
                "CHAR"
            }
        }
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if is_binary(col) {
                "BLOB"
            } else {
                "TEXT"
            }
        }
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        _ => "",
    };
    let integer = matches!(
        col.column_type(),
        MYSQL_TYPE_TINY
            | MYSQL_TYPE_SHORT
            | MYSQL_TYPE_INT24
            | MYSQL_TYPE_LONG
            | MYSQL_TYPE_LONGLONG
    );
    if unsigned && integer {
        format!("{name} UNSIGNED")
    } else {
        name.to_string()
    }
}

fn two(n: u8) -> String {
    format!("{n:02}")
}

/// `YYYY-MM-DD[ HH:MM:SS[.ffffff]]` from a binary-protocol date.
/// (year, month, day, hour, minute, second, microsecond)
type Stamp = (u16, u8, u8, u8, u8, u8, u32);

fn date_text(col: &Column, (y, mo, d, h, mi, s, us): Stamp) -> String {
    let date = format!("{y:04}-{}-{}", two(mo), two(d));
    if matches!(
        col.column_type(),
        ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE
    ) {
        return date;
    }
    let mut out = format!("{date} {}:{}:{}", two(h), two(mi), two(s));
    if us > 0 {
        out.push_str(&format!(".{us:06}"));
    }
    out
}

fn time_text(neg: bool, days: u32, h: u8, mi: u8, s: u8, us: u32) -> String {
    let hours = days * 24 + u32::from(h);
    let mut out = format!(
        "{}{hours:02}:{}:{}",
        if neg { "-" } else { "" },
        two(mi),
        two(s)
    );
    if us > 0 {
        out.push_str(&format!(".{us:06}"));
    }
    out
}

/// One cell. Text-protocol cells arrive as bytes whatever the type, so
/// numbers are read back from their text; exact numbers (`DECIMAL`) keep
/// their text.
pub(crate) fn cell(value: &My, col: &Column, rows: &RowCollector) -> Value {
    use ColumnType::*;
    match value {
        My::NULL => Value::Null,
        My::Int(i) => Value::Integer(i128::from(*i)),
        My::UInt(u) => Value::Integer(i128::from(*u)),
        My::Float(f) => Value::Float(f64::from(*f)),
        My::Double(d) => Value::Float(*d),
        My::Date(y, mo, d, h, mi, s, us) => {
            rows.text(&date_text(col, (*y, *mo, *d, *h, *mi, *s, *us)))
        }
        My::Time(neg, d, h, mi, s, us) => rows.text(&time_text(*neg, *d, *h, *mi, *s, *us)),
        My::Bytes(bytes) => match col.column_type() {
            MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG
            | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => std::str::from_utf8(bytes)
                .ok()
                .and_then(|t| t.parse::<i128>().ok())
                .map_or_else(|| rows.bytes(bytes), Value::Integer),
            MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => std::str::from_utf8(bytes)
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .map_or_else(|| rows.bytes(bytes), Value::Float),
            MYSQL_TYPE_BIT if bytes.len() <= 8 => {
                Value::Integer(bytes.iter().fold(0i128, |n, b| (n << 8) | i128::from(*b)))
            }
            _ if is_binary(col)
                || matches!(col.column_type(), MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY) =>
            {
                rows.bytes(bytes)
            }
            _ => match std::str::from_utf8(bytes) {
                Ok(text) => rows.text(text),
                Err(_) => rows.text(&String::from_utf8_lossy(bytes)),
            },
        },
    }
}

/// First word of a statement, lower-case, after comments and `(`.
pub(crate) fn first_word(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--").or_else(|| rest.strip_prefix('#')) {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix('(') {
            rest = r.trim_start();
        } else {
            break;
        }
    }
    rest.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether `sql` may take a `LIMIT` at its end: a plain `SELECT`/`WITH`
/// that has no `LIMIT` of its own and no locking or `INTO` clause.
pub(crate) fn can_append_limit(sql: &str) -> bool {
    if !matches!(first_word(sql).as_str(), "select" | "with") {
        return false;
    }
    let lower = sql.to_ascii_lowercase();
    !lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| {
            matches!(
                w,
                "limit" | "into" | "lock" | "procedure" | "update" | "outfile"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_identifiers_are_escaped() {
        assert_eq!(lit("a'b\\c"), "'a''b\\\\c'");
        assert_eq!(quote("we`ird"), "`we``ird`");
    }

    #[test]
    fn first_word_skips_comments_and_parentheses() {
        assert_eq!(first_word("  SELECT 1"), "select");
        assert_eq!(
            first_word("# hi\n/* x */ (with a as (select 1) select 1)"),
            "with"
        );
        assert_eq!(first_word("show tables"), "show");
        assert_eq!(first_word(""), "");
    }

    #[test]
    fn a_limit_is_added_only_to_plain_selects() {
        assert!(can_append_limit("SELECT * FROM t"));
        assert!(can_append_limit("with a as (select 1) select * from a"));
        assert!(!can_append_limit("SELECT * FROM t LIMIT 5"));
        assert!(!can_append_limit("select * from t for update"));
        assert!(!can_append_limit("select a into @x from t"));
        assert!(!can_append_limit("show tables"));
        assert!(can_append_limit("select limit_no, updated from t"));
    }

    #[test]
    fn times_and_dates_are_written_in_sql_form() {
        assert_eq!(time_text(true, 1, 2, 3, 4, 500), "-26:03:04.000500");
        assert_eq!(time_text(false, 0, 9, 5, 0, 0), "09:05:00");
    }
}
