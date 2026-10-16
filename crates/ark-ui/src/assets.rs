//! The files a window loads: ARK's workspace (embedded from `web/`) plus the consumer's own
//! scripts, styles and images. `index.html` is composed per app so consumer files load after
//! ARK's. Two environment variables help while developing the page itself: `ARK_UI_WEB=<dir>`
//! serves ARK's files from disk, `ARK_UI_ASSETS=<dir>` does the same for the consumer's.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::path::{Component, Path};
use std::sync::RwLock;
use std::time::UNIX_EPOCH;

use crate::host::Theme;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

const VERSIONED_ROOT: &str = "_ark";
const DEV_REVISION: &str = "__ark_dev_revision";

pub struct Assets {
    title: String,
    consumer: HashMap<String, Cow<'static, [u8]>>,
    scripts: Vec<String>,
    required_scripts: Vec<String>,
    styles: Vec<String>,
    theme: Theme,
    namespace: String,
    release: RwLock<Option<ReleasedAssets>>,
    web_dir: Option<PathBuf>,
    asset_dir: Option<PathBuf>,
    frame_assets: std::collections::HashSet<String>,
    wasm_workers: std::collections::HashSet<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReleasedAssets {
    pub(crate) root: PathBuf,
    pub(crate) revision: String,
}

impl Assets {
    pub(crate) fn new(
        title: String,
        consumer: HashMap<String, Cow<'static, [u8]>>,
        scripts: Vec<String>,
        required_scripts: Vec<String>,
        styles: Vec<String>,
        theme: Theme,
        version: Option<String>,
    ) -> Self {
        let consumer_version = version.filter(|version| {
            !version.is_empty()
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        });
        let namespace = consumer_version
            .map(|consumer| format!("{}/{consumer}", env!("CARGO_PKG_VERSION")))
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        Assets {
            title,
            consumer,
            scripts,
            required_scripts,
            styles,
            theme,
            namespace,
            release: RwLock::new(None),
            web_dir: std::env::var_os("ARK_UI_WEB").map(PathBuf::from),
            asset_dir: std::env::var_os("ARK_UI_ASSETS").map(PathBuf::from),
            frame_assets: std::collections::HashSet::new(),
            wasm_workers: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn versioned(&self, path: &str) -> String {
        format!("{VERSIONED_ROOT}/{}/{path}", self.current_namespace())
    }

    pub(crate) fn with_frame_assets(mut self, paths: std::collections::HashSet<String>) -> Self {
        self.frame_assets = paths;
        self
    }

    pub(crate) fn is_frame_asset(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        let prefix = format!("{VERSIONED_ROOT}/{}/", self.current_namespace());
        let path = path.strip_prefix(&prefix).unwrap_or(path);
        confined_asset_path(path) && self.frame_assets.contains(path)
    }

    pub(crate) fn with_wasm_workers(mut self, paths: std::collections::HashSet<String>) -> Self {
        self.wasm_workers = paths;
        self
    }

    pub(crate) fn is_wasm_worker(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        let prefix = format!("{VERSIONED_ROOT}/{}/", self.current_namespace());
        let path = path.strip_prefix(&prefix).unwrap_or(path);
        confined_asset_path(path) && self.wasm_workers.contains(path)
    }

    fn current_namespace(&self) -> String {
        self.release
            .read()
            .unwrap()
            .as_ref()
            .map(|release| format!("{}/{}", self.namespace, release.revision))
            .unwrap_or_else(|| self.namespace.clone())
    }

    #[cfg(feature = "desktop-updater")]
    pub(crate) fn use_release(&self, release: Option<ReleasedAssets>) {
        *self.release.write().unwrap() = release;
    }

    #[cfg(feature = "desktop-updater")]
    pub(crate) fn release_revision(&self) -> String {
        self.release
            .read()
            .unwrap()
            .as_ref()
            .map_or_else(|| "embedded".into(), |release| release.revision.clone())
    }

    /// The app's name, as the page's title says it.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// A file by path (`index.html`, `js/app.js`, `ext/app.js`), with its media type.
    pub fn get(&self, path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
        let path = path.trim_start_matches('/');
        let prefix = format!("{VERSIONED_ROOT}/{}/", self.current_namespace());
        let path = path.strip_prefix(&prefix).unwrap_or(path);
        let path = if path.is_empty() { "index.html" } else { path };
        if !confined_asset_path(path) {
            return None;
        }
        if path == "index.html" {
            return Some((
                Cow::Owned(self.index().into_bytes()),
                "text/html; charset=utf-8",
            ));
        }
        if path == DEV_REVISION && self.development() {
            return Some((
                Cow::Owned(self.development_revision().into_bytes()),
                "text/plain; charset=utf-8",
            ));
        }
        let mime = mime(path);
        if let Some(rest) = path.strip_prefix("ext/") {
            if let Some(release) = self.release.read().unwrap().as_ref() {
                if let Ok(b) = std::fs::read(release.root.join("consumer").join(rest)) {
                    return Some((Cow::Owned(b), mime));
                }
            }
            if let Some(dir) = &self.asset_dir {
                if let Ok(b) = std::fs::read(dir.join(rest)) {
                    return Some((Cow::Owned(b), mime));
                }
            }
            return self.consumer.get(rest).map(|b| (b.clone(), mime));
        }
        Some((self.ark_file(path)?, mime))
    }

    fn ark_file(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        if let Some(release) = self.release.read().unwrap().as_ref() {
            if let Ok(b) = std::fs::read(release.root.join("web").join(path)) {
                return Some(Cow::Owned(b));
            }
        }
        if let Some(dir) = &self.web_dir {
            if let Ok(b) = std::fs::read(dir.join(path)) {
                return Some(Cow::Owned(b));
            }
        }
        WEB.iter()
            .find(|(p, _)| *p == path)
            .map(|(_, b)| Cow::Borrowed(*b))
    }

    /// Every servable path (for a transport that must enumerate them).
    pub fn paths(&self) -> Vec<String> {
        let mut v: Vec<String> = WEB.iter().map(|(p, _)| p.to_string()).collect();
        v.extend(self.consumer.keys().map(|k| format!("ext/{k}")));
        v
    }

    fn index(&self) -> String {
        let raw = self
            .ark_file("index.html")
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let styles: String = self
            .styles
            .iter()
            .map(|s| format!("    <link rel=\"stylesheet\" href=\"ext/{s}\" />\n"))
            .collect();
        let scripts: String = self
            .scripts
            .iter()
            .map(|s| {
                let id = s
                    .strip_suffix(".js")
                    .unwrap_or(s)
                    .rsplit('/')
                    .next()
                    .unwrap_or(s);
                let required = self.required_scripts.iter().any(|required| required == s);
                format!(
                    "    <script type=\"module\" src=\"ext/{s}\"{}></script>\n",
                    if required {
                        format!(" data-ark-extension=\"{}\"", escape(id))
                    } else {
                        String::new()
                    }
                )
            })
            .collect();
        let reload = if self.development() {
            "    <script type=\"module\" src=\"js/dev-reload.js\"></script>\n"
        } else {
            ""
        };
        raw.replace("<!--ark:title-->", &escape(&self.title))
            .replace("<!--ark:theme-->", self.theme.as_str())
            .replace(
                "<html lang=\"en\"",
                &format!(
                    "<html lang=\"en\" data-ark-assets=\"{}\"",
                    escape(
                        self.release
                            .read()
                            .unwrap()
                            .as_ref()
                            .map_or("embedded", |release| release.revision.as_str())
                    )
                ),
            )
            .replace("    <!--ark:styles-->\n", &styles)
            .replace("    <!--ark:scripts-->\n", &scripts)
            .replace("  </head>\n", &format!("{reload}  </head>\n"))
    }

    fn development(&self) -> bool {
        self.web_dir.is_some() || self.asset_dir.is_some()
    }

    fn development_revision(&self) -> String {
        let mut files = Vec::new();
        for dir in [&self.web_dir, &self.asset_dir].into_iter().flatten() {
            collect_files(dir, dir, &mut files);
        }
        files.sort();
        let mut state = std::collections::hash_map::DefaultHasher::new();
        for (path, len, modified) in files {
            path.hash(&mut state);
            len.hash(&mut state);
            modified.hash(&mut state);
        }
        format!("{:016x}", state.finish())
    }
}

fn confined_asset_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\\')
        && !Path::new(value).is_absolute()
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn collect_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(PathBuf, u64, u128)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        out.push((
            path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
            metadata.len(),
            modified,
        ));
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn valid_worker_path(path: &str) -> bool {
    path.strip_suffix(".js")
        .is_some_and(|stem| valid_frame_path(&format!("{stem}.html")))
}

pub(crate) fn valid_frame_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '%', ':', '?', '#'])
        && path.ends_with(".html")
        && confined_asset_path(path)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_worker_registration_is_exact_across_asset_roots() {
        let mut assets = Assets::new(
            "Test".into(),
            HashMap::from([("reader.js".into(), Cow::Borrowed(b"embedded".as_slice()))]),
            vec![],
            vec![],
            vec![],
            Theme::Light,
            None,
        )
        .with_wasm_workers(std::collections::HashSet::from(["ext/reader.js".into()]));
        for path in [
            "ext/reader.js".to_string(),
            assets.versioned("ext/reader.js"),
        ] {
            assert!(assets.is_wasm_worker(&path));
            assert_eq!(
                assets.get(&path).unwrap().1,
                "text/javascript; charset=utf-8"
            );
        }
        for path in [
            "/",
            "index.html",
            "ext/reader.js.js",
            "ext/%72eader.js",
            "ext/../ext/reader.js",
            "ext/./reader.js",
            "ext/other.js",
        ] {
            assert!(!assets.is_wasm_worker(path), "{path}");
        }
        for path in [
            "../reader.js",
            "/reader.js",
            "a/../reader.js",
            "a\\reader.js",
            "reader.html",
            "reader.js?x",
            "%72eader.js",
        ] {
            assert!(!valid_worker_path(path), "{path}");
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("reader.js"), "development").unwrap();
        assets.asset_dir = Some(root.path().into());
        assert!(assets.is_wasm_worker("ext/reader.js"));
        assert_eq!(
            assets.get("ext/reader.js").unwrap().0.as_ref(),
            b"development"
        );
        std::fs::create_dir(root.path().join("consumer")).unwrap();
        std::fs::write(root.path().join("consumer/reader.js"), "release").unwrap();
        *assets.release.write().unwrap() = Some(ReleasedAssets {
            root: root.path().into(),
            revision: "proof".into(),
        });
        let path = assets.versioned("ext/reader.js");
        assert!(assets.is_wasm_worker(&path));
        assert_eq!(assets.get(&path).unwrap().0.as_ref(), b"release");
        assert!(!assets.is_wasm_worker("ext/reader.wasm"));
    }

    #[test]
    fn only_exact_registered_consumer_html_can_be_framed() {
        let mut assets = Assets::new(
            "Test".into(),
            HashMap::from([
                (
                    "viewer.html".into(),
                    Cow::Borrowed(b"<p>viewer</p>".as_slice()),
                ),
                (
                    "sibling.html".into(),
                    Cow::Borrowed(b"<p>sibling</p>".as_slice()),
                ),
            ]),
            vec![],
            vec![],
            vec![],
            Theme::Light,
            None,
        )
        .with_frame_assets(std::collections::HashSet::from(["ext/viewer.html".into()]));
        for path in [
            "ext/viewer.html".into(),
            format!(
                "/{VERSIONED_ROOT}/{}/ext/viewer.html",
                assets.current_namespace()
            ),
        ] {
            assert!(assets.get(&path).is_some());
            assert!(assets.is_frame_asset(&path));
        }
        for path in [
            "",
            "/",
            "index.html",
            "ext/sibling.html",
            "ext/viewer.html.js",
            "ext/viewer.html/other.html",
            "ext/%76iewer.html",
            "ext/../ext/viewer.html",
            "ext/./viewer.html",
        ] {
            assert!(!assets.is_frame_asset(path), "{path}");
        }
        for path in [
            "",
            "/viewer.html",
            "../viewer.html",
            "a/../viewer.html",
            "a\\viewer.html",
            "viewer.js",
            "%2e.html",
        ] {
            assert!(!valid_frame_path(path), "{path}");
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("viewer.html"), "development").unwrap();
        assets.asset_dir = Some(root.path().into());
        assert!(assets.is_frame_asset("ext/viewer.html"));
        assert_eq!(
            assets.get("ext/viewer.html").unwrap().0.as_ref(),
            b"development"
        );
        std::fs::create_dir(root.path().join("consumer")).unwrap();
        std::fs::write(root.path().join("consumer/viewer.html"), "release").unwrap();
        *assets.release.write().unwrap() = Some(ReleasedAssets {
            root: root.path().into(),
            revision: "proof".into(),
        });
        let versioned = format!(
            "/{VERSIONED_ROOT}/{}/ext/viewer.html",
            assets.current_namespace()
        );
        assert!(assets.is_frame_asset(&versioned));
        assert_eq!(assets.get(&versioned).unwrap().0.as_ref(), b"release");
        assert!(!assets.is_frame_asset("ext/sibling.html"));
    }

    #[test]
    fn index_loads_consumer_files_after_ark() {
        let a = Assets::new(
            "Demo <1>".into(),
            HashMap::from([
                ("demo.js".to_string(), Cow::Borrowed(&b"export {}"[..])),
                ("optional.js".to_string(), Cow::Borrowed(&b"export {}"[..])),
                ("demo.css".to_string(), Cow::Borrowed(&b"body {}"[..])),
            ]),
            vec!["demo.js".into(), "optional.js".into()],
            vec!["demo.js".into()],
            vec!["demo.css".into()],
            Theme::Light,
            Some("app-2.4.1".into()),
        );
        let (html, mime) = a.get("/").unwrap();
        let html = String::from_utf8(html.into_owned()).unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(html.contains("<title>Demo &lt;1&gt;</title>"), "{html}");
        assert!(html.contains(r#"data-theme-default="light""#), "{html}");
        let ark = html.find("js/app.js").unwrap();
        let ext = html.find("ext/demo.js").unwrap();
        assert!(ext > ark, "consumer scripts load after ARK's");
        assert!(html.contains(r#"data-ark-extension="demo""#), "{html}");
        assert!(
            html.contains(r#"src="ext/optional.js"></script>"#),
            "{html}"
        );
        assert!(html.contains("href=\"ext/demo.css\""));
        assert!(a.get("ext/demo.js").is_some());
        assert!(a.get("../Cargo.toml").is_none());
        assert!(a.get("ext//etc/passwd").is_none());
        assert!(a.get("js//etc/passwd").is_none());
        assert!(a.get("js/app.js").is_some());

        let (versioned_html, _) = a.get(&a.versioned("index.html")).unwrap();
        let versioned_html = String::from_utf8(versioned_html.into_owned()).unwrap();
        assert!(versioned_html.contains("js/app.js"));
        assert!(versioned_html.contains("ext/demo.css"));
        for relative in ["app.css", "js/app.js", "ext/demo.css", "ext/demo.js"] {
            assert!(
                a.get(&a.versioned(relative)).is_some(),
                "versioned index dependency does not resolve: {relative}"
            );
        }
        assert_eq!(
            a.versioned("index.html"),
            format!("_ark/{}/app-2.4.1/index.html", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn unsafe_consumer_versions_fall_back_to_the_ark_version() {
        let a = Assets::new(
            "Demo".into(),
            HashMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Theme::Light,
            Some("../old".into()),
        );
        assert_eq!(
            a.versioned("index.html"),
            format!("_ark/{}/index.html", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn development_files_reload_the_page_after_a_save() {
        let root = std::env::temp_dir().join(format!(
            "ark-ui-live-assets-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("change.js");
        std::fs::write(&file, "one").unwrap();

        let mut a = Assets::new(
            "Demo".into(),
            HashMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Theme::Light,
            None,
        );
        assert!(!String::from_utf8(a.get("/").unwrap().0.into_owned())
            .unwrap()
            .contains("dev-reload.js"));
        assert!(a.get(DEV_REVISION).is_none());
        a.asset_dir = Some(root.clone());
        let html = String::from_utf8(a.get("/").unwrap().0.into_owned()).unwrap();
        assert!(html.contains("js/dev-reload.js"), "{html}");
        let before = a.get(DEV_REVISION).unwrap().0;
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&file, "two").unwrap();
        let after = a.get(DEV_REVISION).unwrap().0;
        assert_ne!(before, after);

        std::fs::remove_dir_all(root).unwrap();
    }
}
