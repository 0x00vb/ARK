//! # ark-browser
//!
//! The Browser capability: agents open pages, read them, click and type, in the Chromium-family
//! browser the person already has (Chrome, Edge, Brave), driven over the DevTools protocol
//! ([`cdp`]). Nothing else to install. Why native and not an MCP server: ADR 0015.
//!
//! | Browser | What | Shared how |
//! |---|---|---|
//! | Fresh (default) | one headless browser; each agent gets its own context: no cookies, no history, nothing shared between agents | many agents at once |
//! | Signed in (`signed_in: true`) | a visible browser on ARK's own profile, where the person signs in to sites once | one agent at a time: the others wait until it has been idle for [`IDLE`] |
//!
//! Each agent (or conversation) gets its own [`Browser`] adapter from [`BrowserService::for_owner`];
//! its pages stay open between its turns. Opening and reading pages only observes; clicking and
//! typing change things, so they go through the approval gate. Addresses on this machine or a
//! private network are refused, as `web.fetch` refuses them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_core::{ActionDescriptor, RiskTier};
use ark_tools::{ActionOutput, Adapter, AdapterError, Result, RunCtx};
use async_trait::async_trait;
use serde_json::{json, Value};

pub mod cdp;

use cdp::Chrome;

pub const OPEN: &str = "browser.open";
pub const READ: &str = "browser.read";
pub const CLICK: &str = "browser.click";
pub const TYPE: &str = "browser.type";
pub const BACK: &str = "browser.back";
pub const ACTIONS: [&str; 5] = [OPEN, READ, CLICK, TYPE, BACK];

/// How long the signed-in browser stays with an agent after its last use, while another waits.
pub const IDLE: Duration = Duration::from_secs(60);
/// How long an agent waits for the signed-in browser before giving up.
const LEASE_WAIT: Duration = Duration::from_secs(600);
const SNAPSHOT_JS: &str = include_str!("snapshot.js");
const CARD_TEXT: usize = 800;
const CARD_ELEMENTS: usize = 40;

/// One page an agent drives.
#[derive(Clone)]
struct Page {
    chrome: Arc<Chrome>,
    session: String,
}

#[derive(Default)]
struct OwnerPages {
    fresh: Option<Page>,
    signed: Option<Page>,
    /// The page its last `open` chose.
    on_signed: bool,
}

#[derive(Default)]
struct State {
    fresh: Option<Arc<Chrome>>,
    signed: Option<Arc<Chrome>>,
    owners: HashMap<String, OwnerPages>,
    /// Who holds the signed-in browser, and when they last used it.
    lease: Option<(String, Instant)>,
}

/// The browsers of one ARK process. Cheap to share; browsers start on first use.
pub struct BrowserService {
    bin: Option<PathBuf>,
    /// Where profiles live: `fresh` (wiped at each start) and `signed-in` (kept).
    root: PathBuf,
    private: bool,
    signed_headless: bool,
    idle: Duration,
    state: tokio::sync::Mutex<State>,
}

impl BrowserService {
    /// Browsers with their profiles under `root` (an app's store root `browser` folder).
    pub fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(BrowserService {
            bin: cdp::find_browser(),
            root: root.into(),
            private: false,
            signed_headless: std::env::var_os("ARK_BROWSER_HEADLESS").is_some(),
            idle: IDLE,
            state: tokio::sync::Mutex::new(State::default()),
        })
    }

    /// Allow pages on this machine and private networks (tests, an intranet app).
    pub fn allow_private(self: Arc<Self>, yes: bool) -> Arc<Self> {
        let mut s = Arc::try_unwrap(self).unwrap_or_else(|_| panic!("set before sharing"));
        s.private = yes;
        Arc::new(s)
    }

    /// Run the signed-in browser without a window too (tests).
    pub fn signed_in_headless(self: Arc<Self>, yes: bool) -> Arc<Self> {
        let mut s = Arc::try_unwrap(self).unwrap_or_else(|_| panic!("set before sharing"));
        s.signed_headless = yes;
        Arc::new(s)
    }

    /// How long the signed-in browser stays with an idle agent while another waits (tests).
    pub fn lease_idle(self: Arc<Self>, idle: Duration) -> Arc<Self> {
        let mut s = Arc::try_unwrap(self).unwrap_or_else(|_| panic!("set before sharing"));
        s.idle = idle;
        Arc::new(s)
    }

    /// The browser ARK found, if any.
    pub fn browser(&self) -> Option<&PathBuf> {
        self.bin.as_ref()
    }

    /// The tools of one agent (or conversation), named `owner`.
    pub fn for_owner(self: &Arc<Self>, owner: &str) -> Browser {
        Browser {
            svc: self.clone(),
            owner: owner.to_string(),
        }
    }

    /// Who uses the signed-in browser now, if anyone.
    pub async fn signed_in_user(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .lease
            .as_ref()
            .map(|(o, _)| o.clone())
    }

    async fn chrome(&self, signed: bool) -> std::result::Result<Arc<Chrome>, String> {
        let bin = self.bin.clone().ok_or_else(|| {
            "no Chrome, Edge or Brave found on this computer (set ARK_BROWSER_BIN to one)"
                .to_string()
        })?;
        let mut st = self.state.lock().await;
        let slot = if signed {
            &mut st.signed
        } else {
            &mut st.fresh
        };
        if let Some(c) = slot.as_ref().filter(|c| c.alive()) {
            return Ok(c.clone());
        }
        let profile = self.root.join(if signed { "signed-in" } else { "fresh" });
        if !signed {
            let _ = tokio::fs::remove_dir_all(&profile).await;
        }
        let headless = !signed || self.signed_headless;
        let c = Arc::new(Chrome::launch(&bin, &profile, headless).await?);
        *slot = Some(c.clone());
        // Pages of a browser that stopped are gone.
        for pages in st.owners.values_mut() {
            if signed {
                pages.signed = None;
            } else {
                pages.fresh = None;
            }
        }
        Ok(c)
    }

    /// Wait until `owner` holds the signed-in browser.
    async fn lease(&self, owner: &str) -> std::result::Result<(), String> {
        let until = Instant::now() + LEASE_WAIT;
        loop {
            {
                let mut st = self.state.lock().await;
                let free = match &st.lease {
                    None => true,
                    Some((o, last)) => o == owner || last.elapsed() >= self.idle,
                };
                if free {
                    st.lease = Some((owner.to_string(), Instant::now()));
                    return Ok(());
                }
                if Instant::now() >= until {
                    let holder = st
                        .lease
                        .as_ref()
                        .map(|(o, _)| o.clone())
                        .unwrap_or_default();
                    return Err(format!(
                        "the signed-in browser stayed in use by {holder}; try again later or use a fresh browser"
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The page `owner` drives now: its fresh one, or (holding the lease) its signed-in one.
    async fn page(&self, owner: &str, signed: bool) -> std::result::Result<Page, String> {
        if signed {
            self.lease(owner).await?;
        }
        let chrome = self.chrome(signed).await?;
        {
            let st = self.state.lock().await;
            let have = st.owners.get(owner).and_then(|p| {
                if signed {
                    p.signed.clone()
                } else {
                    p.fresh.clone()
                }
            });
            if let Some(p) = have {
                return Ok(p);
            }
        }
        // A context of its own in the fresh browser; a tab in the signed-in one.
        let mut target = json!({ "url": "about:blank" });
        if !signed {
            let ctx = chrome
                .call(None, "Target.createBrowserContext", json!({}))
                .await?;
            target["browserContextId"] = ctx["browserContextId"].clone();
        }
        let made = chrome.call(None, "Target.createTarget", target).await?;
        let attached = chrome
            .call(
                None,
                "Target.attachToTarget",
                json!({ "targetId": made["targetId"], "flatten": true }),
            )
            .await?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("the browser gave no page session")?
            .to_string();
        chrome
            .call(Some(&session), "Page.enable", json!({}))
            .await?;
        let page = Page { chrome, session };
        let mut st = self.state.lock().await;
        let pages = st.owners.entry(owner.to_string()).or_default();
        if signed {
            pages.signed = Some(page.clone());
        } else {
            pages.fresh = Some(page.clone());
        }
        Ok(page)
    }

    async fn current(&self, owner: &str) -> std::result::Result<Page, String> {
        let signed = {
            let st = self.state.lock().await;
            st.owners.get(owner).is_some_and(|p| p.on_signed)
        };
        self.page(owner, signed).await
    }

    /// Mark the signed-in browser as just used by `owner`, when it holds it.
    async fn touch(&self, owner: &str) {
        let mut st = self.state.lock().await;
        if let Some((o, last)) = st.lease.as_mut() {
            if o == owner {
                *last = Instant::now();
            }
        }
    }

    async fn choose(&self, owner: &str, signed: bool) {
        let mut st = self.state.lock().await;
        st.owners.entry(owner.to_string()).or_default().on_signed = signed;
    }
}

impl Page {
    async fn eval(&self, expr: &str) -> std::result::Result<Value, String> {
        let r = self
            .chrome
            .call(
                Some(&self.session),
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(format!(
                "the page refused: {}",
                e["exception"]["description"]
                    .as_str()
                    .or(e["text"].as_str())
                    .unwrap_or("error")
            ));
        }
        Ok(r["result"]["value"].clone())
    }

    /// Wait for the page to finish loading (at most 15 s), then a moment for scripts.
    async fn settle(&self) {
        let until = Instant::now() + Duration::from_secs(15);
        tokio::time::sleep(Duration::from_millis(150)).await;
        while Instant::now() < until {
            if self
                .eval("document.readyState")
                .await
                .ok()
                .as_ref()
                .and_then(Value::as_str)
                == Some("complete")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn snapshot(&self) -> std::result::Result<Value, String> {
        let mut v = self.eval(SNAPSHOT_JS).await?;
        let lines = v["text"].as_str().map(|t| t.lines().count()).unwrap_or(0);
        v["lines"] = json!(lines);
        Ok(v)
    }

    async fn navigate(&self, url: &str) -> std::result::Result<(), String> {
        let r = self
            .chrome
            .call(Some(&self.session), "Page.navigate", json!({ "url": url }))
            .await?;
        if let Some(e) = r["errorText"].as_str() {
            return Err(format!("could not open {url}: {e}"));
        }
        self.settle().await;
        Ok(())
    }

    /// The middle of element `r`, scrolled into view. `None`: no such element on the page.
    async fn spot(&self, r: u64) -> std::result::Result<Option<(f64, f64)>, String> {
        let v = self
            .eval(&format!(
                "(() => {{ const el = document.querySelector('[data-ark-ref=\"{r}\"]'); if (!el) return null; el.scrollIntoView({{block: 'center', inline: 'center'}}); const b = el.getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2]; }})()"
            ))
            .await?;
        Ok(v.as_array()
            .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))))
    }

    async fn click(&self, x: f64, y: f64) -> std::result::Result<(), String> {
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            self.chrome
                .call(
                    Some(&self.session),
                    "Input.dispatchMouseEvent",
                    json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
                )
                .await?;
        }
        Ok(())
    }

    async fn key_enter(&self) -> std::result::Result<(), String> {
        for kind in ["keyDown", "keyUp"] {
            let mut ev = json!({ "type": kind, "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13 });
            if kind == "keyDown" {
                ev["text"] = json!("\r");
            }
            self.chrome
                .call(Some(&self.session), "Input.dispatchKeyEvent", ev)
                .await?;
        }
        Ok(())
    }
}

/// One agent's browser tools ([`BrowserService::for_owner`]).
#[derive(Clone)]
pub struct Browser {
    svc: Arc<BrowserService>,
    owner: String,
}

fn tool(e: String) -> AdapterError {
    AdapterError::Tool(e)
}

fn element_ref(action: &str, input: &Value) -> Result<u64> {
    input["ref"]
        .as_u64()
        .or_else(|| {
            input["ref"]
                .as_str()
                .and_then(|s| s.trim_start_matches('[').trim_end_matches(']').parse().ok())
        })
        .ok_or_else(|| {
            AdapterError::bad_input(
                action,
                "`ref` is the number of an element in the page's list",
            )
        })
}

impl Browser {
    async fn open(&self, input: &Value) -> Result<ActionOutput> {
        let raw = input["url"].as_str().unwrap_or("").trim();
        let url = reqwest::Url::parse(raw)
            .or_else(|_| reqwest::Url::parse(&format!("https://{raw}")))
            .map_err(|_| AdapterError::bad_input(OPEN, format!("`{raw}` is not an address")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AdapterError::bad_input(
                OPEN,
                "only http and https pages can be opened",
            ));
        }
        if !self.svc.private {
            ark_tools::web::check_public(&url).await.map_err(|e| {
                tool(format!(
                    "{e}. This browser opens public pages only. To show the person a page on \
                     this machine, show it in their browser view instead (it opens any address)."
                ))
            })?;
        }
        let signed = input["signed_in"].as_bool().unwrap_or(false);
        let page = self.svc.page(&self.owner, signed).await.map_err(tool)?;
        self.svc.choose(&self.owner, signed).await;
        page.navigate(url.as_str()).await.map_err(tool)?;
        self.done(&page, signed).await
    }

    async fn done(&self, page: &Page, signed: bool) -> Result<ActionOutput> {
        let mut snap = page.snapshot().await.map_err(tool)?;
        if signed {
            self.svc.touch(&self.owner).await;
        }
        snap["signed_in"] = json!(signed);
        Ok(ActionOutput::json(snap))
    }

    async fn act(&self, action: &str, input: &Value) -> Result<ActionOutput> {
        let signed = {
            let st = self.svc.state.lock().await;
            st.owners.get(&self.owner).is_some_and(|p| p.on_signed)
        };
        let page = self.svc.current(&self.owner).await.map_err(tool)?;
        match action {
            READ => {}
            BACK => {
                page.eval("history.back()").await.map_err(tool)?;
                page.settle().await;
            }
            CLICK => {
                let r = element_ref(action, input)?;
                let (x, y) = page.spot(r).await.map_err(tool)?.ok_or_else(|| {
                    tool(format!(
                        "no element [{r}] on this page; browser.read lists them"
                    ))
                })?;
                page.click(x, y).await.map_err(tool)?;
                page.settle().await;
            }
            TYPE => {
                let r = element_ref(action, input)?;
                let text = input["text"].as_str().unwrap_or("");
                let found = page
                    .eval(&format!(
                        "(() => {{ const el = document.querySelector('[data-ark-ref=\"{r}\"]'); if (!el) return false; el.scrollIntoView({{block: 'center'}}); el.focus(); if (el.select) el.select(); else if (el.isContentEditable) document.execCommand('selectAll'); return true; }})()"
                    ))
                    .await
                    .map_err(tool)?;
                if found != json!(true) {
                    return Err(tool(format!(
                        "no element [{r}] on this page; browser.read lists them"
                    )));
                }
                page.chrome
                    .call(
                        Some(&page.session),
                        "Input.insertText",
                        json!({ "text": text }),
                    )
                    .await
                    .map_err(tool)?;
                if input["submit"].as_bool().unwrap_or(false) {
                    page.key_enter().await.map_err(tool)?;
                    page.settle().await;
                }
            }
            other => return Err(AdapterError::UnknownAction(other.into())),
        }
        self.done(&page, signed).await
    }
}

#[async_trait]
impl Adapter for Browser {
    fn name(&self) -> &str {
        "browser"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let el = |d: &str| json!({ "type": "integer", "description": d });
        vec![
            ActionDescriptor::new(
                OPEN,
                "open a page in a browser and see its text and the elements you can act on, each with a number",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object", "required": ["url"],
                "properties": {
                    "url": { "type": "string" },
                    "signed_in": { "type": "boolean", "description": "use the person's signed-in browser (their accounts on sites); default: a fresh browser" }
                }
            })),
            ActionDescriptor::new(READ, "see the current page again", RiskTier::Observe).volatile(),
            ActionDescriptor::new(BACK, "go back one page", RiskTier::Observe).volatile(),
            ActionDescriptor::new(CLICK, "click an element of the current page, by its number", RiskTier::Mutate)
                .with_input_schema(json!({
                    "type": "object", "required": ["ref"],
                    "properties": { "ref": el("the element's number from the page's list") }
                })),
            ActionDescriptor::new(TYPE, "type into a field of the current page, by its number; submit presses Enter", RiskTier::Mutate)
                .with_input_schema(json!({
                    "type": "object", "required": ["ref", "text"],
                    "properties": {
                        "ref": el("the field's number from the page's list"),
                        "text": { "type": "string", "description": "replaces what the field holds" },
                        "submit": { "type": "boolean" }
                    }
                })),
        ]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            OPEN => self.open(&input).await,
            READ | BACK | CLICK | TYPE => self.act(action, &input).await,
            other => Err(AdapterError::UnknownAction(other.into())),
        }
    }
}

/// The evidence card of a page: where it is, its first elements, the start of its text.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    if !ACTIONS.contains(&action) {
        return None;
    }
    let s = |k: &str| out[k].as_str().unwrap_or("");
    let elements = out["elements"].as_array()?;
    let mut card = format!(
        "\"{}\" {}{} ({} lines, {} elements)",
        s("title"),
        s("url"),
        if out["signed_in"] == json!(true) {
            " · signed in"
        } else {
            ""
        },
        out["lines"].as_u64().unwrap_or(0),
        elements.len()
    );
    for e in elements.iter().take(CARD_ELEMENTS) {
        let mut line = format!(
            "\n[{}] {} \"{}\"",
            e["ref"],
            e["role"].as_str().unwrap_or(""),
            e["name"].as_str().unwrap_or("")
        );
        if let Some(v) = e["value"].as_str() {
            line.push_str(&format!(" = \"{v}\""));
        }
        if let Some(c) = e["checked"].as_bool() {
            line.push_str(if c { " (checked)" } else { " (not checked)" });
        }
        card.push_str(&line);
    }
    if elements.len() > CARD_ELEMENTS {
        card.push_str("\n(more elements: get_evidence on this result, path \"elements\")");
    }
    let text = s("text");
    let head: String = text.chars().take(CARD_TEXT).collect();
    if !head.is_empty() {
        card.push_str("\nText:\n");
        card.push_str(&head);
        if text.chars().count() > CARD_TEXT {
            card.push_str("\n(the rest: get_evidence on this result, path \"text\", with `match`)");
        }
    }
    Some((card, vec![s("url").to_string()]))
}

/// The browser as a per-agent tool: each owner drives its own pages.
pub struct Browsers(pub Arc<BrowserService>);

impl ark_runtime::per_agent::PerAgent for Browsers {
    fn listing(&self) -> Arc<dyn Adapter> {
        Arc::new(self.0.for_owner(""))
    }
    fn for_owner(&self, owner: &ark_runtime::per_agent::Owner) -> Arc<dyn Adapter> {
        Arc::new(self.0.for_owner(owner.name))
    }
    fn distiller(&self) -> Option<ark_runtime::Distiller> {
        Some(Arc::new(distill))
    }
}
