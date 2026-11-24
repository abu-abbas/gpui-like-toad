//! Reads just enough of an `UPDATE` or `DELETE` to find its table and its
//! `WHERE` clause. Anything it does not understand is refused, so Dry-run
//! never guesses which rows a statement touches.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Update,
    Delete,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Update => "UPDATE",
            Kind::Delete => "DELETE",
        }
    }
}

/// An identifier as written: bare words stay as the user typed them
/// (Oracle upper-cases them), quoted ones keep their quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident(pub String);

impl Ident {
    /// The name Oracle stores: bare words upper-case, quoted words as is.
    pub fn name(&self) -> String {
        match self.0.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Some(inner) => inner.replace("\"\"", "\""),
            None => self.0.to_ascii_uppercase(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub kind: Kind,
    pub owner: Option<Ident>,
    pub table: Ident,
    pub alias: Option<Ident>,
    /// Text after `WHERE`, as written; `None` when the statement has none.
    pub where_clause: Option<String>,
}

impl Parsed {
    /// `SCHEMA.TABLE` exactly as the statement wrote it (no alias).
    pub fn table_ref(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{}.{}", owner.0, self.table.0),
            None => self.table.0.clone(),
        }
    }

    /// What columns of the table are qualified with: the alias if there is
    /// one, otherwise the table name.
    pub fn qualifier(&self) -> &str {
        self.alias.as_ref().map_or(self.table.0.as_str(), |a| &a.0)
    }

    /// `table alias` as it goes after `FROM`.
    pub fn table_with_alias(&self) -> String {
        match &self.alias {
            Some(alias) => format!("{} {}", self.table_ref(), alias.0),
            None => self.table_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A bare word or a quoted identifier, with its text as written.
    Word(String),
    Quoted(String),
    Text,
    Number,
    Punct(char),
}

struct Token {
    tok: Tok,
    end: usize,
}

fn lex(sql: &str) -> Result<Vec<Token>, String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let rest = &sql[i + 2..];
            let end = rest.find("*/").ok_or("A comment is not closed.")?;
            i += 2 + end + 2;
        } else if c == b'\'' {
            i += 1;
            loop {
                match bytes.get(i) {
                    None => return Err("A text literal is not closed.".into()),
                    Some(b'\'') if bytes.get(i + 1) == Some(&b'\'') => i += 2,
                    Some(b'\'') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            out.push(Token {
                tok: Tok::Text,
                end: i,
            });
        } else if c == b'"' {
            i += 1;
            loop {
                match bytes.get(i) {
                    None => return Err("A quoted name is not closed.".into()),
                    Some(b'"') if bytes.get(i + 1) == Some(&b'"') => i += 2,
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            out.push(Token {
                tok: Tok::Quoted(sql[start..i].to_string()),
                end: i,
            });
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$' | b'#'))
            {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Word(sql[start..i].to_string()),
                end: i,
            });
        } else if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Number,
                end: i,
            });
        } else if c.is_ascii() {
            i += 1;
            out.push(Token {
                tok: Tok::Punct(c as char),
                end: i,
            });
        } else {
            // Non-ASCII outside of text and quoted names (a letter in a bare
            // name, for instance): Oracle allows it, we do not guess.
            return Err("Unsupported character outside of a quoted name or a text.".into());
        }
    }
    Ok(out)
}

fn is_word(token: Option<&Token>, word: &str) -> bool {
    matches!(token, Some(Token { tok: Tok::Word(w), .. }) if w.eq_ignore_ascii_case(word))
}

fn ident(token: Option<&Token>) -> Option<Ident> {
    match token {
        Some(Token {
            tok: Tok::Word(w) | Tok::Quoted(w),
            ..
        }) => Some(Ident(w.clone())),
        _ => None,
    }
}

/// Words that cannot be a table alias in the statements we read.
const RESERVED: &[&str] = &[
    "SET",
    "WHERE",
    "RETURNING",
    "RETURN",
    "LOG",
    "PARTITION",
    "SUBPARTITION",
    "SAMPLE",
    "VALUES",
    "USING",
];

fn refuse(kind: &str) -> String {
    format!(
        "Dry-run reads UPDATE and DELETE of one table. {kind} is not supported yet; \
         run it for real if it is what you want."
    )
}

pub fn parse(sql: &str) -> Result<Parsed, String> {
    let sql = ora_core::sql::clean_statement(sql);
    let tokens = lex(sql)?;
    if tokens.iter().any(|t| t.tok == Tok::Punct(';')) {
        return Err("Dry-run reads one statement at a time.".into());
    }
    let mut at = 0;
    let first = tokens.first().ok_or("There is no statement to run.")?;
    let kind = match &first.tok {
        Tok::Word(w) if w.eq_ignore_ascii_case("UPDATE") => Kind::Update,
        Tok::Word(w) if w.eq_ignore_ascii_case("DELETE") => Kind::Delete,
        Tok::Word(w) if w.eq_ignore_ascii_case("MERGE") => return Err(refuse("MERGE")),
        Tok::Word(w) if w.eq_ignore_ascii_case("INSERT") => return Err(refuse("INSERT")),
        _ => return Err("Dry-run reads UPDATE and DELETE statements.".into()),
    };
    at += 1;
    if kind == Kind::Delete && is_word(tokens.get(at), "FROM") {
        at += 1;
    }
    let first_name = ident(tokens.get(at)).ok_or_else(|| {
        if matches!(tokens.get(at).map(|t| &t.tok), Some(Tok::Punct('('))) {
            "Dry-run needs a table name here, not a subquery.".to_string()
        } else if is_word(tokens.get(at), "ONLY") {
            refuse("ONLY (...)")
        } else {
            "Dry-run could not find the table name.".to_string()
        }
    })?;
    if is_word(tokens.get(at), "ONLY") {
        return Err(refuse("ONLY (...)"));
    }
    at += 1;
    let (owner, table) = if matches!(tokens.get(at).map(|t| &t.tok), Some(Tok::Punct('.'))) {
        let table = ident(tokens.get(at + 1)).ok_or("Dry-run could not read the table name.")?;
        at += 2;
        (Some(first_name), table)
    } else {
        (None, first_name)
    };
    match tokens.get(at).map(|t| &t.tok) {
        Some(Tok::Punct('@')) => return Err(refuse("A table over a database link")),
        Some(Tok::Punct('.')) => return Err("Dry-run could not read the table name.".into()),
        Some(Tok::Punct('(')) => return Err(refuse("A partition clause or a function table")),
        _ => {}
    }
    let mut alias = None;
    if let Some(Token {
        tok: Tok::Word(w) | Tok::Quoted(w),
        ..
    }) = tokens.get(at)
    {
        let is = |names: &[&str]| names.iter().any(|r| w.eq_ignore_ascii_case(r));
        if is(&["PARTITION", "SUBPARTITION", "SAMPLE"]) {
            return Err(refuse("A partition or sample clause"));
        }
        if !is(RESERVED) {
            alias = Some(Ident(w.clone()));
            at += 1;
        }
    }
    if kind == Kind::Update {
        if !is_word(tokens.get(at), "SET") {
            return Err("Dry-run expected SET after the table name.".into());
        }
        at += 1;
    }
    // Top level scan for WHERE and RETURNING.
    let mut depth = 0i32;
    let mut where_at = None;
    for (index, token) in tokens.iter().enumerate().skip(at) {
        match &token.tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Word(w) if depth == 0 => {
                if w.eq_ignore_ascii_case("RETURNING") || w.eq_ignore_ascii_case("RETURN") {
                    return Err(refuse("A RETURNING clause"));
                }
                if w.eq_ignore_ascii_case("LOG") && is_word(tokens.get(index + 1), "ERRORS") {
                    return Err(refuse("LOG ERRORS"));
                }
                if w.eq_ignore_ascii_case("WHERE") && where_at.is_none() {
                    where_at = Some(index);
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err("The parentheses of the statement do not match.".into());
    }
    let where_clause = match where_at {
        Some(index) => {
            let text = sql[tokens[index].end..].trim();
            if text.is_empty() {
                return Err("The WHERE clause is empty.".into());
            }
            Some(text.to_string())
        }
        None => None,
    };
    Ok(Parsed {
        kind,
        owner,
        table,
        alias,
        where_clause,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(sql: &str) -> Parsed {
        parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    #[test]
    fn update_with_schema_alias_and_where() {
        let p = ok("update hr.employees e set salary = salary * 1.1 where e.dept_id = 10;");
        assert_eq!(p.kind, Kind::Update);
        assert_eq!(p.table_ref(), "hr.employees");
        assert_eq!(p.qualifier(), "e");
        assert_eq!(p.where_clause.as_deref(), Some("e.dept_id = 10"));
        assert_eq!(p.table_with_alias(), "hr.employees e");
    }

    #[test]
    fn update_without_where_or_alias() {
        let p = ok("UPDATE emp SET a = 1");
        assert_eq!(p.alias, None);
        assert_eq!(p.where_clause, None);
        assert_eq!(p.qualifier(), "emp");
    }

    #[test]
    fn delete_forms() {
        assert_eq!(ok("DELETE FROM emp WHERE id = 1").table.name(), "EMP");
        assert_eq!(ok("delete emp where id = 1").table.name(), "EMP");
        let p = ok("DELETE FROM \"Hr\".\"Emp\" x");
        assert_eq!(p.owner.unwrap().name(), "Hr");
        assert_eq!(p.table.name(), "Emp");
        assert_eq!(p.alias.unwrap().0, "x");
    }

    #[test]
    fn where_inside_text_or_subquery_is_not_the_clause() {
        let p = ok("UPDATE t SET note = 'where x' WHERE id IN (SELECT id FROM u WHERE a = 1)");
        assert_eq!(
            p.where_clause.as_deref(),
            Some("id IN (SELECT id FROM u WHERE a = 1)")
        );
        let p = ok("UPDATE t SET a = (SELECT 1 FROM d WHERE d.x = 1)");
        assert_eq!(p.where_clause, None);
    }

    #[test]
    fn comments_and_semicolon() {
        let p = ok("-- fix\n/* why */ UPDATE t SET a = 1 -- tail\nWHERE b = 2;");
        assert_eq!(p.where_clause.as_deref(), Some("b = 2"));
    }

    #[test]
    fn refused_statements() {
        for sql in [
            "MERGE INTO t USING s ON (1=1) WHEN MATCHED THEN UPDATE SET a = 1",
            "INSERT INTO t VALUES (1)",
            "SELECT 1 FROM dual",
            "DROP TABLE t",
            "UPDATE t SET a = 1; DELETE FROM t",
            "UPDATE t SET a = 1 RETURNING a INTO :x",
            "DELETE FROM t@remote WHERE 1 = 1",
            "DELETE FROM (SELECT * FROM t) WHERE 1 = 1",
            "UPDATE t PARTITION (p1) SET a = 1",
            "UPDATE t SET a = 1 WHERE",
            "UPDATE t a = 1",
            "",
        ] {
            assert!(parse(sql).is_err(), "{sql} should be refused");
        }
    }

    #[test]
    fn text_literal_with_quote_and_semicolon() {
        let p = ok("UPDATE t SET a = 'it''s; fine' WHERE id = 1");
        assert_eq!(p.where_clause.as_deref(), Some("id = 1"));
    }
}
