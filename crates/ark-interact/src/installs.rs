//! Asking the person before a program is installed for a process (ADR 0041), in their words:
//! "Install Node.js so ARK can run JavaScript projects", with the exact command. A start whose
//! program is missing asks the install first, then the start once.

use std::sync::Arc;

use ark_sdk::prelude::ark_proc::install::Plan;
use ark_sdk::prelude::ark_proc::{Installs, START};
use ark_sdk::prelude::AcquisitionProvider;
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

impl Installer {
    /// The person's answer to installing `plan` with `command`, asked as `request`.
    async fn ask_install(&self, request: &ApprovalRequest, plan: &Plan, command: &str) -> bool {
        let mut ask = request.clone();
        ask.summary = format!(
            "Install {} so ARK can {}. It runs this on your computer, and you can watch it in Processes:\n  {command}",
            plan.tool.label, plan.tool.says
        );
        ask.preview = format!("Install {}", plan.tool.label);
        self.inner.decide(&ask).await != ApprovalDecision::Deny
    }

    /// A start whose program is missing and installable asks Install first, installs it, then
    /// asks the start once. Without this the start is asked, refused, and asked again after
    /// the install.
    async fn start(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let command = request.input["command"].as_str().unwrap_or("");
        let Some(plan) = self.installs.plan_for_command(command).await else {
            return self.inner.decide(request).await;
        };
        let Some(install) = plan.command.clone().filter(|_| !plan.installed) else {
            return self.inner.decide(request).await;
        };
        // The same question an agent's request to install asks: Install or Not now.
        let mut as_install = request.clone();
        as_install.action = "acquire_capability".into();
        as_install.input = serde_json::json!({ "need": plan.tool.id });
        as_install.class = "acquire_capability".into();
        as_install.ask_each_time = true;
        if !self.ask_install(&as_install, &plan, &install).await {
            return ApprovalDecision::Deny;
        }
        self.installs.acquire(&plan.tool.id, true).await;
        match self.installs.plan(&plan.tool.id).await {
            Some(now) if now.installed => self.inner.decide(request).await,
            // Still missing: the tool refuses the start (the program is not in the PATH) and
            // says so to the agent, so nothing runs that the person did not answer.
            _ => ApprovalDecision::AllowOnce,
        }
    }
}

#[async_trait::async_trait]
impl Approver for Installer {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        if request.action == START {
            return self.start(request).await;
        }
        if request.action != "acquire_capability" {
            return self.inner.decide(request).await;
        }
        let need = request.input["need"].as_str().unwrap_or("");
        let Some(plan) = self.installs.plan(need).await else {
            return self.inner.decide(request).await;
        };
        // Nothing to ask: it is there, or there is no way to install it (the agent is told).
        let Some(command) = plan.command.clone().filter(|_| !plan.installed) else {
            return ApprovalDecision::AllowOnce;
        };
        if self.ask_install(request, &plan, &command).await {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::Deny
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    use ark_sdk::prelude::ark_proc::{Processes, Shell};
    use ark_sdk::prelude::RiskTier;

    use super::*;

    /// The person: says `answer` to every question and remembers each question's line.
    struct Person {
        answer: ApprovalDecision,
        asked: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Approver for Person {
        async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
            self.asked.lock().unwrap().push(request.preview.clone());
            if request.preview.starts_with("Install") {
                assert_eq!(request.action, "acquire_capability", "asked as an install");
            }
            self.answer
        }
    }

    fn start(command: &str) -> ApprovalRequest {
        ApprovalRequest {
            action: "process.start".into(),
            summary: "start a project process".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({ "command": command }),
            class: format!("process.start:{command}"),
            preview: format!("Start {command}"),
            agent: None,
            task: None,
            ask_each_time: false,
            cut_short: false,
        }
    }

    /// A Bun project on a computer without Bun: Install is asked first, then Start once.
    #[tokio::test]
    async fn a_missing_program_is_installed_before_the_start_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // `brew install` here puts a `bun` beside it.
        let brew = bin.join("brew");
        std::fs::write(
            &brew,
            format!(
                "#!/bin/sh\nprintf '#!/bin/sh\\n' > '{0}/bun'\n/bin/chmod +x '{0}/bun'\n",
                bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&brew, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let shell = Shell::fixed("/bin/sh", vec![("PATH".into(), bin.clone().into())]);
        let processes = Processes::with_shell(dir.path().join("processes"), shell).unwrap();
        let ask = |answer| {
            let person = Arc::new(Person {
                answer,
                asked: Mutex::default(),
            });
            (
                person.clone(),
                Installer::new(person, Installs::new(processes.clone())),
            )
        };

        let (person, installer) = ask(ApprovalDecision::Deny);
        let said = installer.decide(&start("bun run dev")).await;
        assert_eq!(said, ApprovalDecision::Deny);
        assert_eq!(*person.asked.lock().unwrap(), ["Install Bun"]);
        assert!(!bin.join("bun").exists());

        let (person, installer) = ask(ApprovalDecision::AllowOnce);
        let said = installer.decide(&start("bun run dev")).await;
        assert_eq!(said, ApprovalDecision::AllowOnce);
        assert_eq!(
            *person.asked.lock().unwrap(),
            ["Install Bun", "Start bun run dev"]
        );
        assert!(bin.join("bun").exists());

        // Installed now: only the start is asked.
        let (person, installer) = ask(ApprovalDecision::AllowOnce);
        installer.decide(&start("bun run dev")).await;
        assert_eq!(*person.asked.lock().unwrap(), ["Start bun run dev"]);
    }
}
