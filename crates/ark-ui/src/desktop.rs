//! The native window (Tauri). The consumer's binary supplies its `tauri::Context` (window
//! size, title bar, icons); ark-ui adds the page, two IPC commands and the event channel.
//!
//! - `ark_call(method, params)` forwards to [`Host::call`].
//! - `ark_subscribe(channel)` streams host events to the window.
//!
//! The page is served through the app's own asset origin, so Tauri treats it as a local page.
//! Files in the consumer's `frontendDist` stay reachable as a fallback.

use std::borrow::Cow;
use std::sync::Arc;

use serde_json::Value;
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};
use tauri::{Manager, Wry};

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

pub(crate) fn run(
    host: Arc<Host>,
    assets: Arc<Assets>,
    mut context: tauri::Context<Wry>,
) -> std::process::ExitCode {
    context.config_mut().app.with_global_tauri = true;
    let fallback = context.set_assets(Box::new(EmptyAssets));
    context.set_assets(Box::new(UiAssets {
        ark: assets,
        fallback,
    }));
    let result = tauri::Builder::default()
        .manage(State(host))
        .invoke_handler(tauri::generate_handler![ark_call, ark_subscribe])
        .setup(move |app| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .run(context);
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
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
