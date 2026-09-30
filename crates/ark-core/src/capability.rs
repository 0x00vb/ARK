//! The **capability taxonomy**: the domain- and tool-neutral vocabulary the agent
//! reasons in *before* it reaches for a tool. A task decomposes into information needs
//! ("read the structure", "recover the relevant names", "run an experiment"), and each
//! of those is a [`Capability`], not a tool name.
//!
//! This is the counterpart to [`crate::RiskTier`]: risk says *how dangerous* an action
//! is, capability says *what kind of knowledge it produces*. Adapters map their concrete
//! verbs onto these categories; the runtime discovers, and can acquire, capabilities
//! rather than hard-coding tools.
//!
//! ## The generalization from the source harness
//!
//! The original workstation baked a fixed enum of nine security capabilities into the
//! core (decompilation, dynamic-tracing, debugging, …). That is exactly the kind of
//! domain leakage ARK removes. Here a [`Capability`] is an open slug, and a
//! [`CapabilityCatalog`] — supplied by the consuming application — gives each slug a
//! one-line description and any natural-language synonyms. A security product registers
//! `decompilation`; a writing agent registers `drafting` and `continuity-check`; a legal
//! agent registers `citation-lookup`. The runtime machinery (coverage, `find_capability`,
//! acquisition) is identical for all of them.

use serde::{Deserialize, Serialize};

/// A category of investigative power, identified by a stable slug (`static-analysis`,
/// `drafting`, `citation-lookup`, …). The agent picks the capabilities a task needs;
/// the environment reports which are *available* (some adapter serves them) and which
/// are *acquirable* (a known provider could be installed/connected).
///
/// A capability names a *kind of work*, never an implementation — that is what keeps it
/// domain-portable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(slug: impl Into<String>) -> Self {
        Capability(normalize(&slug.into()))
    }

    /// The stable slug used in tool arguments and citations.
    pub fn slug(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Capability {
    fn from(s: &str) -> Self {
        Capability::new(s)
    }
}

/// The declaration of one capability: its slug, a one-line statement of *what
/// information it gives you* (the text the agent reasons over), and any synonyms the
/// model is likely to type when it describes a need in free text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilitySpec {
    pub slug: String,
    /// A one-line "what this capability gives you" description.
    pub describes: String,
    /// Natural synonyms that should resolve to this capability from free text.
    #[serde(default)]
    pub synonyms: Vec<String>,
}

impl CapabilitySpec {
    pub fn new(slug: impl Into<String>, describes: impl Into<String>) -> Self {
        CapabilitySpec {
            slug: normalize(&slug.into()),
            describes: describes.into(),
            synonyms: Vec::new(),
        }
    }

    pub fn with_synonyms<S: Into<String>>(mut self, syns: impl IntoIterator<Item = S>) -> Self {
        self.synonyms = syns.into_iter().map(|s| normalize(&s.into())).collect();
        self
    }

    pub fn capability(&self) -> Capability {
        Capability(self.slug.clone())
    }
}

/// The set of capabilities a particular application understands. Supplied by the
/// consumer (an ARK `AgentProfile`), it is what makes "reason about capabilities, not
/// tool names" domain-agnostic: the runtime asks the catalog to describe and resolve
/// capabilities, and never hard-codes any.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapabilityCatalog {
    specs: Vec<CapabilitySpec>,
}

impl CapabilityCatalog {
    pub fn new(specs: impl IntoIterator<Item = CapabilitySpec>) -> Self {
        let mut cat = CapabilityCatalog::default();
        for s in specs {
            cat.register(s);
        }
        cat
    }

    /// Add or replace a capability spec (by slug).
    pub fn register(&mut self, spec: CapabilitySpec) {
        let slug = normalize(&spec.slug);
        self.specs.retain(|s| s.slug != slug);
        self.specs.push(CapabilitySpec { slug, ..spec });
    }

    pub fn specs(&self) -> &[CapabilitySpec] {
        &self.specs
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Every capability, in registration order — for enumerating coverage.
    pub fn all(&self) -> Vec<Capability> {
        self.specs.iter().map(|s| s.capability()).collect()
    }

    /// The slugs, for listing to the model.
    pub fn slugs(&self) -> Vec<&str> {
        self.specs.iter().map(|s| s.slug.as_str()).collect()
    }

    /// The one-line description of a capability, if it is registered.
    pub fn describe(&self, cap: &Capability) -> Option<&str> {
        self.specs
            .iter()
            .find(|s| s.slug == cap.0)
            .map(|s| s.describes.as_str())
    }

    /// Is this capability one the catalog knows about?
    pub fn contains(&self, cap: &Capability) -> bool {
        self.specs.iter().any(|s| s.slug == cap.0)
    }

    /// Resolve a slug or a natural synonym (lenient: accepts `_`/`-` and registered
    /// synonyms), so the agent's free-text `need` argument resolves to a category.
    pub fn resolve_lenient(&self, s: &str) -> Option<Capability> {
        let k = normalize(s);
        if let Some(spec) = self.specs.iter().find(|spec| spec.slug == k) {
            return Some(spec.capability());
        }
        self.specs
            .iter()
            .find(|spec| spec.synonyms.iter().any(|syn| *syn == k))
            .map(|spec| spec.capability())
    }
}

/// Normalize a slug: trim, lower-case, and use `-` as the separator.
fn normalize(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writing_catalog() -> CapabilityCatalog {
        CapabilityCatalog::new([
            CapabilitySpec::new("drafting", "write new prose from an outline or brief")
                .with_synonyms(["write", "compose", "draft"]),
            CapabilitySpec::new(
                "continuity-check",
                "verify facts and timeline consistency across a manuscript",
            )
            .with_synonyms(["consistency", "continuity"]),
        ])
    }

    #[test]
    fn slugs_normalize_and_resolve() {
        let cat = writing_catalog();
        assert_eq!(
            cat.resolve_lenient("Drafting"),
            Some(Capability::new("drafting"))
        );
        assert_eq!(
            cat.resolve_lenient("continuity_check"),
            Some(Capability::new("continuity-check"))
        );
        assert_eq!(cat.resolve_lenient("nonsense"), None);
    }

    #[test]
    fn synonyms_resolve() {
        let cat = writing_catalog();
        assert_eq!(
            cat.resolve_lenient("compose"),
            Some(Capability::new("drafting"))
        );
        assert_eq!(
            cat.resolve_lenient("consistency"),
            Some(Capability::new("continuity-check"))
        );
    }

    #[test]
    fn describe_and_enumerate() {
        let cat = writing_catalog();
        assert_eq!(cat.all().len(), 2);
        assert!(cat
            .describe(&Capability::new("drafting"))
            .unwrap()
            .contains("prose"));
        assert!(cat.contains(&Capability::new("drafting")));
        assert!(!cat.contains(&Capability::new("decompilation")));
    }

    #[test]
    fn register_replaces_by_slug() {
        let mut cat = writing_catalog();
        cat.register(CapabilitySpec::new("drafting", "updated description"));
        assert_eq!(cat.all().len(), 2, "same slug replaces, not appends");
        assert_eq!(
            cat.describe(&Capability::new("drafting")).unwrap(),
            "updated description"
        );
    }
}
