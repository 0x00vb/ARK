//! Tool → capability mapping and capability-first discovery. This is a layer where
//! concrete tool names are allowed to appear: it maps each adapter's verbs onto the neutral
//! [`Capability`] taxonomy the consumer declared in its [`CapabilityCatalog`], so the
//! runtime can reason "I need this capability" and be told *which loaded actions serve it* —
//! or that none do and it must be acquired (see [`crate::acquire`]).
//!
//! A descriptor may declare its capabilities explicitly (authoritative). When it doesn't, a
//! consumer-supplied [`Inferrer`] maps the action name to capabilities. ARK's default
//! inferrer is "declared only" — genuinely generic, no domain assumptions. A domain that
//! prefers name-based inference (so it needn't annotate every action) supplies its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use ark_core::{ActionDescriptor, Capability, CapabilityCatalog};
use ark_tools::Registry;

/// Infer the capabilities an action serves from its descriptor, when it does not declare
/// them. The default is "none" (declared is authoritative). A domain supplies its own to
/// map action names onto capabilities without annotating each descriptor.
pub type Inferrer = Arc<dyn Fn(&ActionDescriptor) -> Vec<Capability> + Send + Sync>;

/// The default inferrer: infer nothing, so only explicitly-declared capabilities count.
pub fn declared_only() -> Inferrer {
    Arc::new(|_desc| Vec::new())
}

/// The capabilities an action serves: its declared set if any, else the inferrer's result.
pub fn capabilities_of(desc: &ActionDescriptor, inferrer: &Inferrer) -> Vec<Capability> {
    if !desc.capabilities.is_empty() {
        return desc.capabilities.clone();
    }
    inferrer(desc)
}

/// One capability's coverage in the current environment.
pub struct Coverage {
    pub capability: Capability,
    /// Action names that serve it and whose backing tool is available.
    pub available: Vec<String>,
    /// Action names that serve it but whose backing tool is missing (health failed).
    pub unavailable: Vec<String>,
}

impl Coverage {
    pub fn is_available(&self) -> bool {
        !self.available.is_empty()
    }
}

/// Compute coverage for every capability in the catalog, consulting adapter health so a
/// registered but non-functional tool is reported as unavailable.
pub async fn coverage(
    registry: &Registry,
    catalog: &CapabilityCatalog,
    inferrer: &Inferrer,
) -> Vec<Coverage> {
    // Which adapter namespaces are actually healthy right now.
    let mut healthy: BTreeMap<String, bool> = BTreeMap::new();
    for a in registry.adapters() {
        healthy.insert(a.name().to_string(), a.health().await.available);
    }

    let mut by_cap: BTreeMap<Capability, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for desc in registry.actions() {
        let ns = desc.adapter_name().to_string();
        let ok = *healthy.get(&ns).unwrap_or(&true);
        for cap in capabilities_of(&desc, inferrer) {
            let entry = by_cap.entry(cap).or_default();
            if ok {
                entry.0.push(desc.name.clone());
            } else {
                entry.1.push(desc.name.clone());
            }
        }
    }

    catalog
        .all()
        .into_iter()
        .map(|capability| {
            let (mut available, mut unavailable) = by_cap.remove(&capability).unwrap_or_default();
            available.sort();
            available.dedup();
            unavailable.sort();
            unavailable.dedup();
            Coverage { capability, available, unavailable }
        })
        .collect()
}

/// The capability-first index shown to the model: every category, whether it is covered,
/// and by which actions. This replaces "list every adapter's verbs" with "reason about what
/// kind of knowledge you need". Compact by construction — one line per category.
pub async fn capability_index(
    registry: &Registry,
    catalog: &CapabilityCatalog,
    inferrer: &Inferrer,
) -> String {
    if catalog.is_empty() {
        // No capability taxonomy declared: fall back to a flat adapter listing.
        return crate::catalog::capability_index(registry);
    }
    let mut s = String::from(
        "Capabilities (reason about what information the task needs, then load the actions that serve it):\n",
    );
    for cov in coverage(registry, catalog, inferrer).await {
        let cap = &cov.capability;
        let describes = catalog.describe(cap).unwrap_or("");
        if cov.is_available() {
            s.push_str(&format!("- {} — {}: {}\n", cap.slug(), describes, cov.available.join(", ")));
        } else if !cov.unavailable.is_empty() {
            s.push_str(&format!(
                "- {} — {}: present but unavailable ({}); acquire or use another approach\n",
                cap.slug(), describes, cov.unavailable.join(", ")
            ));
        } else {
            s.push_str(&format!(
                "- {} — {}: NOT AVAILABLE — call propose_capability(\"{}\") to see if it can be acquired\n",
                cap.slug(), describes, cap.slug()
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::{CapabilitySpec, RiskTier};

    #[test]
    fn declared_capabilities_win_over_inference() {
        let inferrer = declared_only();
        let d = ActionDescriptor::new("mystery.op", "x", RiskTier::Read)
            .with_capabilities([Capability::new("drafting")]);
        assert_eq!(capabilities_of(&d, &inferrer), vec![Capability::new("drafting")]);
    }

    #[test]
    fn default_inferrer_infers_nothing() {
        let inferrer = declared_only();
        let d = ActionDescriptor::new("mystery.op", "x", RiskTier::Read);
        assert!(capabilities_of(&d, &inferrer).is_empty());
    }

    #[test]
    fn custom_inferrer_maps_names() {
        let inferrer: Inferrer = Arc::new(|d: &ActionDescriptor| {
            match d.adapter_name() {
                "draft" => vec![Capability::new("drafting")],
                _ => vec![],
            }
        });
        let d = ActionDescriptor::new("draft.expand", "x", RiskTier::Read);
        assert_eq!(capabilities_of(&d, &inferrer), vec![Capability::new("drafting")]);
    }

    #[tokio::test]
    async fn coverage_iterates_the_catalog() {
        let catalog = CapabilityCatalog::new([
            CapabilitySpec::new("drafting", "write prose"),
            CapabilitySpec::new("research", "gather facts"),
        ]);
        let reg = Registry::new();
        let cov = coverage(&reg, &catalog, &declared_only()).await;
        assert_eq!(cov.len(), 2, "one coverage row per declared capability");
        assert!(cov.iter().all(|c| !c.is_available()), "empty registry covers nothing");
    }
}
