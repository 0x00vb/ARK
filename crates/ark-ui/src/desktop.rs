//! The native window (Tauri). The consumer's binary supplies its `tauri::Context` (window
//! size, title bar, icons); ark-ui adds the page, two IPC commands and the event channel.
//!
//! - `ark_call(method, params)` forwards to [`Host::call`].
//! - `ark_subscribe(channel)` streams host events to the window.
//! - `ark_pick_folder(title)` asks with the system's folder picker (`ark.pickFolder`).
//!
//! The page is served through the app's own asset origin, so Tauri treats it as a local page.
//! Files in the consumer's `frontendDist` stay reachable as a fallback.
//!
//! The window can open a runtime elsewhere in place of its own page
//! ([`visit`](crate::remote::visit), ADR 0029). That page talks to its runtime over HTTP; the
//! commands above answer only the app's own page, and the event channel stops when the window
//! leaves it. The page elsewhere comes back by loading [`OWN_PAGE`], which the window catches.
//!
//! A question that waits while the window is not in front shows a system notification. With
//! [`ArkUi::keep_running`](crate::ArkUi::keep_running), closing the window hides it: the app
//! stays in the menu bar (the tray elsewhere) with Open and Quit, and the Dock icon opens it.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};
use tauri::{Manager, RunEvent, WindowEvent, Wry};
use tauri_plugin_notification::NotificationExt;

use crate::assets::Assets;
use crate::host::Host;

struct UiAssets {
    ark: Arc<Assets>,
    fallback: Box<dyn tauri::Assets<Wry>>,
}

impl tauri::Assets<Wry> for UiAssets {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        match self.ark.get(key.as_ref()) {
            Some((b, _)) => Some(b),
            None => self.fallback.get(key),
        }
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        let ours: Vec<(Cow<'_, str>, Cow<'_, [u8]>)> = self
            .ark
            .paths()
            .into_iter()
            .filter_map(|p| {
                let (b, _) = self.ark.get(&p)?;
                Some((Cow::Owned(format!("/{p}")), b))
            })
            .collect();
        Box::new(ours.into_iter().chain(self.fallback.iter()))
    }
    fn csp_hashes(&self, _html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

struct State(Arc<Host>);

/// The running app, once its window exists.
pub(crate) static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// The address of the window's own page, from the first time it is seen there.
static HOME: OnceLock<tauri::Url> = OnceLock::new();
/// Changes each time the window leaves its own page: event channels opened before stop.
pub(crate) static VISITS: AtomicU64 = AtomicU64::new(0);
/// What a page elsewhere loads to bring the window back to its own page (`ark.openLocal()`).
/// Never fetched: `.invalid` does not resolve, and the window catches it first.
pub(crate) const OWN_PAGE: &str = "https://ark-local.invalid/";

/// The app's own page: its asset origin (`tauri:` on macOS and Linux, `http://tauri.localhost`
/// on Windows). Anything else the window shows is a page elsewhere and gets no IPC.
fn own_page(url: &tauri::Url) -> bool {
    url.scheme() == "tauri" || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
}

fn on_own_page(webview: &tauri::Webview) -> bool {
    match webview.url() {
        Ok(u) if own_page(&u) => {
            let _ = HOME.set(u);
            true
        }
        _ => false,
    }
}

fn from_own_page(webview: &tauri::Webview) -> Result<(), String> {
    if on_own_page(webview) {
        Ok(())
    } else {
        Err("only the app's own page can use the window".into())
    }
}

/// The window back on its own page.
pub(crate) fn go_home(webview: &tauri::Webview) {
    if let Some(home) = HOME.get() {
        let _ = webview.navigate(home.clone());
    }
}

/// Catches [`OWN_PAGE`]: from a page elsewhere, the window goes back to its own page. Loaded
/// on the own page (a frame in it), it is only refused.
fn window_plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri::plugin::Builder::<Wry>::new("ark-window")
        .on_navigation(|webview, url| {
            if url.as_str() != OWN_PAGE {
                return true;
            }
            if !on_own_page(webview) {
                let webview = webview.clone();
                // Not from inside the navigation callback.
                tauri::async_runtime::spawn(async move { go_home(&webview) });
            }
            false
        })
        .build()
}

#[tauri::command]
async fn ark_call(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    state: tauri::State<'_, State>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    from_own_page(&webview)?;
    if method == "quit" {
        app.exit(0);
        return Ok(Value::Null);
    }
    state
        .0
        .call(&method, params)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn ark_subscribe(
    webview: tauri::Webview,
    state: tauri::State<'_, State>,
    channel: tauri::ipc::Channel<Value>,
) -> Result<(), String> {
    from_own_page(&webview)?;
    let mut rx = state.0.subscribe();
    let visit = VISITS.load(Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        loop {
            let next = rx.recv().await;
            // The window left this page: this runtime's events are not sent to the next one.
            if VISITS.load(Ordering::SeqCst) != visit {
                break;
            }
            match next {
                Ok(v) => {
                    if channel.send(v).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = channel.send(serde_json::json!({ "kind": "resync" }));
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}

/// The system's folder picker. `None` when the person dismisses it, or asked from a page
/// elsewhere.
#[tauri::command]
async fn ark_pick_folder(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    title: Option<String>,
) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    from_own_page(&webview).ok()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    dialog.pick_folder(move |p| {
        let _ = tx.send(p);
    });
    let path = rx.await.ok().flatten()?.into_path().ok()?;
    Some(path.display().to_string())
}

pub(crate) fn run(
    host: Arc<Host>,
    assets: Arc<Assets>,
    mut context: tauri::Context<Wry>,
    keep_running: bool,
) -> std::process::ExitCode {
    context.config_mut().app.with_global_tauri = true;
    let name = context
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "ARK".into());
    let fallback = context.set_assets(Box::new(EmptyAssets));
    context.set_assets(Box::new(UiAssets {
        ark: assets,
        fallback,
    }));
    let events = host.subscribe();
    let built = tauri::Builder::default()
        .manage(State(host))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(window_plugin())
        .invoke_handler(tauri::generate_handler![
            ark_call,
            ark_subscribe,
            ark_pick_folder
        ])
        .on_window_event(move |w, ev| {
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if keep_running {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .setup(move |app| {
            let _ = APP.set(app.handle().clone());
            if let Some(w) = app.get_webview_window("main") {
                if let Some(url) = w.url().ok().filter(own_page) {
                    let _ = HOME.set(url);
                }
                let _ = w.show();
            }
            if keep_running {
                tray(app, &name)?;
            }
            notify_questions(app.handle().clone(), events, name.clone());
            Ok(())
        })
        .build(context);
    let app = match built {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    app.run(|app, ev| {
        // The Dock icon (or opening the app again) brings back a hidden window.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = ev {
            show(app);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app, ev);
    });
    std::process::ExitCode::SUCCESS
}

fn show(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The menu-bar item: open the window, or quit (which stops the agents' work).
fn tray(app: &tauri::App, name: &str) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", format!("Open {name}"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", format!("Quit {name}"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut b = TrayIconBuilder::with_id("ark")
        .tooltip(name)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "open" => show(app),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

/// A question that waits while the window is not in front (hidden, minimized, or another app
/// in use) shows a system notification, once per question.
fn notify_questions(
    app: tauri::AppHandle,
    mut events: tokio::sync::broadcast::Receiver<Value>,
    name: String,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            let ev = match events.recv().await {
                Ok(v) => v,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            if ev["kind"] != "approval" {
                continue;
            }
            // In front and showing this runtime: a page elsewhere does not show its questions.
            let in_front = app.get_webview_window("main").is_some_and(|w| {
                w.is_visible().unwrap_or(false)
                    && w.is_focused().unwrap_or(false)
                    && w.url().is_ok_and(|u| own_page(&u))
            });
            if in_front {
                continue;
            }
            let s = |k: &str| ev[k].as_str().unwrap_or("").trim().to_string();
            let body = [s("title"), s("line")]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let _ = app
                .notification()
                .builder()
                .title(format!("{name} is waiting for your answer"))
                .body(body)
                .show();
        }
    });
}

/// Placeholder while swapping the context's assets.
struct EmptyAssets;
impl tauri::Assets<Wry> for EmptyAssets {
    fn get(&self, _key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        None
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }
    fn csp_hashes(&self, _html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_page_is_its_own() {
        let own = |u: &str| own_page(&u.parse().unwrap());
        assert!(own("tauri://localhost/index.html"));
        assert!(own("http://tauri.localhost/"));
        assert!(!own("https://tauri.localhost/"));
        assert!(!own("https://ark-ws-1.fly.dev/"));
        assert!(!own("http://127.0.0.1:8420/"));
        assert!(!own("http://tauri.localhost.example.com/"));
        assert!(!own(OWN_PAGE));
    }
}
