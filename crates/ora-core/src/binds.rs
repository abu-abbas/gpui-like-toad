//! Bind variables typed in the SQL editor, as Toad and SQL Developer
//! treat them: `:name` (or `: name`) in a statement asks the user for a
//! value when it runs.
//!
//! [`find_binds`] lists the placeholders (skipping string literals,
//! quoted identifiers, comments, `::` casts and `:=` assignments, and
//! `:new.` / `:old.` in trigger bodies). [`substitute`] then writes the
//! values the user typed into the SQL as literals, so the rest of the app
//! (query, execute, script, explain, dry-run) keeps running plain SQL and
//! needs no bind plumbing of its own. Oracle only: other engines use other
//! placeholder syntax (and `:` is a slice operator in Postgres).

use std::collections::HashMap;

use crate::sql::{script_spans, statement_kind, StatementKind};

/// One placeholder in the text: `name` as written (no colon), and the byte
/// range of the whole placeholder, colon (and any space after it) included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindRef {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

impl BindRef {
    /// Oracle bind names are case-insensitive.
    pub fn key(&self) -> String {
        bind_key(&self.name)
    }
}

pub fn bind_key(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// How a value is written into the SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindKind {
    #[default]
    Text,
    Number,
    Date,
}

impl BindKind {
    pub const ALL: [BindKind; 3] = [BindKind::Text, BindKind::Number, BindKind::Date];

    pub fn label(self) -> &'static str {
        match self {
            BindKind::Text => "String",
            BindKind::Number => "Number",
            BindKind::Date => "Date",
        }
    }
}

/// A value for one bind; `value: None` is NULL.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BindValue {
    pub kind: BindKind,
    pub value: Option<String>,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '#')
}

/// Placeholders in `sql`, in order of appearance. DDL statements (a
/// `CREATE TRIGGER` body is full of `:new`) are left alone.
pub fn find_binds(sql: &str) -> Vec<BindRef> {
    let mut found = Vec::new();
    let spans = script_spans(sql);
    if spans.is_empty() {
        return found;
    }
    for (a, b) in spans {
        let part = &sql[a..b];
        if matches!(statement_kind(part), StatementKind::Ddl) {
            continue;
        }
        for mut r in scan(part) {
            r.start += a;
            r.end += a;
            found.push(r);
        }
    }
    found
}

fn scan(sql: &str) -> Vec<BindRef> {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|(_, c)| *c);
    let pos = |i: usize| chars.get(i).map_or(sql.len(), |(b, _)| *b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        match c {
            '-' if at(i + 1) == Some('-') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
            }
            '/' if at(i + 1) == Some('*') => {
                i += 2;
                while i < chars.len() && !(chars[i].1 == '*' && at(i + 1) == Some('/')) {
                    i += 1;
                }
                i += 2;
            }
            '\'' => i = skip_quoted(&chars, i, '\''),
            '"' => i = skip_quoted(&chars, i, '"'),
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && is_ident(chars[i].1) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().map(|(_, c)| *c).collect();
                if (word.eq_ignore_ascii_case("q") || word.eq_ignore_ascii_case("nq"))
                    && at(i) == Some('\'')
                    && at(i + 1).is_some()
                {
                    let close = match at(i + 1).unwrap() {
                        '[' => ']',
                        '(' => ')',
                        '{' => '}',
                        '<' => '>',
                        d => d,
                    };
                    i += 2;
                    while i < chars.len() && !(chars[i].1 == close && at(i + 1) == Some('\'')) {
                        i += 1;
                    }
                    i += 2;
                }
            }
            ':' => {
                if matches!(at(i + 1), Some(':' | '=')) {
                    i += 2;
                    continue;
                }
                let mut j = i + 1;
                let mut spaced = false;
                while matches!(at(j), Some(' ' | '\t')) {
                    j += 1;
                    spaced = true;
                }
                let (name, end) = match at(j) {
                    Some('"') => {
                        let close = skip_quoted(&chars, j, '"');
                        let name: String = chars[j..close.min(chars.len())]
                            .iter()
                            .map(|(_, c)| *c)
                            .collect();
                        (name, close)
                    }
                    Some(ch) if ch.is_ascii_alphabetic() || (ch.is_ascii_digit() && !spaced) => {
                        let mut k = j;
                        while k < chars.len() && is_ident(chars[k].1) {
                            k += 1;
                        }
                        (chars[j..k].iter().map(|(_, c)| *c).collect(), k)
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let keyword = spaced
                    && [
                        "FROM", "WHERE", "AND", "OR", "THEN", "ELSE", "END", "WHEN", "SELECT",
                    ]
                    .iter()
                    .any(|w| name.eq_ignore_ascii_case(w));
                if keyword {
                    i += 1;
                    continue;
                }
                let trigger_ref = at(end) == Some('.')
                    && ["NEW", "OLD", "PARENT"]
                        .iter()
                        .any(|w| name.eq_ignore_ascii_case(w));
                if !trigger_ref {
                    out.push(BindRef {
                        name,
                        start: pos(i),
                        end: pos(end),
                    });
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Index just past the closing quote of the literal opening at `i`
/// (a doubled quote stays inside).
fn skip_quoted(chars: &[(usize, char)], i: usize, quote: char) -> usize {
    let mut j = i + 1;
    while j < chars.len() {
        if chars[j].1 == quote {
            if chars.get(j + 1).is_some_and(|(_, c)| *c == quote) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    j
}

/// The distinct bind names (first spelling kept), in order of first use.
pub fn distinct_names(refs: &[BindRef]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    refs.iter()
        .filter(|r| seen.insert(r.key()))
        .map(|r| r.name.clone())
        .collect()
}

/// `sql` with every placeholder replaced by its value as a literal.
/// `values` is keyed by [`bind_key`]. Errors name the bind whose value is
/// not a valid number or date.
pub fn substitute(sql: &str, values: &HashMap<String, BindValue>) -> Result<String, String> {
    let refs = find_binds(sql);
    let mut out = sql.to_string();
    for r in refs.iter().rev() {
        let value = values.get(&r.key()).cloned().unwrap_or_default();
        let literal = literal(&r.name, &value)?;
        out.replace_range(r.start..r.end, &literal);
    }
    Ok(out)
}

fn literal(name: &str, v: &BindValue) -> Result<String, String> {
    let Some(text) = v.value.as_deref() else {
        return Ok("NULL".to_string());
    };
    match v.kind {
        BindKind::Text => Ok(format!("'{}'", text.replace('\'', "''"))),
        BindKind::Number => {
            let t = text.trim();
            if t.is_empty() {
                return Ok("NULL".to_string());
            }
            if valid_number(t) {
                // Parenthesised so `- :n` or `a -:n` never turns into a comment.
                Ok(if t.starts_with(['-', '+']) {
                    format!("({t})")
                } else {
                    t.to_string()
                })
            } else {
                Err(format!("{name}: \"{t}\" is not a number."))
            }
        }
        BindKind::Date => {
            let t = text.trim();
            if t.is_empty() {
                return Ok("NULL".to_string());
            }
            parse_date(t)
                .map(|d| format!("TO_DATE('{d}', 'YYYY-MM-DD HH24:MI:SS')"))
                .ok_or_else(|| {
                    format!("{name}: \"{t}\" is not a date. Use YYYY-MM-DD or YYYY-MM-DD HH:MI:SS.")
                })
        }
    }
}

fn valid_number(t: &str) -> bool {
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(p) => (&t[..p], Some(&t[p + 1..])),
        None => (t, None),
    };
    let mut parts = mant.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits_ok = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let mant_ok = digits_ok(int)
        && frac.is_none_or(digits_ok)
        && (!int.is_empty() || frac.is_some_and(|f| !f.is_empty()));
    let exp_ok = exp.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && digits_ok(e)
    });
    mant_ok && exp_ok
}

/// `YYYY-MM-DD[ T]HH:MI[:SS]` (also `/` as the date separator) to the
/// canonical `YYYY-MM-DD HH24:MI:SS` text.
fn parse_date(t: &str) -> Option<String> {
    let (date, time) = match t.split_once([' ', 'T']) {
        Some((d, tm)) => (d, tm.trim()),
        None => (t, ""),
    };
    let nums = |s: &str, sep: char| -> Option<Vec<u32>> {
        s.split(sep)
            .map(|p| {
                (!p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit()))
                    .then(|| p.parse().ok())
                    .flatten()
            })
            .collect()
    };
    let d = nums(&date.replace('/', "-"), '-')?;
    let tm = if time.is_empty() {
        vec![0, 0, 0]
    } else {
        let mut v = nums(time, ':')?;
        if !(2..=3).contains(&v.len()) {
            return None;
        }
        v.resize(3, 0);
        v
    };
    if d.len() != 3 || d[0] < 1 || !(1..=12).contains(&d[1]) || !(1..=31).contains(&d[2]) {
        return None;
    }
    if tm[0] > 23 || tm[1] > 59 || tm[2] > 59 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        d[0], d[1], d[2], tm[0], tm[1], tm[2]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str) -> Vec<String> {
        find_binds(sql).into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn finds_plain_and_spaced_binds() {
        assert_eq!(names("select * from t where id = :id"), ["id"]);
        assert_eq!(names("where i_id = : id_usulan"), ["id_usulan"]);
        assert_eq!(names("where a = :a and b = :b or a2 = :A"), ["a", "b", "A"]);
        assert_eq!(names("where a = :1 and b = :2"), ["1", "2"]);
        assert_eq!(names("where a = :\"My Bind\""), ["\"My Bind\""]);
    }

    #[test]
    fn ignores_literals_comments_casts_and_assignments() {
        assert!(names("select 'a :b c' from dual").is_empty());
        assert!(names("select 'it''s :x' from dual").is_empty());
        assert!(names("select q'[a :b]' from dual").is_empty());
        assert!(names("select \"A:b\" from dual").is_empty());
        assert!(names("select 1 -- :x\nfrom dual").is_empty());
        assert!(names("select /* :x */ 1 from dual").is_empty());
        assert!(names("select x::int from t").is_empty());
        assert!(names("declare v number; begin v := 1; end;").is_empty());
        assert!(names("select to_char(d, 'HH24:MI') from t").is_empty());
        assert!(names("select a ? : from t").is_empty());
        assert_eq!(names("select '1' from t where x = :x -- :y"), ["x"]);
    }

    #[test]
    fn leaves_ddl_and_trigger_refs_alone() {
        assert!(names(
            "create or replace trigger t before insert on x for each row begin :new.a := 1; end;"
        )
        .is_empty());
        assert!(names("begin if :new.a > 1 then null; end if; end;").is_empty());
        assert_eq!(names("begin :out := f(:inp); end;"), ["out", "inp"]);
    }

    #[test]
    fn scans_every_statement_of_a_script() {
        assert_eq!(
            names("select :a from dual;\nupdate t set x = :b;"),
            ["a", "b"]
        );
    }

    #[test]
    fn distinct_keeps_first_spelling() {
        let refs = find_binds("where a = :Id or b = :ID or c = :other");
        assert_eq!(distinct_names(&refs), ["Id", "other"]);
    }

    fn vals(items: &[(&str, BindKind, Option<&str>)]) -> HashMap<String, BindValue> {
        items
            .iter()
            .map(|(n, k, v)| {
                (
                    bind_key(n),
                    BindValue {
                        kind: *k,
                        value: v.map(str::to_string),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn substitutes_each_kind() {
        let v = vals(&[
            ("id", BindKind::Number, Some("42")),
            ("name", BindKind::Text, Some("O'Neil")),
            ("d", BindKind::Date, Some("2026-11-27")),
            ("n", BindKind::Text, None),
        ]);
        let sql =
            "select * from t where id = : id and nm = :NAME and d > :d and x = :n and id2 = :id";
        assert_eq!(
            substitute(sql, &v).unwrap(),
            "select * from t where id = 42 and nm = 'O''Neil' and d > TO_DATE('2026-11-27 00:00:00', 'YYYY-MM-DD HH24:MI:SS') and x = NULL and id2 = 42"
        );
    }

    #[test]
    fn rejects_bad_numbers_and_dates() {
        let n = vals(&[("a", BindKind::Number, Some("1; drop table t"))]);
        assert!(substitute("select :a from dual", &n).is_err());
        let d = vals(&[("a", BindKind::Date, Some("27-11-2026x"))]);
        assert!(substitute("select :a from dual", &d).is_err());
        let ok = vals(&[("a", BindKind::Number, Some("-1.5e3"))]);
        assert_eq!(
            substitute("select :a from dual", &ok).unwrap(),
            "select (-1.5e3) from dual"
        );
        let t = vals(&[("a", BindKind::Date, Some("2026-11-27 13:05"))]);
        assert!(substitute(":a", &t)
            .unwrap()
            .contains("2026-11-27 13:05:00"));
    }

    #[test]
    fn untouched_without_binds() {
        let sql = "select 'a:b' from dual";
        assert_eq!(substitute(sql, &HashMap::new()).unwrap(), sql);
    }
}
