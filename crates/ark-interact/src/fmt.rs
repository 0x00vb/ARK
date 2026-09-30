//! Small text formatters shared by reports and front-ends, so a number reads the same in the
//! terminal and the window.

use time::OffsetDateTime;

/// `12345` → `12k`, `1840` → `1.8k`, `950` → `950`.
pub fn tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Collapse whitespace and clip to `max` characters, ending in `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// `3m ago`, `2h ago`, `4d ago`.
pub fn ago(t: OffsetDateTime) -> String {
    let secs = (OffsetDateTime::now_utc() - t).whole_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `1234` ms → `1.2s`; under a second → `340ms`.
pub fn millis(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// `n thing` or `n things`.
pub fn count(n: u64, thing: &str) -> String {
    if n == 1 {
        format!("1 {thing}")
    } else {
        format!("{n} {thing}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1840), "1.8k");
        assert_eq!(tokens(64_000), "64k");
        assert_eq!(clip("a  b\nc", 10), "a b c");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(millis(340), "340ms");
        assert_eq!(millis(1250), "1.2s");
        assert_eq!(count(1, "tool"), "1 tool");
        assert_eq!(count(3, "tool"), "3 tools");
    }
}
