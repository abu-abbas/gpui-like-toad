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

/// One lexical unit of a statement. Whitespace is dropped: the formatter
/// rebuilds layout from scratch, so only significant tokens matter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Number(String),
    /// `'...'` literal, verbatim (including quotes and `''` escapes).
    Str(String),
    /// `"..."` quoted identifier, verbatim.
    Quoted(String),
    /// `--...` to end of line, without the newline.
    LineComment(String),
    /// `/*...*/`, verbatim.
    BlockComment(String),
    /// Punctuation/operator, e.g. `,`, `(`, `>=`, `||`.
    Punct(String),
}

fn tokenize(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            toks.push(Tok::LineComment(chars[start..i].iter().collect()));
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            toks.push(Tok::BlockComment(chars[start..i].iter().collect()));
        } else if c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            toks.push(Tok::Str(chars[start..i].iter().collect()));
        } else if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            toks.push(Tok::Quoted(chars[start..i].iter().collect()));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '$' | '#'))
            {
                i += 1;
            }
            toks.push(Tok::Word(chars[start..i].iter().collect()));
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            toks.push(Tok::Number(chars[start..i].iter().collect()));
        } else {
            // Two-character operators first.
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if matches!(
                two.as_str(),
                ">=" | "<=" | "!=" | "<>" | "||" | "=>" | ":=" | "**" | ".."
            ) {
                toks.push(Tok::Punct(two));
                i += 2;
            } else {
                toks.push(Tok::Punct(c.to_string()));
                i += 1;
            }
        }
    }
    toks
}

/// Keywords uppercased by the formatter. Oracle folds unquoted identifiers
/// to uppercase anyway, so uppercasing a keyword-shaped identifier is safe.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "SELECT"
            | "FROM"
            | "WHERE"
            | "JOIN"
            | "LEFT"
            | "RIGHT"
            | "INNER"
            | "OUTER"
            | "FULL"
            | "CROSS"
            | "ON"
            | "GROUP"
            | "BY"
            | "ORDER"
            | "HAVING"
            | "AND"
            | "OR"
            | "NOT"
            | "IN"
            | "IS"
            | "NULL"
            | "AS"
            | "DISTINCT"
            | "UNION"
            | "ALL"
            | "INTERSECT"
            | "MINUS"
            | "EXCEPT"
            | "INSERT"
            | "INTO"
            | "VALUES"
            | "UPDATE"
            | "SET"
            | "DELETE"
            | "CREATE"
            | "TABLE"
            | "VIEW"
            | "INDEX"
            | "ALTER"
            | "DROP"
            | "TRUNCATE"
            | "GRANT"
            | "REVOKE"
            | "DECLARE"
            | "BEGIN"
            | "END"
            | "IF"
            | "THEN"
            | "ELSE"
            | "ELSIF"
            | "LOOP"
            | "WHILE"
            | "FOR"
            | "RETURN"
            | "RETURNING"
            | "FUNCTION"
            | "PROCEDURE"
            | "PACKAGE"
            | "TRIGGER"
            | "CURSOR"
            | "OPEN"
            | "CLOSE"
            | "FETCH"
            | "COMMIT"
            | "ROLLBACK"
            | "SAVEPOINT"
            | "CASE"
            | "WHEN"
            | "EXISTS"
            | "BETWEEN"
            | "LIKE"
            | "ASC"
            | "DESC"
            | "NULLS"
            | "FIRST"
            | "LAST"
            | "LIMIT"
            | "OFFSET"
            | "ROWS"
            | "ONLY"
            | "WITH"
            | "CONNECT"
            | "START"
            | "PRIOR"
            | "LEVEL"
            | "PIVOT"
            | "UNPIVOT"
            | "OVER"
            | "PARTITION"
            | "CAST"
            | "USING"
            | "NATURAL"
            | "CHECK"
            | "CONSTRAINT"
            | "PRIMARY"
            | "KEY"
            | "FOREIGN"
            | "REFERENCES"
            | "UNIQUE"
            | "DEFAULT"
            | "SEQUENCE"
            | "SYNONYM"
            | "EXPLAIN"
            | "PLAN"
    )
}

/// Common Oracle built-in function names, uppercased like keywords but
/// kept distinct from them: a function name before `(` hugs it
/// (`COUNT(*)`), while a keyword before `(` keeps a space (`IN (`).
fn is_builtin_fn(word: &str) -> bool {
    matches!(
        word,
        "COUNT"
            | "SUM"
            | "AVG"
            | "MIN"
            | "MAX"
            | "UPPER"
            | "LOWER"
            | "INITCAP"
            | "TRIM"
            | "LTRIM"
            | "RTRIM"
            | "SUBSTR"
            | "SUBSTRING"
            | "LENGTH"
            | "NVL"
            | "NVL2"
            | "COALESCE"
            | "DECODE"
            | "TO_CHAR"
            | "TO_DATE"
            | "TO_NUMBER"
            | "SYSDATE"
            | "SYSTIMESTAMP"
            | "USER"
            | "ROWID"
            | "ROWNUM"
            | "LISTAGG"
            | "RANK"
            | "DENSE_RANK"
            | "ROW_NUMBER"
            | "LEAD"
            | "LAG"
            | "FIRST_VALUE"
            | "LAST_VALUE"
            | "ABS"
            | "ROUND"
            | "TRUNC"
            | "MOD"
            | "CEIL"
            | "FLOOR"
            | "SIGN"
            | "POWER"
            | "SQRT"
            | "INSTR"
            | "REPLACE"
            | "CONCAT"
            | "LPAD"
            | "RPAD"
            | "ASCII"
            | "CHR"
            | "ADD_MONTHS"
            | "MONTHS_BETWEEN"
            | "LAST_DAY"
            | "NEXT_DAY"
            | "EXTRACT"
            | "GREATEST"
            | "LEAST"
            | "NULLIF"
            | "TO_TIMESTAMP"
            | "TO_DSINTERVAL"
            | "TO_YMINTERVAL"
            | "XMLAGG"
            | "WM_CONCAT"
            | "STDEV"
            | "VARIANCE"
            | "MEDIAN"
            | "CUME_DIST"
            | "NTILE"
            | "PERCENT_RANK"
            | "RATIO_TO_REPORT"
    )
}
/// per line, select-list items on their own indented lines, `AND`/`OR` in
/// conditions broken onto indented lines.
///
/// Only layout and keyword case change. String literals, quoted
/// identifiers, and comments are preserved verbatim. Layout breaks apply
/// only at parenthesis depth 0, so subqueries and function calls stay
/// compact on one line. Best effort: exotic syntax (e.g. `CASE` with
/// `AND`) may break in slightly odd but still valid places.
pub fn format_sql(sql: &str) -> String {
    let toks = tokenize(sql);
    let mut out = String::new();
    // Word at token index, uppercased, for lookahead.
    let word_at = |ix: usize| -> Option<String> {
        match toks.get(ix) {
            Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    };
    let mut depth: i32 = 0;
    let mut line_has_content = false;
    let mut prev_tok: Option<&Tok> = None;
    // After SELECT (or SELECT DISTINCT), the first select item starts on
    // a new indented line.
    let mut select_break_pending = false;
    // Inside a top-level select list: commas break the line.
    let mut in_select_list = false;
    // Inside a top-level WHERE/HAVING/ON condition: AND/OR break the line.
    let mut in_condition = false;
    // Inside a top-level UPDATE ... SET list: `(` hugs function names.
    let mut in_set = false;

    let newline = |out: &mut String, indent: usize, line_has_content: &mut bool| {
        if *line_has_content {
            out.push('\n');
            for _ in 0..indent {
                out.push(' ');
            }
            *line_has_content = false;
        }
    };
    // Space between the previous token and the next one is decided inline
    // below (it needs lookahead the closure cannot see).

    let mut ix = 0;
    while ix < toks.len() {
        let tok = &toks[ix];
        let upper = match tok {
            Tok::Word(w) => Some(w.to_ascii_uppercase()),
            _ => None,
        };
        let upper_ref = upper.as_deref().unwrap_or("");
        let at_top = depth == 0;

        // --- line breaks before this token (depth 0 only) ---
        if at_top && line_has_content {
            let mut brk: Option<usize> = None;
            if let Some(w) = &upper {
                // Two-word clause starts: GROUP BY, ORDER BY, CONNECT BY,
                // START WITH.
                let next = word_at(ix + 1);
                let pair = (w.as_str(), next.as_deref());
                let clause_start = matches!(
                    pair,
                    ("GROUP", Some("BY"))
                        | ("ORDER", Some("BY"))
                        | ("CONNECT", Some("BY"))
                        | ("START", Some("WITH"))
                ) || matches!(
                    w.as_str(),
                    "FROM"
                        | "WHERE"
                        | "HAVING"
                        | "UNION"
                        | "INTERSECT"
                        | "MINUS"
                        | "EXCEPT"
                        | "LIMIT"
                        | "OFFSET"
                        | "FETCH"
                        | "VALUES"
                        | "SET"
                        | "RETURNING"
                );
                if clause_start {
                    brk = Some(0);
                } else if w == "JOIN" {
                    // Not when it completes a modifier phrase
                    // (LEFT/INNER/... JOIN already broke the line).
                    if !matches!(
                        prev_word(&toks, ix).as_deref(),
                        Some("LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS")
                    ) {
                        brk = Some(0);
                    }
                } else if matches!(
                    w.as_str(),
                    "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS"
                ) {
                    // JOIN phrase: [LEFT|RIGHT|INNER|FULL|CROSS] [OUTER] JOIN.
                    // OUTER after LEFT/RIGHT/FULL continues the same phrase.
                    let continues = w == "OUTER"
                        && matches!(
                            prev_word(&toks, ix).as_deref(),
                            Some("LEFT" | "RIGHT" | "FULL")
                        );
                    if !continues {
                        let mut j = ix + 1;
                        if word_at(j).as_deref() == Some("OUTER") {
                            j += 1;
                        }
                        if word_at(j).as_deref() == Some("JOIN") {
                            brk = Some(0);
                        }
                    }
                } else if matches!(w.as_str(), "AND" | "OR") && in_condition {
                    brk = Some(4);
                }
            }
            if let Some(indent) = brk {
                newline(&mut out, indent, &mut line_has_content);
            }
        }
        // Pending break after SELECT / SELECT DISTINCT.
        if select_break_pending && line_has_content {
            if !matches!(tok, Tok::Punct(p) if p == ";") {
                newline(&mut out, 4, &mut line_has_content);
            }
            select_break_pending = false;
        }

        // --- spacing before this token ---
        if line_has_content {
            let space = match (&prev_tok, tok) {
                (Some(Tok::Punct(p)), _) if p == "." => false,
                (_, Tok::Punct(p)) if p == "." => false,
                (_, Tok::Punct(p)) if matches!(p.as_str(), "," | ")" | ";") => false,
                (Some(Tok::Punct(p)), _) if p == "(" => false,
                // Unary sign: previous significant token is an operator,
                // `(`, `,`, or a keyword.
                (Some(Tok::Punct(p)), Tok::Number(_))
                    if matches!(p.as_str(), "+" | "-") && unary_before(&toks, ix) =>
                {
                    false
                }
                // No space before `(` for function calls: a non-keyword word
                // directly before `(` in an expression context (select
                // list, condition, SET list, or inside parens). In DDL /
                // INSERT column lists (`CREATE TABLE t (`, `INSERT INTO t
                // (`) the space is kept.
                (Some(Tok::Word(w)), Tok::Punct(p))
                    if p == "("
                        && !is_keyword(&w.to_ascii_uppercase())
                        && (in_select_list || in_condition || in_set || depth > 0) =>
                {
                    false
                }
                // `*` after `(` or `.` hugs its neighbor.
                (Some(Tok::Punct(p)), Tok::Punct(q))
                    if q == "*" && matches!(p.as_str(), "(" | ".") =>
                {
                    false
                }
                (_, Tok::Punct(q)) if q == "*" => {
                    !matches!(prev_tok, Some(Tok::Punct(p)) if p == "(" || p == ".")
                }
                (Some(Tok::LineComment(_)), _) => false,
                _ => true,
            };
            if space {
                out.push(' ');
            }
        }

        // --- emit the token ---
        match tok {
            Tok::Word(w) => {
                let up = w.to_ascii_uppercase();
                if is_keyword(&up) || is_builtin_fn(&up) {
                    out.push_str(&up);
                } else {
                    out.push_str(w);
                }
            }
            Tok::Number(n) | Tok::Str(n) | Tok::Quoted(n) | Tok::BlockComment(n) => {
                out.push_str(n);
            }
            Tok::LineComment(c) => {
                out.push_str(c);
            }
            Tok::Punct(p) => out.push_str(p),
        }
        line_has_content = true;

        // --- state updates ---
        match tok {
            Tok::Punct(p) if p == "(" => depth += 1,
            Tok::Punct(p) if p == ")" => depth = (depth - 1).max(0),
            Tok::Punct(p) if p == ";" => {
                in_select_list = false;
                in_condition = false;
                in_set = false;
                select_break_pending = false;
                // Next statement starts on a fresh line.
                if ix + 1 < toks.len() {
                    newline(&mut out, 0, &mut line_has_content);
                }
            }
            Tok::Punct(p) if p == "," => {
                if at_top && in_select_list && ix + 1 < toks.len() {
                    newline(&mut out, 4, &mut line_has_content);
                }
            }
            Tok::Word(_) if at_top => {
                match upper_ref {
                    "SELECT" => {
                        in_select_list = true;
                        in_condition = false;
                        // Break after SELECT unless DISTINCT/ALL follows.
                        match word_at(ix + 1).as_deref() {
                            Some("DISTINCT") | Some("ALL") => {}
                            _ => select_break_pending = true,
                        }
                    }
                    "DISTINCT" | "ALL" if prev_is_select(&toks, ix) => {
                        select_break_pending = true;
                    }
                    "FROM" => {
                        in_select_list = false;
                        in_condition = false;
                        in_set = false;
                    }
                    "WHERE" | "HAVING" => {
                        in_condition = true;
                        in_set = false;
                    }
                    "SET" => in_set = true,
                    "ON" => in_condition = true,
                    "GROUP" | "ORDER" | "UNION" | "INTERSECT" | "MINUS" | "EXCEPT" | "LIMIT"
                    | "OFFSET" | "FETCH" => {
                        in_condition = false;
                        in_set = false;
                    }
                    _ => {}
                }
            }
            // Line comments own their line.
            Tok::LineComment(_) if ix + 1 < toks.len() => {
                newline(&mut out, 0, &mut line_has_content);
            }
            _ => {}
        }

        prev_tok = Some(tok);
        ix += 1;
    }

    out.trim_end().to_string()
}

/// True when the token before index `ix` (which is a `+`/`-` sign followed
/// by a number) puts the sign in unary position: start of input, `(`, `,`,
/// another operator, or a keyword.
fn unary_before(toks: &[Tok], ix: usize) -> bool {
    if ix < 2 {
        return true;
    }
    match &toks[ix - 2] {
        Tok::Punct(_) => true,
        Tok::Word(w) => is_keyword(&w.to_ascii_uppercase()),
        _ => false,
    }
}

/// Word directly before index `ix`, uppercased, if any.
fn prev_word(toks: &[Tok], ix: usize) -> Option<String> {
    if ix == 0 {
        return None;
    }
    match &toks[ix - 1] {
        Tok::Word(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

/// True when the word at `ix` is directly preceded by SELECT (for
/// DISTINCT/ALL handling).
fn prev_is_select(toks: &[Tok], ix: usize) -> bool {
    ix >= 1 && matches!(&toks[ix - 1], Tok::Word(w) if w.eq_ignore_ascii_case("select"))
}

/// Only split scripts consisting entirely of result-producing queries. Procedural
/// scripts and DDL keep their original statement boundaries.
pub fn select_script(sql: &str) -> Option<Vec<String>> {
    let parts: Vec<_> = statement_spans(sql)
        .into_iter()
        .map(|(a, b)| sql[a..b].trim())
        .filter(|s| !skip_leading_comments(clean_statement(s)).is_empty())
        .collect();
    (!parts.is_empty()
        && parts
            .iter()
            .all(|s| statement_kind(s) == StatementKind::Select))
    .then(|| parts.into_iter().map(str::to_owned).collect())
}

fn statement_spans(sql: &str) -> Vec<(usize, usize)> {
    let bytes = sql.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'q' | b'Q' if bytes.get(i + 1) == Some(&b'\'') && i + 2 < bytes.len() => {
                let end = match bytes[i + 2] {
                    b'[' => b']',
                    b'(' => b')',
                    b'{' => b'}',
                    b'<' => b'>',
                    c => c,
                };
                i += 3;
                while i < bytes.len() && !(bytes[i] == end && bytes.get(i + 1) == Some(&b'\'')) {
                    i += 1;
                }
                i += 1;
            }
            quote @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b';' => {
                spans.push((start, i + 1));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    spans.push((start, bytes.len()));
    let spans: Vec<(usize, usize)> = spans
        .into_iter()
        .filter(|(a, b)| !clean_statement(&sql[*a..*b]).is_empty())
        .collect();
    spans
}

/// The statement the cursor (byte offset) sits in. Statements end at a
/// `;` outside quotes and comments; a cursor right after a `;` still
/// belongs to the statement that `;` closes. Empty segments are skipped
/// so a cursor on trailing whitespace picks the nearest statement before.
/// Returns the whole text when it holds a single statement.
pub fn statement_at(sql: &str, cursor: usize) -> String {
    let spans = statement_spans(sql);
    let cursor = cursor.min(sql.len());
    let pick = spans
        .iter()
        .find(|(a, b)| cursor >= *a && cursor <= *b)
        .or_else(|| spans.iter().rev().find(|(_, b)| *b <= cursor))
        .or_else(|| spans.first());
    match pick {
        Some((a, b)) => sql[*a..*b].trim().to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_script_preserves_literals_and_ignores_trailing_comments() {
        let sql = "select q'[it's;a]' from dual; select 1 as \"a;b\" from dual; -- done";
        assert_eq!(select_script(sql).unwrap().len(), 2);
        assert!(select_script("begin null; end;").is_none());
    }

    #[test]
    fn statement_at_picks_the_statement_under_the_cursor() {
        let sql = "SELECT 1 FROM dual;\nSELECT ';' AS x FROM dual;\n-- c; x\nSELECT 3 FROM dual";
        assert_eq!(statement_at(sql, 3), "SELECT 1 FROM dual;");
        assert_eq!(statement_at(sql, 19), "SELECT 1 FROM dual;");
        assert_eq!(statement_at(sql, 30), "SELECT ';' AS x FROM dual;");
        assert_eq!(statement_at(sql, sql.len()), "-- c; x\nSELECT 3 FROM dual");
        assert_eq!(statement_at("SELECT 1", 0), "SELECT 1");
        assert_eq!(statement_at("  ", 1), "");
    }

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

    #[test]
    fn kind_detects_dml_ddl_plsql_and_other() {
        assert_eq!(
            statement_kind("MERGE INTO t USING s ON 1 = 1"),
            StatementKind::Dml
        );
        assert_eq!(statement_kind("delete from t"), StatementKind::Dml);
        assert_eq!(
            statement_kind("ALTER TABLE t ADD x NUMBER"),
            StatementKind::Ddl
        );
        assert_eq!(statement_kind("DROP TABLE t"), StatementKind::Ddl);
        assert_eq!(statement_kind("TRUNCATE TABLE t"), StatementKind::Ddl);
        assert_eq!(
            statement_kind("GRANT SELECT ON t TO hr"),
            StatementKind::Ddl
        );
        assert_eq!(
            statement_kind("REVOKE SELECT ON t FROM hr"),
            StatementKind::Ddl
        );
        assert_eq!(statement_kind("BEGIN NULL; END;"), StatementKind::PlSql);
        assert_eq!(statement_kind("COMMIT"), StatementKind::Other);
        assert_eq!(statement_kind("ROLLBACK"), StatementKind::Other);
        // Unterminated block comment: the rest is treated as code, and no
        // keyword follows the comment opener.
        assert_eq!(statement_kind("/* oops"), StatementKind::Empty);
        assert_eq!(statement_kind("123"), StatementKind::Empty);
    }

    #[test]
    fn clean_handles_lone_slash_and_inner_semicolons() {
        assert_eq!(clean_statement("/"), "");
        assert_eq!(clean_statement("SELECT 1\n/"), "SELECT 1");
        assert_eq!(clean_statement("BEGIN NULL; END;\n/\n"), "BEGIN NULL; END");
        assert_eq!(clean_statement("SELECT 'a;b'"), "SELECT 'a;b'");
        assert_eq!(clean_statement(";"), "");
        assert_eq!(
            clean_statement("SELECT 1; -- trailing"),
            "SELECT 1; -- trailing"
        );
    }

    #[test]
    fn format_basic_select() {
        assert_eq!(
            format_sql("select employee_id, first_name from employees where department_id = 10 order by last_name"),
            "SELECT\n    employee_id,\n    first_name\nFROM employees\nWHERE department_id = 10\nORDER BY last_name"
        );
    }

    #[test]
    fn format_joins_and_conditions() {
        assert_eq!(
            format_sql("SELECT e.first_name, d.department_name FROM employees e LEFT OUTER JOIN departments d ON e.department_id = d.department_id WHERE e.salary > 5000 AND d.location_id = 1700 OR e.manager_id IS NULL"),
            "SELECT\n    e.first_name,\n    d.department_name\nFROM employees e\nLEFT OUTER JOIN departments d ON e.department_id = d.department_id\nWHERE e.salary > 5000\n    AND d.location_id = 1700\n    OR e.manager_id IS NULL"
        );
    }

    #[test]
    fn format_group_having_and_functions() {
        assert_eq!(
            format_sql("select department_id, count(*), avg(salary) from employees group by department_id having count(*) > 5"),
            "SELECT\n    department_id,\n    COUNT(*),\n    AVG(salary)\nFROM employees\nGROUP BY department_id\nHAVING COUNT(*) > 5"
        );
    }

    #[test]
    fn format_preserves_strings_comments_and_case() {
        assert_eq!(
            format_sql("select 'from where' as x, \"MixedCase\" from dual -- keep me\nwhere 1=1"),
            "SELECT\n    'from where' AS x,\n    \"MixedCase\"\nFROM dual -- keep me\nWHERE 1 = 1"
        );
        assert_eq!(
            format_sql("SELECT /* inline */ 1 FROM dual"),
            "SELECT\n    /* inline */ 1\nFROM dual"
        );
        assert_eq!(
            format_sql("select 'it''s' from dual"),
            "SELECT\n    'it''s'\nFROM dual"
        );
    }

    #[test]
    fn format_subquery_stays_compact_and_unary_minus() {
        assert_eq!(
            format_sql("select * from employees where department_id in (select department_id from departments where location_id = 1700) and salary > -1"),
            "SELECT\n    *\nFROM employees\nWHERE department_id IN (SELECT department_id FROM departments WHERE location_id = 1700)\n    AND salary > -1"
        );
    }

    #[test]
    fn format_insert_update_and_multiple_statements() {
        assert_eq!(
            format_sql("insert into t (a, b) values (1, 2); update t set a = a + 1 where b = 2"),
            "INSERT INTO t (a, b)\nVALUES (1, 2);\nUPDATE t\nSET a = a + 1\nWHERE b = 2"
        );
    }

    #[test]
    fn format_empty_and_whitespace_only() {
        assert_eq!(format_sql(""), "");
        assert_eq!(format_sql("   \n  "), "");
        assert_eq!(format_sql("select 1"), "SELECT\n    1");
    }
}
