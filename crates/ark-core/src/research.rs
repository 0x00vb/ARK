//! Persistent working state and the compact evidence layer the runtime reasons over.
//! Two ideas live here:
//!
//! - [`EvidenceCard`] — the *LLM-facing* view of an invocation: a short semantic summary
//!   plus the identifiers it surfaced and a reference back to the full raw output. The
//!   runtime shows cards, never raw tool dumps, and retrieves exact rows on demand. This
//!   is what keeps the active context small.
//! - [`ResearchState`] — a compact, structured record of a task (goal, hypotheses,
//!   findings, unresolved questions, next actions). It is *edited* as evidence arrives,
//!   not reconstructed by replaying chat history, so a resumed session continues without
//!   re-sending tens of thousands of tokens.
//!
//! Both are generic: no tool name and no domain name appears here. An `action` field
//! carries a data label. This "research" model fits any investigative or authoring task —
//! a security investigation, a legal memo, a novel's continuity, a data analysis.

use crate::ids::{ArtifactId, InvocationId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How sure a claim is — the distinction that makes an answer honest. A `Fact` is observed
/// in a cited tool result; an `Inference` is derived from facts; a `Hypothesis` is proposed
/// and unconfirmed; `Unknown` marks a gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    Fact,
    Inference,
    Hypothesis,
    Unknown,
}

impl ClaimType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaimType::Fact => "fact",
            ClaimType::Inference => "inference",
            ClaimType::Hypothesis => "hypothesis",
            ClaimType::Unknown => "unknown",
        }
    }
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "fact" => ClaimType::Fact,
            "inference" => ClaimType::Inference,
            "hypothesis" => ClaimType::Hypothesis,
            _ => ClaimType::Unknown,
        }
    }
}

/// The compact, model-facing distillation of one invocation. The full structured output
/// stays on the [`crate::Invocation`] (and large bodies as blob artifacts); this is the
/// summary the runtime keeps resident and cites.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvidenceCard {
    /// The invocation this distills — the anchor for retrieval and citation.
    pub invocation: InvocationId,
    /// The action that produced it (data label, e.g. "search.query").
    pub action: String,
    /// Whether the invocation succeeded.
    pub ok: bool,
    /// A one-to-three-line semantic summary a model can act on.
    pub summary: String,
    /// The salient identifiers this evidence surfaced (names, ids, keys, counts) — what a
    /// later step might search or pivot on.
    #[serde(default)]
    pub identifiers: Vec<String>,
    /// Artifacts this invocation produced (retrievable in full on demand).
    #[serde(default)]
    pub produced: Vec<ArtifactId>,
    /// Rough confidence the summary faithfully represents the output (0..=1).
    pub confidence: f32,
    /// A stable digest of (action + input), used to detect duplicate tool calls.
    pub call_digest: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl EvidenceCard {
    pub fn new(
        invocation: InvocationId,
        action: impl Into<String>,
        ok: bool,
        summary: impl Into<String>,
    ) -> Self {
        EvidenceCard {
            invocation,
            action: action.into(),
            ok,
            summary: summary.into(),
            identifiers: Vec::new(),
            produced: Vec::new(),
            confidence: 1.0,
            call_digest: String::new(),
            created_at: OffsetDateTime::now_utc(),
        }
    }
    pub fn with_identifiers(mut self, ids: Vec<String>) -> Self {
        self.identifiers = ids;
        self
    }
    pub fn with_produced(mut self, p: Vec<ArtifactId>) -> Self {
        self.produced = p;
        self
    }
    pub fn with_confidence(mut self, c: f32) -> Self {
        self.confidence = c;
        self
    }
    pub fn with_call_digest(mut self, d: impl Into<String>) -> Self {
        self.call_digest = d.into();
        self
    }
    /// A single greppable reference line: `[inv:1a2b3c] search.query — 412 hits…`.
    pub fn reference_line(&self) -> String {
        let head: String = self
            .summary
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(160)
            .collect();
        format!(
            "[inv:{}] {} — {}",
            self.invocation.short(),
            self.action,
            head
        )
    }
}

/// A claim the work has produced, with its evidence and honesty level.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Claim {
    pub text: String,
    pub claim_type: ClaimType,
    /// Evidence references supporting it — invocation ids (as short or full strings).
    #[serde(default)]
    pub cites: Vec<String>,
}

impl Claim {
    pub fn new(text: impl Into<String>, claim_type: ClaimType) -> Self {
        Claim {
            text: text.into(),
            claim_type,
            cites: Vec::new(),
        }
    }
    /// Builder helper used in tests and by the runtime.
    pub fn cited(mut self, cites: Vec<String>) -> Self {
        self.cites = cites;
        self
    }
}

/// A node kind in the investigation graph — the shape work actually takes: an objective
/// spawns hypotheses and questions, experiments test them, evidence comes back, and
/// findings settle out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Objective,
    Hypothesis,
    Question,
    Experiment,
    Evidence,
    Finding,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Objective => "objective",
            NodeKind::Hypothesis => "hypothesis",
            NodeKind::Question => "question",
            NodeKind::Experiment => "experiment",
            NodeKind::Evidence => "evidence",
            NodeKind::Finding => "finding",
        }
    }
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "objective" | "goal" => NodeKind::Objective,
            "hypothesis" => NodeKind::Hypothesis,
            "question" => NodeKind::Question,
            "experiment" | "test" => NodeKind::Experiment,
            "evidence" => NodeKind::Evidence,
            "finding" => NodeKind::Finding,
            _ => return None,
        })
    }
}

/// How two nodes relate. These are the edges that let the agent answer "why do we believe
/// this?" and "what is still uncertain?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeRel {
    /// `from` (evidence/experiment) supports `to` (hypothesis/finding).
    Supports,
    /// `from` contradicts `to`.
    Contradicts,
    /// `from` (finding/hypothesis) was derived from `to` (evidence/experiment).
    DerivedFrom,
    /// `from` (experiment) tests `to` (hypothesis/question).
    TestedBy,
    /// `from` (question/hypothesis) is still unresolved (dangling `to` allowed).
    Unresolved,
}

impl EdgeRel {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeRel::Supports => "supports",
            EdgeRel::Contradicts => "contradicts",
            EdgeRel::DerivedFrom => "derived-from",
            EdgeRel::TestedBy => "tested-by",
            EdgeRel::Unresolved => "unresolved",
        }
    }
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        Some(
            match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
                "supports" | "support" => EdgeRel::Supports,
                "contradicts" | "contradict" => EdgeRel::Contradicts,
                "derived-from" | "derives-from" | "derived" => EdgeRel::DerivedFrom,
                "tested-by" | "tests" | "tested" => EdgeRel::TestedBy,
                "unresolved" | "open" => EdgeRel::Unresolved,
                _ => return None,
            },
        )
    }
}

/// A graph node: one objective/hypothesis/question/experiment/evidence/finding.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
}

/// A directed, typed relationship between two nodes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub rel: EdgeRel,
}

/// A compact investigation graph. Deliberately small: it is not a knowledge base, it is
/// the relational spine that tells the agent what is known, why, and what is still
/// uncertain — so it can pick the step that reduces uncertainty next.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct InvestigationGraph {
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    seq: u32,
}

impl InvestigationGraph {
    /// Find a node by (kind, label), case-insensitively.
    fn find(&self, kind: NodeKind, label: &str) -> Option<&GraphNode> {
        let l = label.trim().to_ascii_lowercase();
        self.nodes
            .iter()
            .find(|n| n.kind == kind && n.label.to_ascii_lowercase() == l)
    }

    /// Add a node (deduped by kind+label) and return its id.
    pub fn add_node(&mut self, kind: NodeKind, label: impl Into<String>) -> String {
        let label = label.into();
        if let Some(n) = self.find(kind, &label) {
            return n.id.clone();
        }
        self.seq += 1;
        let id = format!("{}{}", &kind.as_str()[..1], self.seq);
        self.nodes.push(GraphNode {
            id: id.clone(),
            kind,
            label,
        });
        id
    }

    /// Resolve a reference that is either an existing node id or a label; if it is a label
    /// with no match, create a node of `default_kind` for it.
    pub fn resolve_or_add(&mut self, reference: &str, default_kind: NodeKind) -> String {
        let r = reference.trim();
        if self.nodes.iter().any(|n| n.id == r) {
            return r.to_string();
        }
        // Any-kind label match reuses the node; else create under the default kind.
        if let Some(n) = self.nodes.iter().find(|n| n.label.eq_ignore_ascii_case(r)) {
            return n.id.clone();
        }
        self.add_node(default_kind, r)
    }

    /// Link two references with a relation, creating nodes as needed. `from`/`to` may be
    /// ids or labels; unknown labels become nodes of the sensible default kind for that
    /// side of the relation.
    pub fn link(&mut self, from: &str, to: &str, rel: EdgeRel) {
        let (fk, tk) = default_kinds(rel);
        let from = self.resolve_or_add(from, fk);
        let to = if to.trim().is_empty() {
            from.clone()
        } else {
            self.resolve_or_add(to, tk)
        };
        let edge = GraphEdge { from, to, rel };
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn label_of(&self, id: &str) -> String {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// The uncertainty frontier: hypotheses/questions not yet settled by supporting or
    /// contradicting evidence, annotated with the experiment that would test each (if one
    /// is linked). This is the "what should I do next" view.
    pub fn frontier(&self) -> Vec<String> {
        let mut out = Vec::new();
        for n in &self.nodes {
            if !matches!(n.kind, NodeKind::Hypothesis | NodeKind::Question) {
                continue;
            }
            let settled = self
                .edges
                .iter()
                .any(|e| e.to == n.id && matches!(e.rel, EdgeRel::Supports | EdgeRel::Contradicts));
            if settled {
                continue;
            }
            // Is an experiment lined up to test it?
            let test = self
                .edges
                .iter()
                .find(|e| e.to == n.id && e.rel == EdgeRel::TestedBy)
                .map(|e| self.label_of(&e.from).to_string());
            let tag = n.kind.as_str();
            match test {
                Some(x) => out.push(format!(
                    "({tag}) {} — test via: {}",
                    one_line(&n.label, 100),
                    one_line(&x, 80)
                )),
                None => out.push(format!(
                    "({tag}) {} — no experiment yet",
                    one_line(&n.label, 100)
                )),
            }
        }
        out
    }
}

/// The default node kinds for the two ends of a relation, used when a link references a
/// label that doesn't exist yet.
fn default_kinds(rel: EdgeRel) -> (NodeKind, NodeKind) {
    match rel {
        EdgeRel::Supports | EdgeRel::Contradicts => (NodeKind::Evidence, NodeKind::Hypothesis),
        EdgeRel::DerivedFrom => (NodeKind::Finding, NodeKind::Evidence),
        EdgeRel::TestedBy => (NodeKind::Experiment, NodeKind::Hypothesis),
        EdgeRel::Unresolved => (NodeKind::Question, NodeKind::Question),
    }
}

/// Something worth revisiting: an item the agent flagged as interesting, with a note on
/// why.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Interesting {
    /// A data label for the kind (`symbol`, `name`, `artifact`, `passage`, …).
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub why: String,
}

/// The compact, structured state of one task. Small on purpose: it is a working memory,
/// not a transcript. The runtime reads its [`digest`](ResearchState::digest) at the top of
/// each step and edits it as evidence arrives.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ResearchState {
    /// The current objective, in one sentence.
    #[serde(default)]
    pub goal: String,
    /// Open explanations under test.
    #[serde(default)]
    pub hypotheses: Vec<Claim>,
    /// Confirmed conclusions, each citing evidence.
    #[serde(default)]
    pub findings: Vec<Claim>,
    /// Explanations considered and ruled out (so they are not re-explored).
    #[serde(default)]
    pub rejected: Vec<String>,
    /// Items worth revisiting.
    #[serde(default)]
    pub interesting: Vec<Interesting>,
    /// Unresolved questions.
    #[serde(default)]
    pub questions: Vec<String>,
    /// The next steps to take.
    #[serde(default)]
    pub next_actions: Vec<String>,
    /// The relational spine: how hypotheses, experiments, evidence and findings connect,
    /// and what remains uncertain.
    #[serde(default)]
    pub graph: InvestigationGraph,
    #[serde(default)]
    pub updated_at: Option<OffsetDateTime>,
}

impl ResearchState {
    pub fn new(goal: impl Into<String>) -> Self {
        ResearchState {
            goal: goal.into(),
            updated_at: Some(OffsetDateTime::now_utc()),
            ..Default::default()
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Some(OffsetDateTime::now_utc());
    }

    pub fn is_empty(&self) -> bool {
        self.hypotheses.is_empty()
            && self.findings.is_empty()
            && self.rejected.is_empty()
            && self.interesting.is_empty()
            && self.questions.is_empty()
            && self.next_actions.is_empty()
            && self.graph.is_empty()
    }

    /// A compact, budgeted text digest for a prompt. Sections are capped so the digest
    /// stays roughly bounded no matter how long the task runs.
    /// Tidy the working state without losing meaning: drop exact duplicates, drop open
    /// hypotheses that were already rejected, and keep only the most recent `keep_recent`
    /// next actions and questions (older ones are stale plans). Findings, open hypotheses and
    /// rejections are never discarded, only de-duplicated: they are the decisions and the
    /// unresolved work. Returns how many items were removed.
    pub fn compact(&mut self, keep_recent: usize) -> usize {
        fn norm(s: &str) -> String {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        }
        fn dedup_strings(v: &mut Vec<String>) -> usize {
            let before = v.len();
            let mut seen = std::collections::HashSet::new();
            v.retain(|x| seen.insert(norm(x)));
            before - v.len()
        }
        fn dedup_claims(v: &mut Vec<Claim>) -> usize {
            let before = v.len();
            let mut seen = std::collections::HashSet::new();
            v.retain(|c| seen.insert(norm(&c.text)));
            before - v.len()
        }
        fn keep_tail(v: &mut Vec<String>, n: usize) -> usize {
            let drop = v.len().saturating_sub(n);
            v.drain(..drop);
            drop
        }
        let mut removed = 0;
        removed += dedup_claims(&mut self.findings);
        removed += dedup_claims(&mut self.hypotheses);
        removed += dedup_strings(&mut self.rejected);
        let rejected: std::collections::HashSet<String> =
            self.rejected.iter().map(|r| norm(r)).collect();
        let before = self.hypotheses.len();
        self.hypotheses
            .retain(|h| !rejected.contains(&norm(&h.text)));
        removed += before - self.hypotheses.len();
        removed += dedup_strings(&mut self.questions);
        removed += dedup_strings(&mut self.next_actions);
        removed += keep_tail(&mut self.questions, keep_recent);
        removed += keep_tail(&mut self.next_actions, keep_recent);
        let before = self.interesting.len();
        let mut seen = std::collections::HashSet::new();
        self.interesting
            .retain(|i| seen.insert((i.kind.clone(), norm(&i.value))));
        removed += before - self.interesting.len();
        if removed > 0 {
            self.touch();
        }
        removed
    }

    pub fn digest(&self, max_per_section: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        if !self.goal.is_empty() {
            let _ = writeln!(s, "GOAL: {}", self.goal);
        }
        let list = |s: &mut String, title: &str, items: &[String]| {
            if items.is_empty() {
                return;
            }
            let _ = writeln!(s, "{title}:");
            // The newest items: in a long task the oldest are the least likely to matter.
            let skip = items.len().saturating_sub(max_per_section);
            if skip > 0 {
                let _ = writeln!(s, "  … (+{skip} earlier)");
            }
            for it in items.iter().skip(skip) {
                let _ = writeln!(s, "  - {}", one_line(it, 200));
            }
        };
        let claims = |s: &mut String, title: &str, items: &[Claim]| {
            if items.is_empty() {
                return;
            }
            let _ = writeln!(s, "{title}:");
            let skip = items.len().saturating_sub(max_per_section);
            if skip > 0 {
                let _ = writeln!(s, "  … (+{skip} earlier)");
            }
            for c in items.iter().skip(skip) {
                let cites = if c.cites.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", c.cites.join(", "))
                };
                let _ = writeln!(
                    s,
                    "  - ({}) {}{}",
                    c.claim_type.as_str(),
                    one_line(&c.text, 200),
                    cites
                );
            }
        };
        claims(&mut s, "FINDINGS", &self.findings);
        claims(&mut s, "OPEN HYPOTHESES", &self.hypotheses);
        if !self.interesting.is_empty() {
            let _ = writeln!(s, "INTERESTING:");
            let skip = self.interesting.len().saturating_sub(max_per_section);
            for it in self.interesting.iter().skip(skip) {
                let _ = writeln!(
                    s,
                    "  - {}={} {}",
                    it.kind,
                    one_line(&it.value, 80),
                    one_line(&it.why, 100)
                );
            }
        }
        list(&mut s, "OPEN QUESTIONS", &self.questions);
        list(&mut s, "REJECTED", &self.rejected);
        list(&mut s, "NEXT ACTIONS", &self.next_actions);
        // The uncertainty frontier — what is still unsettled and how to test it.
        let frontier = self.graph.frontier();
        if !frontier.is_empty() {
            let _ = writeln!(s, "UNCERTAINTY FRONTIER:");
            for f in frontier.iter().take(max_per_section) {
                let _ = writeln!(s, "  - {}", one_line(f, 200));
            }
            if frontier.len() > max_per_section {
                let _ = writeln!(s, "  … (+{} more)", frontier.len() - max_per_section);
            }
        }
        s
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let t: String = flat.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_type_roundtrips_leniently() {
        assert_eq!(ClaimType::from_str_lenient("FACT"), ClaimType::Fact);
        assert_eq!(ClaimType::from_str_lenient("guess"), ClaimType::Unknown);
        assert_eq!(ClaimType::Hypothesis.as_str(), "hypothesis");
    }

    #[test]
    fn evidence_card_reference_line_is_compact() {
        let card = EvidenceCard::new(
            InvocationId("abcdef123456".into()),
            "search.query",
            true,
            "412 hits",
        )
        .with_identifiers(vec!["term.main".into()]);
        let line = card.reference_line();
        assert!(
            line.starts_with("[inv:abcdef12]"),
            "uses the 8-char short id: {line}"
        );
        assert!(line.contains("search.query"));
        assert!(line.contains("412 hits"));
    }

    #[test]
    fn graph_links_dedupe_and_surface_frontier() {
        let mut g = InvestigationGraph::default();
        let h = g.add_node(
            NodeKind::Hypothesis,
            "the outage is caused by the cache layer",
        );
        // An experiment tests it; nothing confirms it yet → it's on the frontier.
        g.link("replay traffic against the cache", &h, EdgeRel::TestedBy);
        g.link("replay traffic against the cache", &h, EdgeRel::TestedBy); // dup ignored
        assert_eq!(g.edges.len(), 1, "duplicate edge ignored");
        let front = g.frontier();
        assert_eq!(front.len(), 1);
        assert!(front[0].contains("test via: replay traffic"), "{front:?}");

        // Once evidence supports it, it leaves the frontier.
        g.link("inv:abc123", &h, EdgeRel::Supports);
        assert!(
            g.frontier().is_empty(),
            "settled hypotheses drop off the frontier"
        );
    }

    #[test]
    fn digest_includes_frontier() {
        let mut st = ResearchState::new("understand the outage");
        let q = st
            .graph
            .add_node(NodeKind::Question, "which subsystem failed first?");
        st.graph
            .link("experiment: read the timeline", &q, EdgeRel::TestedBy);
        let d = st.digest(6);
        assert!(d.contains("UNCERTAINTY FRONTIER:"), "{d}");
        assert!(d.contains("which subsystem"));
    }

    #[test]
    fn state_digest_is_bounded_and_ordered() {
        let mut st = ResearchState::new("understand the outage");
        for i in 0..10 {
            st.questions.push(format!("q{i}"));
        }
        st.findings.push(
            Claim::new("the cache evicted early", ClaimType::Fact).cited(vec!["inv:1".into()]),
        );
        let d = st.digest(3);
        assert!(d.contains("GOAL: understand"));
        assert!(d.contains("FINDINGS:"));
        assert!(d.contains("(fact) the cache evicted early [inv:1]"));
        assert!(d.contains("(+7 earlier)"), "questions capped at 3 of 10");
        assert!(d.contains("q9") && !d.contains("q0"), "the newest are shown: {d}");
        // findings come before questions in the digest
        assert!(d.find("FINDINGS").unwrap() < d.find("OPEN QUESTIONS").unwrap());
    }

    #[test]
    fn compact_dedupes_and_keeps_decisions() {
        let mut st = ResearchState::new("g");
        st.findings
            .push(Claim::new("Chapter 4 is 3,100 words", ClaimType::Fact));
        st.findings
            .push(Claim::new("chapter 4 is  3,100 words", ClaimType::Fact));
        st.hypotheses.push(Claim::new(
            "the narrator is unreliable",
            ClaimType::Hypothesis,
        ));
        st.hypotheses
            .push(Claim::new("the pacing drags", ClaimType::Hypothesis));
        st.rejected.push("the pacing drags".into());
        for i in 0..10 {
            st.next_actions.push(format!("step {i}"));
        }
        let removed = st.compact(4);
        assert_eq!(st.findings.len(), 1, "duplicate finding merged");
        assert_eq!(st.hypotheses.len(), 1, "rejected hypothesis closed");
        assert_eq!(
            st.next_actions,
            vec!["step 6", "step 7", "step 8", "step 9"]
        );
        assert_eq!(removed, 1 + 1 + 6);
        assert_eq!(st.compact(4), 0, "idempotent");
    }
}
