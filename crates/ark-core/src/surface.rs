//! Surfaces: the composable views that make up the workspace. A `Surface` is a panel
//! showing one thing — the agent, a code/text editor, a terminal, a browser, an artifact
//! viewer, a diff, the notebook, structured data. A UI arranges them in splits and tabs;
//! adapters and plugins can contribute their own kinds (hence the `Other` escape hatch),
//! exactly as they contribute actions and artifacts.
//!
//! The point of modelling surfaces in the core is [`Focus`]: a compact, structured
//! description of *what the user is currently looking at* — which surface, which
//! artifact/file/selection. The agent reads it so a question like "what does this do?"
//! resolves against the thing on screen, without the user restating it. Focus is
//! transient UI state (never persisted as evidence) and names no tool or domain.

use crate::ids::ArtifactId;
use serde::{Deserialize, Serialize};

/// The kind of a surface. Universal formats and roles, never a tool or domain name. A
/// domain contributes its own views through `Other` (a security product adds
/// `Other("decompiler")`; a data agent adds `Other("chart")`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    /// The agent conversation/canvas.
    Agent,
    /// A code/text editor over a workspace file.
    Editor,
    /// A shell/terminal.
    Terminal,
    /// A web browser (a URL).
    Browser,
    /// A generic artifact/file viewer.
    ArtifactViewer,
    /// A diff view (before/after).
    DiffViewer,
    /// The notebook.
    Notebook,
    /// A structured-data (JSON/table) view.
    StructuredData,
    /// A plugin- or domain-contributed surface kind.
    Other(String),
}

impl SurfaceKind {
    pub fn as_tag(&self) -> String {
        match self {
            SurfaceKind::Other(s) => s.clone(),
            other => serde_json::to_value(other)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "surface".into()),
        }
    }
}

/// What a surface is currently showing. Every field but `kind` is optional — a browser
/// has a `locator` (URL), an editor has a `path`, an artifact viewer has an `artifact`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SurfaceRef {
    pub kind_tag: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactId>,
    /// A workspace-relative file path, when the surface shows a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A free-form locator: a URL, an address, a symbol, an invocation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

impl SurfaceRef {
    pub fn new(kind: SurfaceKind) -> Self {
        SurfaceRef {
            kind_tag: kind.as_tag(),
            ..Default::default()
        }
    }
    pub fn with_title(mut self, t: impl Into<String>) -> Self {
        self.title = t.into();
        self
    }
}

/// The current selection within the focused surface (a highlighted range, a clicked line,
/// an address, a symbol). All optional; the agent uses whatever is present.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Selection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.line.is_none()
            && self.address.is_none()
            && self.symbol.is_none()
            && self.function.is_none()
    }
}

/// What the user is looking at right now — the focused surface and any selection. The
/// runtime renders this into the agent's context so on-screen references resolve.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Focus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
}

impl Focus {
    pub fn is_empty(&self) -> bool {
        self.surface.is_none()
            && self
                .selection
                .as_ref()
                .map(|s| s.is_empty())
                .unwrap_or(true)
    }

    /// A compact one-to-few-line description for the agent's context, or `None` when
    /// nothing meaningful is focused. Kept tiny on purpose (this rides in every turn).
    pub fn digest(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = &self.surface {
            let mut d = format!("a {} surface", s.kind_tag);
            if !s.title.is_empty() {
                d.push_str(&format!(" \"{}\"", s.title));
            }
            if let Some(p) = &s.path {
                d.push_str(&format!(" (file {p})"));
            }
            if let Some(l) = &s.locator {
                d.push_str(&format!(" ({l})"));
            }
            if let Some(a) = &s.artifact {
                d.push_str(&format!(" [artifact {}]", a.short()));
            }
            parts.push(d);
        }
        if let Some(sel) = &self.selection {
            if !sel.is_empty() {
                let mut bits = Vec::new();
                if let Some(f) = &sel.function {
                    bits.push(format!("function {f}"));
                }
                if let Some(sym) = &sel.symbol {
                    bits.push(format!("symbol {sym}"));
                }
                if let Some(addr) = &sel.address {
                    bits.push(format!("address {addr}"));
                }
                if let Some(l) = &sel.line {
                    bits.push(format!("line {l}"));
                }
                if let Some(t) = &sel.text {
                    let t = t.replace('\n', " ");
                    let t: String = t.chars().take(120).collect();
                    bits.push(format!("selected \"{t}\""));
                }
                parts.push(format!("selection: {}", bits.join(", ")));
            }
        }
        Some(format!(
            "The user is currently viewing {}.",
            parts.join("; ")
        ))
    }
}

/// A request from the agent to *show the user something* — open an artifact, a file, a
/// diff, a terminal, or to split the workspace. This is the other half of "the agent
/// operates the workspace": the runtime turns a `present` meta-call into one of these and
/// emits it as an event; a UI opens the matching surface beside the conversation. Naming a
/// view and a locator, never a tool or domain, it degrades cleanly on a UI-less front-end
/// (a CLI simply ignores it).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PresentDirective {
    /// Which view to open: a data label the front-end maps to a surface kind — e.g.
    /// `artifact`, `file`, `diff`, `structured`, `browser`, `terminal`, `evidence`,
    /// `split`, or any domain-defined view.
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactId>,
    /// A workspace-relative file path (for `file`/`diff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A free-form locator: a URL (`browser`), an invocation id (`evidence`), a command
    /// (`terminal`), or a domain-specific address/symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    /// Which pane to open in (`left`/`right`); the front-end may choose if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// A one-line reason shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl PresentDirective {
    /// A short human line for a live view, e.g. `show file chapter3.md — the reveal scene`.
    pub fn headline(&self) -> String {
        let mut s = format!("show {}", self.view);
        if let Some(p) = &self.path {
            s.push_str(&format!(" {p}"));
        }
        if let Some(l) = &self.locator {
            s.push_str(&format!(" {l}"));
        }
        if let Some(a) = &self.artifact {
            s.push_str(&format!(" [{}]", a.short()));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!(" — {n}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_focus_has_no_digest() {
        assert!(Focus::default().digest().is_none());
    }

    #[test]
    fn digest_describes_surface_and_selection() {
        let focus = Focus {
            surface: Some(SurfaceRef {
                kind_tag: SurfaceKind::Editor.as_tag(),
                title: "chapter3.md".into(),
                artifact: Some(ArtifactId("abcdef123456".into())),
                path: Some("chapter3.md".into()),
                locator: None,
            }),
            selection: Some(Selection {
                line: Some(12),
                ..Default::default()
            }),
        };
        let d = focus.digest().unwrap();
        assert!(d.contains("editor surface"));
        assert!(d.contains("file chapter3.md"));
        assert!(d.contains("artifact abcdef12"));
        assert!(d.contains("line 12"));
    }

    #[test]
    fn present_directive_headline_and_roundtrip() {
        let d = PresentDirective {
            view: "file".into(),
            path: Some("chapter3.md".into()),
            note: Some("the reveal scene".into()),
            ..Default::default()
        };
        let h = d.headline();
        assert!(h.contains("show file"), "{h}");
        assert!(h.contains("chapter3.md") && h.contains("the reveal scene"));
        let back: PresentDirective =
            serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn domain_surface_kind_via_other() {
        let s = SurfaceKind::Other("decompiler".into());
        assert_eq!(s.as_tag(), "decompiler");
    }
}
