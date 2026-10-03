//! Web access for any agent: `web.search` finds pages and `web.fetch` reads one as text. Both
//! only observe ([`RiskTier::Observe`]): they run without asking.
//!
//! Search keeps no index of its own. It asks [`SearchBackend`]s in order and takes the first
//! that finds something: the model's own search when the app gives one
//! ([`WebAdapter::search_with`]), then keyless sources ([`ResultPage`], [`Wikipedia`]; their
//! limits are in `web/pages.rs`). Every model
//! sees the same tool, and its results are stored and cited like any other tool output.
//!
//! Fetch reads http(s) pages only, at most [`MAX_BYTES`], and turns HTML into lines of text with
//! the page's links apart. It refuses loopback, private and link-local addresses (also when a
//! name resolves to one, and on every redirect), so a page cannot send an agent to the
//! machine's own services. [`WebAdapter::allow_private`] lifts that for tests or an intranet app.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ark_core::{ActionDescriptor, RiskTier};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{ActionOutput, Adapter, AdapterError, Result, RunCtx};

mod html;
mod pages;

pub use html::{Link, Page};
pub use pages::{ResultPage, Wikipedia};

pub const SEARCH: &str = "web.search";
pub const FETCH: &str = "web.fetch";

/// The most a fetch downloads; a longer body is cut there.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// The most text a fetch keeps, in characters.
const MAX_TEXT: usize = 200_000;
const MAX_LINKS: usize = 60;
const DEFAULT_RESULTS: usize = 6;
const MAX_RESULTS: usize = 10;
/// How much of a page its evidence card shows; the rest is read with `get_evidence`.
const CARD_TEXT: usize = 1500;

/// One search result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// A place `web.search` gets results from.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Shown with the results (`brave`, `claude sonnet`).
    fn name(&self) -> String;
    /// Up to `limit` results. Empty: nothing found, or this backend cannot search; the next
    /// backend is tried either way.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>>;
}

/// The `web` adapter: `web.search` and `web.fetch`.
#[derive(Clone)]
pub struct WebAdapter {
    client: reqwest::Client,
    backends: Vec<Arc<dyn SearchBackend>>,
    private: bool,
}

impl Default for WebAdapter {
    fn default() -> Self {
        WebAdapter::new()
    }
}

impl WebAdapter {
    /// Fetch, and search with the keyless sources: Brave's result page, then Wikipedia.
    pub fn new() -> Self {
        let client = client(false);
        WebAdapter {
            backends: vec![
                Arc::new(ResultPage::brave(client.clone())),
                Arc::new(Wikipedia::new(client.clone())),
            ],
            client,
            private: false,
        }
    }

    /// Ask `backend` before the ones already set (the model's own search goes first).
    pub fn search_with(mut self, backend: Arc<dyn SearchBackend>) -> Self {
        self.backends.insert(0, backend);
        self
    }

    /// Search with exactly these backends.
    pub fn backends(mut self, backends: Vec<Arc<dyn SearchBackend>>) -> Self {
        self.backends = backends;
        self
    }

    /// Let fetch read loopback and private addresses. Off by default.
    pub fn allow_private(mut self, yes: bool) -> Self {
        self.private = yes;
        self.client = client(yes);
        self
    }

    async fn search(&self, input: &Value) -> Result<ActionOutput> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if query.is_empty() {
            return Err(AdapterError::bad_input(
                SEARCH,
                "missing string field `query`",
            ));
        }
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_RESULTS, |n| n as usize)
            .clamp(1, MAX_RESULTS);
        let mut tried = Vec::new();
        for b in &self.backends {
            match b.search(query, limit).await {
                Ok(hits) if !hits.is_empty() => {
                    return Ok(ActionOutput::json(json!({
                        "query": query,
                        "engine": b.name(),
                        "results": hits,
                    })));
                }
                Ok(_) => tried.push(format!("{}: no results", b.name())),
                Err(e) => tried.push(format!("{}: {e}", b.name())),
            }
        }
        if tried.is_empty() {
            return Err(AdapterError::Tool("web search has no engine".into()));
        }
        Ok(ActionOutput::json(json!({
            "query": query,
            "results": [],
            "note": format!("no engine returned results ({})", tried.join("; ")),
        })))
    }

    async fn fetch(&self, input: &Value) -> Result<ActionOutput> {
        let raw = input
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let url = reqwest::Url::parse(raw)
            .map_err(|e| AdapterError::bad_input(FETCH, format!("`{raw}` is not a URL: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AdapterError::bad_input(
                FETCH,
                "only http and https addresses can be read",
            ));
        }
        if !self.private {
            if let Some(why) = local_host(&url) {
                return Err(AdapterError::Tool(why));
            }
        }
        let mut resp = self
            .client
            .get(url.clone())
            .header("user-agent", pages::BROWSER_AGENT)
            .header(
                "accept",
                "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5",
            )
            .send()
            .await
            .map_err(|e| AdapterError::Tool(format!("{url}: {}", plain_error(&e))))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AdapterError::Tool(format!("{url} answered {status}")));
        }
        let at = resp.url().clone();
        let kind = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_ascii_lowercase();
        let html = kind.contains("html");
        if !html && !is_text(&kind) {
            return Err(AdapterError::Tool(format!(
                "{at} is {kind}; web.fetch reads HTML and text pages"
            )));
        }
        let mut body = Vec::new();
        let mut cut = false;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AdapterError::Tool(format!("{at}: {}", plain_error(&e))))?
        {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_BYTES {
                body.truncate(MAX_BYTES);
                cut = true;
                break;
            }
        }
        let body = String::from_utf8_lossy(&body);
        let mut page = if html {
            html::read(&body, Some(&at))
        } else {
            Page {
                text: body.trim().to_string(),
                ..Page::default()
            }
        };
        if page.text.chars().count() > MAX_TEXT {
            page.text = page.text.chars().take(MAX_TEXT).collect();
            cut = true;
        }
        page.links.truncate(MAX_LINKS);
        let mut out = json!({
            "url": at.as_str(),
            "title": page.title,
            "lines": page.text.lines().count(),
            "text": page.text,
            "links": page.links,
        });
        if cut {
            out["truncated"] = json!(true);
        }
        Ok(ActionOutput::json(out))
    }
}

#[async_trait]
impl Adapter for WebAdapter {
    fn name(&self) -> &str {
        "web"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                SEARCH,
                "search the web: titles, links and a sentence from each page",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "what to look for, as typed in a search box" },
                    "limit": { "type": "integer", "description": "results to return, 1 to 10 (default 6)" }
                }
            })),
            ActionDescriptor::new(
                FETCH,
                "read a web page as text, with its title and links",
                RiskTier::Observe,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "description": "an http or https address" }
                }
            })),
        ]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            SEARCH => self.search(&input).await,
            FETCH => self.fetch(&input).await,
            other => Err(AdapterError::UnknownAction(other.into())),
        }
    }
}

/// The evidence card of a web result: search results whole (title, link, snippet), and the
/// start of a fetched page with how to read the rest. `None` for any other action.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| out.get(k).and_then(Value::as_str).unwrap_or("");
    match action {
        SEARCH => {
            let hits = out.get("results").and_then(Value::as_array)?;
            if hits.is_empty() {
                let note = s("note");
                return Some((format!("no results for \"{}\". {note}", s("query")), vec![]));
            }
            let mut text = format!(
                "{} result(s) for \"{}\" ({}):",
                hits.len(),
                s("query"),
                s("engine")
            );
            let mut urls = Vec::new();
            for (i, h) in hits.iter().enumerate() {
                let f = |k: &str| h.get(k).and_then(Value::as_str).unwrap_or("");
                text.push_str(&format!("\n{}. {} {}", i + 1, f("title"), f("url")));
                if !f("snippet").is_empty() {
                    text.push_str(&format!("\n   {}", clip(f("snippet"), 200)));
                }
                urls.push(f("url").to_string());
            }
            Some((text, urls))
        }
        FETCH => {
            let body = s("text");
            let lines = out.get("lines").and_then(Value::as_u64).unwrap_or(0);
            let mut text = format!("\"{}\" {} ({lines} lines):\n", s("title"), s("url"));
            text.push_str(&clip(body, CARD_TEXT));
            if body.chars().count() > CARD_TEXT {
                text.push_str(
                    "\n(the rest: get_evidence on this result, path \"text\", with `match` for a passage)",
                );
            }
            Some((text, vec![s("url").to_string()]))
        }
        _ => None,
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn is_text(kind: &str) -> bool {
    kind.starts_with("text/") || kind.contains("json") || kind.contains("xml")
}

/// A reqwest error without its URL (the caller adds it), with its causes.
fn plain_error(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    if let Some(u) = e.url() {
        msg = msg.replace(&format!(" for url ({u})"), "");
    }
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg = format!("{msg}: {s}");
        src = s.source();
    }
    msg
}

/// Why `url` names this machine or a private network, when it does by its host alone.
fn local_host(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let blocked = match bare.parse::<IpAddr>() {
        Ok(ip) => private_ip(ip),
        Err(_) => {
            let d = bare.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
    };
    blocked.then(|| format!("refused: {host} is this machine or a private network"))
}

/// Loopback, private, link-local, shared (CGNAT) and unspecified addresses.
fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg & 0xfe00) == 0xfc00
                || (seg & 0xffc0) == 0xfe80
        }
    }
}

/// Resolves names to their public addresses only.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> = all.into_iter().filter(|a| !private_ip(a.ip())).collect();
            if public.is_empty() {
                return Err(format!("refused: {host} is this machine or a private network").into());
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn client(private: bool) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10));
    if !private {
        b = b
            .dns_resolver(Arc::new(PublicOnly))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 8 {
                    attempt.error("too many redirects")
                } else if let Some(why) = local_host(attempt.url()) {
                    attempt.error(why)
                } else {
                    attempt.follow()
                }
            }));
    }
    b.build().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlobResolver;
    use ark_core::ArtifactId;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct NoBlobs;
    impl BlobResolver for NoBlobs {
        fn path(&self, _id: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    /// A one-request HTTP server on loopback that answers with `kind` and `body`.
    async fn serve(kind: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{addr}/page")
    }

    struct Fixed(&'static str, Vec<Hit>);
    #[async_trait]
    impl SearchBackend for Fixed {
        fn name(&self) -> String {
            self.0.into()
        }
        async fn search(&self, _q: &str, limit: usize) -> Result<Vec<Hit>> {
            Ok(self.1.iter().take(limit).cloned().collect())
        }
    }

    struct Broken;
    #[async_trait]
    impl SearchBackend for Broken {
        fn name(&self) -> String {
            "broken".into()
        }
        async fn search(&self, _q: &str, _l: usize) -> Result<Vec<Hit>> {
            Err(AdapterError::Tool("challenge page".into()))
        }
    }

    fn hit(n: u8) -> Hit {
        Hit {
            title: format!("Page {n}"),
            url: format!("https://example.org/{n}"),
            snippet: format!("About {n}."),
        }
    }

    #[tokio::test]
    async fn a_page_is_read_as_text_with_its_links() {
        let url = serve(
            "text/html; charset=utf-8",
            "<html><head><title>Notes</title></head><body><h2>Kettle</h2><p>Boil &amp; wait.</p><a href=\"/next\">Next page</a></body></html>",
        )
        .await;
        let web = WebAdapter::new().allow_private(true);
        let ctx = RunCtx::new(&NoBlobs);
        let out = web
            .run(FETCH, json!({ "url": url }), &ctx)
            .await
            .unwrap()
            .output;
        assert_eq!(out["title"], "Notes");
        assert_eq!(out["text"], "## Kettle\nBoil & wait.\nNext page");
        assert_eq!(out["links"][0]["text"], "Next page");
        assert!(out["links"][0]["url"].as_str().unwrap().ends_with("/next"));
        let (card, ids) = distill(FETCH, &out).unwrap();
        assert!(card.starts_with("\"Notes\" http://127.0.0.1:"), "{card}");
        assert!(
            card.ends_with("## Kettle\nBoil & wait.\nNext page"),
            "{card}"
        );
        assert_eq!(ids, vec![url]);
    }

    #[tokio::test]
    async fn this_machine_and_private_networks_are_refused() {
        let url = serve("text/plain", "secret").await;
        let web = WebAdapter::new();
        let ctx = RunCtx::new(&NoBlobs);
        for target in [
            url.as_str(),
            "http://localhost:9/",
            "http://10.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let e = web
                .run(FETCH, json!({ "url": target }), &ctx)
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(e.contains("refused"), "{target}: {e}");
        }
        let e = web
            .run(FETCH, json!({ "url": "file:///etc/passwd" }), &ctx)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("http and https"), "{e}");
    }

    #[tokio::test]
    async fn a_binary_page_is_refused_by_its_type() {
        let url = serve("application/pdf", "%PDF-1.4").await;
        let web = WebAdapter::new().allow_private(true);
        let e = web
            .run(FETCH, json!({ "url": url }), &RunCtx::new(&NoBlobs))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("application/pdf"), "{e}");
    }

    #[tokio::test]
    async fn search_takes_the_first_engine_that_finds_something() {
        let web = WebAdapter::new().backends(vec![
            Arc::new(Broken),
            Arc::new(Fixed("empty", vec![])),
            Arc::new(Fixed("second", vec![hit(1), hit(2), hit(3)])),
        ]);
        let ctx = RunCtx::new(&NoBlobs);
        let out = web
            .run(SEARCH, json!({ "query": "kettles", "limit": 2 }), &ctx)
            .await
            .unwrap()
            .output;
        assert_eq!(out["engine"], "second");
        assert_eq!(out["results"].as_array().unwrap().len(), 2);
        let (card, ids) = distill(SEARCH, &out).unwrap();
        assert_eq!(
            card,
            "2 result(s) for \"kettles\" (second):\n1. Page 1 https://example.org/1\n   About 1.\n2. Page 2 https://example.org/2\n   About 2."
        );
        assert_eq!(ids, vec!["https://example.org/1", "https://example.org/2"]);
    }

    #[tokio::test]
    async fn a_search_nothing_answers_says_what_each_engine_did() {
        let web =
            WebAdapter::new().backends(vec![Arc::new(Broken), Arc::new(Fixed("empty", vec![]))]);
        let out = web
            .run(
                SEARCH,
                json!({ "query": "kettles" }),
                &RunCtx::new(&NoBlobs),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(out["results"], json!([]));
        assert_eq!(
            out["note"],
            "no engine returned results (broken: tool error: challenge page; empty: no results)"
        );
        assert!(web
            .run(SEARCH, json!({ "query": " " }), &RunCtx::new(&NoBlobs))
            .await
            .is_err());
    }

    #[test]
    fn private_addresses_are_recognized() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "1.1.1.1", "2606:4700::1111", "100.128.0.1"] {
            assert!(!private_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
