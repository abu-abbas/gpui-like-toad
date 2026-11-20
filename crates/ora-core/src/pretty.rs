//! "Pretty print" for the row Details panel: turns a cell that holds JSON
//! (also JSON stored as an escaped string, as in audit trails) or a PHP
//! `serialize()` string into indented, readable text.
//!
//! Everything here is a plain parser: no code runs, strings are read by
//! their declared byte length, depth and size are capped, and any parse
//! error means "not pretty-printable" (the caller shows the raw value).

/// Longest text that is even tried (bytes).
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// Deepest nesting accepted.
const MAX_DEPTH: usize = 64;
/// How many times a JSON string literal may wrap further JSON.
const MAX_UNWRAP: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrettyKind {
    Json,
    PhpSerialized,
}

impl PrettyKind {
    /// Link text, so the user knows what was recognized.
    pub fn link_label(self) -> &'static str {
        match self {
            PrettyKind::Json => "Pretty print",
            PrettyKind::PhpSerialized => "Pretty print (PHP serialized)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pretty {
    pub kind: PrettyKind,
    pub text: String,
}

/// Parsed value shared by both formats; numbers keep their source text.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    fn is_empty_container(&self) -> bool {
        match self {
            Node::List(v) => v.is_empty(),
            Node::Map(v) => v.is_empty(),
            _ => false,
        }
    }
}

/// Pretty text for `text` if it is a non-empty JSON object/array (possibly
/// wrapped in string literals or with escaped quotes) or a non-empty PHP
/// serialized array/object. `None` for everything else, which is the
/// common case and is decided from the first characters.
pub fn pretty_print(text: &str) -> Option<Pretty> {
    detect(text, 0)
}

fn detect(text: &str, unwrap: usize) -> Option<Pretty> {
    if text.len() > MAX_INPUT_BYTES {
        return None;
    }
    let t = text.trim();
    let first = *t.as_bytes().first()?;
    match first {
        b'{' | b'[' => {
            if let Ok(node) = parse_json(t) {
                return finish(PrettyKind::Json, node);
            }
            // `{\"before\":{...}}`: the quotes are escaped without an
            // outer string literal. Read it as the inside of one.
            if t.contains("\\\"") {
                let wrapped = format!("\"{t}\"");
                if let Ok(Node::Str(inner)) = parse_json(&wrapped) {
                    return detect_inner(&inner, unwrap);
                }
            }
            None
        }
        b'"' => match parse_json(t) {
            Ok(Node::Str(inner)) => detect_inner(&inner, unwrap),
            _ => None,
        },
        b'a' | b'O' => {
            let node = parse_php(t).ok()?;
            finish(PrettyKind::PhpSerialized, node)
        }
        _ => None,
    }
}

fn detect_inner(inner: &str, unwrap: usize) -> Option<Pretty> {
    if unwrap >= MAX_UNWRAP {
        return None;
    }
    detect(inner, unwrap + 1)
}

fn finish(kind: PrettyKind, node: Node) -> Option<Pretty> {
    if node.is_empty_container() || !matches!(node, Node::List(_) | Node::Map(_)) {
        return None;
    }
    let mut out = String::new();
    write_node(&node, 0, &mut out);
    Some(Pretty { kind, text: out })
}

// ---------------------------------------------------------------- output

fn write_node(node: &Node, level: usize, out: &mut String) {
    match node {
        Node::Null => out.push_str("null"),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Num(n) => out.push_str(n),
        Node::Str(s) => write_json_string(s, out),
        Node::List(items) if items.is_empty() => out.push_str("[]"),
        Node::Map(items) if items.is_empty() => out.push_str("{}"),
        Node::List(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(level + 1, out);
                write_node(item, level + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(level, out);
            out.push(']');
        }
        Node::Map(items) => {
            out.push_str("{\n");
            for (i, (key, value)) in items.iter().enumerate() {
                indent(level + 1, out);
                write_json_string(key, out);
                out.push_str(": ");
                write_node(value, level + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(level, out);
            out.push('}');
        }
    }
}

fn indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ------------------------------------------------------------------ JSON

struct Json<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
}

fn parse_json(text: &str) -> Result<Node, ()> {
    let mut p = Json {
        b: text.as_bytes(),
        s: text,
        i: 0,
    };
    let node = p.value(0)?;
    p.ws();
    if p.i == p.b.len() {
        Ok(node)
    } else {
        Err(())
    }
}

impl Json<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), ()> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    fn lit(&mut self, word: &str, node: Node) -> Result<Node, ()> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(node)
        } else {
            Err(())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        self.ws();
        match self.b.get(self.i).ok_or(())? {
            b'{' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Node::Map(items));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    let value = self.value(depth + 1)?;
                    items.push((key, value));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Node::Map(items));
                        }
                        _ => return Err(()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Node::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Node::List(items));
                        }
                        _ => return Err(()),
                    }
                }
            }
            b'"' => Ok(Node::Str(self.string()?)),
            b't' => self.lit("true", Node::Bool(true)),
            b'f' => self.lit("false", Node::Bool(false)),
            b'n' => self.lit("null", Node::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(()),
        }
    }

    fn number(&mut self) -> Result<Node, ()> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits_start = self.i;
        while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == digits_start {
            return Err(());
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            let frac = self.i;
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == frac {
                return Err(());
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let exp = self.i;
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == exp {
                return Err(());
            }
        }
        Ok(Node::Num(self.s[start..self.i].to_string()))
    }

    fn hex4(&mut self) -> Result<u32, ()> {
        let h = self.s.get(self.i..self.i + 4).ok_or(())?;
        self.i += 4;
        u32::from_str_radix(h, 16).map_err(|_| ())
    }

    fn string(&mut self) -> Result<String, ()> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i).ok_or(())?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or(())?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                self.eat(b'\\')?;
                                self.eat(b'u')?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).ok_or(())?);
                        }
                        _ => return Err(()),
                    }
                }
                c if c < 0x20 => return Err(()),
                _ => {
                    // Copy one whole UTF-8 char (`s` is valid UTF-8 and
                    // `i` always sits on a boundary here).
                    let ch = self.s[self.i..].chars().next().ok_or(())?;
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }
}

// ------------------------------------------------------- PHP serialize()

struct Php<'a> {
    s: &'a str,
    i: usize,
}

fn parse_php(text: &str) -> Result<Node, ()> {
    let mut p = Php { s: text, i: 0 };
    let node = p.value(0)?;
    if text[p.i..].trim().is_empty() {
        Ok(node)
    } else {
        Err(())
    }
}

impl Php<'_> {
    fn eat(&mut self, c: u8) -> Result<(), ()> {
        if self.s.as_bytes().get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    /// Text up to (not including) `end`; consumes `end`.
    fn until(&mut self, end: u8) -> Result<&str, ()> {
        let rest = &self.s.as_bytes()[self.i..];
        let n = rest.iter().position(|b| *b == end).ok_or(())?;
        let out = &self.s[self.i..self.i + n];
        self.i += n + 1;
        Ok(out)
    }

    fn count(&mut self, end: u8) -> Result<usize, ()> {
        let t = self.until(end)?;
        if t.is_empty() || t.len() > 9 || !t.bytes().all(|b| b.is_ascii_digit()) {
            return Err(());
        }
        t.parse().map_err(|_| ())
    }

    /// `len:"bytes"` (the `s:` / class-name form), after the type letter
    /// and its colon. Reads exactly `len` bytes, whatever they contain.
    fn quoted(&mut self) -> Result<String, ()> {
        let len = self.count(b':')?;
        self.eat(b'"')?;
        let end = self.i.checked_add(len).ok_or(())?;
        let s = self.s.get(self.i..end).ok_or(())?;
        self.i = end;
        self.eat(b'"')?;
        Ok(s.to_string())
    }

    fn value(&mut self, depth: usize) -> Result<Node, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        let kind = *self.s.as_bytes().get(self.i).ok_or(())?;
        self.i += 1;
        if kind == b'N' {
            self.eat(b';')?;
            return Ok(Node::Null);
        }
        self.eat(b':')?;
        match kind {
            b'b' => {
                let t = self.until(b';')?;
                match t {
                    "0" => Ok(Node::Bool(false)),
                    "1" => Ok(Node::Bool(true)),
                    _ => Err(()),
                }
            }
            b'i' => {
                let t = self.until(b';')?;
                let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(());
                }
                Ok(Node::Num(t.to_string()))
            }
            b'd' => {
                let t = self.until(b';')?;
                let ok = matches!(t, "INF" | "-INF" | "NAN") || t.parse::<f64>().is_ok();
                if !ok {
                    return Err(());
                }
                // INF / NAN are not JSON numbers; show them as text.
                if t.parse::<f64>().is_ok_and(f64::is_finite) {
                    Ok(Node::Num(t.to_string()))
                } else {
                    Ok(Node::Str(t.to_string()))
                }
            }
            b's' => {
                let s = self.quoted()?;
                self.eat(b';')?;
                Ok(Node::Str(s))
            }
            b'a' => {
                let n = self.count(b':')?;
                self.eat(b'{')?;
                let items = self.entries(n, depth)?;
                Ok(array_node(items))
            }
            b'O' => {
                let class = self.quoted()?;
                self.eat(b':')?;
                let n = self.count(b':')?;
                self.eat(b'{')?;
                let mut items = vec![("@class".to_string(), Node::Str(class))];
                for (k, v) in self.entries(n, depth)? {
                    items.push((clean_property(&k), v));
                }
                Ok(Node::Map(items))
            }
            _ => Err(()),
        }
    }

    /// `n` key/value pairs and the closing brace.
    fn entries(&mut self, n: usize, depth: usize) -> Result<Vec<(String, Node)>, ()> {
        // Each entry takes at least 4 bytes (`N;N;`); reject lying counts
        // before reserving anything.
        if n > (self.s.len() - self.i) / 4 + 1 {
            return Err(());
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let key = match self.value(depth + 1)? {
                Node::Num(k) | Node::Str(k) => k,
                _ => return Err(()),
            };
            let value = self.value(depth + 1)?;
            items.push((key, value));
        }
        self.eat(b'}')?;
        Ok(items)
    }
}

/// A PHP array with keys 0, 1, 2, ... is a list; anything else a map.
fn array_node(items: Vec<(String, Node)>) -> Node {
    let is_list = items
        .iter()
        .enumerate()
        .all(|(i, (k, _))| *k == i.to_string());
    if is_list {
        Node::List(items.into_iter().map(|(_, v)| v).collect())
    } else {
        Node::Map(items)
    }
}

/// Private/protected properties are stored as `\0Class\0name` / `\0*\0name`.
fn clean_property(key: &str) -> String {
    match key.strip_prefix('\0') {
        Some(rest) => rest.split_once('\0').map_or(rest, |(_, n)| n).to_string(),
        None => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Option<String> {
        pretty_print(s).map(|p| p.text)
    }

    #[test]
    fn json_object_and_array() {
        let p = pretty_print(r#"{"a":1,"b":[true,null,"x"],"c":{}}"#).unwrap();
        assert_eq!(p.kind, PrettyKind::Json);
        assert_eq!(
            p.text,
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null,\n    \"x\"\n  ],\n  \"c\": {}\n}"
        );
        assert!(text("[1, 2.5e3, -4]")
            .unwrap()
            .starts_with("[\n  1,\n  2.5e3,"));
    }

    #[test]
    fn json_unicode_escapes_and_utf8() {
        let t = text(r#"{"k":"café 😀 é"}"#).unwrap();
        assert!(t.contains("café 😀 é"));
    }

    #[test]
    fn json_rejects_garbage_and_empty() {
        assert_eq!(text("{}"), None);
        assert_eq!(text("[]"), None);
        assert_eq!(text("  [ ]  "), None);
        assert_eq!(text("{\"a\":1"), None);
        assert_eq!(text("{'a':1}"), None);
        assert_eq!(text("[1,]"), None);
        assert_eq!(text("{\"a\":1} trailing"), None);
        assert_eq!(text("hello"), None);
        assert_eq!(text("123"), None);
        assert_eq!(text(""), None);
    }

    #[test]
    fn json_string_literal_wrapping_json() {
        let t = text(r#""{\"before\":{\"x\":1},\"after\":null}""#).unwrap();
        assert!(t.contains("\"before\": {\n    \"x\": 1\n  }"));
        // A plain string is not offered.
        assert_eq!(text(r#""just text""#), None);
    }

    #[test]
    fn escaped_json_without_outer_quotes() {
        let t = text(r#"{\"before\":{\"name\":\"A\"},\"after\":[1,2]}"#).unwrap();
        assert!(t.contains("\"name\": \"A\""));
        assert!(t.contains("\"after\": [\n    1,\n    2\n  ]"));
    }

    #[test]
    fn json_depth_is_limited() {
        let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
        assert_eq!(text(&deep), None);
        let ok = format!("{}1{}", "[".repeat(10), "]".repeat(10));
        assert!(text(&ok).is_some());
    }

    #[test]
    fn php_serialized_array() {
        let src = r#"a:3:{s:9:"succeeded";b:1;s:2:"at";s:25:"2026-11-20T01:00:26+07:00";s:7:"runtime";d:24.26;}"#;
        let p = pretty_print(src).unwrap();
        assert_eq!(p.kind, PrettyKind::PhpSerialized);
        assert_eq!(
            p.text,
            "{\n  \"succeeded\": true,\n  \"at\": \"2026-11-20T01:00:26+07:00\",\n  \"runtime\": 24.26\n}"
        );
    }

    #[test]
    fn php_list_nested_null_and_int() {
        let p = text(r#"a:2:{i:0;a:1:{i:0;i:-5;}i:1;N;}"#).unwrap();
        assert_eq!(p, "[\n  [\n    -5\n  ],\n  null\n]");
    }

    #[test]
    fn php_strings_use_byte_length() {
        // "é" is two bytes; the quote and brace inside must not confuse it.
        assert_eq!(text("a:1:{s:1:\"k\";s:5:\"é\";}x\";}"), None);
        let p = text("a:1:{s:1:\"k\";s:5:\"é\"}x\";}").unwrap();
        assert!(p.contains("\"é\\\"}x\""));
    }

    #[test]
    fn php_object_hides_visibility_prefix() {
        let src = "O:3:\"Foo\":2:{s:4:\"name\";s:1:\"a\";s:6:\"\0*\0age\";i:3;}";
        let p = text(src).unwrap();
        assert_eq!(
            p,
            "{\n  \"@class\": \"Foo\",\n  \"name\": \"a\",\n  \"age\": 3\n}"
        );
    }

    #[test]
    fn php_rejects_bad_input() {
        assert_eq!(text("a:0:{}"), None);
        assert_eq!(text("a:2:{i:0;i:1;}"), None); // count lies
        assert_eq!(text("a:1:{s:9:\"short\";i:1;}"), None); // length lies
        assert_eq!(text("a:1:{i:0;i:1;} junk"), None);
        assert_eq!(text("a:99999999:{}"), None);
        assert_eq!(text("a:1:{i:0;r:1;}"), None); // references unsupported
        assert_eq!(text("apple"), None);
        assert_eq!(text("a:1:{b:1;i:1;}"), None); // key must be int/string
    }

    #[test]
    fn php_depth_is_limited() {
        let mut s = String::new();
        for _ in 0..200 {
            s.push_str("a:1:{i:0;");
        }
        s.push_str("i:1;");
        for _ in 0..200 {
            s.push('}');
        }
        assert_eq!(text(&s), None);
    }

    #[test]
    fn oversized_input_is_ignored() {
        let big = format!("[{}1]", "1,".repeat(MAX_INPUT_BYTES));
        assert_eq!(text(&big), None);
    }
}
