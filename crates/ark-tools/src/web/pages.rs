//! Keyless search: no key and no account. Two sources, both used only when the model has no
//! search of its own or its search failed:
//!
//! | Source | Gives | Limits |
//! |---|---|---|
//! | Brave's public result page ([`ResultPage::brave`]) | general web results | a page meant for people: answers 429 after a few automated queries in a row, and its markup changes without notice |
//! | Wikipedia's search API ([`Wikipedia`]) | encyclopedia articles | Wikipedia only; a documented API that allows automated use |
//!
//! Measured 2026-10-03 from one machine: Brave gave relevant results, then 429 for at least
//! several minutes after about ten queries; DuckDuckGo and Mojeek answered with a challenge;
//! Bing gave unrelated pages, so it is not used. The parsers are tested against trimmed copies
//! of real replies.

use async_trait::async_trait;

use serde_json::Value;

use super::html::{attr, strip_tags};
use super::{Hit, SearchBackend};
use crate::{AdapterError, Result};

/// A desktop browser's user agent: the result pages below serve automated clients nothing.
pub(super) const BROWSER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

/// One engine's result page: where to ask and how to read the answer.
pub struct ResultPage {
    name: &'static str,
    url: &'static str,
    param: &'static str,
    /// Fixed query parameters (language, region).
    extra: &'static [(&'static str, &'static str)],
    parse: fn(&str) -> Vec<Hit>,
    client: reqwest::Client,
}

impl ResultPage {
    /// Brave Search's result page.
    pub fn brave(client: reqwest::Client) -> Self {
        ResultPage {
            name: "brave",
            url: "https://search.brave.com/search",
            param: "q",
            extra: &[("source", "web")],
            parse: parse_brave,
            client,
        }
    }
}

#[async_trait]
impl SearchBackend for ResultPage {
    fn name(&self) -> String {
        self.name.to_string()
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let resp = self
            .client
            .get(self.url)
            .query(&[(self.param, query)])
            .query(self.extra)
            .header("user-agent", BROWSER_AGENT)
            .header("accept-language", "en-US,en;q=0.9")
            .send()
            .await
            .map_err(|e| AdapterError::Tool(format!("{}: {e}", self.name)))?;
        if !resp.status().is_success() {
            return Err(AdapterError::Tool(format!(
                "{} answered {}",
                self.name,
                resp.status()
            )));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| AdapterError::Tool(format!("{}: {e}", self.name)))?;
        let mut hits = (self.parse)(&body);
        hits.truncate(limit);
        Ok(hits)
    }
}

/// Wikipedia's search API (English Wikipedia).
pub struct Wikipedia {
    client: reqwest::Client,
}

impl Wikipedia {
    pub fn new(client: reqwest::Client) -> Self {
        Wikipedia { client }
    }
}

/// Wikimedia asks automated clients to name themselves.
const ARK_AGENT: &str = concat!("ARK/", env!("CARGO_PKG_VERSION"), " (https://aiark.dev)");

#[async_trait]
impl SearchBackend for Wikipedia {
    fn name(&self) -> String {
        "wikipedia".into()
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let limit = limit.to_string();
        let resp = self
            .client
            .get("https://en.wikipedia.org/w/api.php")
            .query(&[
                ("action", "query"),
                ("list", "search"),
                ("format", "json"),
                ("srsearch", query),
                ("srlimit", limit.as_str()),
            ])
            .header("user-agent", ARK_AGENT)
            .send()
            .await
            .map_err(|e| AdapterError::Tool(format!("wikipedia: {e}")))?;
        if !resp.status().is_success() {
            return Err(AdapterError::Tool(format!(
                "wikipedia answered {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AdapterError::Tool(format!("wikipedia: {e}")))?;
        Ok(parse_wikipedia(&body))
    }
}

fn parse_wikipedia(body: &Value) -> Vec<Hit> {
    body["query"]["search"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let title = r["title"].as_str()?;
            Some(Hit {
                title: title.to_string(),
                url: format!(
                    "https://en.wikipedia.org/wiki/{}",
                    title.replace(' ', "_").replace('?', "%3F")
                ),
                snippet: strip_tags(r["snippet"].as_str().unwrap_or("")),
            })
        })
        .collect()
}

/// Each part of the page from one `marker` to the next. The last part stops after 20,000
/// bytes, so the rest of the page is not read as a result.
fn blocks<'a>(page: &'a str, marker: &str) -> Vec<&'a str> {
    let mut parts: Vec<&str> = page.split(marker).skip(1).collect();
    if let Some(last) = parts.last_mut() {
        let mut end = last.len().min(20_000);
        while !last.is_char_boundary(end) {
            end -= 1;
        }
        *last = &last[..end];
    }
    parts
}

/// The inner HTML of the first element after `class_prefix` (`class="title`), up to `close`.
fn inner_after<'a>(block: &'a str, class_prefix: &str, close: &str) -> Option<&'a str> {
    let at = block.find(class_prefix)?;
    let start = block[at..].find('>')? + at + 1;
    let stop = block[start..].find(close)? + start;
    Some(&block[start..stop])
}

/// The first `<a href="http…">` of a block, skipping links to `skip` (the engine's own).
fn first_link(block: &str, skip: Option<&str>) -> Option<String> {
    let mut from = 0;
    while let Some(at) = block[from..].find("<a ") {
        let start = from + at + 1;
        let end = block[start..].find('>')? + start;
        from = end;
        if let Some(href) = attr(&block[start..end], "href") {
            if href.starts_with("http") && !skip.is_some_and(|own| href.contains(own)) {
                return Some(href);
            }
        }
    }
    None
}

fn parse_brave(page: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for block in blocks(page, r#"data-type="web""#) {
        let Some(url) = first_link(block, Some("brave.com")) else {
            continue;
        };
        let title = inner_after(block, r#"class="title"#, "</div>")
            .map(strip_tags)
            .unwrap_or_default();
        let snippet = inner_after(block, r#"class="content"#, "</div>")
            .map(strip_tags)
            .unwrap_or_default();
        if title.is_empty() || hits.iter().any(|h| h.url == url) {
            continue;
        }
        hits.push(Hit {
            title,
            url,
            snippet,
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real Brave result page (2026-10-03): wrappers and icons cut, the
    // elements the parser reads kept as served.
    const BRAVE: &str = r#"<main class="main-column"><div class="snippet svelte-jmfu5f" data-pos="0" data-type="web" data-keynav="true"><div class="result-body"><div class="result-content"><a href="https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html" target="_self" class="svelte-14r20fy l1"><div class="site-name-wrapper"><img src="https://imgs.search.brave.com/x" alt=""/><div class="t-secondary">Docs.rs</div><cite class="snippet-url">docs.rs <span>› tokio</span></cite></div> <div class="title search-snippet-title line-clamp-1 svelte-14r20fy" title="Semaphore in tokio::sync - Rust">Semaphore in tokio::sync - Rust</div></a><div class="generic-snippet"><div class="content desktop-default-regular t-primary line-clamp-dynamic svelte-1cwdgg3"><!---->Each task spawned is given a reference to the semaphore by cloning the Arc&lt;Semaphore&gt;.<!----></div></div></div></div></div><div class="snippet" data-pos="1" data-type="web"><a href="https://search.brave.com/ad">ad</a><a href="https://dev.to/semaphores"><div class="title x">Mastering Tokio Semaphores</div></a><div class="content x">A tutorial.</div></div></main>"#;

    #[test]
    fn brave_results_are_read_and_its_own_links_skipped() {
        let hits = parse_brave(BRAVE);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].url,
            "https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html"
        );
        assert_eq!(hits[0].title, "Semaphore in tokio::sync - Rust");
        assert_eq!(
            hits[0].snippet,
            "Each task spawned is given a reference to the semaphore by cloning the Arc<Semaphore>."
        );
        assert_eq!(hits[1].url, "https://dev.to/semaphores");
        assert_eq!(hits[1].snippet, "A tutorial.");
    }

    // The shape of a real reply (2026-10-03), one result kept.
    #[test]
    fn wikipedia_results_link_to_their_articles() {
        let body = serde_json::json!({ "query": { "search": [
            { "ns": 0, "title": "1755 Lisbon earthquake", "pageid": 352339,
              "snippet": "The <span class=\"searchmatch\">1755</span> Lisbon earthquake, also known as the Great Lisbon earthquake, occurred on the morning of Saturday" }
        ]}});
        let hits = parse_wikipedia(&body);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].url,
            "https://en.wikipedia.org/wiki/1755_Lisbon_earthquake"
        );
        assert!(hits[0]
            .snippet
            .starts_with("The 1755 Lisbon earthquake, also known"));
        assert!(parse_wikipedia(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn a_challenge_page_gives_no_results() {
        let page = "<html><body>Please complete the following challenge to confirm this search was made by a human.</body></html>";
        assert!(parse_brave(page).is_empty());
    }
}
