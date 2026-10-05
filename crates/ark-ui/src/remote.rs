//! Clients of a runtime served elsewhere (ADR 0016): [`Client`] (from `ark-interact`, shared
//! with the terminal) calls its protocol over HTTP, and `run_window` (feature `desktop`) opens
//! a native window on it.
//!
//! The window loads the page the runtime serves, not a copy of its own, so the page always
//! matches the runtime's version; it talks to it over the browser transport (HTTP and
//! server-sent events), like any browser.

pub use ark_interact::remote::Client;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// How much of a file one call carries (the runtime takes up to 8 MiB). Small enough to arrive
/// within the runtime's 30 seconds per request on a slow connection.
const PART: usize = 1024 * 1024;

/// A folder as [`send_folder`] would send it: every file under it, links left out.
pub struct Folder {
    root: PathBuf,
    /// Paths relative to the folder, with `/`, and sizes.
    files: Vec<(String, u64)>,
}

impl Folder {
    /// Read what is in `dir`. Links (to files or folders) are left out: what they point to may
    /// be outside the folder.
    pub fn read(dir: &Path) -> anyhow::Result<Folder> {
        anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
        let mut files = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d)? {
                let e = e?;
                let kind = e.file_type()?;
                if kind.is_dir() {
                    todo.push(e.path());
                } else if kind.is_file() {
                    let rel = e.path().strip_prefix(dir)?.to_path_buf();
                    let rel: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    files.push((rel.join("/"), e.metadata()?.len()));
                }
            }
        }
        files.sort();
        Ok(Folder {
            root: dir.to_path_buf(),
            files,
        })
    }

    /// How many files would be sent.
    pub fn files(&self) -> usize {
        self.files.len()
    }

    /// How many bytes would be sent.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, n)| n).sum()
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
/// when every file arrived; an error or `go_on` answering false removes what was sent.
/// Empty folders are not sent.
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
            client
                .call("upload_project", json!({ "upload": upload, "name": name }))
                .await
        }
        Ok(false) => Err(anyhow::anyhow!("the copy was stopped")),
        Err(e) => Err(e),
    };
    if done.is_err() {
        // Best effort: the runtime also removes uploads left for a day.
        let _ = client
            .call("upload_cancel", json!({ "upload": upload }))
            .await;
    }
    done
}

async fn send_files(
    client: &Client,
    folder: &Folder,
    upload: &str,
    go_on: &mut impl FnMut(Sent) -> bool,
) -> anyhow::Result<bool> {
    use base64::Engine as _;

    let mut sent = Sent::default();
    let mut buf = vec![0u8; PART];
    for (rel, _) in &folder.files {
        let mut file = tokio::fs::File::open(folder.root.join(rel)).await?;
        let mut offset = 0u64;
        loop {
            let n = read_part(&mut file, &mut buf).await?;
            // A file's first part goes even when it is empty, so the file exists there.
            if n > 0 || offset == 0 {
                let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                client
                    .call(
                        "upload_file",
                        json!({ "upload": upload, "path": rel, "data": data, "offset": offset }),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{rel}: {e}"))?;
                offset += n as u64;
                sent.bytes += n as u64;
                if !go_on(sent) {
                    return Ok(false);
                }
            }
            if n < buf.len() {
                break;
            }
        }
        sent.files += 1;
        if !go_on(sent) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Fill `buf` from `file` as far as it goes; fewer bytes than its length means the end.
async fn read_part(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt as _;
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]).await? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
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
    use tauri::Manager;

    let app = crate::desktop::APP
        .get()
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    let client = Client::new(url, token);
    client.hello().await?;
    let target: tauri::Url = client.page_url().parse()?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    // Before leaving: the own page's event channels stop sending from here on.
    crate::desktop::VISITS.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = window.navigate(target) {
        // Still on the own page, with its channels stopped: load it again to start over.
        crate::desktop::go_home(window.as_ref());
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
