//! The files a window loads: ARK's workspace (embedded from `web/`) plus the consumer's own
//! scripts, styles and images. `index.html` is composed per app so consumer files load after
//! ARK's. Two environment variables help while developing the page itself: `ARK_UI_WEB=<dir>`
//! serves ARK's files from disk, `ARK_UI_ASSETS=<dir>` does the same for the consumer's.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub struct Assets {
    title: String,
    consumer: HashMap<String, Cow<'static, [u8]>>,
    scripts: Vec<String>,
    styles: Vec<String>,
    web_dir: Option<PathBuf>,
    asset_dir: Option<PathBuf>,
}

impl Assets {
    pub(crate) fn new(
        title: String,
        consumer: HashMap<String, Cow<'static, [u8]>>,
        scripts: Vec<String>,
        styles: Vec<String>,
    ) -> Self {
        Assets {
            title,
            consumer,
            scripts,
            styles,
            web_dir: std::env::var_os("ARK_UI_WEB").map(PathBuf::from),
            asset_dir: std::env::var_os("ARK_UI_ASSETS").map(PathBuf::from),
        }
    }

    /// A file by path (`index.html`, `js/app.js`, `ext/app.js`), with its media type.
    pub fn get(&self, path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
        let path = path.trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        if path.contains("..") {
            return None;
        }
        if path == "index.html" {
            return Some((
                Cow::Owned(self.index().into_bytes()),
                "text/html; charset=utf-8",
            ));
        }
        let mime = mime(path);
        if let Some(rest) = path.strip_prefix("ext/") {
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
            .map(|s| format!("    <script type=\"module\" src=\"ext/{s}\"></script>\n"))
            .collect();
        raw.replace("<!--ark:title-->", &escape(&self.title))
            .replace("    <!--ark:styles-->\n", &styles)
            .replace("    <!--ark:scripts-->\n", &scripts)
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
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
    fn index_loads_consumer_files_after_ark() {
        let a = Assets::new(
            "Demo <1>".into(),
            HashMap::from([("demo.js".to_string(), Cow::Borrowed(&b"export {}"[..]))]),
            vec!["demo.js".into()],
            vec!["demo.css".into()],
        );
        let (html, mime) = a.get("/").unwrap();
        let html = String::from_utf8(html.into_owned()).unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(html.contains("<title>Demo &lt;1&gt;</title>"), "{html}");
        let ark = html.find("js/app.js").unwrap();
        let ext = html.find("ext/demo.js").unwrap();
        assert!(ext > ark, "consumer scripts load after ARK's");
        assert!(html.contains("href=\"ext/demo.css\""));
        assert!(a.get("ext/demo.js").is_some());
        assert!(a.get("../Cargo.toml").is_none());
        assert!(a.get("js/app.js").is_some());
    }
}
