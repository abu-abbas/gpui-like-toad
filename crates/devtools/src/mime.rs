//! Just enough MIME for a mail viewer: base64, quoted-printable, RFC 2047
//! encoded words in headers, charsets (UTF-8, Latin-1), and HTML as plain
//! text for the preview.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Lenient base64: whitespace and invalid characters are skipped.
pub fn base64_decode(input: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

/// Quoted-printable; `header` also maps `_` to a space (RFC 2047 "Q").
pub fn qp_decode(input: &str, header: bool) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'=' => {
                // Soft line break.
                if bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
                    i += 3;
                    continue;
                }
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 2;
                    continue;
                }
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok());
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'=');
                        i += 1;
                    }
                }
            }
            b'_' if header => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Bytes in `charset` as text. UTF-8 and ASCII are decoded as such,
/// Latin-1 / Windows-1252 byte by byte; anything else falls back to lossy
/// UTF-8.
pub fn decode_charset(bytes: &[u8], charset: &str) -> String {
    let cs = charset.trim().trim_matches('"').to_ascii_lowercase();
    if matches!(
        cs.as_str(),
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" | "iso-8859-15"
    ) {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decode RFC 2047 encoded words (`=?UTF-8?B?...?=`) in a header value.
pub fn decode_header(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let (before, after) = rest.split_at(start);
        let parsed = (|| {
            let body = &after[2..];
            let (charset, body) = body.split_once('?')?;
            let (enc, body) = body.split_once('?')?;
            let end = body.find("?=")?;
            let text = &body[..end];
            let bytes = match enc.to_ascii_uppercase().as_str() {
                "B" => base64_decode(text),
                "Q" => qp_decode(text, true),
                _ => return None,
            };
            let charset = charset.split('*').next().unwrap_or(charset);
            Some((
                decode_charset(&bytes, charset),
                2 + charset.len() + 1 + enc.len() + 1 + end + 2,
            ))
        })();
        match parsed {
            Some((text, consumed)) => {
                // Whitespace between two encoded words is dropped.
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&text);
                rest = &after[consumed.min(after.len())..];
                last_was_word = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &after[2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A parameter of a header value: `charset` of `text/plain; charset=utf-8`.
pub fn header_param(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        let k = k.trim().trim_end_matches('*');
        k.eq_ignore_ascii_case(name).then(|| {
            let v = v.trim().trim_matches('"');
            // RFC 2231 `utf-8''name.pdf`
            match v.split_once("''") {
                Some((_, encoded)) => {
                    String::from_utf8_lossy(&qp_decode(&encoded.replace('%', "="), false))
                        .into_owned()
                }
                None => decode_header(v),
            }
        })
    })
}

/// The bare media type of a Content-Type value, lower case.
pub fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// HTML as readable plain text: drops `<head>`, scripts and styles, turns
/// block ends into line breaks and decodes common entities. Not a renderer,
/// only a preview.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(end_rel) = html[i..].find('>') else {
                break;
            };
            let tag = &lower[i + 1..i + end_rel];
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let closing = tag.starts_with('/');
            i += end_rel + 1;
            if !closing && matches!(name.as_str(), "script" | "style" | "head" | "title") {
                let close = format!("</{name}");
                match lower[i..].find(&close) {
                    Some(p) => {
                        i += p;
                        if let Some(e) = html[i..].find('>') {
                            i += e + 1;
                        }
                    }
                    None => break,
                }
                continue;
            }
            match name.as_str() {
                "br" => out.push('\n'),
                "p" | "div" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "ul"
                | "ol" | "blockquote" | "section" | "article" | "header" | "footer" => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                "li" if !closing => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("• ");
                }
                "td" | "th" if closing => out.push(' '),
                _ => {}
            }
            continue;
        }
        let next = html[i..].find('<').map_or(html.len(), |p| i + p);
        out.push_str(&decode_entities(&html[i..next]));
        i = next;
    }
    // Collapse runs of spaces and blank lines.
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(&line);
        text.push('\n');
    }
    text.trim().to_string()
}

/// `href` targets of links in `html` (http, https and mailto), in order,
/// without duplicates.
pub fn html_links(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(p) = lower[from..].find("href") {
        let mut i = from + p + 4;
        from = i;
        let bytes = lower.as_bytes();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let (start, end) = match bytes.get(i) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let Some(e) = lower[i + 1..].find(q as char) else {
                    break;
                };
                (i + 1, i + 1 + e)
            }
            Some(_) => {
                let e = lower[i..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .map_or(lower.len(), |e| i + e);
                (i, e)
            }
            None => break,
        };
        let url = decode_entities(html[start..end].trim());
        let l = url.to_ascii_lowercase();
        if (l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:"))
            && !out.contains(&url)
        {
            out.push(url);
        }
        from = end;
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let c = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "copy" => Some('©'),
            "hellip" => Some('…'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "user:pass", "héllo wörld"] {
            assert_eq!(base64_decode(&base64_encode(s.as_bytes())), s.as_bytes());
        }
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn quoted_printable() {
        assert_eq!(qp_decode("caf=C3=A9 =\r\nok", false), "café ok".as_bytes());
        assert_eq!(qp_decode("a_b", true), b"a b");
    }

    #[test]
    fn encoded_words() {
        assert_eq!(
            decode_header("=?UTF-8?B?UmVzZXQgcGFzc3dvcmQ=?="),
            "Reset password"
        );
        assert_eq!(
            decode_header("=?utf-8?Q?Caf=C3=A9?= =?utf-8?Q?_ok?= done"),
            "Café ok done"
        );
        assert_eq!(decode_header("plain"), "plain");
        assert_eq!(decode_header("=?bad"), "=?bad");
    }

    #[test]
    fn params() {
        assert_eq!(
            header_param("text/plain; charset=\"UTF-8\"", "charset").as_deref(),
            Some("UTF-8")
        );
        assert_eq!(
            header_param("attachment; filename=report.pdf", "filename").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(media_type("Text/HTML; charset=utf-8"), "text/html");
    }

    #[test]
    fn links() {
        let html = r#"<a href="https://x.test/reset?a=1&amp;b=2">Reset</a> <a href='mailto:a@b.c'>m</a> <a href=#top>t</a> <a HREF = "https://x.test/reset?a=1&amp;b=2">again</a>"#;
        assert_eq!(
            html_links(html),
            ["https://x.test/reset?a=1&b=2", "mailto:a@b.c"]
        );
    }

    #[test]
    fn html_preview() {
        let html = "<html><head><style>p{}</style></head><body><p>Hi&nbsp;Dewi,</p><p>Click <a href='x'>here</a> &amp; go</p><ul><li>One</li><li>Two</li></ul><script>x()</script></body></html>";
        assert_eq!(
            html_to_text(html),
            "Hi Dewi,\nClick here & go\n• One\n• Two"
        );
    }
}
