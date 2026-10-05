//! Typed views of what the agent knows and spends, computed from real runtime structures.
//! Each view is serializable (a GUI inspector or an API reads it) and has a `report()` for
//! front-ends that only render [`Report`]s.

use std::collections::HashMap;

use ark_sdk::prelude::ark_core::{Artifact, ResearchState};
use ark_sdk::prelude::ark_runtime::{Metrics, Post, SessionRecord, Turn};
use ark_sdk::prelude::ContextReport;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::activity::Activity;
use crate::fmt::{ago, clip, count, millis, tokens};
use crate::report::{Report, Table, Tone};

// ---- context -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// The last request a turn actually sent.
    LastRequest,
    /// What the next turn's first request would contain (no turn has run yet).
    Preview,
}

/// One source of tokens in a request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSection {
    pub key: String,
    pub label: String,
    pub tokens: u32,
    /// Why the model receives this.
    pub about: String,
}

/// Something ARK kept out of the request, and how much.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Excluded {
    pub label: String,
    pub detail: String,
    /// Estimated tokens kept out, when measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextView {
    pub source: ContextSource,
    pub step: usize,
    pub total: u32,
    pub budget: u32,
    pub included: Vec<ContextSection>,
    /// Tool schemas sent next to the prompt; the budget does not count them.
    pub tool_schemas: u32,
    pub loaded_actions: usize,
    pub compressed_rounds: usize,
    pub excluded: Vec<Excluded>,
    /// The next turn's first request, when `source` is the last request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_turn: Option<u32>,
    pub report: ContextReport,
}

const SECTIONS: &[(&str, &str, &str)] = &[
    (
        "instructions",
        "Instructions",
        "The agent's persona and method, from its profile.",
    ),
    (
        "project",
        "Project",
        "The project name, the artifact list and the approval rule.",
    ),
    (
        "capabilities",
        "Capabilities",
        "A one-line index of what the tools can do. Schemas load on demand.",
    ),
    (
        "working_state",
        "Working state",
        "Goal, findings, hypotheses and open work, capped per section.",
    ),
    (
        "conversation",
        "Conversation",
        "The last few exchanges, clipped, and one-line gists of older turns.",
    ),
    (
        "workspace",
        "Workspace",
        "What you are looking at and evidence near it.",
    ),
    ("task", "Task", "Your message for this turn."),
    (
        "tool_results",
        "Tool results",
        "This turn's tool calls and their distilled results.",
    ),
    (
        "retrieved",
        "Retrieved context",
        "Stored evidence and knowledge the agent looked up this turn.",
    ),
];

/// Inputs for [`ContextView`] that live outside the report.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeptOut {
    pub transcript_turns: u32,
    pub transcript_chars: usize,
    pub carried_tokens: u32,
    pub gists: usize,
    pub dropped_turns: u32,
    pub compactions: u32,
    pub tool_calls: usize,
    pub raw_output_chars: usize,
    pub distilled_chars: usize,
    pub cards: usize,
    pub artifacts: usize,
    pub state_overflow: usize,
}

impl ContextView {
    pub fn build(
        report: &ContextReport,
        source: ContextSource,
        next_turn: Option<u32>,
        k: &KeptOut,
    ) -> Self {
        let values = [
            report.instructions,
            report.project,
            report.capability_index,
            report.working_state,
            report.conversation,
            report.workspace,
            report.task,
            report.tool_rounds,
            report.retrieved,
        ];
        let included = SECTIONS
            .iter()
            .zip(values)
            .map(|((key, label, about), tokens)| ContextSection {
                key: key.to_string(),
                label: label.to_string(),
                tokens,
                about: about.to_string(),
            })
            .collect();
        let mut excluded = vec![
            Excluded {
                label: "Transcript".into(),
                detail: format!(
                    "{} stored; {} tokens of it carried as conversation",
                    count(k.transcript_turns as u64, "turn"),
                    tokens(k.carried_tokens as u64)
                ),
                tokens: Some(((k.transcript_chars / 4) as u64).saturating_sub(k.carried_tokens as u64)),
            },
            Excluded {
                label: "Raw tool output".into(),
                detail: format!(
                    "{}; the model sees {} distilled evidence cards (~{} tokens) and can fetch exact rows",
                    count(k.tool_calls as u64, "call"),
                    k.cards,
                    tokens((k.distilled_chars / 4) as u64)
                ),
                tokens: Some((k.raw_output_chars / 4) as u64),
            },
        ];
        if k.gists > 0 || k.dropped_turns > 0 {
            excluded.push(Excluded {
                label: "Older turns".into(),
                detail: format!(
                    "{} as one-line gists, {} merged into working state",
                    k.gists, k.dropped_turns
                ),
                tokens: None,
            });
        }
        if report.compressed_rounds > 0 {
            excluded.push(Excluded {
                label: "Earlier steps".into(),
                detail: format!(
                    "{} dropped from this request; their evidence stays retrievable",
                    count(report.compressed_rounds as u64, "round")
                ),
                tokens: None,
            });
        }
        if k.state_overflow > 0 {
            excluded.push(Excluded {
                label: "Older working-state items".into(),
                detail: format!(
                    "{} beyond the per-section cap (stored, not sent)",
                    k.state_overflow
                ),
                tokens: None,
            });
        }
        if k.artifacts > 0 {
            excluded.push(Excluded {
                label: "Artifact contents".into(),
                detail: format!(
                    "{}; the prompt lists names and ids, tools read the bytes",
                    count(k.artifacts as u64, "artifact")
                ),
                tokens: None,
            });
        }
        if k.compactions > 0 {
            excluded.push(Excluded {
                label: "Compactions".into(),
                detail: format!("{} so far this session", k.compactions),
                tokens: None,
            });
        }
        ContextView {
            source,
            step: report.step,
            total: report.total,
            budget: report.budget,
            included,
            tool_schemas: report.tool_schemas,
            loaded_actions: report.loaded_actions,
            compressed_rounds: report.compressed_rounds,
            excluded,
            next_turn,
            report: report.clone(),
        }
    }

    pub fn report(&self) -> Report {
        let label = match self.source {
            ContextSource::LastRequest => format!("last request (step {})", self.step),
            ContextSource::Preview => "next request (preview)".to_string(),
        };
        let mut t = Table::plain(2).right(1);
        for s in &self.included {
            t.push(crate::report::Row {
                cells: vec![s.label.as_str().into(), tokens(s.tokens as u64).into()],
                tone: if s.tokens == 0 {
                    Tone::Muted
                } else {
                    Tone::Normal
                },
                command: None,
            });
        }
        let mut r = Report::titled(format!("Context · {label}"))
            .table(t)
            .meter("of the budget", self.total as u64, self.budget as u64)
            .field_toned(
                "Tool schemas (extra)",
                format!(
                    "{} ({} loaded)",
                    tokens(self.tool_schemas as u64),
                    self.loaded_actions
                ),
                Tone::Muted,
            );
        if let Some(n) = self.next_turn {
            r = r.note(format!(
                "next turn starts at about {} (preview)",
                tokens(n as u64)
            ));
        }
        r = r.heading("Kept out of the request");
        for e in &self.excluded {
            r = r.field(e.label.clone(), e.detail.clone());
        }
        r.data("context", self)
    }
}

// ---- usage ---------------------------------------------------------------------------------

/// Prices in US dollars per million tokens. ARK ships no price table: an app or the
/// `ARK_PRICE` environment variable (`in,out[,cached]`) supplies one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    /// Price of cached input; defaults to the input price.
    pub cached: Option<f64>,
}

impl Pricing {
    pub fn from_env() -> Option<Self> {
        let v = std::env::var("ARK_PRICE").ok()?;
        let n: Vec<f64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        match n.as_slice() {
            [i, o] => Some(Pricing {
                input: *i,
                output: *o,
                cached: None,
            }),
            [i, o, c, ..] => Some(Pricing {
                input: *i,
                output: *o,
                cached: Some(*c),
            }),
            _ => None,
        }
    }
    pub fn cost(&self, prompt: u64, cached: u64, completion: u64) -> f64 {
        let fresh = prompt.saturating_sub(cached) as f64;
        (fresh * self.input
            + cached as f64 * self.cached.unwrap_or(self.input)
            + completion as f64 * self.output)
            / 1_000_000.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageView {
    pub model: String,
    pub turns: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub duplicate_tool_calls: u32,
    pub peak_prompt_tokens: u32,
    pub elapsed_ms: u64,
    /// False when some numbers are estimates (the provider did not report usage).
    pub measured: bool,
    /// The last request's size and the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<(u32, u32)>,
    /// Estimated cost in US dollars, when prices are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl UsageView {
    pub fn report(&self) -> Report {
        let approx = if self.measured || self.turns == 0 {
            ""
        } else {
            "~"
        };
        let mut r = Report::titled("Usage · this session")
            .field("model", self.model.clone())
            .field("turns", self.turns.to_string())
            .field(
                "tokens",
                format!(
                    "{approx}{} ({approx}{} in / {approx}{} out)",
                    tokens(self.total_tokens),
                    tokens(self.prompt_tokens),
                    tokens(self.completion_tokens)
                ),
            );
        if self.cached_tokens > 0 {
            r = r.field("cached input", tokens(self.cached_tokens));
        }
        r = r
            .field(
                "largest request",
                format!("{} tokens", tokens(self.peak_prompt_tokens as u64)),
            )
            .field("model calls", self.model_calls.to_string())
            .field(
                "tool calls",
                format!(
                    "{} ({} repeats avoided)",
                    self.tool_calls, self.duplicate_tool_calls
                ),
            )
            .field("time", millis(self.elapsed_ms));
        if let Some((used, budget)) = self.context {
            r = r.field(
                "context",
                format!("{} / {}", tokens(used as u64), tokens(budget as u64)),
            );
        }
        if let Some(c) = self.cost {
            r = r.field("cost", format!("{approx}${c:.4}"));
        }
        if !self.measured && self.turns > 0 {
            r = r.note("~ some numbers are estimates (the provider did not report usage)");
        }
        r.data("usage", self)
    }
}

// ---- working state -------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub id: String,
    pub short: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    /// The project file this artifact is a snapshot of, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl From<&Artifact> for ArtifactSummary {
    fn from(a: &Artifact) -> Self {
        ArtifactSummary {
            id: a.id.as_str().to_string(),
            short: a.id.short().to_string(),
            name: a.name.clone(),
            kind: a.kind.as_tag().to_string(),
            size: a.size,
            path: a
                .meta
                .get("path")
                .and_then(|p| p.as_str())
                .map(str::to_string),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateView {
    /// The runtime's working state, as stored.
    pub state: ResearchState,
    /// Exactly the text the model receives each request.
    pub digest: String,
    pub digest_tokens: u32,
    /// Items per section the model sees; older ones are counted, not sent.
    pub per_section: usize,
    /// Sections with items beyond the cap: (section, how many are not sent).
    pub not_sent: Vec<(String, usize)>,
    /// The unresolved part of the investigation graph.
    pub frontier: Vec<String>,
    pub artifacts: Vec<ArtifactSummary>,
}

impl StateView {
    pub fn build(state: ResearchState, per_section: usize, artifacts: &[Artifact]) -> Self {
        let digest = state.digest(per_section);
        let digest_tokens = ark_sdk::prelude::ark_model::estimate_tokens(&digest);
        let frontier = state.graph.frontier();
        let over = |n: usize| n.saturating_sub(per_section);
        let not_sent: Vec<(String, usize)> = [
            ("findings", over(state.findings.len())),
            ("hypotheses", over(state.hypotheses.len())),
            ("interesting", over(state.interesting.len())),
            ("questions", over(state.questions.len())),
            ("rejected", over(state.rejected.len())),
            ("next actions", over(state.next_actions.len())),
            ("frontier", over(frontier.len())),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| (s.to_string(), n))
        .collect();
        StateView {
            state,
            digest,
            digest_tokens,
            per_section,
            not_sent,
            frontier,
            artifacts: artifacts.iter().map(ArtifactSummary::from).collect(),
        }
    }

    pub fn overflow(&self) -> usize {
        self.not_sent.iter().map(|(_, n)| n).sum()
    }

    pub fn report(&self) -> Report {
        if self.state.is_empty() && self.state.goal.is_empty() {
            return Report::new()
                .note("The working state is empty.")
                .data("state", self);
        }
        let mut r = Report::titled(format!(
            "Working state · {} tokens",
            tokens(self.digest_tokens as u64)
        ))
        .code(self.digest.trim_end());
        if !self.not_sent.is_empty() {
            let parts: Vec<String> = self
                .not_sent
                .iter()
                .map(|(s, n)| format!("{n} {s}"))
                .collect();
            r = r.note(format!(
                "stored but not sent (over {} per section): {}",
                self.per_section,
                parts.join(", ")
            ));
        }
        r.data("state", self)
    }
}

// ---- sessions and transcript ---------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub short: String,
    pub title: String,
    pub project: String,
    pub project_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub turns: u32,
    pub tokens: u64,
    pub current: bool,
}

impl SessionSummary {
    pub fn of(r: &SessionRecord, project_name: &str, current: bool) -> Self {
        SessionSummary {
            id: r.id.as_str().to_string(),
            short: r.id.short().to_string(),
            title: r.title.clone(),
            project: r.project.as_str().to_string(),
            project_name: project_name.to_string(),
            created_at: r.created_at,
            updated_at: r.updated_at,
            turns: r.turn_count,
            tokens: r.usage.total_tokens(),
            current,
        }
    }
}

pub fn sessions_report(list: &[SessionSummary]) -> Report {
    if list.is_empty() {
        return Report::new().note("No sessions yet.");
    }
    let mut t = Table::plain(4);
    for s in list.iter().take(20) {
        let title = if s.title.is_empty() {
            "(empty)".to_string()
        } else {
            clip(&s.title, 50)
        };
        t.push(crate::report::Row {
            cells: vec![
                crate::report::Cell::new(if s.current { "›" } else { " " }, Tone::Accent),
                crate::report::Cell::new(s.short.clone(), Tone::Accent),
                crate::report::Cell::new(
                    title,
                    if s.title.is_empty() {
                        Tone::Muted
                    } else {
                        Tone::Normal
                    },
                ),
                crate::report::Cell::new(
                    format!("{} · {}", count(s.turns as u64, "turn"), ago(s.updated_at)),
                    Tone::Muted,
                ),
            ],
            tone: Tone::Normal,
            command: Some(format!("/resume {}", s.short)),
        });
    }
    let mut r = Report::new().table(t);
    if list.len() > 20 {
        r = r.note(format!("… {} more", list.len() - 20));
    }
    r.action("switch to a session", "/resume <id>")
        .data("sessions", list)
}

/// Where a chat left off: its title, its last exchanges clipped, and that the agent goes on
/// from its working state. `heading` says how the person got here ("Resumed").
pub fn recap(heading: &str, s: &SessionSummary, last: &[TranscriptTurn]) -> Report {
    let title = if s.title.is_empty() {
        s.short.clone()
    } else {
        clip(&s.title, 60)
    };
    let mut r = Report::new().success(format!(
        "{heading} {title} · {} · {}",
        count(s.turns as u64, "turn"),
        ago(s.updated_at)
    ));
    for t in last {
        r = r.text(format!("› {}", clip(&t.input, 100)));
        r = r.note(match (&t.unfinished, t.cancelled) {
            (_, true) => "  (stopped)".to_string(),
            (Some(why), _) => format!("  (did not finish: {why})"),
            (None, _) => format!("  {}", clip(&t.answer, 160)),
        });
        for p in &t.posts {
            r = r.note(format!("  ↳ {}: {}", p.from, clip(&p.text, 160)));
        }
    }
    r.note("The agent continues from its working state; the transcript is not replayed.")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub target: String,
    pub current: bool,
    /// The folder its tools work in, when one was chosen (else a scratch folder in the store).
    #[serde(default)]
    pub workspace: Option<String>,
}

/// One stored exchange for display, with its activity when a trace was kept.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptTurn {
    pub index: u32,
    pub input: String,
    pub answer: String,
    #[serde(default)]
    pub steered: Vec<String>,
    pub cancelled: bool,
    /// Why the turn did not finish (it failed, or the app stopped while it ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub footer: Vec<String>,
    /// The turn's activity, rebuilt from its stored trace (empty when none was kept).
    pub activity: Vec<Activity>,
    pub has_trace: bool,
    pub elapsed_ms: u64,
    /// Posted in the chat after this turn by work it set up (a scheduled run's result).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub posts: Vec<Post>,
}

impl TranscriptTurn {
    pub fn of(t: &Turn, activity: Vec<Activity>, has_trace: bool) -> Self {
        let trivial = !t.metrics.model_calls.contains_key("plan");
        TranscriptTurn {
            index: t.index,
            input: t.input.clone(),
            answer: t.answer.clone(),
            steered: t.steered.clone(),
            cancelled: t.cancelled,
            unfinished: t
                .incomplete
                .then(|| t.error.clone().unwrap_or_else(|| "did not finish".into())),
            at: t.at,
            footer: footer(
                &t.metrics,
                t.metrics.elapsed_ms,
                !t.cited.is_empty(),
                trivial,
            ),
            activity,
            has_trace,
            elapsed_ms: t.metrics.elapsed_ms,
            posts: Vec::new(),
        }
    }
}

/// The one-line summary under a finished turn: `3 tools · 25k tokens · 15.0s`.
pub fn footer(m: &Metrics, elapsed_ms: u64, grounded: bool, trivial: bool) -> Vec<String> {
    let mut parts = Vec::new();
    if m.tool_calls > 0 {
        parts.push(count(m.tool_calls as u64, "tool"));
    }
    let approx = if m.tokens_measured { "" } else { "~" };
    parts.push(format!("{approx}{} tokens", tokens(m.total_tokens())));
    parts.push(format!("{:.1}s", elapsed_ms as f64 / 1000.0));
    if !grounded && !trivial {
        parts.push("no evidence cited".into());
    }
    parts
}

/// Artifact id → name, for readable activity labels.
pub fn artifact_names(artifacts: &[Artifact]) -> HashMap<String, String> {
    artifacts
        .iter()
        .map(|a| (a.id.as_str().to_string(), a.name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_view_explains_every_section_and_what_was_kept_out() {
        let report = ContextReport {
            step: 2,
            instructions: 400,
            task: 20,
            tool_rounds: 300,
            retrieved: 50,
            compressed_rounds: 1,
            total: 770,
            budget: 6000,
            ..Default::default()
        };
        let k = KeptOut {
            transcript_turns: 40,
            transcript_chars: 80_000,
            carried_tokens: 900,
            gists: 8,
            dropped_turns: 29,
            compactions: 0,
            tool_calls: 120,
            raw_output_chars: 2_000_000,
            distilled_chars: 40_000,
            cards: 120,
            artifacts: 3,
            state_overflow: 4,
        };
        let v = ContextView::build(&report, ContextSource::LastRequest, Some(1200), &k);
        assert_eq!(v.included.len(), 9);
        assert!(v.included.iter().all(|s| !s.about.is_empty()));
        let labels: Vec<&str> = v.excluded.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Transcript",
                "Raw tool output",
                "Older turns",
                "Earlier steps",
                "Older working-state items",
                "Artifact contents"
            ]
        );
        assert_eq!(v.excluded[0].tokens, Some(19_100));
        let text = v.report().plain_text();
        assert!(text.contains("Retrieved context"), "{text}");
        assert!(text.contains("Kept out of the request"));
    }

    #[test]
    fn pricing_counts_cached_input_separately() {
        let p = Pricing {
            input: 3.0,
            output: 15.0,
            cached: Some(0.3),
        };
        let c = p.cost(1_000_000, 500_000, 100_000);
        assert!((c - (1.5 + 0.15 + 1.5)).abs() < 1e-9, "{c}");
    }
}
