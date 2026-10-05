//! Clients of a runtime served elsewhere (ADR 0016): [`Client`] (from `ark-interact`, shared
//! with the terminal) calls its protocol over HTTP, and `run_window` (feature `desktop`) opens
//! a native window on it.
//!
//! The window loads the page the runtime serves, not a copy of its own, so the page always
//! matches the runtime's version; it talks to it over the browser transport (HTTP and
//! server-sent events), like any browser.

pub use ark_interact::remote::Client;

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

    let (app, _) = crate::desktop::WINDOW
        .get()
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    let client = Client::new(url, token);
    client.hello().await?;
    let target: tauri::Url = client.page_url().parse()?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("there is no window to open {url} in"))?;
    crate::desktop::VISITS.fetch_add(1, Ordering::SeqCst);
    window.navigate(target)?;
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
