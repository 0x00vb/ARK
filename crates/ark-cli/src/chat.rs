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

    /// The turn that showed it ended. A question an agent asked for its own task keeps waiting
    /// (its task keeps it; `/waiting` answers it): it is returned, to ask again in the next turn.
    /// Any other is refused.
    pub(crate) fn end_of_turn(self) -> Option<PendingApproval> {
        match self.to {
            Asker::Local(p) if self.request.agent.is_some() => Some(p),
            _ => {
                self.answer(ApprovalDecision::Deny);
                None
            }
        }
    }

    /// Answered or given up elsewhere while it waited to be shown.
    pub(crate) fn is_closed(&self) -> bool {
        match &self.to {
            Asker::Local(p) => p.is_closed(),
            Asker::Remote(_) => false,
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
            Err(_) => json!({ "ok": false, "error": end.error_line() }),
        };
        let out_of_credit = end.out_of_credit();
        if let Some(o) = &out_of_credit {
            result["out_of_credit"] = json!(o);
        }
        result["session"] = json!(ix.session().id().as_str());
        result["project"] = json!(ix.project().name);
        result["context"] = serde_json::to_value(ix.last_context()).unwrap_or_default();
        Ended {
            error: end.error_line(),
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
    /// Agents' questions still open when the last turn ended ([`Question::end_of_turn`]).
    pub held: Vec<PendingApproval>,
}

impl Drop for Local {
    /// The terminal closes: questions nobody answered stay open. Dropping one would answer
    /// "deny", and an agent's task would keep a refusal the person never gave; its task keeps
    /// the question for the next process (`/waiting`, a window).
    fn drop(&mut self) {
        leave_open(&mut self.held, &mut self.approvals);
    }
}

fn leave_open(
    held: &mut Vec<PendingApproval>,
    asked: &mut mpsc::UnboundedReceiver<PendingApproval>,
) {
    for p in held.drain(..) {
        keep_open(p);
    }
    while let Ok(p) = asked.try_recv() {
        keep_open(p);
    }
}

/// Leave a question unanswered for good: nothing ever sends its answer.
fn keep_open(p: PendingApproval) {
    std::mem::forget(p);
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

    /// How many items wait for the person in any project (`/waiting`); 0 when the app has no
    /// agents.
    pub(crate) async fn waiting(&self) -> usize {
        if !self.commands().iter().any(|c| c.name == "waiting") {
            return 0;
        }
        match self {
            Chat::Local(l) => ark_interact::attention::list(l.ix.engine(), l.ix.labels()).len(),
            Chat::Remote(r) => r.waiting().await,
        }
    }

    /// Keep agents' questions a turn left open, for the next turn.
    pub(crate) fn hold(&mut self, questions: Vec<PendingApproval>) {
        if let Chat::Local(l) = self {
            l.held.extend(questions);
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
        let Local {
            ix,
            approvals,
            held,
        } = self;
        let held = std::mem::take(held);
        let end = Box::pin(async move {
            let labels = ix.labels().clone();
            let events = tx.clone();
            let end = {
                let run = ix.run_turn(&input, &ctl, move |ev, a| {
                    let _ = events.send(Update::Event(ev.clone(), a.cloned()));
                });
                // Questions come on their own channel while the turn runs.
                let asks = async {
                    for p in held.into_iter().filter(|p| !p.is_closed()) {
                        let line = labels.approval_line(&p.request);
                        let _ = tx.send(Update::Ask(Question::local(p, line)));
                    }
                    while let Some(p) = approvals.recv().await {
                        // Asked between turns and answered meanwhile (`/waiting`, a window).
                        if p.is_closed() {
                            continue;
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::ChannelApprover;
    use std::sync::Arc;

    fn request(agent: Option<&str>) -> ApprovalRequest {
        ApprovalRequest {
            action: "notes.save".into(),
            summary: "save".into(),
            risk: RiskTier::Mutate,
            input: json!({}),
            class: "notes.save".into(),
            preview: "notes.save()".into(),
            agent: agent.map(str::to_string),
            task: agent.map(|_| "t-1".to_string()),
            ask_each_time: false,
            cut_short: false,
        }
    }

    /// The turn that showed an agent's question ends: the question stays open for the next
    /// turn. A chat's own question is refused with its turn.
    #[tokio::test]
    async fn an_agents_question_outlives_the_turn_that_showed_it() {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = Arc::new(approver);
        let a = approver.clone();
        let agent = tokio::spawn(async move { a.decide(&request(Some("scribe"))).await });
        let own = tokio::spawn(async move { approver.decide(&request(None)).await });
        let mut held = Vec::new();
        for _ in 0..2 {
            let p = rx.recv().await.unwrap();
            held.extend(Question::local(p, String::new()).end_of_turn());
        }
        assert_eq!(own.await.unwrap(), ApprovalDecision::Deny);
        assert_eq!(held.len(), 1);
        assert!(!agent.is_finished());
        held.pop().unwrap().answer(ApprovalDecision::AllowOnce);
        assert_eq!(agent.await.unwrap(), ApprovalDecision::AllowOnce);
    }

    /// The terminal closes with questions unanswered, held or not shown yet: none is refused.
    #[tokio::test]
    async fn closing_the_terminal_refuses_nothing() {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = Arc::new(approver);
        let a = approver.clone();
        let first = tokio::spawn(async move { a.decide(&request(Some("scribe"))).await });
        let mut held = vec![rx.recv().await.unwrap()];
        let second = tokio::spawn(async move { approver.decide(&request(Some("scribe"))).await });
        tokio::task::yield_now().await;
        leave_open(&mut held, &mut rx);
        drop(rx);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !first.is_finished() && !second.is_finished(),
            "nobody answered"
        );
    }
}
