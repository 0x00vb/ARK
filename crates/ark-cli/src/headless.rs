//! Non-interactive runs: `app -p "prompt"`. Same runtime, same session store, no prompt loop.
//!
//! - text (default): activity on stderr, the answer on stdout.
//! - a prompt that starts with `/` runs that command and prints its report.
//! - `--json`: one JSON object on stdout when the turn ends.
//! - `--events`: every runtime event as a JSON line on stdout, then a `result` line.
//!
//! State-changing actions are refused unless `--yes` is given; nothing can prompt here.
//! With `--connect` the turn runs on that runtime and the result has `ok`, `answer`,
//! `cancelled`, `steps`, `session` and `project` (ADR 0024).

use std::io::{IsTerminal, Write};

use ark_sdk::prelude::*;

use crate::app::ShellState;
use crate::chat::Update;
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
    let cfg = st.cfg.clone();
    // A command (`-p "/usage"`) runs as in the shell; its report goes to stdout. A command
    // that hands text to the agent (`/chapter 4`) then runs that turn.
    let mut prompt = prompt;
    if prompt.trim_start().starts_with('/') {
        let reply = st.chat.execute(prompt.trim()).await;
        match mode {
            Mode::Text => {
                let theme = cfg
                    .theme
                    .clone()
                    .with_color(want_color(std::io::stdout().is_terminal()));
                let mut o = std::io::stdout().lock();
                for l in crate::render::render(&reply.report, &theme, 100) {
                    let _ = writeln!(o, "{l}");
                }
            }
            Mode::Json | Mode::Events => {
                let mut v = serde_json::json!({ "ok": true, "report": reply.report });
                if mode == Mode::Events {
                    v["type"] = serde_json::json!("command");
                }
                println!("{v}");
            }
        }
        match reply.effect {
            Some(ark_interact::Effect::Submit(t)) => prompt = t,
            _ => return std::process::ExitCode::SUCCESS,
        }
    }
    let theme = cfg
        .theme
        .clone()
        .with_color(want_color(std::io::stderr().is_terminal()));
    let mut err_out = Out::sink(Box::new(std::io::stderr()));
    let mut view = TurnView::new(&theme, cfg.event_hook.as_ref(), st.chat.detail());
    let mut text_out = StdoutText::default();

    // Nobody can answer a question here. With `--yes` state-changing actions are allowed
    // once each; without it they are refused (a turn here is not even asked).
    let decision = if yes {
        ApprovalDecision::AllowOnce
    } else {
        ApprovalDecision::Deny
    };
    let mut turn = st.chat.turn(&prompt, yes);
    let ctl = turn.control.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            if let Err(error) = ctl.cancel() {
                eprintln!("could not stop the chat: {error}");
            }
        }
    });
    let mut show = |u: Update| {
        let (ev, a) = match u {
            Update::Event(ev, a) => (ev, a),
            Update::Ask(q) => return q.answer(decision),
            Update::Closed(_) => return,
        };
        match mode {
            Mode::Events => {
                if let Ok(line) = serde_json::to_string(&ev) {
                    let mut o = std::io::stdout().lock();
                    let _ = writeln!(o, "{line}");
                    let _ = o.flush();
                }
            }
            Mode::Json => {}
            Mode::Text => {
                // Model text goes to stdout as it streams; everything else is activity.
                let printed = text_out.event(&ev);
                if !printed.is_empty() {
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(printed.as_bytes());
                    let _ = o.flush();
                }
                if !matches!(
                    ev,
                    HarnessEvent::TextDelta { .. } | HarnessEvent::Answer { .. }
                ) {
                    view.event(ev, a, &mut err_out);
                }
            }
        }
    };
    let end = loop {
        tokio::select! {
            biased;
            Some(u) = turn.updates.recv() => show(u),
            end = &mut turn.end => break end,
        }
    };
    while let Ok(u) = turn.updates.try_recv() {
        show(u);
    }

    match mode {
        Mode::Text => {
            if let Some(e) = &end.error {
                err_out.line(&theme.red(&format!("error: {e}")));
                // Retrying a one-shot run does not add credit: say what does.
                if !end.result["out_of_credit"].is_null() {
                    err_out.line(&theme.dim(&end.hint));
                }
            }
        }
        Mode::Json | Mode::Events => {
            let mut v = end.result.clone();
            if mode == Mode::Events {
                v["type"] = serde_json::json!("result");
            }
            println!("{v}");
        }
    }
    std::process::ExitCode::from(end.code())
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
            // Written text cannot be taken back from stdout: the line ends, and the answer is
            // compared with what the retry streams only.
            HarnessEvent::TextWithdrawn { .. } => self.end_segment(),
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

    #[test]
    fn a_retried_answer_is_printed_once_after_the_withdrawn_text() {
        let out = run(vec![
            delta("Half an ans"),
            HarnessEvent::TextWithdrawn {
                text: "Half an ans".into(),
            },
            delta("Whole "),
            delta("answer."),
            answer("Whole answer.", true),
        ]);
        assert_eq!(out, "Half an ans\nWhole answer.\n");
    }
}
