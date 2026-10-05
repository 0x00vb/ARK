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
//! A question that waits while the window is not in front shows a system notification. With
//! [`ArkUi::keep_running`](crate::ArkUi::keep_running), closing the window hides it: the app
//! stays in the menu bar (the tray elsewhere) with Open and Quit, and the Dock icon opens it.

use std::borrow::Cow;
use std::sync::Arc;

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

#[tauri::command]
async fn ark_call(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    method: String,
    params: Value,
) -> Result<Value, String> {
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
fn ark_subscribe(state: tauri::State<'_, State>, channel: tauri::ipc::Channel<Value>) {
    let mut rx = state.0.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
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
}

/// The system's folder picker. `None` when the person dismisses it.
#[tauri::command]
async fn ark_pick_folder(app: tauri::AppHandle, title: Option<String>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
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
            if let Some(w) = app.get_webview_window("main") {
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
            let in_front = app.get_webview_window("main").is_some_and(|w| {
                w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)
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
