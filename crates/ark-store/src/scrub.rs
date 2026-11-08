//! Secrets are taken out of what a conversation stores: the person's words, the answer and
//! the text of a turn's trace. Only these patterns: a password in a URL, `NAME=value` and
//! `"name": "value"` where the name holds KEY, TOKEN, SECRET or PASSWORD as a word, and the shapes of
//! Anthropic and OpenAI keys, GitHub tokens and AWS access key ids.

use serde_json::Value;

const MASK: &str = "[redacted]";

/// `text` with the listed secrets replaced.
pub fn scrub(text: &str) -> String {
    let text = url_passwords(text);
    let text = assignments(&text);
    key_shapes(&text)
}

/// Every string inside `value`, scrubbed in place.
pub fn scrub_all(value: &mut Value) {
    match value {
        Value::String(s) => *s = scrub(s),
        Value::Array(items) => items.iter_mut().for_each(scrub_all),
        Value::Object(map) => map.values_mut().for_each(scrub_all),
        _ => {}
    }
}

/// A stored turn with its input and answer scrubbed.
pub fn turn(turn: &Value) -> Value {
    let mut turn = turn.clone();
    for key in ["input", "answer"] {
        if let Some(Value::String(s)) = turn.get_mut(key) {
            *s = scrub(s);
        }
    }
    turn
}

fn url_passwords(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let run = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\''))
            .unwrap_or(tail.len());
        let redacted = tail[..run].rfind('@').and_then(|at_sign| {
            let (user, password) = tail[..at_sign].split_once(':')?;
            (!is_port_then_path(password)).then_some((user, at_sign))
        });
        match redacted {
            Some((user, at_sign)) => {
                out.push_str(user);
                out.push(':');
                out.push_str(MASK);
                out.push_str(&tail[at_sign..run]);
            }
            None => out.push_str(&tail[..run]),
        }
        rest = &tail[run..];
    }
    out.push_str(rest);
    out
}

/// `8080/a@b`: a host's port and a path, not a password.
fn is_port_then_path(text: &str) -> bool {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && text[digits..].starts_with(['/', '?', '#'])
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// A name holds KEY, TOKEN, SECRET or PASSWORD as a word: it ends the name or `_` or `-`
/// follows it (`API_KEY`, `apiKey`, `db-password`; not `keyboard` or `keys_count`).
fn sensitive(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|word| {
        upper.match_indices(word).any(|(at, _)| {
            matches!(
                upper.as_bytes().get(at + word.len()),
                None | Some(b'_' | b'-')
            )
        })
    })
}

fn assignments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        if !name_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && name_byte(b[i]) {
            i += 1;
        }
        if !sensitive(&text[start..i]) {
            continue;
        }
        let mut sep = i;
        let quoted = sep < b.len() && matches!(b[sep], b'"' | b'\'');
        if quoted {
            sep += 1;
        }
        if sep >= b.len() || !(b[sep] == b'=' || (quoted && b[sep] == b':')) {
            continue;
        }
        let mut value = sep + 1;
        if b[sep] == b':' {
            while value < b.len() && matches!(b[value], b' ' | b'\t') {
                value += 1;
            }
        }
        if value >= b.len() || b[value].is_ascii_whitespace() || text[value..].starts_with("//") {
            continue;
        }
        let end = match b[value] {
            quote @ (b'"' | b'\'') => text[value + 1..]
                .find(quote as char)
                .map_or(b.len(), |close| value + 2 + close),
            _ => text[value..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\''))
                .map_or(b.len(), |n| value + n),
        };
        if text[value..end].starts_with(MASK) {
            continue;
        }
        out.push_str(&text[copied..value]);
        out.push_str(MASK);
        copied = end;
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// A secret shape: its prefix and the bytes of its body, at least `min` of them, or exactly
/// `min` when `exact`.
struct Shape {
    prefix: &'static str,
    body: fn(u8) -> bool,
    min: usize,
    exact: bool,
}

const SHAPES: [Shape; 5] = [
    Shape {
        prefix: "sk-ant-",
        body: |b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'),
        min: 8,
        exact: false,
    },
    Shape {
        prefix: "sk-",
        body: |b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'),
        min: 20,
        exact: false,
    },
    Shape {
        prefix: "ghp_",
        body: |b| b.is_ascii_alphanumeric(),
        min: 20,
        exact: false,
    },
    Shape {
        prefix: "github_pat_",
        body: |b| b.is_ascii_alphanumeric() || b == b'_',
        min: 20,
        exact: false,
    },
    Shape {
        prefix: "AKIA",
        body: |b| b.is_ascii_uppercase() || b.is_ascii_digit(),
        min: 16,
        exact: true,
    },
];

fn key_shapes(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        let bounded = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let hit = bounded
            .then(|| {
                SHAPES.iter().find_map(|shape| {
                    let body = text.get(i..)?.strip_prefix(shape.prefix)?.as_bytes();
                    let run = body.iter().take_while(|&&c| (shape.body)(c)).count();
                    (run >= shape.min)
                        .then_some(shape.prefix.len() + if shape.exact { shape.min } else { run })
                })
            })
            .flatten();
        match hit {
            Some(len) => {
                out.push_str(&text[copied..i]);
                out.push_str(MASK);
                i += len;
                copied = i;
            }
            None => i += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::scrub;

    #[test]
    fn a_url_keeps_its_user_and_loses_its_password() {
        assert_eq!(
            scrub("DATABASE_URL=postgresql://u:p4ss@h/db"),
            "DATABASE_URL=postgresql://u:[redacted]@h/db"
        );
        assert_eq!(
            scrub("see https://example.com/a@b"),
            "see https://example.com/a@b"
        );
    }

    #[test]
    fn named_secrets_lose_their_value() {
        assert_eq!(scrub("API_KEY=abc123 next"), "API_KEY=[redacted] next");
        assert_eq!(scrub("Password: hunter2"), "Password: hunter2");
        assert_eq!(
            scrub(r#""client_secret": "abc def""#),
            r#""client_secret": [redacted]"#
        );
    }

    #[test]
    fn a_name_matches_only_with_the_word_at_its_end_or_before_a_separator() {
        for name in [
            "API_KEY",
            "apiKey",
            "SECRET_KEY_BASE",
            "GITHUB_TOKEN",
            "db-password",
        ] {
            assert_eq!(scrub(&format!("{name}=abc")), format!("{name}=[redacted]"));
        }
        for text in ["keyboard=qwerty", "tokenizer=bpe", "keys_count=3"] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn a_password_with_url_characters_is_redacted() {
        assert_eq!(
            scrub("postgresql://u:pa/ss@h/db"),
            "postgresql://u:[redacted]@h/db"
        );
        assert_eq!(
            scrub("postgresql://u:p#ss@h/db"),
            "postgresql://u:[redacted]@h/db"
        );
        assert_eq!(
            scrub("http://example.com:8080/a@b"),
            "http://example.com:8080/a@b"
        );
    }

    #[test]
    fn prose_with_a_colon_stays() {
        for text in ["Key: the first point", "Token: the unit"] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn key_shapes_lose_their_body() {
        assert_eq!(
            scrub("use sk-ant-api03-abcdefghij now"),
            "use [redacted] now"
        );
        assert_eq!(scrub("sk-abcdefghijklmnopqrstuv"), "[redacted]");
        assert_eq!(scrub("ghp_abcdefghijklmnopqrstuvwx"), "[redacted]");
        assert_eq!(scrub("id AKIAABCDEFGHIJKLMNOP."), "id [redacted].");
    }

    #[test]
    fn ordinary_code_and_prose_stay() {
        for text in [
            "let password_len = 3;",
            "the task-list has sk-1 and a key point",
            "a turn: fixed the bug",
            "caf\u{e9} \u{2713} sk-1 \u{4e2d}\u{6587}",
        ] {
            assert_eq!(scrub(text), text, "{text}");
        }
    }
}
