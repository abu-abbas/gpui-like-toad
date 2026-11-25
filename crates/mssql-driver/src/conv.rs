//! Values and errors between SQL Server's wire format and Kilat's model.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use ora_core::{DbError, RowCollector, Value};
use tiberius::{error::Error, ColumnData, ColumnType, FromSql};

/// The message of a failure as the user should see it: the server's own
/// text and number, or what went wrong on the way.
pub(crate) fn db_err(err: Error) -> DbError {
    match &err {
        Error::Server(e) => {
            DbError::new(format!("SQL Server: {} [{}]", e.message().trim(), e.code()))
        }
        other => DbError::new(format!("SQL Server: {other}")),
    }
}

/// A failed connect, with a pointer to the connection option that allows
/// it when TLS stopped it: an older TLS version than 1.2, or a certificate
/// the system does not trust (SQL Server's default self-signed one).
pub(crate) fn connect_err(err: Error, legacy_tls: bool, trust_cert: bool) -> DbError {
    let mut e = db_err(err);
    // OpenSSL's words for the server choosing an older version, or
    // refusing the client's minimum with an alert.
    if !legacy_tls
        && (e.0.contains("unsupported protocol") || e.0.contains("alert protocol version"))
    {
        e.0.push_str(LEGACY_TLS_HINT);
    } else if !trust_cert && e.0.contains("certificate verify failed") {
        e.0.push_str(TRUST_CERT_HINT);
    }
    e
}

const LEGACY_TLS_HINT: &str = " (The server only offers TLS 1.0 or 1.1. \
     For a server you trust, tick Allow legacy TLS (1.0/1.1) in the connection's Advanced settings.)";

const TRUST_CERT_HINT: &str = " (The server's certificate is not trusted by this computer. \
     For a server you know, such as one with a self-signed certificate, tick Trust server certificate \
     in the connection's Advanced settings.)";

/// `'text'` as a T-SQL literal for catalog queries (doubled quotes).
pub(crate) fn lit(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

/// `[name]`.
pub(crate) fn quote(ident: &str) -> String {
    format!("[{}]", ident.replace(']', "]]"))
}

/// Type name shown in the grid header.
pub(crate) fn type_label(ty: ColumnType) -> &'static str {
    use ColumnType::*;
    match ty {
        Null => "NULL",
        Bit | Bitn => "BIT",
        Int1 => "TINYINT",
        Int2 => "SMALLINT",
        Int4 => "INT",
        Int8 => "BIGINT",
        Intn => "INT",
        Datetime4 => "SMALLDATETIME",
        Datetime | Datetimen => "DATETIME",
        Float4 => "REAL",
        Float8 | Floatn => "FLOAT",
        Money => "MONEY",
        Money4 => "SMALLMONEY",
        Guid => "UNIQUEIDENTIFIER",
        Decimaln | Numericn => "DECIMAL",
        Daten => "DATE",
        Timen => "TIME",
        Datetime2 => "DATETIME2",
        DatetimeOffsetn => "DATETIMEOFFSET",
        BigVarBin => "VARBINARY",
        BigBinary => "BINARY",
        BigVarChar => "VARCHAR",
        BigChar => "CHAR",
        NVarchar => "NVARCHAR",
        NChar => "NCHAR",
        Xml => "XML",
        Udt => "UDT",
        Text => "TEXT",
        NText => "NTEXT",
        Image => "IMAGE",
        SSVariant => "SQL_VARIANT",
    }
}

fn decimal_text(value: i128, scale: u8) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let body = if scale == 0 {
        digits
    } else {
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int, frac) = padded.split_at(padded.len() - scale);
        format!("{int}.{frac}")
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

fn float_text(f: f64) -> Value {
    Value::Float(f)
}

/// One cell, with text and bytes cut at the fetch's per-cell limit.
pub(crate) fn cell(data: ColumnData<'static>, rows: &RowCollector) -> Value {
    fn dt<T: ToString>(v: Option<T>) -> Value {
        v.map_or(Value::Null, |v| Value::Text(v.to_string()))
    }
    let text = |v: Option<String>| v.map_or(Value::Null, Value::Text);
    match &data {
        ColumnData::U8(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I16(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I32(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        ColumnData::I64(v) => v.map_or(Value::Null, |v| Value::Integer(v.into())),
        // A single-precision value shown as the shortest text that reads
        // back the same, not as the double it widens to.
        ColumnData::F32(v) => v.map_or(Value::Null, |v| {
            float_text(v.to_string().parse::<f64>().unwrap_or(f64::from(v)))
        }),
        ColumnData::F64(v) => v.map_or(Value::Null, float_text),
        ColumnData::Bit(v) => v.map_or(Value::Null, |v| Value::Integer(i128::from(v))),
        ColumnData::String(v) => v.as_deref().map_or(Value::Null, |s| rows.text(s)),
        ColumnData::Guid(v) => text(v.map(|g| g.to_string().to_uppercase())),
        ColumnData::Binary(v) => v.as_deref().map_or(Value::Null, |b| rows.bytes(b)),
        ColumnData::Numeric(v) => text(v.map(|n| decimal_text(n.value(), n.scale()))),
        ColumnData::Xml(v) => v
            .as_deref()
            .map_or(Value::Null, |x| rows.text(&x.clone().into_string())),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            dt(NaiveDateTime::from_sql(&data)
                .ok()
                .flatten()
                .map(|v| v.format("%Y-%m-%d %H:%M:%S%.f").to_string()))
        }
        ColumnData::Date(_) => dt(NaiveDate::from_sql(&data).ok().flatten()),
        ColumnData::Time(_) => dt(NaiveTime::from_sql(&data)
            .ok()
            .flatten()
            .map(|v| v.format("%H:%M:%S%.f").to_string())),
        ColumnData::DateTimeOffset(_) => {
            dt(chrono::DateTime::<chrono::FixedOffset>::from_sql(&data)
                .ok()
                .flatten()
                .map(|v| v.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string()))
        }
    }
}

/// First word of a statement, lower-case.
pub(crate) fn first_word(sql: &str) -> String {
    sql.trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_keep_their_scale() {
        assert_eq!(decimal_text(12345, 2), "123.45");
        assert_eq!(decimal_text(-5, 3), "-0.005");
        assert_eq!(decimal_text(7, 0), "7");
        assert_eq!(decimal_text(0, 2), "0.00");
    }

    #[test]
    fn identifiers_and_literals_are_escaped() {
        assert_eq!(quote("a]b"), "[a]]b]");
        assert_eq!(lit("o'k"), "N'o''k'");
    }
}
