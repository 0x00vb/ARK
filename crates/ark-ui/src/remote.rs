//! Clients of a runtime served elsewhere (ADR 0016): [`Client`] calls its protocol over HTTP,
//! and `run_window` (feature `desktop`) opens a native window on it.
//!
//! The window loads the page the runtime serves, not a copy of its own, so the page always
//! matches the runtime's version; it talks to it over the browser transport (HTTP and
//! server-sent events), like any browser.

use serde_json::{json, Value};

/// Calls a served runtime's methods ([`crate::Host::call`] on the other side).
#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// A runtime at `url` (`https://ark.example.com`), reached with an access token.
    pub fn new(url: &str, token: &str) -> Client {
        Client {
            base: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// The runtime's address, without a trailing slash.
    pub fn url(&self) -> &str {
        &self.base
    }

    /// One protocol call: what `ark.call(method, params)` does in the page.
    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let r = self
            .http
            .post(format!("{}/rpc", self.base))
            .bearer_auth(&self.token)
            .json(&json!({ "method": method, "params": params }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("cannot reach the ARK runtime at {}: {e}", self.base))?;
        match r.status().as_u16() {
            200 => {}
            401 => anyhow::bail!("the ARK runtime at {} refused the access token", self.base),
            s => anyhow::bail!("the ARK runtime at {} answered HTTP {s}", self.base),
        }
        let v: Value = r.json().await?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v["result"].clone())
        } else {
            anyhow::bail!("{}", v["error"].as_str().unwrap_or("the call failed"))
        }
    }

    /// The runtime's `hello`: its version, current session and running turn. Checks that the
    /// address and the token work.
    pub async fn hello(&self) -> anyhow::Result<Value> {
        self.call("hello", json!({})).await
    }

    /// The page's address with the token, for a window or a browser to open. The page keeps
    /// the token for the tab and removes it from the address bar.
    pub fn page_url(&self) -> String {
        format!("{}/?token={}", self.base, self.token)
    }
}

/// Open the native window on the runtime at `url`. The window's title bar is the system's:
/// the remote page does not know it sits under window controls. If the runtime cannot be
/// reached, or refuses the token, the window says so instead. `context` is the app's
/// `tauri::generate_context!()`; its first window is used.
#[cfg(feature = "desktop")]
pub fn run_window(
    url: &str,
    token: &str,
    mut context: tauri::Context<tauri::Wry>,
) -> std::process::ExitCode {
    let client = Client::new(url, token);
    let target = match tauri::async_runtime::block_on(client.hello()) {
        Ok(_) => client.page_url(),
        Err(e) => unreachable_page(url, &format!("{e:#}")),
    };
    let Ok(target) = target.parse::<tauri::Url>() else {
        eprintln!("error: `{url}` is not an address");
        return std::process::ExitCode::FAILURE;
    };
    let config = context.config_mut();
    // A remote page must not see the window's IPC.
    config.app.with_global_tauri = false;
    if let Some(w) = config.app.windows.first_mut() {
        w.url = tauri::WebviewUrl::External(target);
        w.title_bar_style = tauri::TitleBarStyle::Visible;
        w.hidden_title = false;
    }
    match tauri::Builder::default().run(context) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// A small page that says the runtime could not be opened, as a `data:` address.
#[cfg(feature = "desktop")]
fn unreachable_page(url: &str, error: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>ARK</title>\
<body style=\"font:15px system-ui;max-width:34em;margin:4em auto;padding:0 1em;color:#1d2433\">\
<h2>ARK on the other computer isn't answering</h2>\
<p>Check that the other computer is on and ARK is open there, then open ARK again.</p>\
<details style=\"color:#6b7280\"><summary>Details</summary><p>{}</p><p>{}</p></details>",
        esc(url),
        esc(error)
    );
    let mut out = String::from("data:text/html;charset=utf-8,");
    for b in html.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
