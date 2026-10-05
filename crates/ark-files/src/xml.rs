//! A small XML tokenizer for the Office formats. It gives each tag and text run with its byte
//! range, so a format can rewrite one element of a part and copy every other byte as it was.
//! Office parts are well formed and have no DTD; comments, processing instructions and CDATA
//! are skipped or read as text.

use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tok<'a> {
    /// `<name attrs>` or `<name attrs/>` (`empty`).
    Open {
        name: &'a str,
        attrs: &'a str,
        empty: bool,
        start: usize,
        end: usize,
    },
    /// `</name>`.
    Close {
        name: &'a str,
        start: usize,
        end: usize,
    },
    /// Text between tags, still escaped.
    Text {
        raw: &'a str,
        start: usize,
        end: usize,
    },
}

impl<'a> Tok<'a> {
    pub fn start(&self) -> usize {
        match *self {
            Tok::Open { start, .. } | Tok::Close { start, .. } | Tok::Text { start, .. } => start,
        }
    }
    pub fn end(&self) -> usize {
        match *self {
            Tok::Open { end, .. } | Tok::Close { end, .. } | Tok::Text { end, .. } => end,
        }
    }
}

pub struct Lexer<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(s: &'a str) -> Self {
        Lexer { s, pos: 0 }
    }
    pub fn at(s: &'a str, pos: usize) -> Self {
        Lexer { s, pos }
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        loop {
            let s = self.s;
            let b = s.as_bytes();
            if self.pos >= b.len() {
                return None;
            }
            let start = self.pos;
            if b[start] != b'<' {
                let end = s[start..].find('<').map_or(s.len(), |i| start + i);
                self.pos = end;
                return Some(Tok::Text {
                    raw: &s[start..end],
                    start,
                    end,
                });
            }
            let rest = &s[start..];
            if rest.starts_with("<!--") {
                self.pos = rest.find("-->").map_or(s.len(), |i| start + i + 3);
                continue;
            }
            if rest.starts_with("<![CDATA[") {
                let end = rest.find("]]>").map_or(s.len(), |i| start + i + 3);
                self.pos = end;
                let inner = &s[start + 9..end.saturating_sub(3).max(start + 9)];
                return Some(Tok::Text {
                    raw: inner,
                    start,
                    end,
                });
            }
            if rest.starts_with("<?") || rest.starts_with("<!") {
                self.pos = rest.find('>').map_or(s.len(), |i| start + i + 1);
                continue;
            }
            // A tag: find its `>` outside quoted attribute values.
            let mut i = start + 1;
            let mut quote = 0u8;
            while i < b.len() {
                let c = b[i];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                i += 1;
            }
            let end = (i + 1).min(s.len());
            self.pos = end;
            let inner = &s[start + 1..i.min(s.len())];
            if let Some(name) = inner.strip_prefix('/') {
                return Some(Tok::Close {
                    name: name.trim(),
                    start,
                    end,
                });
            }
            let (inner, empty) = match inner.strip_suffix('/') {
                Some(x) => (x, true),
                None => (inner, false),
            };
            let split = inner
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(inner.len());
            return Some(Tok::Open {
                name: &inner[..split],
                attrs: &inner[split..],
                empty,
                start,
                end,
            });
        }
    }
}

/// The value of attribute `key` (with its prefix, `r:id`), unescaped.
pub fn attr<'a>(attrs: &'a str, key: &str) -> Option<Cow<'a, str>> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let eq = rest.find('=')?;
        let name = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let q = after.chars().next()?;
        if q != '"' && q != '\'' {
            return None;
        }
        let close = after[1..].find(q)? + 1;
        let value = &after[1..close];
        if name == key {
            return Some(unescape(value));
        }
        rest = &after[close + 1..];
    }
}

/// Every attribute as `(name, raw value)`, in order.
pub fn attrs(attrs: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else {
            return out;
        };
        let name = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            return out;
        };
        let Some(close) = after[1..].find(q).map(|i| i + 1) else {
            return out;
        };
        out.push((name, &after[1..close]));
        rest = &after[close + 1..];
    }
}

/// The local part of a prefixed name (`w:p` is `p`).
pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';').filter(|n| *n <= 12) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Text or an attribute value, escaped. Characters XML 1.0 cannot carry are dropped.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// The byte range of the element that opens at `open` (its start tag's start) up to the end
/// of its matching close tag.
pub fn element_end(s: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for t in Lexer::at(s, open) {
        match t {
            Tok::Open {
                empty: true, end, ..
            } if depth == 0 => return end,
            Tok::Open { empty: false, .. } => depth += 1,
            Tok::Close { end, .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return end;
                }
            }
            _ => {}
        }
    }
    s.len()
}

/// The first element named `name` (with its prefix) at or after `from`: (start, end).
pub fn find(s: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    Lexer::at(s, from).find_map(|t| match t {
        Tok::Open { name: n, start, .. } if n == name => Some((start, element_end(s, start))),
        _ => None,
    })
}

/// All the text inside `s` that sits in elements whose local name is `t`, unescaped.
pub fn text_of(s: &str, t: &str) -> String {
    let mut out = String::new();
    let mut inside = 0usize;
    for tok in Lexer::new(s) {
        match tok {
            Tok::Open {
                name, empty: false, ..
            } if local(name) == t => inside += 1,
            Tok::Close { name, .. } if local(name) == t => inside = inside.saturating_sub(1),
            Tok::Text { raw, .. } if inside > 0 => out.push_str(&unescape(raw)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_carry_their_ranges() {
        let s = r#"<?xml version="1.0"?><a x="1 > 2"><b/>t &amp; u<!-- c --></a>"#;
        let toks: Vec<Tok> = Lexer::new(s).collect();
        assert_eq!(toks.len(), 4);
        match toks[0] {
            Tok::Open { name, attrs, .. } => {
                assert_eq!(name, "a");
                assert_eq!(attr(attrs, "x").as_deref(), Some("1 > 2"));
            }
            _ => panic!(),
        }
        assert!(matches!(
            toks[1],
            Tok::Open {
                name: "b",
                empty: true,
                ..
            }
        ));
        match toks[2] {
            Tok::Text { raw, .. } => assert_eq!(unescape(raw), "t & u"),
            _ => panic!(),
        }
        let (a0, a1) = find(s, "a", 0).unwrap();
        assert!(s[a0..a1].starts_with("<a ") && s[a0..a1].ends_with("</a>"));
    }

    #[test]
    fn escapes_round_trip() {
        let t = "a<b & \"c\" 'd' é &#233;";
        assert_eq!(unescape(&escape(t)), t);
        assert_eq!(unescape("&#x41;&#66;&bogus;"), "AB&bogus;");
    }
}
