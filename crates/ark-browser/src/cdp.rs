//! A small client for the Chrome DevTools Protocol: start a browser with a profile folder,
//! connect to it over WebSocket, send commands and wait for their answers. Pages are driven
//! through flattened target sessions (`sessionId` on each command).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// How long a command may take before the browser counts as stuck.
const COMMAND_TIME: Duration = Duration::from_secs(30);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A running browser and the connection to it. Dropping it stops the browser.
pub struct Chrome {
    child: Mutex<Option<tokio::process::Child>>,
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.start_kill();
        }
    }
}

impl Chrome {
    /// Start `bin` on `profile` (a folder it keeps cookies and sign-ins in) and connect.
    /// `headless`: no window.
    pub async fn launch(bin: &Path, profile: &Path, headless: bool) -> Result<Chrome, String> {
        tokio::fs::create_dir_all(profile)
            .await
            .map_err(|e| format!("profile folder {}: {e}", profile.display()))?;
        let port_file = profile.join("DevToolsActivePort");
        let _ = tokio::fs::remove_file(&port_file).await;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-sync",
                "--password-store=basic",
                "--use-mock-keychain",
            ]);
        if headless {
            cmd.args(["--headless=new", "--window-size=1280,900"]);
        }
        cmd.arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
        let ws = wait_for_port(&port_file).await?;
        let (stream, _) = tokio_tungstenite::connect_async(&ws)
            .await
            .map_err(|e| format!("could not connect to the browser: {e}"))?;
        let (mut sink, mut source) = stream.split();
        let (out, mut rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        let answers = pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let Some(id) = v["id"].as_u64() else { continue };
                if let Some(tx) = answers.lock().unwrap().remove(&id) {
                    let r = match v.get("error") {
                        Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                        None => Ok(v["result"].clone()),
                    };
                    let _ = tx.send(r);
                }
            }
            // The browser went away: every waiting command fails.
            for (_, tx) in answers.lock().unwrap().drain() {
                let _ = tx.send(Err("the browser closed".into()));
            }
        });
        Ok(Chrome {
            child: Mutex::new(Some(child)),
            out,
            pending,
            next: AtomicU64::new(1),
        })
    }

    /// Send a command (to a page when `session` is set) and wait for its result.
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.out
            .send(msg.to_string())
            .map_err(|_| "the browser closed".to_string())?;
        match tokio::time::timeout(COMMAND_TIME, rx).await {
            Ok(Ok(r)) => r.map_err(|e| format!("{method}: {e}")),
            Ok(Err(_)) => Err("the browser closed".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("{method}: the browser did not answer"))
            }
        }
    }

    /// Whether the browser process is still running.
    pub fn alive(&self) -> bool {
        match self.child.lock().unwrap().as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }
}

/// The DevTools address the browser writes once it listens.
async fn wait_for_port(file: &Path) -> Result<String, String> {
    for _ in 0..200 {
        if let Ok(text) = tokio::fs::read_to_string(file).await {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{}{}", port.trim(), path.trim()));
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the browser did not start within 10 s".into())
}

/// An installed Chromium-family browser: `ARK_BROWSER_BIN`, else Chrome, Edge, Brave or
/// Chromium in their usual places.
pub fn find_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ARK_BROWSER_BIN") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
            "Chromium.app/Contents/MacOS/Chromium",
        ] {
            candidates.push(Path::new("/Applications").join(app));
            if let Some(home) = std::env::var_os("HOME") {
                candidates.push(Path::new(&home).join("Applications").join(app));
            }
        }
    } else if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            if let Some(b) = std::env::var_os(base) {
                let b = PathBuf::from(b);
                candidates.push(b.join("Google/Chrome/Application/chrome.exe"));
                candidates.push(b.join("Microsoft/Edge/Application/msedge.exe"));
                candidates.push(b.join("BraveSoftware/Brave-Browser/Application/brave.exe"));
            }
        }
    } else {
        let path: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "brave-browser",
        ] {
            for d in &path {
                candidates.push(d.join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}
