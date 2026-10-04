//! Asking for a capability mid-task. The connections are the runtime's acquisition source
//! ([`AcquisitionProvider`]): a turn's first message names what is one sign-in away, the agent
//! calls `acquire_capability`, the approval gate asks the person (a front-end turns the yes
//! into the sign-in), and once connected the capability's tools join the running turn.

use ark_runtime::{AcquisitionProvider, AcquisitionResult};

use crate::catalog::Capability;
use crate::Connections;

impl Connections {
    /// The capability a free-text need names: its id or label, or a word of its line.
    pub fn capability_for(&self, need: &str) -> Option<&Capability> {
        let cat = self.catalog();
        if let Some(c) = cat.capability(need) {
            return Some(c);
        }
        let need = need.to_lowercase();
        cat.capabilities
            .iter()
            .filter(|c| c.provider.is_some())
            .find(|c| {
                need.contains(&c.id)
                    || need.contains(&c.label.to_lowercase())
                    || c.tools.iter().any(|t| need.contains(t.as_str()))
            })
    }
}

#[async_trait::async_trait]
impl AcquisitionProvider for Connections {
    fn active(&self) -> bool {
        !self.connectable().is_empty()
    }

    fn offer(&self) -> Option<String> {
        let open = self.connectable();
        if open.is_empty() {
            return None;
        }
        let mut s = String::from(
            "Not connected yet. When the task needs one, call acquire_capability with its name; the person connects it in a click:",
        );
        for c in open {
            s.push_str(&format!("\n- {}: {}", c.id, c.says));
        }
        Some(s)
    }

    async fn find(&self, need: &str) -> String {
        match self.capability_for(need) {
            Some(c) if self.account_for(c).is_some() => format!(
                "{} is ready: its tools are {}.*",
                c.label,
                c.tools.join(".*, ")
            ),
            Some(c) => format!(
                "{} ({}) is not connected. acquire_capability with need \"{}\" asks the person to connect it.",
                c.label, c.says, c.id
            ),
            None => format!("no capability matches `{need}`."),
        }
    }

    async fn propose(&self, need: &str) -> String {
        match self.capability_for(need) {
            Some(c) => format!(
                "Connecting {} opens the sign-in page of {} for the person. Nothing changes until they allow it.",
                c.label,
                c.provider.as_deref().unwrap_or("its provider")
            ),
            None => format!("no capability matches `{need}`."),
        }
    }

    async fn acquire(&self, need: &str, approved: bool) -> AcquisitionResult {
        let Some(c) = self.capability_for(need).cloned() else {
            return AcquisitionResult::info(format!("no capability matches `{need}`."));
        };
        if let Some(tools) = self.tools_of(&c) {
            let names: Vec<String> = tools.actions().into_iter().map(|d| d.name).collect();
            let who = self.account_for(&c).map(|a| a.name).unwrap_or_default();
            return AcquisitionResult::changed(format!(
                "{} is connected ({who}). Its tools are ready to call: {}.",
                c.label,
                names.join(", ")
            ))
            .with_tools(vec![tools]);
        }
        if !approved {
            return AcquisitionResult::info(format!(
                "The person did not connect {}. Do what you can without it and say what is missing.",
                c.label
            ));
        }
        AcquisitionResult::info(format!(
            "{} is not connected: the sign-in was not finished. Do what you can without it and say what is missing.",
            c.label
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, MemorySecrets};
    use std::sync::Arc;

    fn conns(client: &str) -> (tempfile::TempDir, Connections) {
        let mut c = Catalog::builtin();
        for p in &mut c.providers {
            p.oauth.client_id = client.into();
            p.oauth.client_id_env = None;
        }
        let dir = tempfile::tempdir().unwrap();
        let conns = Connections::open(dir.path(), c, Arc::new(MemorySecrets::default()));
        (dir, conns)
    }

    #[tokio::test]
    async fn what_can_be_added_is_offered_and_found_by_name_or_words() {
        let (_d, c) = conns("id");
        assert!(c.active());
        let offer = c.offer().unwrap();
        assert!(
            offer.contains("- mail: send email from your account"),
            "{offer}"
        );
        assert!(offer.contains("- calendar:"), "{offer}");
        assert_eq!(c.capability_for("Mail").unwrap().id, "mail");
        assert_eq!(c.capability_for("send an email to bob").unwrap().id, "mail");
        assert_eq!(c.capability_for("calendar.create").unwrap().id, "calendar");
        assert!(c.capability_for("a spreadsheet").is_none());
        assert!(c.find("mail").await.contains("not connected"));
        let r = c.acquire("mail", false).await;
        assert!(!r.changed && r.tools.is_empty() && r.summary.contains("did not connect"));
        let r = c.acquire("mail", true).await;
        assert!(!r.changed && r.summary.contains("sign-in was not finished"));
    }

    #[test]
    fn without_a_client_there_is_nothing_to_offer() {
        let (_d, c) = conns("");
        assert!(!c.active());
        assert!(c.offer().is_none());
    }
}
