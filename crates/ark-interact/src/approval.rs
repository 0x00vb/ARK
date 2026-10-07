//! Carrying approval questions from the runtime to a person and back. The runtime's
//! [`ApprovalGate`] decides *when* to ask and remembers grants; [`ChannelApprover`] only moves
//! each question to whichever front-end is listening. The terminal draws a menu, the GUI draws
//! a card; both answer through [`PendingApproval::answer`].
//!
//! [`ApprovalGate`]: ark_sdk::prelude::ApprovalGate

use ark_sdk::prelude::{ApprovalDecision, ApprovalRequest, Approver};
use tokio::sync::{mpsc, oneshot};

/// The question's headline, the same in every front-end: who asks, and for which task.
pub fn approval_title(req: &ApprovalRequest) -> String {
    if req.action == "acquire_capability" {
        return match &req.agent {
            Some(a) => format!("{a} needs this to finish:"),
            None => "ARK needs this to finish:".into(),
        };
    }
    match &req.agent {
        Some(a) => format!("{a} wants to:"),
        None => "ARK wants to:".into(),
    }
}

/// One answer a person can give, as both front-ends show it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Choice {
    pub label: String,
    pub decision: ApprovalDecision,
    /// A short note beside the label (what "for this session" covers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The answers to offer, in order; the first is the default. A request to connect a capability
/// gets "Connect" and "Not now": connecting is one sign-in, not a grant to remember.
pub fn approval_choices(req: &ApprovalRequest) -> Vec<Choice> {
    let choice = |label: &str, decision, hint: Option<String>| Choice {
        label: label.into(),
        decision,
        hint,
    };
    if req.action == "acquire_capability" {
        // A source words its question ("Connect Google", "Install Node.js"); its first word is
        // the yes. A question no source worded keeps "Connect".
        let yes = req
            .preview
            .split_whitespace()
            .next()
            .filter(|w| !w.contains('('))
            .unwrap_or("Connect");
        return vec![
            choice(yes, ApprovalDecision::AllowOnce, None),
            choice("Not now", ApprovalDecision::Deny, None),
        ];
    }
    // Nothing covers it ahead: each call is its own question.
    if req.ask_each_time {
        return vec![
            choice("Allow once", ApprovalDecision::AllowOnce, None),
            choice("Deny", ApprovalDecision::Deny, None),
        ];
    }
    vec![
        choice("Allow once", ApprovalDecision::AllowOnce, None),
        choice(
            "Allow for this session",
            ApprovalDecision::AllowSession,
            Some(grant_scope(req)),
        ),
        choice(
            "Always in this project",
            ApprovalDecision::AllowAlways,
            Some("until you remove it in /grants".into()),
        ),
        choice("Deny", ApprovalDecision::Deny, None),
    ]
}

/// What a "for this session" or "always" answer covers, in words: every Computer action in
/// one app, or the approval class.
pub fn grant_scope(req: &ApprovalRequest) -> String {
    match req.class.split_once(':') {
        Some(("computer", _)) => {
            let app = req.input.get("app").and_then(|v| v.as_str()).unwrap_or("");
            format!("don't ask again for anything in {}", app.trim())
        }
        Some(("process.start", _)) => {
            let s = |k: &str| {
                req.input
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let folder = s("folder")
                .trim_start_matches("./")
                .trim_matches('/')
                .to_string();
            let place = if folder.is_empty() {
                "the project folder".to_string()
            } else {
                folder
            };
            format!(
                "don't ask again for `{}` in {place}",
                s("command")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        _ => format!("don't ask again for `{}`", req.class),
    }
}

/// A question waiting for a person. Dropping it answers "deny".
pub struct PendingApproval {
    pub request: ApprovalRequest,
    reply: oneshot::Sender<ApprovalDecision>,
}

impl PendingApproval {
    pub fn answer(self, d: ApprovalDecision) {
        let _ = self.reply.send(d);
    }
    /// Resolves when the turn stops waiting for this answer.
    pub async fn closed(&mut self) {
        self.reply.closed().await
    }
}

/// An [`Approver`] that forwards every question over a channel.
pub struct ChannelApprover {
    tx: mpsc::UnboundedSender<PendingApproval>,
}

impl ChannelApprover {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<PendingApproval>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ChannelApprover { tx }, rx)
    }
}

#[async_trait::async_trait]
impl Approver for ChannelApprover {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let (reply, answer) = oneshot::channel();
        if self
            .tx
            .send(PendingApproval {
                request: request.clone(),
                reply,
            })
            .is_err()
        {
            return ApprovalDecision::Deny;
        }
        // A dropped question (turn cancelled, front-end gone) is a "no".
        answer.await.unwrap_or(ApprovalDecision::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_sdk::prelude::RiskTier;

    fn req() -> ApprovalRequest {
        ApprovalRequest {
            action: "notes.save".into(),
            summary: "save".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({}),
            class: "notes.save".into(),
            preview: "notes.save()".into(),
            agent: None,
            task: None,
            ask_each_time: false,
        }
    }

    #[tokio::test]
    async fn answers_travel_back_and_drops_deny() {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = std::sync::Arc::new(approver);
        let a2 = approver.clone();
        let task = tokio::spawn(async move { a2.decide(&req()).await });
        rx.recv()
            .await
            .unwrap()
            .answer(ApprovalDecision::AllowSession);
        assert_eq!(task.await.unwrap(), ApprovalDecision::AllowSession);

        let task = tokio::spawn(async move { approver.decide(&req()).await });
        drop(rx.recv().await.unwrap());
        assert_eq!(task.await.unwrap(), ApprovalDecision::Deny);
    }
}
