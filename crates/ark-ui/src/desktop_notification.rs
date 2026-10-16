//! Keep the real notification service, but bootstrap its page API only in the own main page.

use tauri::ipc::Invoke;
use tauri::plugin::{Plugin, TauriPlugin};
use tauri::webview::PageLoadPayload;
use tauri::{AppHandle, RunEvent, Url, Webview, Window, Wry};

const GUARD: &str = include_str!("desktop_notification.js");
const SCRIPT: &str = "/* __ARK_NOTIFICATION_SCRIPT__ */";

pub(crate) fn init() -> impl Plugin<Wry> {
    MainPageNotification(tauri_plugin_notification::init())
}

struct MainPageNotification(TauriPlugin<Wry>);

impl Plugin<Wry> for MainPageNotification {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn initialize(
        &mut self,
        app: &AppHandle<Wry>,
        config: serde_json::Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.0.initialize(app, config)
    }

    fn initialization_script(&self) -> Option<String> {
        // The legacy default keeps upstream's current main-frame-only setting. The test below
        // fails if a dependency upgrade changes that setting or removes the original script.
        self.0
            .initialization_script()
            .map(|script| GUARD.replace(SCRIPT, &script))
    }

    fn window_created(&mut self, window: Window<Wry>) {
        self.0.window_created(window);
    }

    fn webview_created(&mut self, webview: Webview<Wry>) {
        self.0.webview_created(webview);
    }

    fn on_navigation(&mut self, webview: &Webview<Wry>, url: &Url) -> bool {
        self.0.on_navigation(webview, url)
    }

    fn on_page_load(&mut self, webview: &Webview<Wry>, payload: &PageLoadPayload<'_>) {
        self.0.on_page_load(webview, payload);
    }

    fn on_event(&mut self, app: &AppHandle<Wry>, event: &RunEvent) {
        self.0.on_event(app, event);
    }

    fn extend_api(&mut self, invoke: Invoke<Wry>) -> bool {
        self.0.extend_api(invoke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_real_plugin_and_its_main_frame_script() {
        let original = tauri_plugin_notification::init::<Wry>();
        let original_script = original.initialization_script_2().unwrap();
        assert!(original_script.for_main_frame_only);
        assert!(!original_script.script.is_empty());
        assert_eq!(GUARD.matches(SCRIPT).count(), 1);
        let adapter = init();
        assert_eq!(adapter.name(), original.name());
        let guarded = adapter.initialization_script_2().unwrap();
        assert_eq!(
            guarded.for_main_frame_only,
            original_script.for_main_frame_only
        );
        assert_eq!(
            guarded.script,
            GUARD.replace(SCRIPT, &original_script.script)
        );
    }

    #[test]
    fn both_window_builders_use_the_guarded_plugin() {
        let builders = include_str!("desktop.rs");
        assert_eq!(
            builders
                .matches(".plugin(crate::desktop_notification::init())")
                .count(),
            2
        );
        assert!(!builders.contains(".plugin(tauri_plugin_notification::init())"));
    }

    #[test]
    fn actual_upstream_bootstrap_obeys_the_context_guard_when_requested() {
        if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
            return;
        }
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/web/notification.mjs"
            ))
            .arg("--upstream")
            .stdin(Stdio::piped())
            .spawn()
            .expect("node is on PATH");
        let payload = serde_json::json!({
            "original": tauri_plugin_notification::init::<Wry>().initialization_script().unwrap(),
            "guarded": init().initialization_script().unwrap(),
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }
}
