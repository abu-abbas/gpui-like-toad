//! Pieces of literal writing that several dialects share.

/// A plain decimal number: digits, sign, point, exponent; nothing a
/// database could read as anything else.
pub(crate) fn looks_numeric(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty()
        && t.parse::<f64>().is_ok_and(f64::is_finite)
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
}

/// `'text'` with quotes doubled (standard SQL).
pub(crate) fn quote_text(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A boolean as text (`true`/`false`, `t`/`f`) as the digit a numeric
/// column keeps it as. Other engines write booleans that way.
pub(crate) fn bool_digit(text: &str) -> Option<&'static str> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" => Some("1"),
        "false" | "f" => Some("0"),
        _ => None,
    }
}

pub(crate) fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02X}")).collect()
}

pub(crate) fn no_nul(text: &str) -> Result<(), String> {
    if text.contains('\0') {
        Err("a text value with a NUL character cannot be copied".to_string())
    } else {
        Ok(())
    }
}

/// A date or timestamp written as `YYYY-MM-DD[ T]HH:MM:SS[.fraction]`.
/// Returns the text up to the seconds and the fraction digits, if any.
pub(crate) fn split_timestamp(text: &str) -> Option<(&str, Option<&str>)> {
    let t = text.trim();
    let b = t.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if !(b.len() >= 10
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10))
    {
        return None;
    }
    if b.len() == 10 {
        return Some((t, None));
    }
    if !(b.len() >= 19
        && matches!(b[10], b' ' | b'T')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19))
    {
        return None;
    }
    match b.len() {
        19 => Some((t, None)),
        n if n > 20 && b[19] == b'.' && digits(20..n.min(29)) && n <= 29 => {
            Some((&t[..19], Some(&t[20..])))
        }
        _ => None,
    }
}

/// Type name up to its parameters, upper-case: `varchar2(20)` -> `VARCHAR2`.
pub(crate) fn base_type(data_type: &str) -> String {
    data_type
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_uppercase()
}

/// The numbers inside a type's parentheses: `NUMBER(8,2)` -> `[8, 2]`.
pub(crate) fn type_args(data_type: &str) -> Vec<u32> {
    data_type
        .split_once('(')
        .map(|(_, rest)| rest.split(')').next().unwrap_or(""))
        .unwrap_or("")
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect()
}
