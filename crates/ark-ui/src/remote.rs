//! Clients of a runtime served elsewhere (ADR 0016): [`Client`] (from `ark-interact`, shared
//! with the terminal) calls its protocol over HTTP, and `run_window` (feature `desktop`) opens
//! a native window on it.
//!
//! The window loads the page the runtime serves, not a copy of its own, so the page always
//! matches the runtime's version; it talks to it over the browser transport (HTTP and
//! server-sent events), like any browser.

pub use ark_interact::remote::Client;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// How much one call carries at most: one part of a large file, or several small files. Small
/// enough to arrive within the runtime's 30 seconds per request on a slow connection (about
/// 200 kbit/s), large enough that a folder of small files needs few calls.
const CALL: usize = 512 * 1024;
/// How many times a call is tried before the copy stops, and how long one may take.
const TRIES: u32 = 3;
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// A folder as [`send_folder`] would send it: every file under it, links left out.
pub struct Folder {
    root: PathBuf,
    /// Paths relative to the folder, with `/`, and sizes.
    files: Vec<(String, u64)>,
    skipped: usize,
}

impl Folder {
    /// Read what is in `dir`. Links (to files or folders) are left out: what they point to may
    /// be outside the folder. So are names that are not UTF-8 (`skipped` counts both).
    pub fn read(dir: &Path) -> anyhow::Result<Folder> {
        Ok(Self::read_at_most(dir, usize::MAX, u64::MAX)?.expect("no limit"))
    }

    /// [`read`](Self::read), stopping early with `None` once there are more than `files` files
    /// or `bytes` bytes: a large folder (a home folder) is not walked to the end.
    pub fn read_at_most(dir: &Path, files: usize, bytes: u64) -> anyhow::Result<Option<Folder>> {
        anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
        let mut out = Vec::new();
        let (mut total, mut skipped) = (0u64, 0);
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            let entries =
                std::fs::read_dir(&d).map_err(|e| anyhow::anyhow!("{}: {e}", d.display()))?;
            for e in entries {
                let e = e?;
                let kind = e.file_type()?;
                if !kind.is_dir() && !kind.is_file() {
                    skipped += 1;
                    continue;
                }
                let Some(rel) = e
                    .path()
                    .strip_prefix(dir)
                    .ok()
                    .and_then(|r| r.to_str())
                    .map(|r| r.replace(std::path::MAIN_SEPARATOR, "/"))
                else {
                    skipped += 1;
                    continue;
                };
                if kind.is_dir() {
                    todo.push(e.path());
                    continue;
                }
                let size = e.metadata()?.len();
                total += size;
                out.push((rel, size));
                if out.len() > files || total > bytes {
                    return Ok(None);
                }
            }
        }
        out.sort();
        Ok(Some(Folder {
            root: dir.to_path_buf(),
            files: out,
            skipped,
        }))
    }

    /// How many files would be sent.
    pub fn files(&self) -> usize {
        self.files.len()
    }

    /// How many bytes would be sent.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, n)| n).sum()
    }

    /// Links and names that are not UTF-8, which are not sent.
    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

/// How far a [`send_folder`] is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sent {
    pub files: usize,
    pub bytes: u64,
}

/// Copy `folder` to the runtime `client` reaches, where it becomes a new project named `name`
/// (`name 2` when taken), and answer `{id, name}` there (ADR 0030). The project appears only
/// when every file arrived. A call that fails is tried again; when the copy stops (an error,
/// or `go_on` answering false), what was sent is removed. Empty folders are not sent.
pub async fn send_folder(
    client: &Client,
    folder: &Folder,
    name: &str,
    mut go_on: impl FnMut(Sent) -> bool,
) -> anyhow::Result<Value> {
    let upload = uuid::Uuid::new_v4().simple().to_string();
    let sent = send_files(client, folder, &upload, &mut go_on).await;
    let done = match sent {
        Ok(true) => {
            call(
                client,
                "upload_project",
                json!({ "upload": upload, "name": name }),
            )
            .await
        }
        Ok(false) => Err(anyhow::anyhow!("the copy was stopped")),
        Err(e) => Err(e),
    };
    if done.is_err() {
        // Best effort: the runtime also removes uploads left for a day when it starts.
        let _ = call(client, "upload_cancel", json!({ "upload": upload })).await;
    }
    done
}

/// One call, tried again after a failure that is not the runtime's refusal of the request
/// itself (no answer, a timeout, a dropped connection).
async fn call(client: &Client, method: &str, params: Value) -> anyhow::Result<Value> {
    let mut last = None;
    for attempt in 0..TRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
        }
        match tokio::time::timeout(CALL_TIMEOUT, client.call(method, params.clone())).await {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) if !retryable(&e) => return Err(e),
            Ok(Err(e)) => last = Some(e),
            Err(_) => last = Some(anyhow::anyhow!("the runtime did not answer in time")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("the call failed")))
}

/// A transport failure, worth trying again. The runtime's own answers (`{ok: false}`, a
/// refused token) are not.
fn retryable(e: &anyhow::Error) -> bool {
    let text = e.to_string();
    text.starts_with("cannot reach the ARK runtime") || text.contains("answered HTTP 5")
}

async fn send_files(
    client: &Client,
    folder: &Folder,
    upload: &str,
    go_on: &mut impl FnMut(Sent) -> bool,
) -> anyhow::Result<bool> {
    use base64::Engine as _;
    use tokio::io::AsyncReadExt as _;

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    // Starts the upload, so an empty folder is one too.
    call(
        client,
        "upload_parts",
        json!({ "upload": upload, "parts": [] }),
    )
    .await?;
    let mut sent = Sent::default();
    let mut batch: Vec<Value> = Vec::new();
    let mut batch_bytes = 0usize;
    // Files finished in the batch not yet sent: counted once it is.
    let mut batch_files = 0usize;
    let mut buf = vec![0u8; CALL];
    for (rel, _) in &folder.files {
        let mut file = tokio::fs::File::open(folder.root.join(rel))
            .await
            .map_err(|e| anyhow::anyhow!("{rel}: {e}"))?;
        let mut offset = 0u64;
        loop {
            let room = CALL - batch_bytes;
            let mut n = 0;
            while n < room {
                match file
                    .read(&mut buf[n..room])
                    .await
                    .map_err(|e| anyhow::anyhow!("{rel}: {e}"))?
                {
                    0 => break,
                    k => n += k,
                }
            }
            let end = n < room;
            // A file's first part goes even when it is empty, so the file exists there.
            if n > 0 || offset == 0 {
                batch.push(json!({ "path": rel, "offset": offset, "data": b64(&buf[..n]) }));
                batch_bytes += n;
                offset += n as u64;
            }
            if end {
                batch_files += 1;
            }
            if batch_bytes >= CALL || (!end && batch_bytes > 0) || batch.len() >= 200 {
                flush(client, upload, &mut batch).await?;
                sent.bytes += batch_bytes as u64;
                sent.files += batch_files;
                (batch_bytes, batch_files) = (0, 0);
                if !go_on(sent) {
                    return Ok(false);
                }
            }
            if end {
                break;
            }
        }
    }
    if !batch.is_empty() {
        flush(client, upload, &mut batch).await?;
    }
    sent.bytes += batch_bytes as u64;
    sent.files += batch_files;
    Ok(go_on(sent))
}

async fn flush(client: &Client, upload: &str, batch: &mut Vec<Value>) -> anyhow::Result<()> {
    let parts = std::mem::take(batch);
    let first = parts[0]["path"].as_str().unwrap_or("").to_string();
    call(
        client,
        "upload_parts",
        json!({ "upload": upload, "parts": parts }),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{first}: {e}"))?;
    Ok(())
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

/// Open the window on the runtime at `url` in place of the app's own page (ADR 0029): the page
/// that runtime serves, which talks to it over HTTP like a browser. The window's own runtime
/// goes on running; the page elsewhere brings the window back with `ark.openLocal()`.
///
/// For an app's handler, while [`ArkUi::run_desktop`](crate::ArkUi::run_desktop) runs. Fails,
/// leaving the window where it is, when there is no window, or the runtime cannot be reached
/// or refuses `token`.
#[cfg(feature = "desktop")]
pub async fn visit(url: &str, token: &str) -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;

    let app = crate::desktop::APP
        .get()
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    let client = Client::new(url, token);
    client.hello().await?;
    let target: tauri::Url = client.page_url().parse()?;
    let window = crate::desktop::main_webview(app)
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    // Before leaving: the own page's event channels stop sending from here on.
    crate::desktop::VISITS.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = window.navigate(target) {
        // Still on the own page, with its channels stopped: load it again to start over.
        crate::desktop::go_home(&window);
        return Err(e.into());
    }
    Ok(())
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
