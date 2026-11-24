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

/// Result of [`toggle_line_comment`]: replace `range` of the original text
/// with `text`, then select `selection` (offsets into the new full text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentEdit {
    pub range: std::ops::Range<usize>,
    pub text: String,
    pub selection: std::ops::Range<usize>,
}

/// Toggle `-- ` line comments on the lines touched by `sel` (byte offsets).
///
/// If every non-empty line is already commented they are all uncommented
/// (the `--` and one following space are removed); otherwise every
/// non-empty line is commented at the shallowest indentation of the block.
/// A selection ending at column 0 of a line does not include that line.
/// Returns `None` when `sel` is outside `text`.
pub fn toggle_line_comment(text: &str, sel: std::ops::Range<usize>) -> Option<CommentEdit> {
    let (start, end) = (sel.start.min(sel.end), sel.start.max(sel.end));
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    let block_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut last = end;
    if end > start && text[..end].ends_with('\n') {
        last = end - 1;
    }
    let block_end = text[last..].find('\n').map_or(text.len(), |i| last + i);

    let lines: Vec<&str> = text[block_start..block_end].split('\n').collect();
    let indent_of = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let is_blank = |l: &str| l.trim().is_empty();
    let all_blank = lines.iter().all(|l| is_blank(l));
    let uncomment = !all_blank
        && lines
            .iter()
            .filter(|l| !is_blank(l))
            .all(|l| l[indent_of(l)..].starts_with("--"));
    let min_indent = lines
        .iter()
        .filter(|l| all_blank || !is_blank(l))
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(0);

    // (position in original text, bytes removed, bytes inserted)
    let mut edits: Vec<(usize, usize, usize)> = Vec::new();
    let mut out = String::new();
    let mut pos = block_start;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if uncomment && !is_blank(line) {
            let at = indent_of(line);
            let rest = &line[at + 2..];
            let removed = 2 + usize::from(rest.starts_with(' '));
            out.push_str(&line[..at]);
            out.push_str(&line[at + removed..]);
            edits.push((pos + at, removed, 0));
        } else if !uncomment && (all_blank || !is_blank(line)) {
            let at = if is_blank(line) {
                line.len().min(min_indent)
            } else {
                min_indent
            };
            out.push_str(&line[..at]);
            out.push_str("-- ");
            out.push_str(&line[at..]);
            edits.push((pos + at, 0, 3));
        } else {
            out.push_str(line);
        }
        pos += line.len() + 1;
    }

    let map = |o: usize| -> usize {
        let mut n = o as isize;
        for &(p, rem, ins) in &edits {
            if o >= p + rem && (rem > 0 || o >= p) {
                n += ins as isize - rem as isize;
            } else if o > p {
                n -= (o - p) as isize; // inside a removed run: clamp to its start
            }
        }
        n.max(block_start as isize) as usize
    };
    let hi = block_start + out.len();
    let selection = map(start).min(hi)..map(end).min(hi);
    Some(CommentEdit {
        range: block_start..block_end,
        text: out,
        selection,
    })
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

/// Conservative classification for Read Only and production confirmation.
pub fn is_read_query(sql: &str) -> bool {
    let Some(parts) = select_script(sql) else {
        return false;
    };
    if parts.len() != 1 {
        return false;
    }
    !tokenize(&parts[0]).iter().any(|token| match token {
        Tok::Word(word) => [
            "UPDATE",
            "INSERT",
            "DELETE",
            "MERGE",
            "FUNCTION",
            "PROCEDURE",
            "NEXTVAL",
        ]
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword)),
        Tok::Quoted(name) => name.eq_ignore_ascii_case("\"NEXTVAL\""),
        _ => false,
    })
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

/// Statement boundaries the way Toad draws them, for the editor's Run
/// buttons: a statement ends at a `;` outside quotes and comments, at a
/// blank line, or at a line holding only `/`. PL/SQL (`BEGIN`/`DECLARE`
/// blocks, `CREATE [OR REPLACE] FUNCTION|PROCEDURE|PACKAGE|TRIGGER|TYPE`,
/// `WITH FUNCTION`) is the exception: its `;` and blank lines belong to the
/// block, which ends only at the `/` line (or the end of the text).
/// Comment-only chunks never form a statement of their own; they stay with
/// the statement after them.
pub fn script_spans(sql: &str) -> Vec<(usize, usize)> {
    let bytes = sql.as_bytes();
    let mut spans = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    let (mut content, mut plsql) = (false, false);
    while i < bytes.len() {
        if i == 0 || bytes[i - 1] == b'\n' {
            let end = sql[i..].find('\n').map_or(sql.len(), |n| i + n);
            if sql[i..end].trim() == "/" {
                if content {
                    spans.push((start, i));
                }
                start = (end + 1).min(sql.len());
                i = start;
                content = false;
                plsql = false;
                continue;
            }
        }
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        let comment = (b == b'-' && next == Some(b'-')) || (b == b'/' && next == Some(b'*'));
        if !content && !b.is_ascii_whitespace() && !comment {
            content = true;
            plsql = sql.get(i..).is_some_and(starts_plsql);
        }
        match b {
            b'q' | b'Q' if next == Some(b'\'') && i + 2 < bytes.len() => {
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
            b'-' if next == Some(b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if next == Some(b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b';' if !plsql => {
                spans.push((start, i + 1));
                start = i + 1;
                content = false;
            }
            b'\n' if content && !plsql => {
                // A blank line (only whitespace up to the next newline) ends it.
                let rest = &sql[i + 1..];
                if rest.find('\n').is_some_and(|n| rest[..n].trim().is_empty()) {
                    spans.push((start, i));
                    start = i + 1;
                    content = false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if content {
        spans.push((start, bytes.len()));
    }
    spans
}

/// True when `rest` (starting at a statement's first real token) opens a
/// PL/SQL unit that runs on until a `/` line.
fn starts_plsql(rest: &str) -> bool {
    let mut words = rest.split_whitespace().map(|w| {
        w.chars()
            .take_while(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_ascii_uppercase()
    });
    match words.next().as_deref() {
        Some("BEGIN" | "DECLARE") => true,
        Some("WITH") => matches!(words.next().as_deref(), Some("FUNCTION" | "PROCEDURE")),
        Some("CREATE") => {
            for word in words {
                match word.as_str() {
                    "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE" => {}
                    "FUNCTION" | "PROCEDURE" | "PACKAGE" | "TRIGGER" | "TYPE" => return true,
                    _ => return false,
                }
            }
            false
        }
        _ => false,
    }
}

/// The statements of editor text (or a selection), in order, each trimmed.
pub fn script_statements(sql: &str) -> Vec<String> {
    script_spans(sql)
        .into_iter()
        .map(|(a, b)| sql[a..b].trim().to_string())
        .collect()
}

/// The statement the cursor (byte offset) sits in, split as by
/// [`script_spans`]. A cursor right after a statement's end still belongs
/// to it. Empty segments are skipped so a cursor on trailing whitespace
/// picks the nearest statement before. Returns the whole text when it
/// holds a single statement.
pub fn statement_at(sql: &str, cursor: usize) -> String {
    let spans = script_spans(sql);
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
    fn protected_query_checks_locking_writes_and_plsql() {
        assert!(is_read_query(
            "/* update the report */ SELECT 'NEXTVAL' FROM dual;"
        ));
        assert!(is_read_query(
            "WITH sample AS (SELECT 1 id FROM dual) SELECT id FROM sample"
        ));
        for sql in [
            "SELECT * FROM accounts FOR /* lock */ UPDATE",
            "SELECT s.NEXTVAL FROM dual",
            "WITH FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END; SELECT f FROM dual",
            "WITH sample AS (SELECT 1 id FROM dual) DELETE FROM accounts",
            "SELECT 1 FROM dual; DELETE FROM accounts",
        ] {
            assert!(!is_read_query(sql), "{sql}");
        }
    }

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
    fn blank_lines_and_slash_lines_end_statements() {
        let sql = "SELECT 1\nFROM dual\n\nSELECT 2 FROM dual\n/\nSELECT 3 FROM dual";
        assert_eq!(
            script_statements(sql),
            [
                "SELECT 1\nFROM dual",
                "SELECT 2 FROM dual",
                "SELECT 3 FROM dual"
            ]
        );
        assert_eq!(statement_at(sql, 10), "SELECT 1\nFROM dual");
        assert_eq!(statement_at(sql, 24), "SELECT 2 FROM dual");
        // Blank lines inside a block comment or string do not split.
        assert_eq!(
            script_statements("SELECT /* a\n\nb */ 1, 'x\n\ny' FROM dual").len(),
            1
        );
        assert!(script_statements("-- only\n\n  \n").is_empty());
    }

    #[test]
    fn plsql_blocks_are_not_split_on_semicolons_or_blank_lines() {
        let sql =
            "UPDATE t SET a = 1;\nBEGIN\n  x := 1;\n\n  y := 2;\nEND;\n/\nSELECT 1 FROM dual;";
        let parts = script_statements(sql);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], "BEGIN\n  x := 1;\n\n  y := 2;\nEND;");
        assert_eq!(statement_at(sql, 40), parts[1]);
        let create =
            "CREATE OR REPLACE PROCEDURE p IS\nBEGIN\n  NULL;\nEND;\n/\nSELECT 1 FROM dual";
        assert_eq!(script_statements(create).len(), 2);
        let with_fn =
            "WITH FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END;\nSELECT f FROM dual;\n/";
        assert_eq!(script_statements(with_fn).len(), 1);
        // Plain DDL is still split on `;`.
        assert_eq!(
            script_statements("CREATE TABLE a (x NUMBER); CREATE TABLE b (y NUMBER);").len(),
            2
        );
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

    fn toggled(text: &str, sel: std::ops::Range<usize>) -> (String, std::ops::Range<usize>) {
        let e = toggle_line_comment(text, sel).unwrap();
        let mut t = text.to_string();
        t.replace_range(e.range.clone(), &e.text);
        (t, e.selection)
    }

    #[test]
    fn toggle_comment_single_line_and_back() {
        let (t, sel) = toggled("select 1", 3..3);
        assert_eq!(t, "-- select 1");
        assert_eq!(sel, 6..6);
        let (t, sel) = toggled(&t, sel);
        assert_eq!(t, "select 1");
        assert_eq!(sel, 3..3);
    }

    #[test]
    fn toggle_comment_block_uses_shallowest_indent_and_skips_blank() {
        let src = "a\n    b\n\n  c";
        let (t, _) = toggled(src, 0..src.len());
        assert_eq!(t, "-- a\n--     b\n\n--   c");
        let (back, _) = toggled(&t, 0..t.len());
        assert_eq!(back, src);
    }

    #[test]
    fn toggle_comment_mixed_comments_all_lines() {
        let (t, _) = toggled("-- a\nb", 0..6);
        assert_eq!(t, "-- -- a\n-- b");
    }

    #[test]
    fn toggle_comment_keeps_indent_and_handles_no_space() {
        let (t, _) = toggled("  --x\n\t-- y", 0..11);
        assert_eq!(t, "  x\n\ty");
    }

    #[test]
    fn toggle_comment_selection_ending_at_line_start_excludes_that_line() {
        let (t, _) = toggled("a\nb\nc", 0..4);
        assert_eq!(t, "-- a\n-- b\nc");
    }

    #[test]
    fn toggle_comment_empty_line_and_bounds() {
        let (t, sel) = toggled("a\n\nb", 2..2);
        assert_eq!(t, "a\n-- \nb");
        assert_eq!(sel, 5..5);
        assert!(toggle_line_comment("abc", 0..9).is_none());
        assert!(toggle_line_comment("é", 1..1).is_none());
        let (t, _) = toggled("", 0..0);
        assert_eq!(t, "-- ");
    }

    #[test]
    fn toggle_comment_selection_follows_text() {
        let (t, sel) = toggled("ab\ncd", 1..4);
        assert_eq!(t, "-- ab\n-- cd");
        assert_eq!(&t[sel], "b\n-- c");
    }
}
