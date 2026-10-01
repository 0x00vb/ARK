//! Non-interactive runs: `app -p "prompt"`. Same runtime, same session store, no prompt loop.
//!
//! - text (default): activity on stderr, the answer on stdout.
//! - `--json`: one JSON object on stdout when the turn ends.
//! - `--events`: every runtime event as a JSON line on stdout, then a `result` line.
//!
//! State-changing actions are refused unless `--yes` is given; nothing can prompt here.

use std::io::{IsTerminal, Write};

use ark_interact::{Interaction, TurnControl, TurnEnd};
use ark_sdk::prelude::*;

use crate::app::ShellState;
use crate::term::Out;
use crate::theme::want_color;
use crate::view::TurnView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Text,
    Json,
    Events,
}

pub(crate) async fn run(
    mut st: ShellState,
    prompt: String,
    mode: Mode,
    yes: bool,
) -> std::process::ExitCode {
    let ctl = TurnControl {
        ask: yes,
        ..TurnControl::new()
    };
    let c2 = ctl.cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c2.cancel();
        }
    });
    // Nobody can answer an approval here. With `--yes` every question gets "allow once";
    // without it the gate is not consulted and state-changing actions are refused.
    let decision = ApprovalDecision::AllowOnce;
    let mut approvals =
        std::mem::replace(&mut st.approvals, tokio::sync::mpsc::unbounded_channel().1);
    tokio::spawn(async move {
        while let Some(p) = approvals.recv().await {
            p.answer(decision);
        }
    });

    let cfg = st.cfg.clone();
    let theme = cfg
        .theme
        .clone()
        .with_color(want_color(std::io::stderr().is_terminal()));
    let mut err_out = Out::sink(Box::new(std::io::stderr()));
    let mut view = TurnView::new(&theme, cfg.event_hook.as_ref(), st.ix.detail());
    let mut text_out = StdoutText::default();

    let end = st
        .ix
        .run_turn(&prompt, &ctl, |ev, a| match mode {
            Mode::Events => {
                if let Ok(line) = serde_json::to_string(ev) {
                    let mut o = std::io::stdout().lock();
                    let _ = writeln!(o, "{line}");
                    let _ = o.flush();
                }
            }
            Mode::Json => {}
            Mode::Text => {
                // Model text goes to stdout as it streams; everything else is activity.
                let printed = text_out.event(ev);
                if !printed.is_empty() {
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(printed.as_bytes());
                    let _ = o.flush();
                }
                if !matches!(
                    ev,
                    HarnessEvent::TextDelta { .. } | HarnessEvent::Answer { .. }
                ) {
                    view.event(ev.clone(), a.cloned(), &mut err_out);
                }
            }
        })
        .await;

    let code = match &end.result {
        Ok(o) if o.cancelled => 130,
        Ok(_) => 0,
        Err(_) => 1,
    };
    match mode {
        Mode::Text => {
            if let Err(e) = &end.result {
                err_out.line(&theme.red(&format!("error: {e}")));
            }
        }
        Mode::Json | Mode::Events => {
            println!("{}", result_json(&st.ix, &end, mode == Mode::Events));
        }
    }
    std::process::ExitCode::from(code)
}

fn result_json(ix: &Interaction, end: &TurnEnd, tagged: bool) -> serde_json::Value {
    let mut v = match &end.result {
        Ok(o) => serde_json::json!({
            "ok": true,
            "answer": o.answer,
            "cancelled": o.cancelled,
            "grounded": o.grounded(),
            "cited": o.cited.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "steps": o.steps,
            "metrics": o.metrics,
            "findings": o.state.findings.iter().map(|f| &f.text).collect::<Vec<_>>(),
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    v["session"] = serde_json::json!(ix.session().id().as_str());
    v["project"] = serde_json::json!(ix.project().name);
    v["context"] = serde_json::to_value(ix.last_context()).unwrap_or_default();
    if tagged {
        v["type"] = serde_json::json!("result");
    }
    v
}

/// What text mode writes to stdout. Each step's streamed text is its own segment: narration
/// before a tool call ends at the call, so the final answer is compared with its own stream
/// only. (Comparing with everything streamed printed the answer twice after any narration.)
#[derive(Default)]
struct StdoutText {
    segment: String,
}

impl StdoutText {
    fn event(&mut self, ev: &HarnessEvent) -> String {
        match ev {
            HarnessEvent::TextDelta { text } => {
                self.segment.push_str(text);
                text.clone()
            }
            HarnessEvent::Step { .. }
            | HarnessEvent::ToolCall { .. }
            | HarnessEvent::Steered { .. } => self.end_segment(),
            HarnessEvent::Answer { text, streamed } => {
                let out = if *streamed && text.trim() == self.segment.trim() {
                    "\n".to_string()
                } else {
                    format!("{}{text}\n", self.end_segment())
                };
                self.segment.clear();
                out
            }
            _ => String::new(),
        }
    }

    fn end_segment(&mut self) -> String {
        if self.segment.is_empty() {
            return String::new();
        }
        self.segment.clear();
        "\n".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: Vec<HarnessEvent>) -> String {
        let mut t = StdoutText::default();
        events.iter().map(|e| t.event(e)).collect()
    }
    fn delta(s: &str) -> HarnessEvent {
        HarnessEvent::TextDelta { text: s.into() }
    }
    fn call() -> HarnessEvent {
        HarnessEvent::ToolCall {
            id: "c".into(),
            action: "notes.read".into(),
            input: serde_json::json!({}),
        }
    }
    fn answer(s: &str, streamed: bool) -> HarnessEvent {
        HarnessEvent::Answer {
            text: s.into(),
            streamed,
        }
    }

    #[test]
    fn narration_then_answer_prints_the_answer_once() {
        let out = run(vec![
            HarnessEvent::Step { index: 1 },
            delta("Reading the notes."),
            call(),
            HarnessEvent::Step { index: 2 },
            delta("The notes mention "),
            delta("a locket."),
            answer("The notes mention a locket.", true),
        ]);
        assert_eq!(out, "Reading the notes.\nThe notes mention a locket.\n");
    }

    #[test]
    fn an_unstreamed_or_revised_answer_is_printed() {
        assert_eq!(run(vec![answer("Plain.", false)]), "Plain.\n");
        let out = run(vec![
            delta("Draft answer."),
            answer("Revised answer.", true),
        ]);
        assert_eq!(out, "Draft answer.\nRevised answer.\n");
    }
}
