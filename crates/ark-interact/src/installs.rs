//! Asking the person before a program is installed for a process (ADR 0041), in their words:
//! "Install Node.js so ARK can run JavaScript projects", with the exact command.

use std::sync::Arc;

use ark_sdk::prelude::ark_proc::Installs;
use ark_sdk::prelude::{ApprovalDecision, ApprovalRequest, Approver};

/// Words an agent's `acquire_capability` for a program ARK installs; every other question
/// goes to the front-end's approver unchanged. The install itself runs once the answer is yes.
pub struct Installer {
    inner: Arc<dyn Approver>,
    installs: Installs,
}

impl Installer {
    pub fn new(inner: Arc<dyn Approver>, installs: Installs) -> Self {
        Installer { inner, installs }
    }
}

/// `approver`, with installs worded when the app offers processes.
pub(crate) fn wrap(approver: Arc<dyn Approver>, ark: &ark_sdk::Ark) -> Arc<dyn Approver> {
    match ark.processes() {
        Some(p) => Arc::new(Installer::new(approver, Installs::new(p.clone()))),
        None => approver,
    }
}

#[async_trait::async_trait]
impl Approver for Installer {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        if request.action != "acquire_capability" {
            return self.inner.decide(request).await;
        }
        let need = request.input["need"].as_str().unwrap_or("");
        let Some(plan) = self.installs.plan(need).await else {
            return self.inner.decide(request).await;
        };
        // Nothing to ask: it is there, or there is no way to install it (the agent is told).
        let Some(command) = plan.command.filter(|_| !plan.installed) else {
            return ApprovalDecision::AllowOnce;
        };
        let mut ask = request.clone();
        ask.summary = format!(
            "Install {} so ARK can {}. It runs this on your computer, and you can watch it in Processes:\n  {command}",
            plan.tool.label, plan.tool.says
        );
        ask.preview = format!("Install {}", plan.tool.label);
        match self.inner.decide(&ask).await {
            ApprovalDecision::Deny => ApprovalDecision::Deny,
            _ => ApprovalDecision::AllowOnce,
        }
    }
}
