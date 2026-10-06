//! The chat the shell drives: one opened in this process, or the one open on a runtime served
//! elsewhere (ADR 0024). The shell, the turn driver and headless runs see only this. What
//! differs is where the turn runs and how its events and questions travel:
//!
//! ```text
//!   Local   Interaction::run_turn ──callback──┐
//!           ChannelApprover ───────────────────┼─► Update ─► driver
//!   Remote  /events (server-sent) ─────────────┘
//! ```

use std::future::Future;
use std::pin::Pin;

use ark_interact::remote::Client;
use ark_interact::{
    Activity, CommandInfo, Interaction, PendingApproval, Reply, Report, Summary, TurnControl,
    TurnEnd,
};
use ark_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::remote::Remote;

/// Something that happened during a turn, in the order it happened.
pub(crate) enum Update {
    Event(HarnessEvent, Option<Activity>),
    Ask(Question),
    /// A question was answered elsewhere (another window), or its turn moved on.
    Closed(u64),
}

/// An approval question and the way back to whoever asked it.
pub(crate) struct Question {
    pub id: u64,
    pub request: ApprovalRequest,
    /// The call as the app describes it.
    pub line: String,
    to: Asker,
}

enum Asker {
    Local(PendingApproval),
    Remote(Client),
}

impl Question {
    pub(crate) fn local(p: PendingApproval, line: String) -> Question {
        Question {
            id: 0,
            request: p.request.clone(),
            line,
            to: Asker::Local(p),
        }
    }

    pub(crate) fn remote(client: &Client, id: u64, request: ApprovalRequest, line: String) -> Self {
        Question {
            id,
            request,
            line,
            to: Asker::Remote(client.clone()),
        }
    }

    pub(crate) fn answer(self, d: ApprovalDecision) {
        match self.to {
            Asker::Local(p) => p.answer(d),
            Asker::Remote(c) => {
                let id = self.id;
                tokio::spawn(async move {
                    // Answered elsewhere already, or the turn ended: nothing to do.
                    let _ = c.call("approve", json!({ "id": id, "decision": d })).await;
                });
            }
        }
    }
}

/// How to reach a running turn.
#[derive(Clone)]
pub(crate) enum Control {
    Local(TurnControl),
    Remote(crate::remote::Steer),
}

impl Control {
    /// Text for the model at its next step.
    pub(crate) fn steer(&self, text: &str) {
        match self {
            Control::Local(c) => c.steer.push(text),
            Control::Remote(s) => s.steer(text),
        }
    }
    pub(crate) fn cancel(&self) {
        match self {
            Control::Local(c) => c.cancel.cancel(),
            Control::Remote(s) => s.cancel(),
        }
    }
}

/// How a turn ended, as the views and `--json` show it.
pub(crate) struct Ended {
    pub error: Option<String>,
    /// What to do next after an error.
    pub hint: String,
    pub cancelled: bool,
    /// `3 tools · 25k tokens · 15.0s`.
    pub footer: Vec<String>,
    /// Messages sent mid-turn that arrived too late; the shell sends them next.
    pub undelivered: Vec<String>,
    /// The `--json` result object.
    pub result: Value,
}

pub(crate) const RETRY: &str =
    "The session is intact. Try again, /model to switch, or /trace for details.";

impl Ended {
    pub(crate) fn failed(error: impl std::fmt::Display, hint: &'static str) -> Ended {
        let error = format!("{error:#}");
        Ended {
            result: json!({ "ok": false, "error": error }),
            error: Some(error),
            hint: hint.into(),
            cancelled: false,
            footer: Vec::new(),
            undelivered: Vec::new(),
        }
    }

    /// The process exit code of a one-shot run.
    pub(crate) fn code(&self) -> u8 {
        match (&self.error, self.cancelled) {
            (Some(_), _) => 1,
            (None, true) => 130,
            (None, false) => 0,
        }
    }

    fn local(ix: &Interaction, end: TurnEnd) -> Ended {
        let mut result = match &end.result {
            Ok(o) => json!({
                "ok": true,
                "answer": o.answer,
                "cancelled": o.cancelled,
                "grounded": o.grounded(),
                "cited": o.cited.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                "steps": o.steps,
                "metrics": o.metrics,
                "findings": o.state.findings.iter().map(|f| &f.text).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        };
        let out_of_credit = end.out_of_credit();
        if let Some(o) = &out_of_credit {
            result["out_of_credit"] = json!(o);
        }
        result["session"] = json!(ix.session().id().as_str());
        result["project"] = json!(ix.project().name);
        result["context"] = serde_json::to_value(ix.last_context()).unwrap_or_default();
        Ended {
            error: end.result.as_ref().err().map(|e| e.to_string()),
            hint: out_of_credit.map_or_else(|| RETRY.into(), |o| o.hint),
            cancelled: end.result.as_ref().is_ok_and(|o| o.cancelled),
            footer: end.footer,
            undelivered: end.undelivered,
            result,
        }
    }
}

/// A running turn: what it reports, how to reach it, and its end.
pub(crate) struct Turn<'a> {
    pub updates: mpsc::UnboundedReceiver<Update>,
    pub control: Control,
    pub end: Pin<Box<dyn Future<Output = Ended> + 'a>>,
}

pub(crate) struct Local {
    pub ix: Interaction,
    pub approvals: mpsc::UnboundedReceiver<PendingApproval>,
}

pub(crate) enum Chat {
    Local(Box<Local>),
    Remote(Remote),
}

impl Chat {
    pub(crate) async fn summary(&self) -> anyhow::Result<Summary> {
        match self {
            Chat::Local(l) => Ok(l.ix.summary()),
            Chat::Remote(r) => r.summary().await,
        }
    }

    /// Where the chat left off, when it has turns.
    pub(crate) async fn recap(&self) -> Option<Report> {
        match self {
            Chat::Local(l) => (l.ix.session().turn_count() > 0).then(|| l.ix.recap()),
            Chat::Remote(r) => r.recap().await,
        }
    }

    pub(crate) fn commands(&self) -> Vec<CommandInfo> {
        match self {
            Chat::Local(l) => l.ix.commands(),
            Chat::Remote(r) => r.commands(),
        }
    }

    /// Run a slash command. A command that hands text to the agent returns `Submit`; run that
    /// with [`Chat::turn`].
    pub(crate) async fn execute(&mut self, line: &str) -> Reply {
        match self {
            Chat::Local(l) => l.ix.execute(line).await,
            Chat::Remote(r) => r.execute(line).await,
        }
    }

    pub(crate) fn detail(&self) -> bool {
        match self {
            Chat::Local(l) => l.ix.detail(),
            Chat::Remote(r) => r.detail(),
        }
    }

    /// Run one turn. `ask`: put state-changing actions to the person; off for runs where nobody
    /// can answer (`-p` without `--yes`), which then refuse them.
    pub(crate) fn turn(&mut self, input: &str, ask: bool) -> Turn<'_> {
        match self {
            Chat::Local(l) => l.turn(input, ask),
            Chat::Remote(r) => r.turn(input, ask),
        }
    }

    /// The person leaves: agents' work under way here reaches a safe point first, and continues
    /// the next time the app opens (ADR 0038). Nothing to wait for on a remote runtime.
    pub(crate) async fn leave(&self) {
        if let Chat::Local(l) = self {
            let engine = l.ix.ark().engine();
            engine.stop_soon();
            let grace = ark_sdk::prelude::ark_runtime::cancel::stop_grace();
            let _ = tokio::time::timeout(grace, engine.runs_ended()).await;
        }
    }

    /// The line the shell prints when the person leaves.
    pub(crate) fn farewell(&self, bin: &str) -> Option<String> {
        match self {
            Chat::Local(l) => {
                let s = l.ix.session();
                (s.turn_count() > 0).then(|| {
                    format!(
                        "Session {} saved ({} turns). Continue with `{bin} --resume`.",
                        s.id().short(),
                        s.turn_count()
                    )
                })
            }
            Chat::Remote(r) => Some(format!(
                "Left ARK at {}. Its chats and agents keep running there.",
                r.url()
            )),
        }
    }
}

impl Local {
    fn turn(&mut self, input: &str, ask: bool) -> Turn<'_> {
        let ctl = TurnControl {
            ask,
            ..TurnControl::new()
        };
        let (tx, updates) = mpsc::unbounded_channel();
        let control = Control::Local(ctl.clone());
        let input = input.to_string();
        let Local { ix, approvals } = self;
        let end = Box::pin(async move {
            let labels = ix.labels().clone();
            let events = tx.clone();
            let end = {
                let run = ix.run_turn(&input, &ctl, move |ev, a| {
                    let _ = events.send(Update::Event(ev.clone(), a.cloned()));
                });
                // Questions come on their own channel while the turn runs.
                let asks = async {
                    while let Some(p) = approvals.recv().await {
                        let line = labels.approval_line(&p.request);
                        let _ = tx.send(Update::Ask(Question::local(p, line)));
                    }
                    std::future::pending::<()>().await
                };
                tokio::select! {
                    end = run => end,
                    _ = asks => unreachable!("asks never ends"),
                }
            };
            Ended::local(ix, end)
        });
        Turn {
            updates,
            control,
            end,
        }
    }
}
