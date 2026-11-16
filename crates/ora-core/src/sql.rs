/// What kind of statement the editor text is, from its first keyword.
/// The UI uses this to decide between `query()` (rows back) and `execute()`
/// (rows affected). Leading `--` / `/* */` comments are skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Select,
    Dml,
    Ddl,
    PlSql,
    Other,
    Empty,
}

/// Strip what the thin driver rejects: trailing semicolons and a trailing
/// `/` terminator on its own line (PL/SQL). A `/` inside an expression such
/// as `SELECT 10/2` is left alone.
pub fn clean_statement(sql: &str) -> &str {
    let mut s = sql.trim();
    loop {
        s = s.trim_end();
        if let Some(rest) = s.strip_suffix(';') {
            s = rest;
            continue;
        }
        // Trailing "/" only when it is the whole last line.
        match s.rfind('\n') {
            Some(idx) if s[idx + 1..].trim() == "/" => {
                s = s[..idx].trim_end();
                continue;
            }
            None if s.trim() == "/" => {
                s = "";
            }
            _ => {}
        }
        break;
    }
    s.trim()
}

pub fn statement_kind(sql: &str) -> StatementKind {
    let text = skip_leading_comments(clean_statement(sql));
    let first_word: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match first_word.to_ascii_uppercase().as_str() {
        "" => StatementKind::Empty,
        "SELECT" | "WITH" => StatementKind::Select,
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" => StatementKind::Dml,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "GRANT" | "REVOKE" => StatementKind::Ddl,
        "DECLARE" | "BEGIN" => StatementKind::PlSql,
        _ => StatementKind::Other,
    }
}

/// Skip leading whitespace, `--` line comments, and `/* */` block comments.
/// Unterminated block comments stop the scan (the rest is treated as code).
fn skip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            match rest.find('\n') {
                Some(idx) => s = &rest[idx + 1..],
                None => return "",
            }
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(idx) => s = &rest[idx + 2..],
                None => return s,
            }
        } else {
            return s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_semicolons_and_slash_terminator() {
        assert_eq!(clean_statement("SELECT 1;"), "SELECT 1");
        assert_eq!(clean_statement("  SELECT 1;  \n"), "SELECT 1");
        assert_eq!(clean_statement("SELECT 1;;"), "SELECT 1");
        assert_eq!(clean_statement("BEGIN NULL; END;\n/"), "BEGIN NULL; END");
        assert_eq!(clean_statement("SELECT 10/2"), "SELECT 10/2");
        assert_eq!(clean_statement("SELECT 10 / 2;"), "SELECT 10 / 2");
        assert_eq!(clean_statement(""), "");
    }

    #[test]
    fn kind_from_first_keyword_skipping_comments() {
        assert_eq!(statement_kind("SELECT * FROM dual"), StatementKind::Select);
        assert_eq!(
            statement_kind("with x as (select 1 from dual) select * from x"),
            StatementKind::Select
        );
        assert_eq!(
            statement_kind("-- hi\n/*+ hint */ SELECT 1 FROM dual"),
            StatementKind::Select
        );
        assert_eq!(statement_kind("  update t set x = 1"), StatementKind::Dml);
        assert_eq!(
            statement_kind("create table t (a number)"),
            StatementKind::Ddl
        );
        assert_eq!(
            statement_kind("DECLARE x NUMBER; BEGIN NULL; END;"),
            StatementKind::PlSql
        );
        assert_eq!(
            statement_kind("EXPLAIN PLAN FOR SELECT 1 FROM dual"),
            StatementKind::Other
        );
        assert_eq!(statement_kind("  -- only a comment"), StatementKind::Empty);
        assert_eq!(statement_kind(""), StatementKind::Empty);
    }
}
