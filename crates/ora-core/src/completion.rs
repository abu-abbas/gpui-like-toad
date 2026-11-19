//! SQL autocomplete engine for the editor. UI-free and synchronous: the GUI
//! hands it the editor text, the caret offset and an in-memory [`Catalog`],
//! and gets back a ranked list of suggestions plus the byte range they
//! replace.
//!
//! Context comes from a small hand-written tokenizer instead of a real SQL
//! parser, because the text under the caret is half-typed by definition and
//! a strict parser rejects it. The rules are:
//!
//! - after `FROM` / `JOIN` / `INTO` / `UPDATE` / `TABLE` / `DESC`: tables
//!   and views of the default schema, plus schema names;
//! - after `alias.` or `table.`: that table's columns;
//! - after `schema.`: that schema's objects;
//! - anywhere else: columns of the tables the statement mentions, aliases,
//!   keywords, built-in functions and callable schema objects.
//!
//! Nothing here talks to the database. The catalog is filled by the GUI
//! from [`crate::DbClient::catalog_objects`] on the worker thread.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::model::{ColumnDef, ObjectKind};

/// Most suggestions returned for one request. The popup is a short list;
/// anything past this is noise the user narrows by typing.
pub const MAX_SUGGESTIONS: usize = 100;

/// One schema object as the completion engine sees it. `columns` is empty
/// for anything that is not a table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogObject {
    pub name: String,
    pub kind: ObjectKind,
    pub columns: Vec<ColumnDef>,
}

/// Metadata cache behind the suggestions: schema names, and per loaded
/// schema its objects with their columns. Owners are stored uppercase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    default_owner: Option<String>,
    schemas: Vec<String>,
    owners: BTreeMap<String, Vec<CatalogObject>>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (on disconnect).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Schema used for unqualified names: the session user.
    pub fn set_default_owner(&mut self, owner: Option<String>) {
        self.default_owner = owner.map(|o| o.to_ascii_uppercase());
    }

    pub fn default_owner(&self) -> Option<&str> {
        self.default_owner.as_deref()
    }

    pub fn set_schemas(&mut self, schemas: Vec<String>) {
        self.schemas = schemas;
    }

    pub fn set_owner(&mut self, owner: &str, objects: Vec<CatalogObject>) {
        self.owners.insert(owner.to_ascii_uppercase(), objects);
    }

    pub fn has_owner(&self, owner: &str) -> bool {
        self.owners.contains_key(&owner.to_ascii_uppercase())
    }

    fn is_schema(&self, name: &str) -> Option<&str> {
        self.schemas
            .iter()
            .find(|s| s.eq_ignore_ascii_case(name))
            .map(String::as_str)
    }

    fn objects(&self, owner: &str) -> &[CatalogObject] {
        self.owners
            .get(&owner.to_ascii_uppercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Find a table/view by name. An explicit owner is honoured; without
    /// one the default schema wins, then any other loaded schema.
    fn find_table(&self, owner: Option<&str>, name: &Name) -> Option<(&str, &CatalogObject)> {
        let in_owner = |owner: &str| {
            self.owners
                .get_key_value(&owner.to_ascii_uppercase())
                .and_then(|(key, objects)| {
                    objects
                        .iter()
                        .find(|o| has_columns(&o.kind) && name.matches(&o.name))
                        .map(|o| (key.as_str(), o))
                })
        };
        if let Some(owner) = owner {
            return in_owner(owner);
        }
        if let Some(found) = self.default_owner.as_deref().and_then(in_owner) {
            return Some(found);
        }
        self.owners.keys().find_map(|owner| in_owner(owner))
    }
}

fn has_columns(kind: &ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
    )
}

/// What a suggestion is, so the UI can pick an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Function,
    Schema,
    Table,
    View,
    Column,
    /// A table alias defined in the current statement.
    Alias,
    /// Sequence, procedure, package, synonym, ...
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Text shown in the popup.
    pub label: String,
    pub kind: SuggestionKind,
    /// Short right-hand note: data type, owning table, object type.
    pub detail: String,
    /// Text written into the editor when accepted.
    pub insert: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completions {
    /// Byte range of the editor text the accepted suggestion replaces: the
    /// word being typed, ending at the caret.
    pub replace: Range<usize>,
    pub items: Vec<Suggestion>,
    /// The user typed `SCHEMA.` for a schema whose objects are not in the
    /// catalog yet. The GUI loads it in the background so the next
    /// keystroke has suggestions.
    pub missing_schema: Option<String>,
}

// ----- tokenizer -----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    /// `"Quoted Identifier"`.
    Quoted,
    Number,
    Punct(char),
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

/// Tokenize, dropping comments and string literals. Returns `None` when
/// `caret` sits inside a comment or a string, where suggesting is wrong.
fn tokenize(text: &str, caret: usize) -> Option<Vec<Token>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = text[i..].chars().next().unwrap();
        let start = i;
        if c.is_whitespace() {
            i += c.len_utf8();
        } else if text[i..].starts_with("--") {
            let end = text[i..].find('\n').map(|n| i + n).unwrap_or(text.len());
            if caret > start && caret <= end {
                return None;
            }
            i = end;
        } else if text[i..].starts_with("/*") {
            let (end, closed) = match text[i + 2..].find("*/") {
                Some(n) => (i + 2 + n + 2, true),
                None => (text.len(), false),
            };
            if caret > start && (caret < end || !closed) {
                return None;
            }
            i = end;
        } else if c == '\'' {
            // '' is an escaped quote inside the literal.
            let mut j = i + 1;
            let mut closed = false;
            while j < bytes.len() {
                if bytes[j] == b'\'' {
                    if bytes.get(j + 1) == Some(&b'\'') {
                        j += 2;
                        continue;
                    }
                    j += 1;
                    closed = true;
                    break;
                }
                j += 1;
            }
            if caret > start && (caret < j || !closed) {
                return None;
            }
            i = j;
        } else if c == '"' {
            let (end, closed) = match text[i + 1..].find('"') {
                Some(n) => (i + 1 + n + 1, true),
                None => (text.len(), false),
            };
            if caret > start && (caret < end || !closed) {
                return None;
            }
            tokens.push(Token {
                kind: TokenKind::Quoted,
                start,
                end,
            });
            i = end;
        } else if is_ident_char(c) {
            let mut end = i;
            for ch in text[i..].chars() {
                if !is_ident_char(ch) {
                    break;
                }
                end += ch.len_utf8();
            }
            let kind = if c.is_ascii_digit() {
                TokenKind::Number
            } else {
                TokenKind::Word
            };
            tokens.push(Token { kind, start, end });
            i = end;
        } else {
            i += c.len_utf8();
            tokens.push(Token {
                kind: TokenKind::Punct(c),
                start,
                end: i,
            });
        }
    }
    Some(tokens)
}

/// An identifier as written: unquoted names compare case-insensitively,
/// quoted ones exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Name {
    text: String,
    quoted: bool,
}

impl Name {
    fn of(text: &str, token: &Token) -> Name {
        let raw = &text[token.start..token.end];
        if token.kind == TokenKind::Quoted {
            Name {
                text: raw.trim_matches('"').to_string(),
                quoted: true,
            }
        } else {
            Name {
                text: raw.to_string(),
                quoted: false,
            }
        }
    }

    fn matches(&self, other: &str) -> bool {
        if self.quoted {
            self.text == other
        } else {
            self.text.eq_ignore_ascii_case(other)
        }
    }
}

fn is_name(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Word | TokenKind::Quoted)
}

fn word_is(text: &str, token: &Token, keywords: &[&str]) -> bool {
    token.kind == TokenKind::Word
        && keywords
            .iter()
            .any(|k| text[token.start..token.end].eq_ignore_ascii_case(k))
}

// ----- statement scope -----

/// Keywords that introduce a table reference.
const TABLE_INTRO: &[&str] = &[
    "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "DESC", "DESCRIBE",
];

/// Clause keywords: finding one of these while walking back from a comma
/// tells which list the comma belongs to.
const CLAUSES: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "SET",
    "BY",
    "INTO",
    "VALUES",
    "ON",
    "HAVING",
    "USING",
    "JOIN",
    "UPDATE",
    "RETURNING",
];

/// A table mentioned in the statement, with its alias if it has one.
#[derive(Debug, Clone)]
struct TableRef {
    owner: Option<Name>,
    name: Name,
    alias: Option<Name>,
}

/// Collect every `[schema.]table [AS] alias` that follows FROM / JOIN /
/// UPDATE / INTO in the statement, including comma-separated FROM lists.
/// Subqueries are scanned too: over-collecting is harmless, it only adds
/// candidate columns.
fn table_refs(text: &str, tokens: &[Token]) -> Vec<TableRef> {
    let mut refs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !word_is(text, &tokens[i], &["FROM", "JOIN", "UPDATE", "INTO"]) {
            i += 1;
            continue;
        }
        let list = word_is(text, &tokens[i], &["FROM"]);
        i += 1;
        while let Some(first) = tokens.get(i).filter(|t| is_name(t)) {
            if is_reserved(text, first) {
                break;
            }
            let mut owner = None;
            let mut name = Name::of(text, first);
            i += 1;
            if matches!(tokens.get(i).map(|t| t.kind), Some(TokenKind::Punct('.'))) {
                if let Some(second) = tokens.get(i + 1).filter(|t| is_name(t)) {
                    owner = Some(name);
                    name = Name::of(text, second);
                    i += 2;
                }
            }
            if tokens.get(i).is_some_and(|t| word_is(text, t, &["AS"])) {
                i += 1;
            }
            let mut alias = None;
            if let Some(next) = tokens.get(i).filter(|t| is_name(t)) {
                if !is_reserved(text, next) {
                    alias = Some(Name::of(text, next));
                    i += 1;
                }
            }
            refs.push(TableRef { owner, name, alias });
            if list && matches!(tokens.get(i).map(|t| t.kind), Some(TokenKind::Punct(','))) {
                i += 1;
                continue;
            }
            break;
        }
    }
    refs
}

fn is_reserved(text: &str, token: &Token) -> bool {
    token.kind == TokenKind::Word
        && KEYWORDS
            .iter()
            .any(|k| text[token.start..token.end].eq_ignore_ascii_case(k))
}

/// True when the caret is where a table name belongs.
fn in_table_position(text: &str, before: &[Token]) -> bool {
    let Some(prev) = before.last() else {
        return false;
    };
    if word_is(text, prev, TABLE_INTRO) {
        return true;
    }
    if prev.kind != TokenKind::Punct(',') {
        return false;
    }
    // A comma: it continues a FROM list only if FROM is the nearest clause
    // keyword at the same parenthesis depth.
    let mut depth = 0i32;
    for token in before.iter().rev().skip(1) {
        match token.kind {
            TokenKind::Punct(')') => depth += 1,
            TokenKind::Punct('(') => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            TokenKind::Word if depth == 0 && word_is(text, token, CLAUSES) => {
                return word_is(text, token, &["FROM"]);
            }
            _ => {}
        }
    }
    false
}

// ----- matching -----

/// Case-insensitive fuzzy score, higher is better; `None` when `query`
/// does not match. Prefix beats a match at a word boundary (after `_`),
/// which beats a plain substring, which beats scattered characters.
pub fn fuzzy_score(candidate: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let cand = candidate.to_lowercase();
    let query = query.to_lowercase();
    let extra = cand.len().saturating_sub(query.len()).min(100) as i32;
    if cand.starts_with(&query) {
        return Some(1000 - extra);
    }
    if let Some(pos) = cand.find(&query) {
        let boundary = cand[..pos].ends_with(['_', '$', '#']);
        return Some(if boundary { 700 } else { 500 } - extra);
    }
    // Subsequence: every query char in order, penalising the gaps.
    let mut chars = cand.chars();
    let mut gaps = 0i32;
    for q in query.chars() {
        loop {
            match chars.next() {
                Some(c) if c == q => break,
                Some(_) => gaps += 1,
                None => return None,
            }
        }
    }
    Some(300 - gaps.min(200) - extra)
}

/// An unquoted Oracle identifier: stored uppercase, usable without quotes.
fn is_plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$' | '#'))
}

/// Text to insert for an identifier: follow the user's case when the name
/// allows it (`emp` completes to `employees`), quote it when it needs it.
fn ident_insert(name: &str, lower: bool) -> String {
    if !is_plain_ident(name) {
        format!("\"{name}\"")
    } else if lower {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

struct Ranked {
    score: i32,
    suggestion: Suggestion,
}

struct Collector<'a> {
    prefix: &'a str,
    lower: bool,
    ranked: Vec<Ranked>,
    exact: bool,
}

impl<'a> Collector<'a> {
    fn new(prefix: &'a str) -> Self {
        let lower = prefix.chars().any(|c| c.is_ascii_lowercase())
            && !prefix.chars().any(|c| c.is_ascii_uppercase());
        Collector {
            prefix,
            lower,
            ranked: Vec::new(),
            exact: false,
        }
    }

    fn push(
        &mut self,
        label: &str,
        kind: SuggestionKind,
        detail: String,
        insert: String,
        bonus: i32,
    ) {
        let Some(score) = fuzzy_score(label, self.prefix) else {
            return;
        };
        if label.eq_ignore_ascii_case(self.prefix) {
            self.exact = true;
        }
        if self
            .ranked
            .iter()
            .any(|r| r.suggestion.label == label && r.suggestion.kind == kind)
        {
            return;
        }
        self.ranked.push(Ranked {
            score: score + bonus,
            suggestion: Suggestion {
                label: label.to_string(),
                kind,
                detail,
                insert,
            },
        });
    }

    fn ident(&mut self, name: &str, kind: SuggestionKind, detail: String, bonus: i32) {
        let insert = ident_insert(name, self.lower);
        self.push(name, kind, detail, insert, bonus);
    }

    /// `with_table` adds the owning table to the note; pointless after
    /// `alias.`, where every row would repeat the same name.
    fn columns(&mut self, object: &CatalogObject, with_table: bool, bonus: i32) {
        for column in &object.columns {
            let detail = if with_table {
                format!("{} · {}", column.data_type, object.name)
            } else {
                column.data_type.clone()
            };
            self.ident(&column.name, SuggestionKind::Column, detail, bonus);
        }
    }

    fn objects(&mut self, objects: &[CatalogObject], owner: &str, tables_only: bool, bonus: i32) {
        for object in objects {
            let kind = match object.kind {
                ObjectKind::Table => SuggestionKind::Table,
                ObjectKind::View | ObjectKind::MaterializedView => SuggestionKind::View,
                ObjectKind::Synonym => SuggestionKind::Object,
                // Indexes, triggers and database links are never named like
                // a table in a query.
                ObjectKind::Index | ObjectKind::Trigger | ObjectKind::DbLink => continue,
                _ if tables_only => continue,
                _ => SuggestionKind::Object,
            };
            let rank = if has_columns(&object.kind) { 20 } else { 0 };
            self.ident(
                &object.name,
                kind,
                format!("{} · {owner}", object.kind.oracle_type().to_lowercase()),
                bonus + rank,
            );
        }
    }

    fn keywords(&mut self, bonus: i32) {
        for keyword in KEYWORDS {
            let insert = if self.lower {
                keyword.to_ascii_lowercase()
            } else {
                keyword.to_string()
            };
            self.push(
                keyword,
                SuggestionKind::Keyword,
                "keyword".into(),
                insert,
                bonus,
            );
        }
    }

    fn functions(&mut self, bonus: i32) {
        for function in FUNCTIONS {
            let name = if self.lower {
                function.to_ascii_lowercase()
            } else {
                function.to_string()
            };
            self.push(
                function,
                SuggestionKind::Function,
                "function".into(),
                format!("{name}("),
                bonus,
            );
        }
    }

    fn finish(mut self) -> Vec<Suggestion> {
        // The word under the caret is already complete. Showing a popup
        // now would make Enter re-insert it instead of breaking the line.
        if self.exact && !self.prefix.is_empty() {
            return Vec::new();
        }
        // Stable: equal scores keep insertion order (column order, then
        // the alphabetical order the catalog arrived in).
        self.ranked.sort_by_key(|r| std::cmp::Reverse(r.score));
        self.ranked.truncate(MAX_SUGGESTIONS);
        self.ranked.into_iter().map(|r| r.suggestion).collect()
    }
}

/// Suggestions for the caret at byte `offset` of `text`.
pub fn complete(text: &str, offset: usize, catalog: &Catalog) -> Completions {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let none = Completions {
        replace: offset..offset,
        ..Completions::default()
    };
    let Some(tokens) = tokenize(text, offset) else {
        return none;
    };

    // The word being typed ends at the caret.
    let prefix_len: usize = text[..offset]
        .chars()
        .rev()
        .take_while(|c| is_ident_char(*c))
        .map(char::len_utf8)
        .sum();
    let prefix_start = offset - prefix_len;
    let prefix = &text[prefix_start..offset];
    if prefix.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return none;
    }

    // Narrow to the statement around the caret (`;` separated).
    let stmt_start = tokens
        .iter()
        .rposition(|t| t.kind == TokenKind::Punct(';') && t.end <= prefix_start)
        .map(|ix| ix + 1)
        .unwrap_or(0);
    let stmt_end = tokens
        .iter()
        .position(|t| t.kind == TokenKind::Punct(';') && t.start >= offset)
        .unwrap_or(tokens.len());
    let statement = &tokens[stmt_start..stmt_end];
    let mut before: &[Token] = {
        let count = statement
            .iter()
            .take_while(|t| t.end <= prefix_start)
            .count();
        &statement[..count]
    };

    // `a.` / `a.b.` right before the word: the qualifier chain.
    let mut qualifiers: Vec<Name> = Vec::new();
    while qualifiers.len() < 2 {
        let [rest @ .., name, dot] = before else {
            break;
        };
        if dot.kind != TokenKind::Punct('.') || !is_name(name) || dot.end > prefix_start {
            break;
        }
        // The dot must touch what follows it, else it is unrelated.
        let next_start = if qualifiers.is_empty() {
            prefix_start
        } else {
            before_end(before, text.len())
        };
        if dot.end != next_start && qualifiers.is_empty() {
            break;
        }
        qualifiers.insert(0, Name::of(text, name));
        before = rest;
    }

    let refs = table_refs(text, statement);
    let table_position = in_table_position(text, before);
    let mut out = Collector::new(prefix);
    let mut missing_schema = None;

    match qualifiers.as_slice() {
        [owner, table] => {
            if let Some((_, object)) = catalog.find_table(Some(&owner.text), table) {
                out.columns(object, false, 0);
            } else if let Some(schema) = catalog.is_schema(&owner.text) {
                if !catalog.has_owner(schema) {
                    missing_schema = Some(schema.to_string());
                }
            }
        }
        [qualifier] => {
            let by_alias = refs.iter().find(|r| {
                r.alias
                    .as_ref()
                    .is_some_and(|alias| alias.text.eq_ignore_ascii_case(&qualifier.text))
            });
            let by_name = refs
                .iter()
                .find(|r| r.name.text.eq_ignore_ascii_case(&qualifier.text));
            let table = if table_position {
                None
            } else if let Some(table_ref) = by_alias.or(by_name) {
                catalog.find_table(
                    table_ref.owner.as_ref().map(|o| o.text.as_str()),
                    &table_ref.name,
                )
            } else {
                None
            };
            if let Some((_, object)) = table {
                out.columns(object, false, 0);
            } else if let Some(schema) = catalog.is_schema(&qualifier.text) {
                if catalog.has_owner(schema) {
                    out.objects(catalog.objects(schema), schema, table_position, 0);
                } else {
                    missing_schema = Some(schema.to_string());
                }
            } else if let Some((_, object)) = catalog.find_table(None, qualifier) {
                // `employees.` without the table being in a FROM yet.
                out.columns(object, false, 0);
            }
        }
        _ if table_position => {
            if let Some(owner) = catalog.default_owner() {
                out.objects(catalog.objects(owner), owner, true, 40);
            }
            for schema in &catalog.schemas {
                out.ident(schema, SuggestionKind::Schema, "schema".into(), 0);
            }
        }
        _ => {
            for table_ref in &refs {
                let owner = table_ref.owner.as_ref().map(|o| o.text.as_str());
                if let Some((_, object)) = catalog.find_table(owner, &table_ref.name) {
                    out.columns(object, true, 80);
                }
                if let Some(alias) = &table_ref.alias {
                    out.push(
                        &alias.text,
                        SuggestionKind::Alias,
                        format!("alias · {}", table_ref.name.text),
                        alias.text.clone(),
                        70,
                    );
                }
            }
            out.keywords(40);
            out.functions(20);
            if let Some(owner) = catalog.default_owner() {
                out.objects(catalog.objects(owner), owner, false, 0);
            }
        }
    }

    Completions {
        replace: prefix_start..offset,
        items: out.finish(),
        missing_schema,
    }
}

fn before_end(before: &[Token], fallback: usize) -> usize {
    before.last().map(|t| t.end).unwrap_or(fallback)
}

// ----- vocabulary -----

/// SQL and PL/SQL keywords offered as suggestions. Also the "reserved"
/// list that stops a keyword from being read as a table alias.
pub const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BODY",
    "BULK",
    "BY",
    "CASCADE",
    "CASE",
    "CHECK",
    "CLOSE",
    "COLLECT",
    "COLUMN",
    "COMMENT",
    "COMMIT",
    "CONNECT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURSOR",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "ELSIF",
    "END",
    "EXCEPTION",
    "EXECUTE",
    "EXISTS",
    "EXIT",
    "FETCH",
    "FIRST",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "IMMEDIATE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LOOP",
    "MERGE",
    "MINUS",
    "NATURAL",
    "NEXT",
    "NOT",
    "NULL",
    "NULLS",
    "OF",
    "OFFSET",
    "ON",
    "ONLY",
    "OPEN",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PACKAGE",
    "PARTITION",
    "PRIMARY",
    "PRIOR",
    "PROCEDURE",
    "PURGE",
    "RAISE",
    "REFERENCES",
    "RENAME",
    "REPLACE",
    "RETURN",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWID",
    "ROWNUM",
    "ROWS",
    "SAVEPOINT",
    "SELECT",
    "SEQUENCE",
    "SET",
    "START",
    "SYNONYM",
    "TABLE",
    "THEN",
    "TO",
    "TRIGGER",
    "TRUNCATE",
    "TYPE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
];

/// Common Oracle built-in functions; accepted as `NAME(`.
pub const FUNCTIONS: &[&str] = &[
    "ABS",
    "ADD_MONTHS",
    "AVG",
    "CAST",
    "CEIL",
    "COALESCE",
    "CONCAT",
    "COUNT",
    "DECODE",
    "DENSE_RANK",
    "EXTRACT",
    "FLOOR",
    "GREATEST",
    "INITCAP",
    "INSTR",
    "LAG",
    "LAST_DAY",
    "LEAD",
    "LEAST",
    "LENGTH",
    "LISTAGG",
    "LOWER",
    "LPAD",
    "LTRIM",
    "MAX",
    "MIN",
    "MOD",
    "MONTHS_BETWEEN",
    "NULLIF",
    "NVL",
    "NVL2",
    "POWER",
    "RANK",
    "REGEXP_LIKE",
    "REGEXP_REPLACE",
    "REGEXP_SUBSTR",
    "REPLACE",
    "ROUND",
    "ROW_NUMBER",
    "RPAD",
    "RTRIM",
    "SUBSTR",
    "SUM",
    "SYSDATE",
    "SYSTIMESTAMP",
    "TO_CHAR",
    "TO_DATE",
    "TO_NUMBER",
    "TO_TIMESTAMP",
    "TRIM",
    "TRUNC",
    "UPPER",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
        }
    }

    fn catalog() -> Catalog {
        let mut catalog = Catalog::new();
        catalog.set_default_owner(Some("hr".into()));
        catalog.set_schemas(vec!["HR".into(), "OE".into(), "SCOTT".into()]);
        catalog.set_owner(
            "HR",
            vec![
                CatalogObject {
                    name: "EMPLOYEES".into(),
                    kind: ObjectKind::Table,
                    columns: vec![
                        column("EMPLOYEE_ID", "NUMBER(6)"),
                        column("FIRST_NAME", "VARCHAR2(20)"),
                        column("DEPARTMENT_ID", "NUMBER(4)"),
                    ],
                },
                CatalogObject {
                    name: "DEPARTMENTS".into(),
                    kind: ObjectKind::Table,
                    columns: vec![
                        column("DEPARTMENT_ID", "NUMBER(4)"),
                        column("DEPARTMENT_NAME", "VARCHAR2(30)"),
                    ],
                },
                CatalogObject {
                    name: "EMP_DETAILS_VIEW".into(),
                    kind: ObjectKind::View,
                    columns: vec![column("EMPLOYEE_ID", "NUMBER(6)")],
                },
                CatalogObject {
                    name: "EMP_SEQ".into(),
                    kind: ObjectKind::Sequence,
                    columns: vec![],
                },
                CatalogObject {
                    name: "EMP_EMAIL_UK".into(),
                    kind: ObjectKind::Index,
                    columns: vec![],
                },
                CatalogObject {
                    name: "Mixed Case".into(),
                    kind: ObjectKind::Table,
                    columns: vec![column("Id", "NUMBER")],
                },
            ],
        );
        catalog.set_owner(
            "OE",
            vec![CatalogObject {
                name: "ORDERS".into(),
                kind: ObjectKind::Table,
                columns: vec![column("ORDER_ID", "NUMBER(12)")],
            }],
        );
        catalog
    }

    /// `|` marks the caret.
    fn run(sql: &str) -> Completions {
        let offset = sql.find('|').expect("caret marker");
        let text = sql.replace('|', "");
        complete(&text, offset, &catalog())
    }

    fn labels(sql: &str) -> Vec<String> {
        run(sql).items.into_iter().map(|s| s.label).collect()
    }

    #[test]
    fn tables_after_from() {
        let got = labels("select * from emp|");
        assert_eq!(got[0], "EMPLOYEES");
        assert!(got.contains(&"EMP_DETAILS_VIEW".to_string()));
        // Not tables: sequence, index, keywords.
        assert!(!got.contains(&"EMP_SEQ".to_string()));
        assert!(!got.contains(&"EMP_EMAIL_UK".to_string()));
    }

    #[test]
    fn tables_and_schemas_after_join_and_comma() {
        let got = labels("select * from employees e join |");
        assert!(got.contains(&"DEPARTMENTS".to_string()));
        assert!(got.contains(&"OE".to_string()));
        assert!(!got.contains(&"SELECT".to_string()));
        let got = labels("select * from employees e, dep|");
        assert_eq!(got[0], "DEPARTMENTS");
    }

    #[test]
    fn comma_in_select_list_is_not_a_table_position() {
        let got = labels("select first_name, dep| from employees");
        assert_eq!(got[0], "DEPARTMENT_ID");
    }

    #[test]
    fn columns_after_alias_dot() {
        let got = labels("select e.| from employees e join departments d on 1=1");
        assert_eq!(got, ["EMPLOYEE_ID", "FIRST_NAME", "DEPARTMENT_ID"]);
        let got = labels("select * from employees e join departments d on d.dep|");
        assert_eq!(got, ["DEPARTMENT_ID", "DEPARTMENT_NAME"]);
    }

    #[test]
    fn columns_after_table_name_dot() {
        assert_eq!(
            labels("select employees.fir| from employees"),
            ["FIRST_NAME"]
        );
        // Also before the FROM clause exists.
        assert_eq!(labels("select departments.|").len(), 2);
    }

    #[test]
    fn schema_dot_lists_that_schemas_objects() {
        assert_eq!(labels("select * from oe.|"), ["ORDERS"]);
        assert_eq!(labels("select * from oe.orders o where o.|"), ["ORDER_ID"]);
        assert_eq!(labels("select oe.orders.| from dual"), ["ORDER_ID"]);
    }

    #[test]
    fn unloaded_schema_is_reported() {
        let got = run("select * from scott.|");
        assert!(got.items.is_empty());
        assert_eq!(got.missing_schema.as_deref(), Some("SCOTT"));
    }

    #[test]
    fn general_context_ranks_scope_columns_over_keywords() {
        let got = run("select * from employees where f|");
        assert_eq!(got.items[0].label, "FIRST_NAME");
        assert_eq!(got.items[0].kind, SuggestionKind::Column);
        assert!(got.items.iter().any(|s| s.label == "FROM"));
    }

    #[test]
    fn keywords_at_statement_start_follow_case() {
        let got = run("sel|");
        assert_eq!(got.items[0].label, "SELECT");
        assert_eq!(got.items[0].insert, "select");
        assert_eq!(got.replace, 0..3);
        assert_eq!(run("SEL|").items[0].insert, "SELECT");
    }

    #[test]
    fn functions_insert_an_open_paren() {
        let got = run("select nv| from dual");
        assert_eq!(got.items[0].label, "NVL");
        assert_eq!(got.items[0].insert, "nvl(");
    }

    #[test]
    fn fuzzy_matches_scattered_characters() {
        assert_eq!(
            labels("select * from employees where dptid|"),
            ["DEPARTMENT_ID"]
        );
        assert!(
            fuzzy_score("EMPLOYEE_ID", "emp").unwrap() > fuzzy_score("EMPLOYEE_ID", "id").unwrap()
        );
        assert!(
            fuzzy_score("EMPLOYEE_ID", "id").unwrap() > fuzzy_score("EMPLOYEE_ID", "eid").unwrap()
        );
        assert_eq!(fuzzy_score("EMPLOYEE_ID", "xyz"), None);
    }

    #[test]
    fn complete_word_shows_nothing() {
        assert!(labels("select * from|").is_empty());
        assert!(labels("select * from employees|").is_empty());
    }

    #[test]
    fn nothing_inside_strings_comments_or_numbers() {
        assert!(labels("select 'sel|' from dual").is_empty());
        assert!(labels("select 1 -- sel|").is_empty());
        assert!(labels("select /* sel| */ 1 from dual").is_empty());
        assert!(labels("select 12| from dual").is_empty());
        // Right after a closed string is fine.
        assert!(!labels("select 'x' fr|").is_empty());
    }

    #[test]
    fn only_the_statement_around_the_caret_counts() {
        let got =
            labels("select * from departments; select e.| from employees e; select 1 from dual");
        assert_eq!(got, ["EMPLOYEE_ID", "FIRST_NAME", "DEPARTMENT_ID"]);
    }

    #[test]
    fn insert_and_update_targets_are_in_scope() {
        assert_eq!(labels("insert into departments (dep|")[0], "DEPARTMENT_ID");
        assert_eq!(labels("update employees set fir|")[0], "FIRST_NAME");
    }

    #[test]
    fn mixed_case_names_are_quoted() {
        let got = run("select * from mix|");
        assert_eq!(got.items[0].insert, "\"Mixed Case\"");
        let got = run("select t.| from \"Mixed Case\" t");
        assert_eq!(got.items[0].insert, "\"Id\"");
    }

    #[test]
    fn aliases_are_suggested() {
        let got = run("select em| from employees emp_alias");
        assert!(got
            .items
            .iter()
            .any(|s| s.kind == SuggestionKind::Alias && s.label == "emp_alias"));
    }

    #[test]
    fn caret_offset_is_clamped() {
        let got = complete("sel", 99, &catalog());
        assert_eq!(got.items[0].label, "SELECT");
        // Never panics on a non-boundary offset.
        complete("select 'é'", 9, &catalog());
    }

    #[test]
    fn empty_catalog_still_offers_keywords() {
        let got = complete("sel", 3, &Catalog::new());
        assert_eq!(got.items[0].label, "SELECT");
        assert!(complete("select * from x", 15, &Catalog::new())
            .items
            .is_empty());
    }
}
