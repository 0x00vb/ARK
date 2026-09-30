//! Artifacts: any object of study or authorship. Content-addressed, typed,
//! provenance-tracked.

use crate::ids::{ArtifactId, InvocationId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The kind of an artifact — its *format or role*, never a domain or platform name.
///
/// ARK ships only universal kinds; a domain adds its own formats through the
/// `Other(String)` escape hatch (a security product uses `Other("macho")`, a writing
/// agent might use `Other("manuscript")`). Keeping this open is what lets new domains add
/// types without a core change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Plain UTF-8 text (notes, logs, prose, a strings dump).
    Text,
    /// Structured JSON produced by a tool.
    Json,
    /// A source-code file (any language) — an object of study or of authorship.
    Source,
    /// An image (screenshot, diagram, figure).
    Image,
    /// An archive of files (zip, tar, …).
    Archive,
    /// Opaque binary bytes we hold but do not classify further.
    Binary,
    /// Anything else we hold but do not yet classify.
    Blob,
    /// Escape hatch for a named kind a domain adapter understands (e.g. `"macho"`,
    /// `"manuscript"`, `"contract"`).
    Other(String),
}

impl ArtifactKind {
    /// A named, domain-specific kind. Prefer this in adapters over reusing a core variant.
    pub fn named(tag: impl Into<String>) -> Self {
        ArtifactKind::Other(tag.into())
    }

    pub fn as_tag(&self) -> String {
        match self {
            ArtifactKind::Other(s) => s.clone(),
            other => serde_json::to_value(other)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "blob".into()),
        }
    }

    /// Whether this kind is text-shaped (worth full-text indexing).
    pub fn is_textual(&self) -> bool {
        matches!(self, ArtifactKind::Text | ArtifactKind::Json | ArtifactKind::Source)
    }
}

/// Where an artifact came from. Every artifact has a provenance so the record is
/// reproducible and the agent can explain any evidence it cites.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Provenance {
    /// Imported from a local file path by the user.
    Ingested { path: String },
    /// Produced as the output of a recorded invocation.
    Produced { invocation: InvocationId },
    /// Captured from some external source — fetched from a URL, cloned from a repo,
    /// pulled from a connected system. `origin` is a data label for the kind of source
    /// (e.g. "http", "git", "device") and `locator` is where (a URL, a repo, a path).
    Captured { origin: String, locator: String },
}

/// A stored object of study or authorship.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    /// Human label, e.g. the original filename.
    pub name: String,
    pub size: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub provenance: Provenance,
    /// Free-form extracted facts, filled in by adapters. Kept as JSON so the core stays
    /// format-agnostic.
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl Artifact {
    pub fn new(
        id: ArtifactId,
        kind: ArtifactKind,
        name: impl Into<String>,
        size: u64,
        provenance: Provenance,
    ) -> Self {
        Artifact {
            id,
            kind,
            name: name.into(),
            size,
            created_at: OffsetDateTime::now_utc(),
            provenance,
            meta: serde_json::Value::Null,
        }
    }
}
