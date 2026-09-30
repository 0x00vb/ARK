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
    let mut answer_streamed = String::new();

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
            Mode::Text => match ev {
                // The answer text goes to stdout as it streams; everything else is activity.
                HarnessEvent::TextDelta { text } => {
                    answer_streamed.push_str(text);
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(text.as_bytes());
                    let _ = o.flush();
                }
                HarnessEvent::Answer { text, streamed } => {
                    let mut o = std::io::stdout().lock();
                    if !streamed || text.trim() != answer_streamed.trim() {
                        let sep = if answer_streamed.is_empty() { "" } else { "\n" };
                        let _ = writeln!(o, "{sep}{text}");
                    } else {
                        let _ = writeln!(o);
                    }
                }
                other => view.event(other.clone(), a.cloned(), &mut err_out),
            },
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
