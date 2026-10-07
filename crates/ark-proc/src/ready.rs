//! When a process is ready, decided by code: an address on this machine in its output whose
//! port answers, or a port the agent named that answers.

use std::time::Duration;

/// Hosts that mean this machine, as servers print them.
const LOCAL: &[&str] = &["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"];

/// Output without terminal colour and cursor codes.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: parameters, then one final byte in @..~
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The first `http://` address on this machine in `text`, as `http://localhost:PORT/path`,
/// and its port.
pub fn local_address(text: &str) -> Option<(String, u16)> {
    for (at, _) in text.match_indices("http") {
        let rest = &text[at..];
        let Some(after) = rest
            .strip_prefix("http://")
            .or_else(|| rest.strip_prefix("https://"))
        else {
            continue;
        };
        let scheme = if rest.starts_with("https") {
            "https"
        } else {
            "http"
        };
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | ','))
            .unwrap_or(after.len());
        let addr = after[..end].trim_end_matches(['.', ';']);
        let (hostport, path) = match addr.find('/') {
            Some(i) => (&addr[..i], &addr[i..]),
            None => (addr, ""),
        };
        let Some(colon) = hostport.rfind(':') else {
            continue;
        };
        let (host, port) = (&hostport[..colon], &hostport[colon + 1..]);
        if !LOCAL.contains(&host) {
            continue;
        }
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let path = if path.is_empty() { "/" } else { path };
        return Some((format!("{scheme}://localhost:{port}{path}"), port));
    }
    None
}

/// Whether something on this machine accepts a connection on `port`.
pub async fn answers(port: u16) -> bool {
    let probe = |addr: &'static str| async move {
        matches!(
            tokio::time::timeout(
                Duration::from_millis(500),
                tokio::net::TcpStream::connect((addr, port)),
            )
            .await,
            Ok(Ok(_))
        )
    };
    probe("127.0.0.1").await || probe("::1").await
}

/// `base` (an address with a path) with `open` (a path such as `/home`, or a whole local
/// address) in place of its path.
pub fn with_path(base: &str, open: &str) -> String {
    let open = open.trim();
    if open.starts_with("http://") || open.starts_with("https://") {
        return open.to_string();
    }
    let root = match base.find("://").map(|i| i + 3) {
        Some(i) => match base[i..].find('/') {
            Some(j) => &base[..i + j],
            None => base,
        },
        None => base,
    };
    if open.is_empty() {
        return format!("{root}/");
    }
    let sep = if open.starts_with('/') { "" } else { "/" };
    format!("{root}{sep}{open}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_codes_and_carriage_returns_go() {
        assert_eq!(
            plain("\x1b[32m  ➜  Local:\x1b[39m   \x1b[36mhttp://localhost:\x1b[1m5173\x1b[22m/\x1b[39m\r\n"),
            "  ➜  Local:   http://localhost:5173/\n"
        );
        assert_eq!(plain("\x1b]0;title\x07ok"), "ok");
    }

    #[test]
    fn servers_print_their_address_in_many_ways() {
        let cases = [
            (
                "   - Local:        http://localhost:3000",
                "http://localhost:3000/",
                3000,
            ),
            (
                "Starting development server at http://127.0.0.1:8000/",
                "http://localhost:8000/",
                8000,
            ),
            (
                "* Listening on http://0.0.0.0:3000",
                "http://localhost:3000/",
                3000,
            ),
            (
                "Serving HTTP on :: port 8000 (http://[::]:8000/) ...",
                "http://localhost:8000/",
                8000,
            ),
            (
                "Running on http://127.0.0.1:5000.",
                "http://localhost:5000/",
                5000,
            ),
        ];
        for (line, url, port) in cases {
            assert_eq!(local_address(line), Some((url.to_string(), port)), "{line}");
        }
        assert_eq!(local_address("docs at https://nextjs.org/docs"), None);
        assert_eq!(local_address("http://localhost without a port"), None);
    }

    #[test]
    fn a_path_replaces_the_address_path() {
        assert_eq!(
            with_path("http://localhost:3000/", "/home"),
            "http://localhost:3000/home"
        );
        assert_eq!(
            with_path("http://localhost:3000/app/", "home"),
            "http://localhost:3000/home"
        );
        assert_eq!(
            with_path("http://localhost:3000/", ""),
            "http://localhost:3000/"
        );
        assert_eq!(
            with_path("http://localhost:3000/", "http://localhost:4000/x"),
            "http://localhost:4000/x"
        );
    }
}
