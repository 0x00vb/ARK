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
//! The Browser panel shows its page in a webview of its own placed over the panel
//! (`ark_browser_*`, ADR 0044), so pages that refuse to be framed still show. That view is a
//! page elsewhere: it gets no IPC and never loads the app's own page.
//!
//! A question that waits while the window is not in front shows a system notification. With
//! [`ArkUi::keep_running`](crate::ArkUi::keep_running), closing the window hides it: the app
//! stays in the menu bar (the tray elsewhere) with Open and Quit, and the Dock icon opens it.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent};
use tauri::{
    Emitter as _, LogicalPosition, LogicalSize, Manager, RunEvent, WebviewUrl, WindowEvent, Wry,
};
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

/// The window (and its own page's webview).
const MAIN: &str = "main";
/// The Browser panel's webview, added to the window over the panel.
const BROWSER: &str = "ark-browser";
const COMPUTER_TARGET: &str = "ark-computer-target";
const COMPUTER_CONTROL: &str = "ark-computer-control";
/// The Browser's cookies and storage, apart from the app's own page (macOS 14 and later; before,
/// the system's default store).
const BROWSER_STORE: [u8; 16] = *b"ark-browser-view";

/// Changes each time the window's page loads again: a Browser view being created for the page
/// before is closed as soon as it exists.
static PAGE_LOADS: AtomicU64 = AtomicU64::new(0);
/// When the Browser last sent a download to the person's browser (ms since the epoch): at most
/// one every few seconds, so a page cannot open tab after tab.
static LAST_DOWNLOAD: AtomicU64 = AtomicU64::new(0);
static REDUCE_COMPUTER_MOTION: AtomicBool = AtomicBool::new(false);

/// Where the Browser panel's events go: the page that last showed it.
#[derive(Default)]
struct BrowserEvents(Mutex<Option<tauri::ipc::Channel<Value>>>);

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

/// The window, found by label: once the Browser's webview is added, the window has two webviews
/// and is no longer a `WebviewWindow`.
pub(crate) fn main_window(app: &tauri::AppHandle) -> Option<tauri::Window> {
    app.get_window(MAIN)
}

/// The window's own page (or the runtime elsewhere it visits).
pub(crate) fn main_webview(app: &tauri::AppHandle) -> Option<tauri::Webview> {
    app.get_webview(MAIN)
}

/// Whether the Browser's webview may load `url`: anything but the app's own page and the
/// schemes that reach the app (its assets, IPC, files). Frames inside a page ask too, so
/// `about:`, `data:` and `blob:` frames stay allowed.
fn browser_may_open(url: &tauri::Url) -> bool {
    const APP_HOSTS: [&str; 4] = [
        "tauri.localhost",
        "ipc.localhost",
        "asset.localhost",
        "ark-local.invalid",
    ];
    if url.as_str() == OWN_PAGE || own_page(url) {
        return false;
    }
    if matches!(url.scheme(), "tauri" | "asset" | "ipc" | "file") {
        return false;
    }
    !url.host_str().is_some_and(|h| APP_HOSTS.contains(&h))
}

/// The addresses of `url` when it names this computer: `localhost` and its subdomains (RFC
/// 6761, never asked of DNS), loopback and the unspecified address. `None` for anything else.
fn this_computer(url: &tauri::Url) -> Option<Vec<std::net::SocketAddr>> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let port = url.port_or_known_default()?;
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let ips: Vec<IpAddr> = if host == "localhost" || host.ends_with(".localhost") {
        vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()]
    } else {
        match host.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() || ip.is_unspecified() => vec![ip],
            _ => return None,
        }
    };
    Some(
        ips.into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect(),
    )
}

/// Whether anything answers at `url`, as far as can be told without loading it. A failed load
/// shows nothing in the view and is not reported, so the panel asks first. On this computer:
/// whether the port takes a connection (nothing is sent). Elsewhere: whether the name is known;
/// nothing is connected, so a proxy never matters. A check that takes too long counts as
/// reached, and the view tries.
async fn reach(url: &tauri::Url) -> bool {
    use tokio::time::{timeout, Duration};
    let wait = Duration::from_secs(2);
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    if let Some(addrs) = this_computer(url) {
        for a in addrs {
            match timeout(wait, tokio::net::TcpStream::connect(a)).await {
                Ok(Ok(_)) | Err(_) => return true,
                Ok(Err(_)) => {}
            }
        }
        return false;
    }
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return true;
    };
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    match timeout(wait, tokio::net::lookup_host((host, port))).await {
        Ok(Ok(mut found)) => found.next().is_some(),
        Ok(Err(_)) => false,
        Err(_) => true,
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
            if webview.label() == BROWSER {
                return browser_may_open(url);
            }
            if url.as_str() != OWN_PAGE {
                return true;
            }
            if webview.label() == MAIN && !on_own_page(webview) {
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

fn browser_event(app: &tauri::AppHandle, ev: Value) {
    if let Some(ch) = app.state::<BrowserEvents>().0.lock().unwrap().as_ref() {
        let _ = ch.send(ev);
    }
}

/// The Browser's webview: its loads go to the page; a new window opens in the same view; a
/// download is not saved (the person's browser opens its address instead, at most one every
/// few seconds).
fn browser_builder(app: &tauri::AppHandle, url: tauri::Url) -> tauri::webview::WebviewBuilder<Wry> {
    let (loads, opens) = (app.clone(), app.clone());
    tauri::webview::WebviewBuilder::new(BROWSER, WebviewUrl::External(url))
        .data_store_identifier(BROWSER_STORE)
        // Clear for its whole life: the panel paints what shows through, its own colour until a
        // page arrives, then white (a page without a background of its own expects white).
        .background_color(tauri::webview::Color(0, 0, 0, 0))
        .on_page_load(move |_, p| {
            let loading = matches!(p.event(), PageLoadEvent::Started);
            browser_event(
                &loads,
                serde_json::json!({ "url": p.url().as_str(), "loading": loading }),
            );
        })
        .on_new_window(move |url, _| {
            if browser_may_open(&url) {
                let app = opens.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(w) = app.get_webview(BROWSER) {
                        let _ = w.navigate(url);
                    }
                });
            }
            NewWindowResponse::Deny
        })
        .on_download(|_, ev| {
            if let DownloadEvent::Requested { url, .. } = ev {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                let last = LAST_DOWNLOAD.load(Ordering::SeqCst);
                if matches!(url.scheme(), "http" | "https")
                    && now.saturating_sub(last) > 3000
                    && LAST_DOWNLOAD
                        .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    ark_interact::capabilities::open_in_browser(url.as_str());
                }
            }
            false
        })
}

/// Show the Browser panel's page at the panel's place (logical pixels in the window), creating
/// the view the first time. `url` loads an address.
/// Creating a view brings the app to the front on macOS, so it waits while the window is not
/// in front: `{ shown: false }`, and the page asks again.
#[tauri::command]
async fn ark_browser_show(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    url: Option<String>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<Value, String> {
    from_own_page(&webview)?;
    let target = match url.as_deref().map(str::parse::<tauri::Url>) {
        None => None,
        Some(Ok(u)) if browser_may_open(&u) => Some(u),
        Some(_) => return Err("the Browser does not open that address".into()),
    };
    let position = LogicalPosition::new(x.max(0.0), y.max(0.0));
    let size = LogicalSize::new(width.max(1.0), height.max(1.0));
    let err = |e: tauri::Error| e.to_string();
    if let Some(view) = app.get_webview(BROWSER) {
        view.set_position(position).map_err(err)?;
        view.set_size(size).map_err(err)?;
        view.show().map_err(err)?;
        if let Some(u) = target {
            view.navigate(u).map_err(err)?;
        }
        return Ok(serde_json::json!({ "shown": true }));
    }
    let window = webview.window();
    let Some(u) = target else {
        return Ok(serde_json::json!({ "shown": false }));
    };
    if !window.is_focused().unwrap_or(false) {
        return Ok(serde_json::json!({ "shown": false }));
    }
    let loads = PAGE_LOADS.load(Ordering::SeqCst);
    let view = window
        .add_child(browser_builder(&app, u), position, size)
        .map_err(err)?;
    // The page that asked loaded again (or left) while the view was made: nobody will place it.
    if PAGE_LOADS.load(Ordering::SeqCst) != loads {
        let _ = view.close();
        return Ok(serde_json::json!({ "shown": false }));
    }
    Ok(serde_json::json!({ "shown": true }))
}

/// Whether anything answers at `url` (`{ reached }`), asked before the panel loads an address.
#[tauri::command]
async fn ark_browser_reach(webview: tauri::Webview, url: String) -> Result<Value, String> {
    from_own_page(&webview)?;
    let reached = match url.parse::<tauri::Url>() {
        Ok(u) => reach(&u).await,
        Err(_) => true,
    };
    Ok(serde_json::json!({ "reached": reached }))
}

/// Where the Browser's loads (`{url, loading}`) go: the page that asks.
#[tauri::command]
fn ark_browser_events(
    webview: tauri::Webview,
    events: tauri::State<'_, BrowserEvents>,
    channel: tauri::ipc::Channel<Value>,
) -> Result<(), String> {
    from_own_page(&webview)?;
    *events.0.lock().unwrap() = Some(channel);
    Ok(())
}

/// `hide` keeps the page, `close` ends it, `back`, `forward` and `reload` act on it.
#[tauri::command]
fn ark_browser(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    action: String,
) -> Result<(), String> {
    from_own_page(&webview)?;
    let Some(view) = app.get_webview(BROWSER) else {
        return Ok(());
    };
    let done = match action.as_str() {
        "hide" => view.hide(),
        "close" => view.close(),
        "back" => view.eval("history.back()"),
        "forward" => view.eval("history.forward()"),
        "reload" => view.reload(),
        _ => return Err(format!("unknown Browser action {action}")),
    };
    done.map_err(|e| e.to_string())
}

/// The packaged app's signed-update state. `None` in development and in apps that did not opt
/// in. Windows reports an explicit unavailable state because its installer must close the app.
/// This is native-window state, not a runtime protocol method.
#[tauri::command]
fn ark_update_status(webview: tauri::Webview) -> Result<Option<serde_json::Value>, String> {
    from_own_page(&webview)?;
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        not(target_os = "windows")
    ))]
    return serde_json::to_value(crate::updates::status())
        .map(Some)
        .map_err(|e| e.to_string());
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        target_os = "windows"
    ))]
    return Ok(Some(serde_json::json!({
        "state": "unavailable",
        "reason": "platform"
    })));
    #[cfg(any(not(feature = "desktop-updater"), debug_assertions))]
    Ok(None)
}

/// Ask the embedded, HTTPS-only endpoint now. The page cannot supply a URL, headers, version or
/// downgrade policy.
#[tauri::command]
async fn ark_update_check(webview: tauri::Webview) -> Result<Option<serde_json::Value>, String> {
    from_own_page(&webview)?;
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        not(target_os = "windows")
    ))]
    return crate::updates::check_now()
        .await
        .and_then(|v| serde_json::to_value(v).map_err(Into::into))
        .map(Some)
        .map_err(|e| format!("{e:#}"));
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        target_os = "windows"
    ))]
    return Ok(Some(serde_json::json!({
        "state": "unavailable",
        "reason": "platform"
    })));
    #[cfg(any(not(feature = "desktop-updater"), debug_assertions))]
    Ok(None)
}

/// Update state changes for the own page. A page elsewhere in the window cannot subscribe.
#[tauri::command]
fn ark_update_events(
    webview: tauri::Webview,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    from_own_page(&webview)?;
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        not(target_os = "windows")
    ))]
    crate::updates::listen(channel);
    #[cfg(all(
        feature = "desktop-updater",
        not(debug_assertions),
        target_os = "windows"
    ))]
    let _ = channel.send(serde_json::json!({
        "state": "unavailable",
        "reason": "platform"
    }));
    #[cfg(any(not(feature = "desktop-updater"), debug_assertions))]
    let _ = channel;
    Ok(())
}

pub(crate) fn run(
    host: Arc<Host>,
    assets: Arc<Assets>,
    mut context: tauri::Context<Wry>,
    keep_running: bool,
    overlay_accent: Option<String>,
) -> std::process::ExitCode {
    // WebKit keeps local app modules across updates. A versioned base path makes every relative
    // stylesheet, script and module URL new for this ARK release, while local storage stays put.
    if let Some(window) = context
        .config_mut()
        .app
        .windows
        .iter_mut()
        .find(|window| window.label == MAIN)
    {
        window.url = WebviewUrl::App(assets.versioned("index.html").into());
    }
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
    let computer_events = host.uses_computer().then(|| host.subscribe());
    let builder = tauri::Builder::default()
        // A second launch brings this window forward instead of starting a second runtime on
        // the same store.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show(app)
        }));
    #[cfg(feature = "desktop-updater")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    let built = builder
        .manage(State(host))
        .manage(BrowserEvents::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(window_plugin())
        .invoke_handler(tauri::generate_handler![
            ark_call,
            ark_subscribe,
            ark_pick_folder,
            ark_browser_show,
            ark_browser_events,
            ark_browser_reach,
            ark_browser,
            ark_update_status,
            ark_update_check,
            ark_update_events
            ,ark_computer_stop
            ,ark_computer_check
            ,ark_computer_motion
        ])
        // The window's page loads again (a reload, a visit to a runtime elsewhere, the way
        // back): the Browser's view belonged to the page before.
        .on_page_load(|webview, p| {
            if webview.label() == MAIN && matches!(p.event(), PageLoadEvent::Started) {
                PAGE_LOADS.fetch_add(1, Ordering::SeqCst);
                if let Some(view) = webview.app_handle().get_webview(BROWSER) {
                    let _ = view.close();
                }
            }
        })
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
            if let Some(v) = main_webview(app.handle()) {
                if let Some(url) = v.url().ok().filter(own_page) {
                    let _ = HOME.set(url);
                }
            }
            if let Some(w) = main_window(app.handle()) {
                fit_to_screen(&w);
                let _ = w.show();
            }
            if keep_running {
                tray(app, &name)?;
            }
            if let Some(events) = computer_events {
                if let Err(error) = start_computer_overlay(
                    app,
                    events,
                    overlay_accent.as_deref().unwrap_or("#335eb7"),
                ) {
                    tracing::warn!(%error, "Computer overlay is unavailable; the main window controls remain available");
                }
            }
            #[cfg(all(
                feature = "desktop-updater",
                not(debug_assertions),
                not(target_os = "windows")
            ))]
            crate::updates::start(app.handle().clone());
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
        // Quitting: agents' work under way reaches a safe point and continues next time the app
        // opens, instead of being cut in the middle of a change (ADR 0038).
        if let RunEvent::Exit = ev {
            let host = app.state::<State>().0.clone();
            tauri::async_runtime::block_on(
                host.stop(ark_sdk::prelude::ark_runtime::cancel::stop_grace()),
            );
            return;
        }
        // The Dock icon (or opening the app again) brings back a hidden window.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = ev {
            show(app);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = app;
    });
    std::process::ExitCode::SUCCESS
}

#[tauri::command]
async fn ark_computer_stop(
    webview: tauri::Webview,
    state: tauri::State<'_, State>,
) -> Result<Value, String> {
    if !matches!(webview.label(), COMPUTER_CONTROL | MAIN) {
        return Err("only ARK's Computer controls can stop it".into());
    }
    Ok(serde_json::json!({ "stopped": state.0.computer_stop() }))
}

#[tauri::command]
async fn ark_computer_check(
    webview: tauri::Webview,
    state: tauri::State<'_, State>,
) -> Result<Value, String> {
    if !matches!(webview.label(), COMPUTER_CONTROL | MAIN) {
        return Err("only ARK's Computer controls can check it".into());
    }
    state.0.computer_check().await
}

#[tauri::command]
fn ark_computer_motion(webview: tauri::Webview, reduce: bool) -> Result<(), String> {
    if !matches!(webview.label(), COMPUTER_TARGET | COMPUTER_CONTROL) {
        return Err("only ARK's Computer overlay can set its motion preference".into());
    }
    REDUCE_COMPUTER_MOTION.store(reduce, Ordering::SeqCst);
    Ok(())
}

fn start_computer_overlay(
    app: &tauri::App,
    mut events: tokio::sync::broadcast::Receiver<Value>,
    accent: &str,
) -> tauri::Result<()> {
    let target = tauri::WebviewWindowBuilder::new(
        app,
        COMPUTER_TARGET,
        WebviewUrl::App("computer-overlay.html?kind=target".into()),
    )
    .inner_size(52.0, 52.0)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .focused(false)
    .visible(false)
    .build()?;
    target.set_ignore_cursor_events(true)?;
    let _control = tauri::WebviewWindowBuilder::new(
        app,
        COMPUTER_CONTROL,
        WebviewUrl::App("computer-overlay.html?kind=control".into()),
    )
    .inner_size(300.0, 44.0)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .focused(false)
    .visible(false)
    .build()?;
    let accent = accent.to_string();
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut newest = 0;
        let mut last_target: Option<(f64, f64)> = None;
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            if event["kind"] != "computer_action" {
                continue;
            }
            let Some(activity) = event.get("activity") else {
                continue;
            };
            let sequence = activity["sequence"].as_u64().unwrap_or(0);
            if !new_computer_activity(sequence, newest) {
                continue;
            }
            newest = sequence;
            let phase = activity["phase"].as_str().unwrap_or("idle");
            let line = event["line"].as_str().unwrap_or("");
            let payload = serde_json::json!({
                "sequence": sequence, "phase": phase, "line": line, "accent": accent
            });
            if let Some(window) = handle.get_webview_window(COMPUTER_TARGET) {
                let _ = window.emit("computer-overlay", &payload);
            }
            if let Some(window) = handle.get_webview_window(COMPUTER_CONTROL) {
                let _ = window.emit("computer-overlay", &payload);
            }
            let target_frame = activity.get("target").filter(|v| v.is_object());
            if let Some(frame) = target_frame {
                let x = frame["x"].as_f64().unwrap_or(0.0);
                let y = frame["y"].as_f64().unwrap_or(0.0);
                let w = frame["w"].as_f64().unwrap_or(0.0);
                let h = frame["h"].as_f64().unwrap_or(0.0);
                let center = (x + w / 2.0, y + h / 2.0);
                if let Some(window) = handle.get_webview_window(COMPUTER_TARGET) {
                    let _ = window.show();
                    if let Some(from) = last_target {
                        if !REDUCE_COMPUTER_MOTION.load(Ordering::SeqCst) {
                            for step in 1..=7 {
                                let t = step as f64 / 7.0;
                                let eased = 1.0 - (1.0 - t).powi(3);
                                let px = from.0 + (center.0 - from.0) * eased - 26.0;
                                let py = from.1 + (center.1 - from.1) * eased - 26.0;
                                let _ = window.set_position(LogicalPosition::new(px, py));
                                tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                            }
                        } else {
                            let _ = window.set_position(LogicalPosition::new(
                                center.0 - 26.0,
                                center.1 - 26.0,
                            ));
                        }
                    } else {
                        let _ = window
                            .set_position(LogicalPosition::new(center.0 - 26.0, center.1 - 26.0));
                    }
                }
                last_target = Some(center);
                if let Some(window) = handle.get_webview_window(COMPUTER_CONTROL) {
                    let (px, py) = overlay_control_position(&handle, x + w / 2.0, y, 300.0, 44.0);
                    let _ = window.set_position(LogicalPosition::new(px, py));
                }
            } else if let Some(window) = handle.get_webview_window(COMPUTER_TARGET) {
                let _ = window.hide();
                last_target = None;
            }
            if computer_overlay_hidden(phase) {
                if let Some(window) = handle.get_webview_window(COMPUTER_TARGET) {
                    let _ = window.hide();
                }
                if let Some(window) = handle.get_webview_window(COMPUTER_CONTROL) {
                    let _ = window.hide();
                }
                last_target = None;
            } else if let Some(window) = handle.get_webview_window(COMPUTER_CONTROL) {
                let _ = window.show();
            }
        }
    });
    Ok(())
}

fn new_computer_activity(sequence: u64, newest: u64) -> bool {
    sequence > newest
}

fn computer_overlay_hidden(phase: &str) -> bool {
    matches!(
        phase,
        "idle" | "stopped" | "failed" | "refused" | "confirmed"
    )
}

fn overlay_control_position(
    app: &tauri::AppHandle,
    target_x: f64,
    target_y: f64,
    width: f64,
    height: f64,
) -> (f64, f64) {
    let Some(monitor) = app.monitor_from_point(target_x, target_y).ok().flatten() else {
        return (target_x - width / 2.0, target_y - height - 12.0);
    };
    let scale = monitor.scale_factor();
    let position = monitor.position().to_logical::<f64>(scale);
    let size = monitor.size().to_logical::<f64>(scale);
    let x = (target_x - width / 2.0).clamp(position.x + 8.0, position.x + size.width - width - 8.0);
    let above = target_y - height - 12.0;
    let y = if above >= position.y + 8.0 {
        above
    } else {
        target_y + 64.0
    };
    (
        x,
        y.clamp(position.y + 8.0, position.y + size.height - height - 8.0),
    )
}

fn show(app: &tauri::AppHandle) {
    if let Some(w) = main_window(app) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The menu-bar item: controls that remain reachable while another app is in front.
fn tray(app: &tauri::App, name: &str) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", format!("Open {name}"), true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "computer-stop", "Stop Computer", true, None::<&str>)?;
    let check = MenuItem::with_id(
        app,
        "computer-check",
        "Check message again",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", format!("Quit {name}"), true, None::<&str>)?;
    let menu = if app.state::<State>().0.uses_computer() {
        Menu::with_items(app, &[&open, &stop, &check, &quit])?
    } else {
        Menu::with_items(app, &[&open, &quit])?
    };
    let mut b = TrayIconBuilder::with_id("ark")
        .tooltip(name)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "open" => show(app),
            "computer-stop" => {
                app.state::<State>().0.computer_stop();
            }
            "computer-check" => {
                let host = app.state::<State>().0.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = host.computer_check().await {
                        tracing::warn!(%error, "Computer message check did not clear the safety block");
                    }
                });
            }
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
            let in_front = main_window(&app).is_some_and(|w| {
                w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)
            }) && main_webview(&app)
                .is_some_and(|v| v.url().is_ok_and(|u| own_page(&u)));
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

/// Keep the window inside the screen it opens on: a size made for a large display is cut to the
/// screen's work area (a 1366×768 laptop at 125% has about 1090×580 to show) and centered.
fn fit_to_screen(w: &tauri::Window) {
    let (Ok(Some(monitor)), Ok(size)) = (w.current_monitor(), w.outer_size()) else {
        return;
    };
    let area = monitor.work_area().size;
    if size.width > area.width || size.height > area.height {
        let fitted =
            tauri::PhysicalSize::new(size.width.min(area.width), size.height.min(area.height));
        let _ = w.set_size(fitted);
        let _ = w.center();
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

    #[test]
    fn terminal_computer_activity_hides_the_overlay_but_attention_stays_visible() {
        for phase in ["idle", "stopped", "failed", "refused", "confirmed"] {
            assert!(computer_overlay_hidden(phase), "{phase}");
        }
        for phase in [
            "locating",
            "acting",
            "verifying",
            "draft_present",
            "submitted_unconfirmed",
        ] {
            assert!(!computer_overlay_hidden(phase), "{phase}");
        }
    }

    #[test]
    fn stale_computer_activity_cannot_restore_an_old_overlay() {
        assert!(new_computer_activity(8, 7));
        assert!(!new_computer_activity(7, 7));
        assert!(!new_computer_activity(6, 7));
    }

    #[test]
    fn the_browser_never_loads_the_apps_own_page() {
        let may = |u: &str| browser_may_open(&u.parse().unwrap());
        for refused in [
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/",
            "ipc://localhost/ark_call",
            "http://ipc.localhost/ark_call",
            "asset://localhost/x",
            "file:///etc/passwd",
            OWN_PAGE,
            "https://ark-local.invalid/elsewhere",
        ] {
            assert!(!may(refused), "{refused}");
        }
        for allowed in [
            "http://localhost:3000/",
            "http://app.localhost:5173/",
            "https://github.com/",
            "about:blank",
            "about:srcdoc",
            "data:text/html,hi",
            "blob:https://example.com/0b1c",
        ] {
            assert!(may(allowed), "{allowed}");
        }
    }

    #[test]
    fn this_computer_is_localhost_and_loopback_only() {
        let here = |u: &str| this_computer(&u.parse().unwrap()).map(|a| a.len());
        assert_eq!(here("http://localhost:3000/"), Some(2));
        assert_eq!(here("http://app.localhost:3000/"), Some(2));
        assert_eq!(here("http://127.0.0.1:8080/"), Some(1));
        assert_eq!(here("http://[::1]:8080/"), Some(1));
        assert_eq!(here("http://0.0.0.0:8080/"), Some(1));
        assert_eq!(here("http://10.0.0.1/"), None);
        assert_eq!(here("https://example.com/"), None);
        assert_eq!(here("https://localhost.example.com/"), None);
    }

    /// A port nothing listens on is not reached; a listening one is, by name (`localhost` and
    /// its subdomains go to this computer without DNS) or by number.
    #[tokio::test]
    async fn the_browser_asks_this_computer_whether_its_port_answers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reached = |u: String| async move { reach(&u.parse().unwrap()).await };
        assert!(reached(format!("http://127.0.0.1:{port}/")).await);
        assert!(reached(format!("http://localhost:{port}/")).await);
        assert!(reached(format!("http://app.localhost:{port}/")).await);
        drop(listener);
        assert!(!reached(format!("http://localhost:{port}/")).await);
        assert!(!reached(format!("http://127.0.0.1:{port}/")).await);
        // Not the Browser's to judge: the view says what it refuses.
        assert!(reached("file:///etc/hosts".into()).await);
        assert!(reached("about:blank".into()).await);
    }
}
