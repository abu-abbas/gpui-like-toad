//! Values and errors between PostgreSQL's text format and Kilat's model.

use bytes::BytesMut;
use ora_core::{DbError, RowCollector, Value};
use postgres::types::{to_sql_checked, Format, IsNull, ToSql, Type};

/// A bind value sent as text. The server parses it as the type it
/// expects at that place, like an untyped literal, so `WHERE id = :1`
/// works for a number column with the text "42".
#[derive(Debug)]
pub(crate) struct TextParam<'a>(pub &'a str);

impl ToSql for TextParam<'_> {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

/// `E'...'` literal that is safe whatever `standard_conforming_strings` is.
pub(crate) fn lit(text: &str) -> String {
    format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

/// The message of a server error as the user should see it.
pub(crate) fn db_err(err: postgres::Error) -> DbError {
    if let Some(e) = err.as_db_error() {
        let mut text = format!("PostgreSQL: {}", e.message());
        if let Some(detail) = e.detail() {
            text.push_str(&format!("\nDetail: {detail}"));
        }
        if let Some(hint) = e.hint() {
            text.push_str(&format!("\nHint: {hint}"));
        }
        text.push_str(&format!(" [{}]", e.code().code()));
        return DbError::new(text);
    }
    let mut text = format!("PostgreSQL: {err}");
    let mut source = std::error::Error::source(&err);
    while let Some(inner) = source {
        text.push_str(&format!(": {inner}"));
        source = inner.source();
    }
    DbError::new(text)
}

/// Type name shown in the grid header, in the words the catalog uses.
pub(crate) fn type_label(ty: &Type) -> String {
    match ty.name() {
        "int2" => "SMALLINT".to_string(),
        "int4" => "INTEGER".to_string(),
        "int8" => "BIGINT".to_string(),
        "float4" => "REAL".to_string(),
        "float8" => "DOUBLE PRECISION".to_string(),
        "bool" => "BOOLEAN".to_string(),
        "bpchar" => "CHAR".to_string(),
        "timestamptz" => "TIMESTAMPTZ".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// One cell from the server's text form. Numbers keep their exact text
/// unless they are plain integers or floats; `bytea` becomes bytes.
pub(crate) fn cell(text: Option<&str>, ty: &Type, rows: &RowCollector) -> Value {
    let Some(text) = text else {
        return Value::Null;
    };
    match *ty {
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => text
            .parse::<i128>()
            .map_or_else(|_| rows.text(text), Value::Integer),
        Type::FLOAT4 | Type::FLOAT8 => text
            .parse::<f64>()
            .map_or_else(|_| rows.text(text), Value::Float),
        Type::BOOL => rows.text(if text == "t" { "true" } else { "false" }),
        Type::BYTEA => match decode_hex_bytea(text) {
            Some(bytes) => rows.bytes(&bytes),
            None => rows.text(text),
        },
        _ => rows.text(text),
    }
}

/// `\xDEADBEEF` (the default `bytea_output`).
fn decode_hex_bytea(text: &str) -> Option<Vec<u8>> {
    let hex = text.strip_prefix("\\x")?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// First word of a statement, lower-case, after comments and `(`.
pub(crate) fn first_word(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ora_core::FetchLimits;

    #[test]
    fn literals_are_escaped_for_any_string_setting() {
        assert_eq!(lit("a'b\\c"), "E'a''b\\\\c'");
        assert_eq!(lit("public"), "E'public'");
    }

    #[test]
    fn first_word_skips_comments_and_parentheses() {
        assert_eq!(first_word("  SELECT 1"), "select");
        assert_eq!(
            first_word("-- hi\n/* x */ (with a as (select 1) select 1)"),
            "with"
        );
        assert_eq!(first_word("show server_version"), "show");
        assert_eq!(first_word(""), "");
    }

    #[test]
    fn cells_follow_their_type() {
        let rows = RowCollector::new(FetchLimits::rows(10));
        assert_eq!(cell(None, &Type::INT4, &rows), Value::Null);
        assert_eq!(cell(Some("42"), &Type::INT8, &rows), Value::Integer(42));
        assert_eq!(cell(Some("1.5"), &Type::FLOAT8, &rows), Value::Float(1.5));
        assert_eq!(
            cell(Some("12345678901234567890.123"), &Type::NUMERIC, &rows),
            Value::Text("12345678901234567890.123".into())
        );
        assert_eq!(
            cell(Some("t"), &Type::BOOL, &rows),
            Value::Text("true".into())
        );
        assert_eq!(
            cell(Some("\\xdead"), &Type::BYTEA, &rows),
            Value::Bytes(vec![0xde, 0xad])
        );
        assert_eq!(
            cell(Some("\\xzz"), &Type::BYTEA, &rows),
            Value::Text("\\xzz".into())
        );
    }
}
