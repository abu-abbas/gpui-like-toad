//! Minimal JSON value with a hand-written parser and serializer.

// ---------------------------------------------------------------------------
// Minimal JSON
// ---------------------------------------------------------------------------

/// Minimal JSON value: enough for the small settings files this module
/// owns. Objects preserve insertion order (a `Vec` of pairs) so files
/// stay human-readable and stable across saves.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        }
    }
}

/// Parse a JSON document. Returns a human-readable error on malformed
/// input; the store's loaders treat that as "use the default".
pub fn parse_json(input: &str) -> Result<Json, String> {
    parse_json_with_depth(input, 64)
}

pub(crate) fn parse_json_with_depth(input: &str, max_depth: usize) -> Result<Json, String> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
        max_depth,
    };
    p.skip_ws();
    let value = p.parse_value(0)?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(format!("trailing characters at offset {}", p.pos));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    max_depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.next() == Some(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' at offset {}", self.pos))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Json, String> {
        if depth >= self.max_depth {
            return Err("JSON is nested too deeply".into());
        }
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(depth + 1),
            Some('[') => self.parse_array(depth + 1),
            Some('"') => Ok(Json::Str(self.parse_string()?)),
            Some('t') => self.parse_literal("true", Json::Bool(true)),
            Some('f') => self.parse_literal("false", Json::Bool(false)),
            Some('n') => self.parse_literal("null", Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{c}' at offset {}", self.pos)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn parse_literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for expected in word.chars() {
            if self.next() != Some(expected) {
                return Err(format!("invalid literal at offset {}", self.pos));
            }
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number '{text}' at offset {start}"))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next() {
                None => return Err("unterminated string".to_string()),
                Some('"') => return Ok(out),
                Some('\\') => {
                    let esc = self.next().ok_or("unterminated escape")?;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000C}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // High surrogate: expect a \uXXXX low
                                // surrogate right after.
                                if self.next() == Some('\\') && self.next() == Some('u') {
                                    let lo = self.parse_hex4()?;
                                    let combined =
                                        0x10000 + ((cp - 0xD800) << 10) + lo.wrapping_sub(0xDC00);
                                    out.push(
                                        char::from_u32(combined)
                                            .unwrap_or(char::REPLACEMENT_CHARACTER),
                                    );
                                } else {
                                    return Err(format!(
                                        "unpaired surrogate at offset {}",
                                        self.pos
                                    ));
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
                            }
                        }
                        other => {
                            return Err(format!(
                                "invalid escape '\\{other}' at offset {}",
                                self.pos
                            ))
                        }
                    }
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let c = self.next().ok_or("truncated \\u escape")?;
            let digit = c.to_digit(16).ok_or("invalid \\u escape")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.next() {
                Some(',') => {}
                Some(']') => return Ok(Json::Arr(items)),
                _ => return Err(format!("expected ',' or ']' at offset {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Json, String> {
        self.expect('{')?;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(format!("expected object key at offset {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value(depth)?;
            pairs.push((key, value));
            self.skip_ws();
            match self.next() {
                Some(',') => {}
                Some('}') => return Ok(Json::Obj(pairs)),
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.pos)),
            }
        }
    }
}

/// Serialize a value as compact JSON. Strings are escaped per RFC 8259;
/// non-ASCII characters pass through as UTF-8.
pub fn to_json(value: &Json) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_json(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9_007_199_254_740_992.0 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Json::Str(s) => write_json_string(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
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
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
