//! Renders one turn's runtime events as terminal activity. This is the only place that decides
//! how an event looks; the runtime just reports what happened. The normal view shows what the
//! agent is doing in plain words; the debug view adds arguments, timings and model calls.

use std::collections::HashMap;
use std::time::Instant;

use ark_sdk::prelude::*;

use crate::activity::Activity;
use crate::app::{EventHook, TurnRecord};
use crate::markdown::MdStream;
use crate::term::{clip, tokens, Out};
use crate::theme::Theme;

pub(crate) struct TurnView<'a> {
    theme: &'a Theme,
    activity: &'a Activity,
    hook: Option<&'a EventHook>,
    pub verbose: bool,
    /// Draw the spinner/status line (interactive only).
    pub live: bool,
    md: MdStream,
    streaming: bool,
    started: Instant,
    step: usize,
    tools: u32,
    tokens_used: u64,
    frame: usize,
    pub typeahead: String,
    pub cancelling: bool,
    pub in_menu: bool,
    artifact_names: HashMap<String, String>,
    events: Vec<(u64, HarnessEvent)>,
    pub last_context: Option<ContextReport>,
    printed_answer: bool,
}

impl<'a> TurnView<'a> {
    pub(crate) fn new(
        theme: &'a Theme,
        activity: &'a Activity,
        hook: Option<&'a EventHook>,
        verbose: bool,
    ) -> Self {
        TurnView {
            theme,
            activity,
            hook,
            verbose,
            live: false,
            md: MdStream::new(theme.color_enabled(), theme.raw_accent_sgr()),
            streaming: false,
            started: Instant::now(),
            step: 0,
            tools: 0,
            tokens_used: 0,
            frame: 0,
            typeahead: String::new(),
            cancelling: false,
            in_menu: false,
            artifact_names: HashMap::new(),
            events: Vec::new(),
            last_context: None,
            printed_answer: false,
        }
    }

    /// Artifact ids the agent passes as `artifact` are shown by name.
    pub(crate) fn with_artifacts(mut self, names: HashMap<String, String>) -> Self {
        self.artifact_names = names;
        self
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn record(&mut self, ev: &HarnessEvent) {
        let t = self.elapsed_ms();
        // Merge consecutive text deltas so the trace stays readable.
        if let HarnessEvent::TextDelta { text } = ev {
            if let Some((_, HarnessEvent::TextDelta { text: prev })) = self.events.last_mut() {
                prev.push_str(text);
                return;
            }
        }
        self.events.push((t, ev.clone()));
    }

    fn close_stream(&mut self, out: &mut Out) {
        if self.streaming {
            let tail = self.md.finish();
            out.text(&tail);
            out.end_line();
            self.streaming = false;
        }
    }

    fn activity_line(&mut self, out: &mut Out, label: &str) {
        self.close_stream(out);
        let bullet = self.theme.accented(&self.theme.glyphs.activity);
        out.line(&format!("{bullet} {label}"));
    }

    fn detail(&mut self, out: &mut Out, text: &str) {
        let width = out.width().saturating_sub(6);
        out.line(&format!(
            "  {} {}",
            self.theme.dim(&self.theme.glyphs.result),
            self.theme.dim(&clip(text, width))
        ));
    }

    fn debug(&mut self, out: &mut Out, text: &str) {
        if self.verbose {
            self.close_stream(out);
            let width = out.width().saturating_sub(4);
            out.line(&format!("    {}", self.theme.dim(&clip(text, width))));
        }
    }

    fn display_input(&self, input: &serde_json::Value) -> serde_json::Value {
        let mut v = input.clone();
        if let Some(a) = v
            .get("artifact")
            .and_then(|a| a.as_str())
            .map(str::to_string)
        {
            if let Some(name) = self.artifact_names.get(&a).or_else(|| {
                self.artifact_names
                    .iter()
                    .find(|(k, _)| k.starts_with(&a))
                    .map(|(_, n)| n)
            }) {
                v["artifact"] = serde_json::Value::String(name.clone());
            }
        }
        v
    }

    pub(crate) fn event(&mut self, ev: HarnessEvent, out: &mut Out) {
        self.record(&ev);
        if let Some(hook) = self.hook {
            if let Some(line) = hook(&ev, self.theme) {
                self.close_stream(out);
                out.line(&line);
                return;
            }
        }
        let t = self.theme;
        match ev {
            HarnessEvent::Classified { .. } => {}
            HarnessEvent::Plan { steps } => {
                self.activity_line(out, "Planning");
                for s in steps.iter().take(6) {
                    out.line(&format!(
                        "    {}",
                        t.dim(&clip(s, out.width().saturating_sub(6)))
                    ));
                }
            }
            HarnessEvent::Step { index } => {
                self.step = index;
                self.debug(out, &format!("step {index}"));
            }
            HarnessEvent::Context { report } => {
                if self.verbose {
                    self.debug(
                        out,
                        &format!(
                            "context {} / {} · state {} · conversation {} · tool results {}{}",
                            tokens(report.total as u64),
                            tokens(report.budget as u64),
                            report.working_state,
                            report.conversation,
                            report.tool_rounds,
                            if report.compressed_rounds > 0 {
                                format!(" · {} rounds compressed", report.compressed_rounds)
                            } else {
                                String::new()
                            }
                        ),
                    );
                }
                self.last_context = Some(report);
            }
            HarnessEvent::TextDelta { text } => {
                if !self.streaming {
                    self.streaming = true;
                    out.end_line();
                }
                let styled = self.md.push(&text);
                out.text(&styled);
            }
            HarnessEvent::ModelCall {
                role,
                model,
                usage,
                elapsed_ms,
            } => {
                self.tokens_used += usage.total() as u64;
                self.debug(
                    out,
                    &format!(
                        "model {role} · {model} · {} in / {} out{} · {:.1}s",
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        if usage.measured { "" } else { " (est.)" },
                        elapsed_ms as f64 / 1000.0
                    ),
                );
            }
            HarnessEvent::ToolCall { action, input, .. } => {
                let shown = self.display_input(&input);
                let label = self.activity.call_label(&action, &shown);
                self.activity_line(out, &label);
                self.debug(out, &format!("{action} {}", input));
            }
            HarnessEvent::ToolResult {
                action,
                ok,
                summary,
                duration_ms,
                invocation,
                error,
                ..
            } => {
                self.tools += 1;
                if ok {
                    let d = self.activity.result_detail(&action, ok, &summary);
                    if !d.is_empty() {
                        self.detail(out, &d);
                    }
                } else {
                    let msg = error.unwrap_or(summary);
                    out.line(&format!(
                        "  {} {}",
                        t.red(&t.glyphs.result),
                        t.red(&clip(&msg, out.width().saturating_sub(6)))
                    ));
                }
                let d = duration_ms.map(|d| format!("{d}ms · ")).unwrap_or_default();
                self.debug(
                    out,
                    &format!("{d}inv:{}", &invocation[..8.min(invocation.len())]),
                );
            }
            HarnessEvent::Duplicate { action, reused } => {
                self.activity_line(out, &format!("Reusing an earlier result · {action}"));
                self.debug(out, &reused);
            }
            HarnessEvent::Retrieval { what } => {
                let label = if what.starts_with("search_knowledge") {
                    "Consulting background knowledge"
                } else if what.starts_with("search_evidence") {
                    "Searching gathered evidence"
                } else {
                    "Recalling evidence"
                };
                self.activity_line(out, label);
                self.debug(out, &what);
            }
            HarnessEvent::NeedsApproval { action } => {
                self.close_stream(out);
                out.line(&format!(
                    "  {} {}",
                    t.yellow(&t.glyphs.warn),
                    t.yellow(&format!("{action} needs approval; not run"))
                ));
            }
            HarnessEvent::ApprovalRequested { .. } => {}
            HarnessEvent::ApprovalResolved {
                decision, class, ..
            } => {
                let text = match decision {
                    ApprovalDecision::AllowOnce => t.dim("allowed once"),
                    ApprovalDecision::AllowSession => {
                        t.dim(&format!("allowed for this session ({class})"))
                    }
                    ApprovalDecision::Deny => t.yellow("denied"),
                };
                out.line(&format!("  {} {text}", t.dim(&t.glyphs.result)));
            }
            HarnessEvent::Capability { action, detail } => {
                let verb = match action.as_str() {
                    "find_capability" => "Looking for a capability",
                    "propose_capability" => "Proposing a capability",
                    _ => "Acquiring a capability",
                };
                self.activity_line(out, verb);
                self.detail(out, detail.lines().next().unwrap_or(""));
            }
            HarnessEvent::StateUpdate { kind, text } => {
                self.close_stream(out);
                let w = out.width().saturating_sub(12);
                out.line(&format!(
                    "  {} {}",
                    t.dim(&format!("noted {}:", kind.replace('_', " "))),
                    t.dim(&clip(&text, w))
                ));
            }
            HarnessEvent::Note { kind, body } => {
                self.close_stream(out);
                let w = out.width().saturating_sub(12);
                out.line(&format!(
                    "  {} {}",
                    t.dim(&format!("note ({kind}):")),
                    t.dim(&clip(&body, w))
                ));
            }
            HarnessEvent::Present { directive } => {
                self.activity_line(out, &format!("Showing {}", directive.headline()));
            }
            HarnessEvent::SelfCheck { passed, detail } => {
                if !passed {
                    self.close_stream(out);
                    out.line(&format!(
                        "  {} {}",
                        t.yellow(&t.glyphs.warn),
                        t.yellow(&detail)
                    ));
                } else {
                    self.debug(out, &format!("self-check: {detail}"));
                }
            }
            HarnessEvent::Cancelled => {
                self.close_stream(out);
                out.line(&t.yellow("Stopped."));
            }
            HarnessEvent::Answer { text, streamed } => {
                if streamed {
                    self.close_stream(out);
                } else {
                    self.close_stream(out);
                    out.end_line();
                    let mut md = MdStream::new(t.color_enabled(), t.raw_accent_sgr());
                    let styled = md.push(&text) + &md.finish();
                    out.text(&styled);
                    out.end_line();
                }
                self.printed_answer = true;
            }
        }
    }

    /// Redraw the status line (interactive).
    pub(crate) fn tick(&mut self, out: &mut Out) {
        if !self.live || self.in_menu || self.streaming {
            return;
        }
        self.frame = (self.frame + 1) % self.theme.glyphs.spinner.len().max(1);
        let spin = self.theme.accented(&self.theme.glyphs.spinner[self.frame]);
        let secs = self.started.elapsed().as_secs();
        let what = if self.cancelling {
            "Stopping".to_string()
        } else {
            "Working".to_string()
        };
        let mut parts = vec![format!("{secs}s")];
        if self.step > 0 {
            parts.push(format!("step {}", self.step));
        }
        if self.tokens_used > 0 {
            parts.push(format!("{} tokens", tokens(self.tokens_used)));
        }
        let hint = if self.cancelling {
            String::new()
        } else {
            self.theme.dim("  esc to stop")
        };
        let queued = if self.typeahead.is_empty() {
            String::new()
        } else {
            format!(
                "  {} {}",
                self.theme.dim("next:"),
                clip(&self.typeahead, 40)
            )
        };
        out.status(&format!(
            "{spin} {what} {}{hint}{queued}",
            self.theme.dim(&format!("· {}", parts.join(" · ")))
        ));
    }

    /// End of turn: close the stream and print the footer.
    pub(crate) fn finish(
        mut self,
        out: &mut Out,
        input: &str,
        result: &Result<HarnessOutcome, ArkError>,
    ) -> TurnRecord {
        self.close_stream(out);
        out.clear_status();
        let elapsed_ms = self.elapsed_ms();
        let (cancelled, error) = match result {
            Ok(o) => (o.cancelled, None),
            Err(e) => (false, Some(e.to_string())),
        };
        match result {
            Err(e) => {
                out.line(&format!(
                    "{} {}",
                    self.theme.red(&self.theme.glyphs.fail),
                    self.theme.red(&format!("{e}"))
                ));
                out.line(&self.theme.dim(
                    "  The session is intact. Try again, /model to switch, or /trace for details.",
                ));
            }
            Ok(o) if !o.cancelled && self.theme.turn_footer => {
                let m = &o.metrics;
                let mut parts = Vec::new();
                if m.tool_calls > 0 {
                    parts.push(format!(
                        "{} tool{}",
                        m.tool_calls,
                        if m.tool_calls == 1 { "" } else { "s" }
                    ));
                }
                let approx = if m.tokens_measured { "" } else { "~" };
                parts.push(format!("{approx}{} tokens", tokens(m.total_tokens())));
                parts.push(format!("{:.1}s", elapsed_ms as f64 / 1000.0));
                if !o.grounded() && !o.trivial {
                    parts.push("no evidence cited".into());
                }
                out.line("");
                out.line(&self.theme.dim(&format!("  {}", parts.join(" · "))));
            }
            _ => {}
        }
        TurnRecord {
            input: input.to_string(),
            events: self.events,
            elapsed_ms,
            cancelled,
            error,
        }
    }
}
