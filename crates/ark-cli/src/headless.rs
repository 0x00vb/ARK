//! Non-interactive runs: `app -p "prompt"`. Same runtime, same session store, no prompt loop.
//!
//! - text (default): activity on stderr, the answer on stdout.
//! - `--json`: one JSON object on stdout when the turn ends.
//! - `--events`: every runtime event as a JSON line on stdout, then a `result` line.
//!
//! State-changing actions are refused unless `--yes` is given; nothing can prompt here.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

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
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c2.cancel();
        }
    });
    let approvals = yes.then(|| {
        Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
            ApprovalDecision::AllowOnce,
        ))))
    });
    let opts = TurnOptions {
        cancel: Some(cancel),
        approvals,
        focus: None,
    };

    let cfg = st.cfg.clone();
    let theme = cfg
        .theme
        .clone()
        .with_color(want_color(std::io::stderr().is_terminal()));
    let mut err_out = Out::sink(Box::new(std::io::stderr()));
    let mut view = TurnView::new(&theme, &cfg.activity, cfg.event_hook.as_ref(), st.verbose);
    let mut last_context: Option<ContextReport> = None;
    let mut answer_streamed = String::new();

    let ShellState { ark, session, .. } = &mut st;
    let result = ark
        .ask_session(session, &prompt, opts, |ev| match mode {
            Mode::Events => {
                if let Ok(line) = serde_json::to_string(&ev) {
                    let mut o = std::io::stdout().lock();
                    let _ = writeln!(o, "{line}");
                    let _ = o.flush();
                }
            }
            Mode::Json => {
                if let HarnessEvent::Context { report } = &ev {
                    last_context = Some(report.clone());
                }
            }
            Mode::Text => match ev {
                // The answer text goes to stdout as it streams; everything else is activity.
                HarnessEvent::TextDelta { text } => {
                    answer_streamed.push_str(&text);
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(text.as_bytes());
                    let _ = o.flush();
                }
                HarnessEvent::Answer { text, streamed } => {
                    if !streamed || text.trim() != answer_streamed.trim() {
                        let mut o = std::io::stdout().lock();
                        let _ = writeln!(
                            o,
                            "{}{text}",
                            if answer_streamed.is_empty() { "" } else { "\n" }
                        );
                    } else {
                        let _ = writeln!(std::io::stdout());
                    }
                }
                HarnessEvent::Context { report } => last_context = Some(report),
                other => view.event(other, &mut err_out),
            },
        })
        .await;

    let code = match &result {
        Ok(o) if o.cancelled => 130,
        Ok(_) => 0,
        Err(_) => 1,
    };
    match mode {
        Mode::Text => {
            if let Err(e) = &result {
                err_out.line(&theme.red(&format!("error: {e}")));
            }
        }
        Mode::Json | Mode::Events => {
            let obj = result_json(&st, &result, last_context.as_ref(), mode == Mode::Events);
            println!("{obj}");
        }
    }
    std::process::ExitCode::from(code)
}

fn result_json(
    st: &ShellState,
    result: &Result<HarnessOutcome, ArkError>,
    context: Option<&ContextReport>,
    tagged: bool,
) -> serde_json::Value {
    let mut v = match result {
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
    v["session"] = serde_json::json!(st.session.id().as_str());
    v["project"] = serde_json::json!(st.project.name);
    v["context"] = serde_json::to_value(context).unwrap_or_default();
    if tagged {
        v["type"] = serde_json::json!("result");
    }
    v
}
