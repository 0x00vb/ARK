//! The simple agent loop: a straightforward ReAct-style tool-calling loop. It answers from
//! what the tools returned, not from the model's prior, and every tool call and note goes
//! through the same invocation log and approval gate as a human's. For richer investigations
//! with a managed context budget, model routing and a persistent working state, use
//! [`crate::Harness`].

use crate::engine::Engine;
use crate::error::AgentError;
use crate::profile::AgentProfile;
use ark_core::{Author, InvocationId, InvocationStatus, Note, NoteKind, ProjectId, RiskTier};
use ark_model::{Message, Provider, ToolDef};
use std::fmt::Write as _;

/// The meta-tool the agent uses to preserve notes. Handled inside the loop, so it never
/// touches the approval gate.
const RECORD_NOTE: &str = "record_note";

/// Events emitted as the agent works, so a front-end can show live progress.
pub enum AgentEvent<'a> {
    ToolCall {
        action: &'a str,
        input: &'a serde_json::Value,
    },
    ToolResult {
        action: &'a str,
        ok: bool,
        summary: String,
    },
    NeedsApproval {
        action: &'a str,
    },
    Note {
        kind: &'a str,
        body: &'a str,
    },
    Answer {
        text: &'a str,
    },
}

pub struct AgentOutcome {
    pub answer: String,
    pub cited: Vec<InvocationId>,
    pub notes: usize,
    pub steps: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub peak_prompt_tokens: u32,
}

pub struct Agent {
    provider: Box<dyn Provider>,
    allow_mutate: bool,
    max_steps: usize,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Agent {
            provider,
            allow_mutate: false,
            max_steps: 10,
        }
    }
    pub fn allow_mutate(mut self, yes: bool) -> Self {
        self.allow_mutate = yes;
        self
    }
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }
    pub fn describe(&self) -> String {
        self.provider.describe()
    }

    /// Ask a question grounded in a project. Drives the tool-calling loop, records every tool
    /// run as an invocation and every note in the notebook, and persists the final answer as
    /// a cited finding. `profile` supplies the agent's persona and result distillers.
    pub async fn ask(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
        question: &str,
        mut on_event: impl FnMut(AgentEvent),
    ) -> Result<AgentOutcome, AgentError> {
        let tools = self.tool_defs(engine);
        let system = self.system_prompt(engine, profile, project)?;
        let mut messages = vec![Message::user(question)];
        let mut cited: Vec<InvocationId> = Vec::new();
        let mut notes = 0usize;
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut peak_prompt_tokens = 0u32;

        for step in 1..=self.max_steps {
            let completion = self
                .provider
                .complete(&system, &messages, &tools)
                .await
                .map_err(|e| AgentError::Provider(e.to_string()))?;
            prompt_tokens += completion.usage.prompt_tokens as u64;
            completion_tokens += completion.usage.completion_tokens as u64;
            peak_prompt_tokens = peak_prompt_tokens.max(completion.usage.prompt_tokens);

            if completion.tool_calls.is_empty() {
                let answer = completion.content.trim().to_string();
                on_event(AgentEvent::Answer { text: &answer });
                self.persist_finding(engine, project, &answer, &cited)?;
                return Ok(AgentOutcome {
                    answer,
                    cited,
                    notes,
                    steps: step,
                    prompt_tokens,
                    completion_tokens,
                    peak_prompt_tokens,
                });
            }

            messages.push(Message::assistant_tool_calls(completion.tool_calls.clone()));

            for call in &completion.tool_calls {
                if call.name == RECORD_NOTE {
                    let (kind, body) = parse_note(&call.arguments);
                    on_event(AgentEvent::Note {
                        kind: kind_str(kind),
                        body: &body,
                    });
                    let mut note = Note::new(kind, &body, Author::Agent);
                    note.cites_invocations = cited.clone();
                    engine.store.put_note(project, &note)?;
                    notes += 1;
                    messages.push(Message::tool_result(
                        call,
                        format!("recorded {} note", kind_str(kind)),
                    ));
                    continue;
                }

                let Some(desc) = engine.registry.descriptor(&call.name) else {
                    messages.push(Message::tool_result(
                        call,
                        format!(
                            "error: unknown tool `{}`. Use only the provided tools.",
                            call.name
                        ),
                    ));
                    continue;
                };

                on_event(AgentEvent::ToolCall {
                    action: &call.name,
                    input: &call.arguments,
                });

                if desc.risk == RiskTier::Mutate && !self.allow_mutate {
                    on_event(AgentEvent::NeedsApproval { action: &call.name });
                    messages.push(Message::tool_result(
                        call,
                        "denied: this action changes state and needs human approval. Do not retry; reason from read-only evidence instead.",
                    ));
                    continue;
                }

                let inv = engine
                    .run_action(
                        project,
                        &call.name,
                        call.arguments.clone(),
                        self.allow_mutate,
                    )
                    .await?;
                let ok = matches!(inv.status, InvocationStatus::Ok);
                let summary = summarize_for_model(&inv);
                on_event(AgentEvent::ToolResult {
                    action: &call.name,
                    ok,
                    summary: short(&summary, 240),
                });
                cited.push(inv.id.clone());
                messages.push(Message::tool_result(call, summary));
            }
        }

        messages.push(Message::user(&profile.prompts.force_answer));
        let final_c = self
            .provider
            .complete(&system, &messages, &[])
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;
        prompt_tokens += final_c.usage.prompt_tokens as u64;
        completion_tokens += final_c.usage.completion_tokens as u64;
        peak_prompt_tokens = peak_prompt_tokens.max(final_c.usage.prompt_tokens);
        let answer = final_c.content.trim().to_string();
        on_event(AgentEvent::Answer { text: &answer });
        self.persist_finding(engine, project, &answer, &cited)?;
        Ok(AgentOutcome {
            answer,
            cited,
            notes,
            steps: self.max_steps,
            prompt_tokens,
            completion_tokens,
            peak_prompt_tokens,
        })
    }

    fn persist_finding(
        &self,
        engine: &Engine,
        project: &ProjectId,
        answer: &str,
        cited: &[InvocationId],
    ) -> Result<(), AgentError> {
        let mut note = Note::new(NoteKind::Finding, answer, Author::Agent);
        note.cites_invocations = cited.to_vec();
        engine.store.put_note(project, &note)?;
        Ok(())
    }

    fn tool_defs(&self, engine: &Engine) -> Vec<ToolDef> {
        let mut defs: Vec<ToolDef> = engine
            .registry
            .actions()
            .into_iter()
            .map(|d| ToolDef {
                description: format!("[{}] {}", risk_word(d.risk), d.summary),
                name: d.name,
                parameters: d.input_schema,
            })
            .collect();
        defs.push(ToolDef {
            name: RECORD_NOTE.into(),
            description: "[record] Preserve a note. Use it to state a hypothesis, mark a fact you established, record an assumption, flag a contradiction, or name the next step.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["kind", "body"],
                "properties": {
                    "kind": { "type": "string", "enum": ["hypothesis", "assumption", "fact", "finding", "contradiction", "next_step", "question"] },
                    "body": { "type": "string", "description": "one or two sentences" }
                }
            }),
        });
        defs
    }

    fn system_prompt(
        &self,
        engine: &Engine,
        profile: &AgentProfile,
        project: &ProjectId,
    ) -> Result<String, AgentError> {
        let p = engine
            .store
            .get_project(project)?
            .ok_or_else(|| AgentError::ProjectNotFound(project.to_string()))?;
        let artifacts = engine.store.list_artifacts(project)?;
        let notes = engine.store.list_notes(project)?;

        let mut s = String::new();
        // The consumer's persona + method — the whole identity of the agent.
        s.push_str(&profile.prompts.instructions);
        s.push_str("\n\n");
        // The runtime's mechanical footer: project, artifacts, and the approval note.
        let _ = writeln!(s, "Project: {} — {}", p.name, p.target);
        let _ = writeln!(s, "\nArtifacts (pass an id as the `artifact` argument):");
        if artifacts.is_empty() {
            let _ = writeln!(s, "  (none yet — ingest a file or capture one first)");
        }
        for a in &artifacts {
            let _ = writeln!(
                s,
                "  - id={} name={} kind={}",
                a.id.as_str(),
                a.name,
                a.kind.as_tag()
            );
        }
        if !notes.is_empty() {
            let _ = writeln!(s, "\nRecord so far (build on it, don't repeat it):");
            for n in notes.iter().rev().take(8).rev() {
                let _ = writeln!(s, "  - [{}] {}", kind_str(n.kind), short(&n.body, 160));
            }
        }
        let _ = write!(
            s,
            "\nRules: prefer read-only actions; state-changing actions require human approval and are denied \
in this session, so reason from observation instead. Cite the artifact ids and tool outputs you used."
        );
        Ok(s)
    }
}

fn parse_note(args: &serde_json::Value) -> (NoteKind, String) {
    let kind = args
        .get("kind")
        .and_then(|k| k.as_str())
        .map(note_kind_from_str)
        .unwrap_or(NoteKind::Note);
    let body = args
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    (kind, body)
}

fn note_kind_from_str(s: &str) -> NoteKind {
    match s {
        "hypothesis" => NoteKind::Hypothesis,
        "assumption" => NoteKind::Assumption,
        "fact" => NoteKind::Fact,
        "finding" => NoteKind::Finding,
        "contradiction" => NoteKind::Contradiction,
        "next_step" => NoteKind::NextStep,
        "question" => NoteKind::Question,
        _ => NoteKind::Note,
    }
}

fn kind_str(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Hypothesis => "hypothesis",
        NoteKind::Assumption => "assumption",
        NoteKind::Fact => "fact",
        NoteKind::Evidence => "evidence",
        NoteKind::Finding => "finding",
        NoteKind::Contradiction => "contradiction",
        NoteKind::NextStep => "next_step",
        NoteKind::Question => "question",
        NoteKind::Note => "note",
    }
}

fn risk_word(r: RiskTier) -> &'static str {
    match r {
        RiskTier::Read => "read-only",
        RiskTier::Observe => "observe",
        RiskTier::Mutate => "changes-state",
    }
}

/// What the model sees after a tool runs: the structured output, or the error.
fn summarize_for_model(inv: &ark_core::Invocation) -> String {
    match inv.status {
        InvocationStatus::Ok => {
            let body = serde_json::to_string(&inv.output).unwrap_or_default();
            short(&body, 3000)
        }
        InvocationStatus::Failed => format!("error: {}", inv.error.clone().unwrap_or_default()),
        InvocationStatus::PendingApproval => "pending human approval".into(),
        InvocationStatus::Denied => "denied by human".into(),
        InvocationStatus::Running => "running".into(),
    }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}… (truncated)")
    }
}
