//! Whether a page lets the Browser panel show it in a frame (`frame_check`).
//!
//! A page in a plain browser's Browser panel is an `<iframe>`. Pages that refuse frames are
//! blanked by the browser without telling the page around them, so the panel asks first and
//! says when a page refuses. The window shows pages in a view of their own and needs no check.
//!
//! Only the response's headers are read, of public addresses only: a runtime on a server never
//! probes its own machine or network. When the page cannot be asked or reached the panel loads
//! it as before.

use std::time::Duration;

use ark_sdk::prelude::ark_tools::web::{check_public, public_client};

/// Why a page refuses to be shown in another page's frame, from its response headers; `None`
/// when it allows it. As browsers decide: an enforced `frame-ancestors` decides alone, and
/// refuses unless it lists `*`; without one, `X-Frame-Options` refuses when it says `DENY` or
/// only `SAMEORIGIN` (any other value is ignored). Report-only policies do not refuse.
pub(crate) fn refusal<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let mut xfo: Vec<String> = Vec::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-frame-options") {
            xfo.extend(
                value
                    .split(',')
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty()),
            );
        }
        if name.eq_ignore_ascii_case("content-security-policy") {
            for directive in value.split(';') {
                let mut words = directive.split_ascii_whitespace();
                if !words
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case("frame-ancestors"))
                {
                    continue;
                }
                return (!words.any(|w| w == "*"))
                    .then(|| format!("Content-Security-Policy: {}", directive.trim()));
            }
        }
    }
    let refuses = xfo.iter().any(|v| v == "deny")
        || (!xfo.is_empty() && xfo.iter().all(|v| v == "sameorigin"));
    refuses.then(|| format!("X-Frame-Options: {}", xfo.join(", ").to_ascii_uppercase()))
}

/// Ask `url` for its headers (redirects followed, the body not read) and say whether a frame
/// may show it: `{ "framable": bool, "reason"?: string }`. Anything not asked (not http or
/// https, this machine or a private network) or not answered in a few seconds is
/// `framable: true`.
pub(crate) async fn check(url: &str) -> serde_json::Value {
    let framable = serde_json::json!({ "framable": true });
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return framable;
    };
    // An address written as numbers never reaches the client's resolver: refused here first.
    if !matches!(parsed.scheme(), "http" | "https") || check_public(&parsed).await.is_err() {
        return framable;
    }
    let ask = public_client()
        .get(parsed)
        .header("accept", "text/html,*/*")
        .send();
    let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), ask).await else {
        return framable;
    };
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str(), v.to_str().ok()?)))
        .collect::<Vec<_>>();
    match refusal(headers) {
        Some(reason) => serde_json::json!({ "framable": false, "reason": reason }),
        None => framable,
    }
}

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn x_frame_options_refuses_on_deny_or_sameorigin_only() {
        assert!(refusal([("X-Frame-Options", "SAMEORIGIN")]).is_some());
        assert!(refusal([("x-frame-options", "deny")]).is_some());
        assert!(refusal([("X-Frame-Options", "sameorigin, deny")]).is_some());
        assert!(refusal([
            ("X-Frame-Options", "SAMEORIGIN"),
            ("X-Frame-Options", "SAMEORIGIN")
        ])
        .is_some());
        // Browsers ignore what they do not know, and show the page.
        assert!(refusal([("X-Frame-Options", "ALLOW-FROM https://a.example")]).is_none());
        assert!(refusal([("X-Frame-Options", "ALLOWALL")]).is_none());
        assert!(refusal([("X-Frame-Options", "sameorigin, allowall")]).is_none());
        assert!(refusal([("X-Frame-Options", " ")]).is_none());
    }

    #[test]
    fn frame_ancestors_decides_over_x_frame_options() {
        let both = |xfo, csp| refusal([("X-Frame-Options", xfo), ("Content-Security-Policy", csp)]);
        assert!(both("DENY", "frame-ancestors *").is_none());
        assert!(both("ALLOWALL", "frame-ancestors 'none'").is_some());
    }

    #[test]
    fn frame_ancestors_refuses_unless_it_lists_every_origin() {
        let csp = |v| refusal([("Content-Security-Policy", v)]);
        assert!(csp("default-src 'self'; frame-ancestors 'self'; base-uri 'self'").is_some());
        assert!(csp("frame-ancestors 'none'").is_some());
        assert!(csp("frame-ancestors https://a.example").is_some());
        assert!(csp("FRAME-ANCESTORS *").is_none());
        assert!(csp("default-src 'self'; frame-src 'self'").is_none());
        assert!(refusal([(
            "Content-Security-Policy-Report-Only",
            "frame-ancestors 'none'"
        )])
        .is_none());
    }

    #[test]
    fn a_page_without_either_header_may_be_framed() {
        assert!(refusal([("content-type", "text/html")]).is_none());
        assert!(refusal(Vec::<(&str, &str)>::new()).is_none());
    }

    /// A runtime never asks its own machine or network: those answer "framable" unasked.
    #[tokio::test]
    async fn private_addresses_are_not_asked() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(super::check(&url).await["framable"], true);
        assert!(
            listener.accept().is_err(),
            "the check connected to this machine"
        );
        assert_eq!(super::check("http://10.0.0.1/").await["framable"], true);
        assert_eq!(super::check("file:///etc/passwd").await["framable"], true);
    }

    /// The page that showed white in ARK Desktop (a Next.js app whose headers refuse frames).
    #[test]
    fn the_next_js_app_that_showed_white_refuses() {
        let headers = [
            ("X-Frame-Options", "SAMEORIGIN"),
            ("Content-Security-Policy", "default-src 'self'; frame-ancestors 'self'; frame-src 'self' https://www.facebook.com"),
        ];
        assert_eq!(
            refusal(headers).as_deref(),
            Some("Content-Security-Policy: frame-ancestors 'self'")
        );
    }
}
