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
    match (&req.agent, &req.task) {
        (Some(a), Some(t)) => format!("{a} (task {t}) wants to run:"),
        (Some(a), None) => format!("{a} wants to run:"),
        _ => "The agent wants to run:".into(),
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
