//! Positional bind variables.
//!
//! Kilat always writes `:1`, `:2`, ... in SQL it builds. Oracle reads them
//! as they are; a driver for an engine with another spelling converts the
//! statement with [`rewrite`] right before it runs.

/// How an engine writes a positional bind variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindStyle {
    /// `:1` (Oracle, SQLite).
    Colon,
    /// `$1` (PostgreSQL).
    Dollar,
    /// `?`, bound in order of appearance (MySQL).
    Question,
    /// `@P1` (SQL Server, as the Tiberius driver names its parameters).
    AtP,
}

/// A statement rewritten for [`BindStyle`], with the bind values in the
/// order the engine takes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    pub sql: String,
    pub binds: Vec<String>,
}

/// Rewrite the `:n` placeholders of `sql`. Text inside `'...'`, `"..."`,
/// `` `...` ``, `-- ...` and `/* ... */` is left alone, and so is `::`
/// (a PostgreSQL cast). `binds[n - 1]` is the value of `:n`; a placeholder
/// without a value is an error. For [`BindStyle::Question`] a value used
/// twice is repeated, and the result lists values in order of appearance.
pub fn rewrite(sql: &str, binds: &[String], style: BindStyle) -> Result<Rewritten, String> {
    if style == BindStyle::Colon {
        return Ok(Rewritten {
            sql: sql.to_string(),
            binds: binds.to_vec(),
        });
    }
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut ordered = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' if style == BindStyle::AtP => {
                // A bracket-quoted identifier; `]]` stands for one `]`.
                let end = skip_bracketed(&chars, i);
                out.extend(&chars[i..end]);
                i = end;
            }
            '\'' | '"' | '`' => {
                let end = skip_quoted(&chars, i);
                out.extend(&chars[i..end]);
                i = end;
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                let end = chars[i..]
                    .iter()
                    .position(|&x| x == '\n')
                    .map_or(chars.len(), |p| i + p);
                out.extend(&chars[i..end]);
                i = end;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let mut end = i + 2;
                while end < chars.len() && !(chars[end] == '*' && chars.get(end + 1) == Some(&'/'))
                {
                    end += 1;
                }
                let end = (end + 2).min(chars.len());
                out.extend(&chars[i..end]);
                i = end;
            }
            ':' if chars.get(i + 1) == Some(&':') => {
                out.push_str("::");
                i += 2;
            }
            ':' if chars.get(i + 1).is_some_and(char::is_ascii_digit) => {
                let mut end = i + 1;
                while chars.get(end).is_some_and(char::is_ascii_digit) {
                    end += 1;
                }
                let n: usize = chars[i + 1..end]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .map_err(|_| "bind number out of range".to_string())?;
                let value = n
                    .checked_sub(1)
                    .and_then(|ix| binds.get(ix))
                    .ok_or_else(|| format!("bind :{n} has no value"))?;
                match style {
                    BindStyle::Dollar => out.push_str(&format!("${n}")),
                    BindStyle::AtP => out.push_str(&format!("@P{n}")),
                    BindStyle::Question => {
                        out.push('?');
                        ordered.push(value.clone());
                    }
                    BindStyle::Colon => unreachable!(),
                }
                i = end;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(Rewritten {
        sql: out,
        binds: if style == BindStyle::Question {
            ordered
        } else {
            binds.to_vec()
        },
    })
}

/// Index just past the `[...]` run that starts at `start`.
fn skip_bracketed(chars: &[char], start: usize) -> usize {
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == ']' {
            if chars.get(i + 1) == Some(&']') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

/// Index just past the quoted run that starts at `start` (quotes doubled
/// inside are part of it).
fn skip_quoted(chars: &[char], start: usize) -> usize {
    let quote = chars[start];
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == quote {
            if chars.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dollar_style_keeps_numbers() {
        let r = rewrite(
            "UPDATE t SET a = :1 WHERE b = :2 AND c = :1",
            &v(&["x", "y"]),
            BindStyle::Dollar,
        )
        .unwrap();
        assert_eq!(r.sql, "UPDATE t SET a = $1 WHERE b = $2 AND c = $1");
        assert_eq!(r.binds, v(&["x", "y"]));
    }

    #[test]
    fn question_style_orders_and_repeats_values() {
        let r = rewrite("SELECT :2, :1, :2", &v(&["a", "b"]), BindStyle::Question).unwrap();
        assert_eq!(r.sql, "SELECT ?, ?, ?");
        assert_eq!(r.binds, v(&["b", "a", "b"]));
    }

    #[test]
    fn quotes_comments_and_casts_are_left_alone() {
        let sql =
            "SELECT ':1', \":2\", `:3`, x::int -- :1\n, :1 /* :1 */ FROM t WHERE s = 'it''s :1'";
        let r = rewrite(sql, &v(&["z"]), BindStyle::Dollar).unwrap();
        assert_eq!(
            r.sql,
            "SELECT ':1', \":2\", `:3`, x::int -- :1\n, $1 /* :1 */ FROM t WHERE s = 'it''s :1'"
        );
    }

    #[test]
    fn missing_value_is_an_error_and_colon_is_untouched() {
        assert!(rewrite("SELECT :2", &v(&["a"]), BindStyle::Dollar).is_err());
        assert!(rewrite("SELECT :0", &v(&["a"]), BindStyle::Question).is_err());
        let r = rewrite("SELECT :1", &v(&["a"]), BindStyle::Colon).unwrap();
        assert_eq!(r.sql, "SELECT :1");
    }
}
