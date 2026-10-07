//! Reading stored tool output back into the context, a slice at a time. The full output of
//! every call stays in the store; the model sees a distilled card, and `get_evidence` returns
//! the part it asks for:
//!
//! - `path`: a dotted key path into the output (`lines`, `result.rows`).
//! - `match`: keep only array items, or lines of a string, that contain this text (any case).
//!   Each kept item carries its original index, so the model can cite it or page around it.
//! - `offset` / `limit`: a window over the (filtered) items, for reading more.
//!
//! Nothing is summarized by a model and nothing is lost: what does not fit is counted and the
//! reply says how to get the next window.

use serde_json::{json, Value};

/// Largest reply, in characters.
pub const MAX_CHARS: usize = 6000;

/// What the model asked for.
#[derive(Clone, Debug, Default)]
pub struct Query<'a> {
    pub path: Option<&'a str>,
    pub matching: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

/// The requested slice of `output`, as text for the model.
pub fn extract(output: &Value, q: &Query) -> String {
    let mut cur = output;
    if let Some(p) = q.path.filter(|p| !p.is_empty()) {
        for seg in p.split('.').filter(|s| !s.is_empty()) {
            let next = match cur {
                Value::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
                other => other.get(seg),
            };
            match next {
                Some(v) => cur = v,
                None => {
                    return format!(
                        "no such path `{p}` in this evidence (top-level keys: {}).",
                        top_keys(output)
                    )
                }
            }
        }
    }
    let limit = q.limit.max(1);
    let needle = q
        .matching
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_lowercase);

    // A long multi-line string is paged like an array of lines.
    let lines: Option<Vec<Value>> = match cur {
        Value::String(s) if needle.is_some() || s.lines().count() > limit => {
            Some(s.lines().map(|l| Value::String(l.to_string())).collect())
        }
        _ => None,
    };
    let items: Option<&[Value]> = match (&lines, cur) {
        (Some(l), _) => Some(l.as_slice()),
        (None, Value::Array(a)) => Some(a.as_slice()),
        _ => None,
    };

    if let Some(items) = items {
        return window(items, needle.as_deref(), q.offset, limit);
    }
    if let (Some(n), Value::Object(m)) = (&needle, cur) {
        // No list selected: filter inside each top-level entry that mentions the needle.
        let mut parts = Vec::new();
        for (k, v) in m {
            let lines: Vec<Value>;
            let items: Option<&[Value]> = match v {
                Value::Array(a) => Some(a.as_slice()),
                Value::String(s) if s.contains('\n') => {
                    lines = s.lines().map(|l| Value::String(l.to_string())).collect();
                    Some(lines.as_slice())
                }
                _ => None,
            };
            match items {
                Some(items) if items.iter().any(|x| text_of(x).to_lowercase().contains(n)) => {
                    parts.push(format!("{k}: {}", window(items, Some(n), q.offset, limit)));
                }
                Some(_) => {}
                None if k.to_lowercase().contains(n) || text_of(v).to_lowercase().contains(n) => {
                    parts.push(format!("{k}: {}", one_line(v)));
                }
                None => {}
            }
        }
        if parts.is_empty() {
            return format!("nothing here matches `{n}`.");
        }
        return clip(parts.join("\n"));
    }
    clip(serde_json::to_string_pretty(&cap_arrays(cur, limit)).unwrap_or_default())
}

/// A window over `items`, filtered by `needle`, with original indices and a "read more" hint.
fn window(items: &[Value], needle: Option<&str>, offset: usize, limit: usize) -> String {
    let selected: Vec<(usize, &Value)> = items
        .iter()
        .enumerate()
        .filter(|(_, v)| match needle {
            None => true,
            Some(n) => text_of(v).to_lowercase().contains(n),
        })
        .collect();
    let total = selected.len();
    let what = match needle {
        Some(n) => format!("{total} of {} items match `{n}`", items.len()),
        None => format!("{total} items"),
    };
    if total == 0 {
        return format!("{what}.");
    }
    let mut out = what;
    let shown: Vec<&(usize, &Value)> = selected.iter().skip(offset).take(limit).collect();
    let mut used = out.len();
    let mut printed = 0usize;
    let mut body = String::new();
    for (i, v) in &shown {
        let line = format!("\n[{i}] {}", one_line(v));
        if used + line.len() > MAX_CHARS {
            break;
        }
        used += line.len();
        body.push_str(&line);
        printed += 1;
    }
    let end = offset + printed;
    if offset > 0 || end < total {
        out.push_str(&format!(
            " (showing {}-{}{})",
            offset + 1,
            end,
            if end < total {
                format!("; pass offset={end} for more")
            } else {
                String::new()
            }
        ));
    }
    out.push(':');
    out.push_str(&body);
    out
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn one_line(v: &Value) -> String {
    let t = text_of(v).replace('\n', " ");
    if t.chars().count() > 400 {
        let s: String = t.chars().take(400).collect();
        format!("{s}…")
    } else {
        t
    }
}

fn clip(body: String) -> String {
    if body.chars().count() > MAX_CHARS {
        let t: String = body.chars().take(MAX_CHARS).collect();
        format!("{t}\n… (truncated; narrow with `path` or `match`)")
    } else {
        body
    }
}

pub fn cap_arrays(v: &Value, limit: usize) -> Value {
    match v {
        Value::Array(a) => {
            let mut out: Vec<Value> = a.iter().take(limit).map(|x| cap_arrays(x, limit)).collect();
            if a.len() > limit {
                out.push(json!(format!(
                    "… (+{} more of {}; use offset or match)",
                    a.len() - limit,
                    a.len()
                )));
            }
            Value::Array(out)
        }
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), cap_arrays(x, limit)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn top_keys(v: &Value) -> String {
    // Sorted: an object keeps its keys in the order they were written.
    v.as_object()
        .map(|m| {
            let mut k: Vec<&String> = m.keys().collect();
            k.sort();
            k.into_iter().cloned().collect::<Vec<_>>().join(", ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logs() -> Value {
        let lines: Vec<String> = (0..3000)
            .map(|i| {
                if i == 1873 {
                    "ERROR charge failed code=E4721".to_string()
                } else {
                    format!("INFO ok {i}")
                }
            })
            .collect();
        json!({ "service": "payments", "lines": lines })
    }

    fn q<'a>(path: Option<&'a str>, m: Option<&'a str>, offset: usize) -> Query<'a> {
        Query {
            path,
            matching: m,
            offset,
            limit: 40,
        }
    }

    #[test]
    fn match_finds_one_line_among_thousands_with_its_index() {
        let out = extract(&logs(), &q(Some("lines"), Some("error"), 0));
        assert!(out.starts_with("1 of 3000 items match `error`"), "{out}");
        assert!(
            out.contains("[1873] ERROR charge failed code=E4721"),
            "{out}"
        );
        assert!(out.len() < 200);
    }

    #[test]
    fn match_without_path_searches_the_whole_object() {
        let out = extract(&logs(), &q(None, Some("E4721"), 0));
        assert!(
            out.starts_with("lines: 1 of 3000 items match `e4721`"),
            "{out}"
        );
        assert!(
            out.contains("[1873] ERROR charge failed code=E4721"),
            "{out}"
        );
        assert!(!out.contains("INFO"), "only matching items: {out}");
    }

    #[test]
    fn offset_pages_through_items() {
        let first = extract(&logs(), &q(Some("lines"), None, 0));
        assert!(
            first.contains("(showing 1-40; pass offset=40 for more)"),
            "{first}"
        );
        assert!(first.contains("[39] INFO ok 39"));
        let next = extract(&logs(), &q(Some("lines"), None, 40));
        assert!(next.contains("[40] INFO ok 40"), "{next}");
        assert!(next.contains("pass offset=80"));
        let last = extract(&logs(), &q(Some("lines"), None, 2990));
        assert!(last.contains("(showing 2991-3000)"), "{last}");
        assert!(!last.contains("pass offset"));
    }

    #[test]
    fn a_long_string_is_paged_by_line() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let out = extract(
            &json!({ "text": text }),
            &q(Some("text"), Some("line 17"), 0),
        );
        // "line 17" and "line 170" to "line 179".
        assert!(out.starts_with("11 of 200 items match `line 17`"), "{out}");
        assert!(out.contains("[17] line 17") && out.contains("[179] line 179"));
    }

    #[test]
    fn unknown_path_lists_the_keys() {
        let out = extract(&logs(), &q(Some("nope"), None, 0));
        assert!(out.contains("top-level keys: lines, service"), "{out}");
    }

    #[test]
    fn plain_reads_keep_the_old_shape() {
        let out = extract(&json!({ "a": [1, 2, 3], "b": "x" }), &q(None, None, 0));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], json!([1, 2, 3]));
    }
}
