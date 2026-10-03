//! Progressive tool disclosure. A naive loop sends every action's full JSON Schema on every
//! turn; a dozen adapters' worth of schemas dominate the prompt. Instead the runtime hands
//! the model a tiny **capability index** — one line per adapter/capability — and a small set
//! of always-available **meta-tools** to discover and load the exact action schema it needs,
//! only when it needs it:
//!
//! - `list_actions(adapter)` → the actions in one adapter, with summaries and risk.
//! - `load_action(name)`     → the full input schema for one action (then callable).
//! - `get_evidence(inv, …)`  → exact rows from a past invocation's full output.
//! - `search_evidence(query)`→ full-text over the project's indexed evidence.
//! - `record_state(…)`       → edit the persistent working state.
//! - `record_note(…)`        → preserve a notebook entry.
//! - `present(view, …)`      → ask a UI to show the user something.
//!
//! Optional meta-tools appear only when their backing capability is configured:
//! `search_knowledge` (a [`crate::knowledge::KnowledgeProvider`]) and the capability
//! acquisition tools (`find_capability`/`propose_capability`/`acquire_capability`).

use std::collections::BTreeMap;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::ToolDef;
use ark_tools::Registry;

pub const LIST_ACTIONS: &str = "list_actions";
pub const LOAD_ACTION: &str = "load_action";
pub const GET_EVIDENCE: &str = "get_evidence";
pub const SEARCH_EVIDENCE: &str = "search_evidence";
pub const SEARCH_KNOWLEDGE: &str = "search_knowledge";
pub const RECORD_STATE: &str = "record_state";
pub const RECORD_NOTE: &str = "record_note";
pub const PRESENT: &str = "present";
pub const FIND_CAPABILITY: &str = "find_capability";
pub const PROPOSE_CAPABILITY: &str = "propose_capability";
pub const ACQUIRE_CAPABILITY: &str = "acquire_capability";

/// The set of meta-tool names — everything the runtime handles itself rather than
/// dispatching to an adapter (so they never touch the approval gate).
pub const META_TOOLS: &[&str] = &[
    LIST_ACTIONS,
    LOAD_ACTION,
    GET_EVIDENCE,
    SEARCH_EVIDENCE,
    SEARCH_KNOWLEDGE,
    RECORD_STATE,
    RECORD_NOTE,
    PRESENT,
    FIND_CAPABILITY,
    PROPOSE_CAPABILITY,
    ACQUIRE_CAPABILITY,
];

pub fn is_meta_tool(name: &str) -> bool {
    META_TOOLS.contains(&name)
}

/// Meta-tools that bring stored evidence or external knowledge back into the context.
pub fn is_retrieval(name: &str) -> bool {
    matches!(name, GET_EVIDENCE | SEARCH_EVIDENCE | SEARCH_KNOWLEDGE)
}

/// Which optional meta-tools to expose, so a bare agent doesn't advertise tools it can't
/// fulfil.
#[derive(Clone, Copy, Debug)]
pub struct MetaToolConfig {
    /// Expose `search_knowledge` (a [`crate::knowledge::KnowledgeProvider`] is configured).
    pub knowledge: bool,
    /// Expose `find/propose/acquire_capability` (a capabilities spec file is configured).
    pub acquisition: bool,
    /// Expose `present` (a UI can act on directives; a CLI may still accept and ignore it).
    pub present: bool,
}

impl Default for MetaToolConfig {
    fn default() -> Self {
        MetaToolConfig {
            knowledge: false,
            acquisition: false,
            present: true,
        }
    }
}

fn risk_word(r: RiskTier) -> &'static str {
    match r {
        RiskTier::Read => "read-only",
        RiskTier::Observe => "observe",
        RiskTier::Mutate => "changes-state",
    }
}

/// A flat capability index: one compact line per adapter, listing its verbs. Used when no
/// [`ark_core::CapabilityCatalog`] is declared (see [`crate::capmap::capability_index`] for
/// the capability-first version).
pub fn capability_index(registry: &Registry) -> String {
    let mut by_adapter: BTreeMap<String, Vec<(String, RiskTier)>> = BTreeMap::new();
    for d in registry.actions() {
        let (ns, verb) = split_action(&d.name);
        by_adapter.entry(ns).or_default().push((verb, d.risk));
    }
    let mut s = String::from(
        "Tools (use load_action(name) to get an action's schema before calling it):\n",
    );
    for (ns, mut verbs) in by_adapter {
        verbs.sort_by(|a, b| a.0.cmp(&b.0));
        let highest = verbs
            .iter()
            .map(|(_, r)| *r)
            .max_by_key(|r| *r as u8)
            .unwrap_or(RiskTier::Read);
        let names: Vec<String> = verbs.iter().map(|(v, _)| format!("{ns}.{v}")).collect();
        s.push_str(&format!(
            "- {ns} [{}]: {}\n",
            risk_word(highest),
            names.join(", ")
        ));
    }
    s
}

fn split_action(name: &str) -> (String, String) {
    match name.split_once('.') {
        Some((ns, verb)) => (ns.to_string(), verb.to_string()),
        None => (name.to_string(), String::new()),
    }
}

/// The actions offered by one adapter, for `list_actions`.
pub fn list_actions(registry: &Registry, adapter: &str) -> Vec<ActionDescriptor> {
    registry
        .actions()
        .into_iter()
        .filter(|d| split_action(&d.name).0 == adapter)
        .collect()
}

/// Up to `n` actions whose name or summary shares words with `task`, best first. A word of
/// the action's name scores 2, as does a summary word of six letters or more (specific
/// enough: "temperature"); a shorter summary word scores 1. Words match when one is a prefix
/// of the other and the shorter has at least four letters ("orders" and "order"). An action
/// needs a score of 2, and at least the best score minus 1 unless it belongs to the same
/// adapter as an action that has: one coincidental word ("current") does not bring in another
/// adapter's action next to a strong match, which misleads a small model, while an adapter's
/// actions that work together (find, then list) stay together. No model call.
pub fn matching_actions(registry: &Registry, task: &str, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .map(str::to_lowercase)
            .filter(|w| !STOP.contains(&w.as_str()))
            .collect()
    };
    let same = |a: &str, b: &str| {
        let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
        short == long || (short.len() >= 4 && long.starts_with(short))
    };
    let mut asked = words(task);
    asked.sort();
    asked.dedup();
    let mut scored: Vec<(u32, String)> = registry
        .actions()
        .into_iter()
        .filter_map(|d| {
            let name = words(&d.name);
            let summary = words(&d.summary);
            let mut score = 0;
            for q in &asked {
                if name.iter().any(|w| same(q, w)) {
                    score += 2;
                } else if let Some(w) = summary.iter().find(|w| same(q, w)) {
                    score += if w.len() >= 6 && q.len() >= 6 { 2 } else { 1 };
                }
            }
            (score >= 2).then_some((score, d.name))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let floor = scored
        .first()
        .map(|(s, _)| s.saturating_sub(1))
        .unwrap_or(0);
    let adapter = |name: &str| name.split('.').next().unwrap_or("").to_string();
    let strong: Vec<String> = scored
        .iter()
        .filter(|(s, _)| *s >= floor)
        .map(|(_, name)| adapter(name))
        .collect();
    scored
        .into_iter()
        .filter(|(s, name)| *s >= floor || strong.contains(&adapter(name)))
        .take(n)
        .map(|(_, name)| name)
        .collect()
}

/// Words that say nothing about which tool a task needs.
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "what", "which", "then", "tell", "all",
    "her", "his", "its", "their", "them", "are", "was", "were", "has", "have", "can", "you",
    "your", "please", "into", "about", "now", "after", "before", "there", "here", "how", "why",
    "when", "who", "get", "any", "one", "two",
];

/// Render one action descriptor as a provider tool definition (its full schema).
pub fn action_tooldef(d: &ActionDescriptor) -> ToolDef {
    ToolDef {
        name: d.name.clone(),
        description: format!("[{}] {}", risk_word(d.risk), d.summary),
        parameters: d.input_schema.clone(),
    }
}

/// The meta-tools offered to the model, filtered by [`MetaToolConfig`]. These are handled
/// inside the runtime loop, never dispatched to an adapter, so they never touch the approval
/// gate.
///
/// The default set is small on purpose: every definition is sent with every request. Live
/// runs with a small model (bench/m4) showed it choosing a vaguely described runtime tool over
/// the application's own (`record_note` for "save a note", `search_evidence` for anything).
/// `list_actions` and `record_note` are not offered (the index lists every action, and
/// `record_state` takes notes), but the runtime answers them if a model calls them.
pub fn meta_tool_defs(cfg: MetaToolConfig) -> Vec<ToolDef> {
    let mut tools = vec![
        ToolDef {
            name: LOAD_ACTION.into(),
            description: "Load an action from the capability index so you can call it. Returns its summary; its input schema is then available.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["name"],
                "properties": { "name": { "type": "string", "description": "action name from the index, e.g. fs.read" } }
            }),
        },
        ToolDef {
            name: GET_EVIDENCE.into(),
            description: "Read the full output of a tool you already ran, by the id in its [inv:...] reference. Use match to find lines in a large output.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["invocation"],
                "properties": {
                    "invocation": { "type": "string", "description": "id from [inv:1a2b3c]" },
                    "path": { "type": "string", "description": "dotted key path into the output" },
                    "match": { "type": "string", "description": "keep only items or lines containing this text" },
                    "offset": { "type": "integer", "description": "skip this many items" },
                    "limit": { "type": "integer", "description": "max items (default 40)" }
                }
            }),
        },
        ToolDef {
            name: SEARCH_EVIDENCE.into(),
            description: "Search the text of files and documents stored in this project. It does not search tool results (use get_evidence) and does not run tools.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["query"],
                "properties": { "query": { "type": "string" }, "limit": { "type": "integer", "description": "max hits (default 10)" } }
            }),
        },
        ToolDef {
            name: RECORD_STATE.into(),
            description: "Update the working state that persists across turns: a finding (cite evidence), a hypothesis, a rejected hypothesis, an open question, the next action, or a note. Optional graph edits: experiment, link.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["kind", "text"],
                "properties": {
                    "kind": { "type": "string", "enum": ["finding", "hypothesis", "reject", "interesting", "question", "next_action", "note", "experiment", "link"] },
                    "text": { "type": "string" },
                    "claim_type": { "type": "string", "enum": ["fact", "inference", "hypothesis", "unknown"] },
                    "cites": { "type": "array", "items": { "type": "string" }, "description": "e.g. inv:1a2b3c" },
                    "tests": { "type": "string", "description": "experiment: what it tests" },
                    "from": { "type": "string", "description": "link: source" },
                    "to": { "type": "string", "description": "link: target" },
                    "rel": { "type": "string", "enum": ["supports", "contradicts", "derived-from", "tested-by", "unresolved"] }
                }
            }),
        },
    ];

    if cfg.present {
        tools.push(ToolDef {
            name: PRESENT.into(),
            description: "Show the user something in the workspace beside the conversation: an artifact, a file, a diff, a terminal, or a domain view.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["view"],
                "properties": {
                    "view": { "type": "string", "description": "artifact, file, diff, structured, browser, terminal, evidence, split, or a domain view" },
                    "artifact": { "type": "string" }, "path": { "type": "string" },
                    "locator": { "type": "string" }, "pane": { "type": "string" }, "note": { "type": "string" }
                }
            }),
        });
    }

    if cfg.knowledge {
        tools.push(ToolDef {
            name: SEARCH_KNOWLEDGE.into(),
            description: "Search external, public knowledge (docs, references) — separate from this project's \
                evidence. Use it to recall how something generally works. Results are cited [kb:source]; this is \
                background, NOT an observation about the target — confirm on the target before asserting it.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["query"],
                "properties": { "query": { "type": "string" }, "limit": { "type": "integer", "description": "max snippets (default 4)" } }
            }),
        });
    }

    if cfg.acquisition {
        tools.push(ToolDef {
            name: FIND_CAPABILITY.into(),
            description: "Reason about capabilities, not tool names. Given the kind of information you need (a \
                capability slug from the index, or a free-text description), returns which loaded actions serve \
                it, or reports it is not available so you can propose acquiring it.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["need"],
                "properties": { "need": { "type": "string", "description": "a capability slug or a short description of the information you need" } }
            }),
        });
        tools.push(ToolDef {
            name: PROPOSE_CAPABILITY.into(),
            description: "When a capability is NOT AVAILABLE, discover whether a known provider could supply it. \
                Returns candidate providers with provenance, dependencies, and the exact check/install commands — \
                an auditable proposal. This only reads; it installs nothing.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["need"],
                "properties": { "need": { "type": "string", "description": "a capability slug or a description of what you need" } }
            }),
        });
        tools.push(ToolDef {
            name: ACQUIRE_CAPABILITY.into(),
            description: "Acquire a provider proposed by propose_capability, by its id. This CHANGES the \
                environment and therefore needs human approval: without approval it returns the exact plan and \
                does nothing.".into(),
            parameters: serde_json::json!({
                "type": "object", "required": ["id"],
                "properties": { "id": { "type": "string" } }
            }),
        });
    }

    tools
}

#[cfg(test)]
mod tests {
    #[test]
    fn matching_actions_follow_the_tasks_words() {
        use ark_core::ActionDescriptor;
        struct T;
        #[async_trait::async_trait]
        impl ark_tools::Adapter for T {
            fn name(&self) -> &str {
                "t"
            }
            fn actions(&self) -> Vec<ActionDescriptor> {
                vec![
                    ActionDescriptor::new(
                        "crm.find_customer",
                        "look a customer up by name",
                        RiskTier::Read,
                    ),
                    ActionDescriptor::new("crm.orders", "list a customer's orders", RiskTier::Read),
                    ActionDescriptor::new(
                        "weather.current",
                        "current temperature for a city",
                        RiskTier::Read,
                    ),
                    ActionDescriptor::new("units.convert", "convert between units", RiskTier::Read),
                ]
            }
            async fn run(
                &self,
                _: &str,
                _: serde_json::Value,
                _: &ark_tools::RunCtx<'_>,
            ) -> ark_tools::Result<ark_tools::ActionOutput> {
                unreachable!()
            }
        }
        let mut r = Registry::new();
        r.register(std::sync::Arc::new(T));
        assert_eq!(
            matching_actions(&r, "Find the customer Ada and total her orders", 3),
            vec!["crm.find_customer", "crm.orders"]
        );
        assert_eq!(
            matching_actions(&r, "What temperature is it in Lima?", 3),
            vec!["weather.current"]
        );
        assert!(matching_actions(&r, "Say hello in French.", 3).is_empty());
        // A weak match from another adapter does not ride along with a strong one...
        assert_eq!(
            matching_actions(
                &r,
                "Convert dollars with the converter at the current rate",
                3
            ),
            vec!["units.convert"]
        );
        // ...but a weaker sibling from the same adapter does (find, then list).
        assert_eq!(
            matching_actions(&r, "Total of the orders of the customer Ada", 3),
            vec!["crm.orders", "crm.find_customer"]
        );
        assert!(matching_actions(&r, "Find the customer", 0).is_empty());
    }

    use super::*;

    #[test]
    fn optional_meta_tools_are_gated() {
        let base = meta_tool_defs(MetaToolConfig {
            knowledge: false,
            acquisition: false,
            present: false,
        });
        assert!(base.iter().all(|t| t.name != SEARCH_KNOWLEDGE));
        assert!(base.iter().all(|t| t.name != PRESENT));
        assert!(base.iter().all(|t| t.name != FIND_CAPABILITY));

        let full = meta_tool_defs(MetaToolConfig {
            knowledge: true,
            acquisition: true,
            present: true,
        });
        assert!(full.iter().any(|t| t.name == SEARCH_KNOWLEDGE));
        assert!(full.iter().any(|t| t.name == PRESENT));
        assert!(full.iter().any(|t| t.name == ACQUIRE_CAPABILITY));
        // The core four are always offered; list_actions and record_note no longer are.
        for name in [LOAD_ACTION, GET_EVIDENCE, SEARCH_EVIDENCE, RECORD_STATE] {
            assert!(full.iter().any(|t| t.name == name), "{name} missing");
        }
        for name in [LIST_ACTIONS, RECORD_NOTE] {
            assert!(base.iter().all(|t| t.name != name), "{name} offered");
            assert!(is_meta_tool(name), "{name} is still handled");
        }
    }
}
