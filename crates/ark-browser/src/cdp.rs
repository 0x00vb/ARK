//! A small client for the Chrome DevTools Protocol: start a browser with a profile folder,
//! connect to it over WebSocket, send commands and wait for their answers. Pages are driven
//! through flattened target sessions (`sessionId` on each command).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// How long a command may take before the browser counts as stuck.
const COMMAND_TIME: Duration = Duration::from_secs(30);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

struct EventContext<'a> {
    pending: &'a Pending,
    events: &'a Mutex<Vec<Value>>,
    notify: &'a tokio::sync::Notify,
    subscriptions: &'a Mutex<Vec<(String, String)>>,
    inflight: &'a AtomicUsize,
}

fn deliver(value: Value, context: &EventContext<'_>, bounded_event: bool, size: usize) -> bool {
    let EventContext {
        pending,
        events,
        notify,
        subscriptions,
        inflight,
    } = context;
    let Some(id) = value["id"].as_u64() else {
        if bounded_event {
            let session = value["sessionId"].as_str().unwrap_or("");
            let method = value["method"].as_str().unwrap_or("");
            if !subscriptions
                .lock()
                .unwrap()
                .iter()
                .any(|(s, m)| s == session && m == method)
            {
                return true;
            }
        }
        if bounded_event && size > 64 * 1024 {
            return false;
        }
        let mut events = events.lock().unwrap();
        if bounded_event
            && events
                .len()
                .saturating_add(inflight.load(Ordering::Acquire))
                >= 128
        {
            return false;
        }
        events.push(value);
        if events.len() > 128 {
            events.drain(..64);
        }
        notify.notify_waiters();
        return true;
    };
    if let Some(tx) = pending.lock().unwrap().remove(&id) {
        let result = match value.get("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or("error").to_string()),
            None => Ok(value["result"].clone()),
        };
        let _ = tx.send(result);
    }
    true
}

fn close_pending(pending: &Pending) {
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err("the browser closed".into()));
    }
}

/// A delivered pipe event remains in flight until its consumer drops this value.
pub struct BrowserEvent {
    pub value: Value,
    inflight: Arc<AtomicUsize>,
}
impl Drop for BrowserEvent {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Outbound {
    text: String,
    active: Option<Arc<AtomicBool>>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

struct LimitedJson(Vec<u8>);
impl std::io::Write for LimitedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > 32 * 1024 * 1024)
        {
            return Err(std::io::Error::other("browser pipe request exceeds limits"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PendingCall {
    id: u64,
    pending: Pending,
    active: Option<Arc<AtomicBool>>,
}
impl Drop for PendingCall {
    fn drop(&mut self) {
        if let Some(active) = &self.active {
            active.store(false, Ordering::Release);
        }
        self.pending.lock().unwrap().remove(&self.id);
    }
}

/// A running browser and the connection to it. Dropping it stops the browser.
pub struct Chrome {
    child: Mutex<Option<tokio::process::Child>>,
    /// The browser with its helper processes, which hold the profile's files until they end.
    tree: Mutex<ark_os::process::Tree>,
    out: mpsc::UnboundedSender<Outbound>,
    pending: Pending,
    /// Unsolicited Target events, kept briefly so a person-driven surface can adopt a popup.
    events: Arc<Mutex<Vec<Value>>>,
    events_notify: Arc<tokio::sync::Notify>,
    subscriptions: Arc<Mutex<Vec<(String, String)>>>,
    event_inflight: Arc<AtomicUsize>,
    next: AtomicU64,
    pipe_closed: Option<Arc<AtomicBool>>,
    pipe_slots: Option<Arc<tokio::sync::Semaphore>>,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        self.tree.lock().unwrap().kill();
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.start_kill();
        }
    }
}

impl Chrome {
    #[cfg(test)]
    pub(crate) fn crash_for_test(&self) {
        self.tree.lock().unwrap().kill();
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }

    /// Start `bin` on `profile` (a folder it keeps cookies and sign-ins in) and connect.
    /// `headless`: no window.
    pub async fn launch(bin: &Path, profile: &Path, headless: bool) -> Result<Chrome, String> {
        tokio::fs::create_dir_all(profile)
            .await
            .map_err(|e| format!("profile folder {}: {e}", profile.display()))?;
        let port_file = profile.join("DevToolsActivePort");
        let _ = tokio::fs::remove_file(&port_file).await;
        let mut cmd = ark_os::process::command(bin);
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
        let (out, mut rx) = mpsc::unbounded_channel::<Outbound>();
        let pending: Pending = Arc::default();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if sink.send(Message::Text(message.text.into())).await.is_err() {
                    break;
                }
            }
        });
        let answers = pending.clone();
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let events_notify = Arc::new(tokio::sync::Notify::new());
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let event_inflight = Arc::new(AtomicUsize::new(0));
        let seen_inflight = event_inflight.clone();
        let seen_subscriptions = subscriptions.clone();
        let seen_notify = events_notify.clone();
        let seen = events.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let context = EventContext {
                    pending: &answers,
                    events: &seen,
                    notify: &seen_notify,
                    subscriptions: &seen_subscriptions,
                    inflight: &seen_inflight,
                };
                deliver(v, &context, false, text.len());
            }
            close_pending(&answers);
        });
        Ok(Chrome {
            tree: Mutex::new(ark_os::process::Tree::of(child.id())),
            child: Mutex::new(Some(child)),
            out,
            pending,
            events,
            events_notify,
            subscriptions,
            event_inflight,
            next: AtomicU64::new(1),
            pipe_closed: None,
            pipe_slots: None,
        })
    }

    /// Start an isolated browser whose DevTools connection does not use a network socket.
    #[cfg(unix)]
    pub async fn launch_pipe(bin: &Path, profile: &Path) -> Result<Chrome, String> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const MAX_FRAME: usize = 112 * 1024 * 1024;
        tokio::fs::create_dir_all(profile)
            .await
            .map_err(|e| format!("profile folder {}: {e}", profile.display()))?;
        let (to_browser, browser_input) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| format!("could not create browser input: {e}"))?;
        let (from_browser, browser_output) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| format!("could not create browser output: {e}"))?;
        let duplicate = |fd| -> Result<OwnedFd, String> {
            let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
            if copy < 0 {
                return Err(format!(
                    "could not prepare browser pipe: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(unsafe { OwnedFd::from_raw_fd(copy) })
        };
        let input_fd = duplicate(browser_input.as_raw_fd())?;
        let output_fd = duplicate(browser_output.as_raw_fd())?;
        let mut cmd = ark_os::process::command(bin);
        cmd.arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--remote-debugging-pipe",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-sync",
                "--password-store=basic",
                "--use-mock-keychain",
                "--headless=new",
                "--window-size=1280,900",
                "about:blank",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let raw_input = input_fd.as_raw_fd();
        let raw_output = output_fd.as_raw_fd();
        unsafe {
            cmd.as_std_mut().pre_exec(move || {
                if libc::dup2(raw_input, 3) < 0 || libc::dup2(raw_output, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
        let tree = ark_os::process::Tree::of(child.id());
        drop(input_fd);
        drop(output_fd);
        drop(browser_input);
        drop(browser_output);
        to_browser
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        from_browser
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        let mut writer = tokio::net::UnixStream::from_std(to_browser).map_err(|e| e.to_string())?;
        let mut reader =
            tokio::net::UnixStream::from_std(from_browser).map_err(|e| e.to_string())?;
        let (out, mut rx) = mpsc::unbounded_channel::<Outbound>();
        let pending: Pending = Arc::default();
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let events_notify = Arc::new(tokio::sync::Notify::new());
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let event_inflight = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let writer_closed = closed.clone();
        let writer_answers = pending.clone();
        let writer_notify = events_notify.clone();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let Outbound {
                    text,
                    active,
                    _permit,
                } = message;
                if active
                    .as_ref()
                    .is_some_and(|active| !active.load(Ordering::Acquire))
                {
                    continue;
                }
                if text.len() > 32 * 1024 * 1024
                    || writer.write_all(text.as_bytes()).await.is_err()
                    || writer.write_all(&[0]).await.is_err()
                {
                    break;
                }
            }
            writer_closed.store(true, Ordering::Release);
            writer_notify.notify_waiters();
            close_pending(&writer_answers);
        });
        let answers = pending.clone();
        let reader_closed = closed.clone();
        let seen_notify = events_notify.clone();
        let seen_subscriptions = subscriptions.clone();
        let seen_inflight = event_inflight.clone();
        let seen = events.clone();
        tokio::spawn(async move {
            let mut frame = Vec::new();
            let mut chunk = [0u8; 16 * 1024];
            'receive: loop {
                let count = match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for byte in &chunk[..count] {
                    if *byte == 0 {
                        if let Ok(value) = serde_json::from_slice::<Value>(&frame) {
                            let context = EventContext {
                                pending: &answers,
                                events: &seen,
                                notify: &seen_notify,
                                subscriptions: &seen_subscriptions,
                                inflight: &seen_inflight,
                            };
                            if !deliver(value, &context, true, frame.len()) {
                                break 'receive;
                            }
                        } else {
                            break 'receive;
                        }
                        if frame.capacity() > 1024 * 1024 {
                            frame = Vec::new();
                        } else {
                            frame.clear();
                        }
                    } else {
                        if frame.len() == MAX_FRAME {
                            break 'receive;
                        }
                        frame.push(*byte);
                    }
                }
            }
            reader_closed.store(true, Ordering::Release);
            seen_notify.notify_waiters();
            close_pending(&answers);
        });
        Ok(Chrome {
            tree: Mutex::new(tree),
            child: Mutex::new(Some(child)),
            out,
            pending,
            events,
            events_notify,
            subscriptions,
            event_inflight,
            next: AtomicU64::new(1),
            pipe_closed: Some(closed),
            pipe_slots: Some(Arc::new(tokio::sync::Semaphore::new(2))),
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
        let permit = self
            .pipe_slots
            .as_ref()
            .map(|slots| {
                slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "browser pipe request queue is full".to_string())
            })
            .transpose()?;
        let text = if self.pipe_slots.is_some() {
            let mut output = LimitedJson(Vec::new());
            serde_json::to_writer(&mut output, &msg)
                .map_err(|e| format!("cannot encode browser pipe request: {e}"))?;
            String::from_utf8(output.0).map_err(|e| format!("invalid browser pipe request: {e}"))?
        } else {
            msg.to_string()
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if let Some(closed) = &self.pipe_closed {
                if closed.load(Ordering::Acquire) {
                    return Err("the browser closed".into());
                }
                if text.len() > 32 * 1024 * 1024 || pending.len() >= 2 {
                    return Err("browser pipe request exceeds limits".into());
                }
            }
            pending.insert(id, tx);
        }
        let active = self
            .pipe_slots
            .as_ref()
            .map(|_| Arc::new(AtomicBool::new(true)));
        let _call = PendingCall {
            id,
            pending: self.pending.clone(),
            active: active.clone(),
        };
        self.out
            .send(Outbound {
                text,
                active,
                _permit: permit,
            })
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

    /// Subscribe before enabling a CDP event source. Pipe events without a subscriber are discarded.
    #[cfg(unix)]
    pub fn subscribe_event(&self, session: &str, method: &str) -> Result<(), String> {
        if self.pipe_closed.is_none()
            || session.is_empty()
            || method.is_empty()
            || session.len() > 256
            || method.len() > 256
        {
            return Err("browser event subscription is unavailable".into());
        }
        let mut subscriptions = self.subscriptions.lock().unwrap();
        if subscriptions
            .iter()
            .any(|(s, m)| s == session && m == method)
        {
            return Ok(());
        }
        if subscriptions.len() >= 16 {
            return Err("browser event subscriptions exceed limits".into());
        }
        subscriptions.push((session.to_string(), method.to_string()));
        Ok(())
    }

    /// Wait for a subscribed event. One consumer owns a session's events; the caller sets a deadline.
    #[cfg(unix)]
    pub async fn wait_event(&self, session: &str, method: &str) -> Result<BrowserEvent, String> {
        let closed = self
            .pipe_closed
            .as_ref()
            .ok_or("browser event stream is unavailable")?;
        if !self
            .subscriptions
            .lock()
            .unwrap()
            .iter()
            .any(|(s, m)| s == session && m == method)
        {
            return Err("browser event is not subscribed".into());
        }
        loop {
            let notified = self.events_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut events = self.events.lock().unwrap();
                if let Some(index) = events.iter().position(|event| {
                    event["sessionId"].as_str() == Some(session)
                        && event["method"].as_str() == Some(method)
                }) {
                    self.event_inflight.fetch_add(1, Ordering::AcqRel);
                    return Ok(BrowserEvent {
                        value: events.remove(index),
                        inflight: self.event_inflight.clone(),
                    });
                }
            }
            if closed.load(Ordering::Acquire) {
                return Err("the browser closed".into());
            }
            notified.await;
        }
    }

    /// Whether all subscribed events have been consumed and acknowledged.
    #[cfg(unix)]
    pub fn events_idle(&self) -> Result<bool, String> {
        let closed = self
            .pipe_closed
            .as_ref()
            .ok_or("browser event stream is unavailable")?;
        let events = self.events.lock().unwrap();
        if closed.load(Ordering::Acquire) {
            return Err("the browser closed".into());
        }
        Ok(events.is_empty() && self.event_inflight.load(Ordering::Acquire) == 0)
    }

    /// Whether the browser process is still running.
    pub fn alive(&self) -> bool {
        match self.child.lock().unwrap().as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// The newest popup created by `opener` in `context`, if one.
    pub fn take_popup(&self, context: &str, opener: &str) -> Option<String> {
        let mut events = self.events.lock().unwrap();
        let at = events.iter().rposition(|v| {
            v["method"] == "Target.targetCreated"
                && v["params"]["targetInfo"]["browserContextId"].as_str() == Some(context)
                && v["params"]["targetInfo"]["openerId"].as_str() == Some(opener)
        })?;
        let v = events.remove(at);
        v["params"]["targetInfo"]["targetId"]
            .as_str()
            .map(str::to_string)
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
    ark_os::find::app(&BROWSERS)
}

/// Chrome, Edge, Brave and Chromium, in that order, where their installers put them.
const BROWSERS: ark_os::find::App = ark_os::find::App {
    macos: &[
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "Brave Browser.app/Contents/MacOS/Brave Browser",
        "Chromium.app/Contents/MacOS/Chromium",
    ],
    windows: &[
        "Google/Chrome/Application/chrome.exe",
        "Microsoft/Edge/Application/msedge.exe",
        "BraveSoftware/Brave-Browser/Application/brave.exe",
    ],
    other: &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ],
};

#[cfg(all(test, unix))]
mod pipe_tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_calls_cannot_fill_an_unbounded_pipe_queue() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let binary = fixture.path().join("browser");
        let marker = fixture.path().join("started");
        let marker_shell = marker.to_str().unwrap().replace('\'', "'\\''");
        std::fs::write(&binary, format!("#!/bin/sh\ndd bs=1 count=1 <&3 >/dev/null 2>/dev/null\n: > '{marker_shell}'\nsleep 30\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let browser = Arc::new(Chrome::launch_pipe(&binary, fixture.path()).await.unwrap());
        let first_browser = browser.clone();
        let first = tokio::spawn(async move {
            first_browser
                .call(None, "Test.block", json!({"body": "x".repeat(1024 * 1024)}))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first command did not begin writing");
        first.abort();
        let _ = first.await;
        let second_browser = browser.clone();
        let second = tokio::spawn(async move {
            second_browser
                .call(
                    None,
                    "Test.queued",
                    json!({"body": "x".repeat(1024 * 1024)}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while browser.pipe_slots.as_ref().unwrap().available_permits() != 0
                || browser.pending.lock().unwrap().is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second command did not queue");
        second.abort();
        let _ = second.await;
        let result = browser.call(None, "Test.overflow", json!({})).await;
        assert!(matches!(result, Err(error) if error.contains("queue is full")));
    }

    #[tokio::test]
    async fn cancelled_queued_pipe_command_is_not_sent() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let binary = fixture.path().join("browser");
        let release = fixture.path().join("release");
        let unexpected = fixture.path().join("unexpected");
        let checked = fixture.path().join("checked");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, select, time
release = {release:?}
unexpected = {unexpected:?}
checked = {checked:?}
while not os.path.exists(release): time.sleep(0.01)
data = b''
while b'\0' not in data:
    part = os.read(3, 65536)
    if not part: raise SystemExit(1)
    data += part
frame, rest = data.split(b'\0', 1)
message = json.loads(frame)
os.write(4, json.dumps({{'id': message['id'], 'result': {{}}}}).encode() + b'\0')
ready, _, _ = select.select([3], [], [], 0.5)
if rest or (ready and os.read(3, 65536)): open(unexpected, 'w').close()
open(checked, 'w').close()
"#,
            release = release.to_str().unwrap(),
            unexpected = unexpected.to_str().unwrap(),
            checked = checked.to_str().unwrap()
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let browser = Arc::new(Chrome::launch_pipe(&binary, fixture.path()).await.unwrap());
        let first_browser = browser.clone();
        let first = tokio::spawn(async move {
            first_browser
                .call(None, "Test.block", json!({"body":"x".repeat(1024 * 1024)}))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while browser.pending.lock().unwrap().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first request did not start");
        let second_browser = browser.clone();
        let second =
            tokio::spawn(
                async move { second_browser.call(None, "Test.cancelled", json!({})).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            while browser.pending.lock().unwrap().len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second request did not queue");
        second.abort();
        let _ = second.await;
        std::fs::write(&release, b"go").unwrap();
        first.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !checked.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child did not complete its request check");
        assert!(!unexpected.exists(), "cancelled queued request was sent");
    }

    #[tokio::test]
    async fn pipe_events_wake_matching_consumers_and_remain_inflight_until_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let binary = fixture.path().join("browser");
        std::fs::write(&binary, b"#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let browser = Arc::new(Chrome::launch_pipe(&binary, fixture.path()).await.unwrap());
        browser.subscribe_event("a", "Fetch.requestPaused").unwrap();
        browser.subscribe_event("b", "Fetch.requestPaused").unwrap();
        let mut first = Box::pin(browser.wait_event("a", "Fetch.requestPaused"));
        let mut second = Box::pin(browser.wait_event("b", "Fetch.requestPaused"));
        assert!(futures::poll!(first.as_mut()).is_pending());
        assert!(futures::poll!(second.as_mut()).is_pending());
        let event =
            json!({"sessionId":"a", "method":"Fetch.requestPaused", "params":{"requestId":"one"}});
        let context = EventContext {
            pending: &browser.pending,
            events: &browser.events,
            notify: &browser.events_notify,
            subscriptions: &browser.subscriptions,
            inflight: &browser.event_inflight,
        };
        assert!(deliver(event, &context, true, 100));
        let first = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.value["params"]["requestId"], "one");
        assert!(!browser.events_idle().unwrap());
        let event =
            json!({"sessionId":"b", "method":"Fetch.requestPaused", "params":{"requestId":"two"}});
        assert!(deliver(event, &context, true, 100));
        let second = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.value["params"]["requestId"], "two");
        drop(first);
        assert!(!browser.events_idle().unwrap());
        drop(second);
        assert!(browser.events_idle().unwrap());
    }

    #[tokio::test]
    #[ignore = "requires an installed Chromium-family browser"]
    async fn pipe_launch_drives_a_real_browser_without_a_tcp_debugger() {
        let binary = find_browser().expect("Chromium is required for this test");
        let profile = tempfile::tempdir().unwrap();
        let browser = Chrome::launch_pipe(&binary, profile.path()).await.unwrap();
        let version = browser
            .call(None, "Browser.getVersion", json!({}))
            .await
            .unwrap();
        assert!(version["product"].as_str().is_some_and(|v| !v.is_empty()));
        assert!(!profile.path().join("DevToolsActivePort").exists());
        let target = browser
            .call(None, "Target.createTarget", json!({"url":"about:blank"}))
            .await
            .unwrap();
        let id = target["targetId"].as_str().unwrap();
        let attached = browser
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":id,"flatten":true}),
            )
            .await
            .unwrap();
        let session = attached["sessionId"].as_str().unwrap();
        let result = browser
            .call(
                Some(session),
                "Runtime.evaluate",
                json!({"expression":"6 * 7", "returnByValue":true}),
            )
            .await
            .unwrap();
        assert_eq!(result["result"]["value"], 42);
        browser
            .call(Some(session), "Page.enable", json!({}))
            .await
            .unwrap();
        browser
            .subscribe_event(session, "Fetch.requestPaused")
            .unwrap();
        browser
            .call(
                Some(session),
                "Fetch.enable",
                json!({"patterns":[{"urlPattern":"*"}]}),
            )
            .await
            .unwrap();
        let serve = async {
            let event = browser
                .wait_event(session, "Fetch.requestPaused")
                .await
                .unwrap();
            assert_eq!(
                event.value["params"]["request"]["url"],
                "https://render.invalid/screen.html"
            );
            let request = event.value["params"]["requestId"].as_str().unwrap();
            browser
                .call(
                    Some(session),
                    "Fetch.fulfillRequest",
                    json!({
                        "requestId":request,"responseCode":200,
                        "responseHeaders":[{"name":"Content-Type","value":"text/html"}],
                        "body":"PGh0bWw+PGJvZHk+b2ZmbGluZTwvYm9keT48L2h0bWw+"
                    }),
                )
                .await
                .unwrap();
        };
        let navigate = async {
            browser
                .call(
                    Some(session),
                    "Page.navigate",
                    json!({"url":"https://render.invalid/screen.html"}),
                )
                .await
                .unwrap();
        };
        tokio::join!(serve, navigate);
        let text = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(result) = browser
                    .call(
                        Some(session),
                        "Runtime.evaluate",
                        json!({"expression":"document.body?.innerText", "returnByValue":true}),
                    )
                    .await
                {
                    if result["result"]["value"] == "offline" {
                        break result;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("intercepted document did not load");
        assert_eq!(text["result"]["value"], "offline");
    }
}
