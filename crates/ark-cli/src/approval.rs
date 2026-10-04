//! The terminal side of an approval. The runtime's [`ApprovalGate`] decides *when* to ask and
//! remembers grants; `ark-interact`'s channel approver carries the question here. This module
//! only draws a [`Menu`] and returns the person's pick.
//!
//! [`ApprovalGate`]: ark_sdk::prelude::ApprovalGate

use ark_interact::PendingApproval;
use ark_sdk::prelude::{ApprovalDecision, ApprovalRequest};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::theme::Theme;

/// What a key press did to the menu.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Redraw,
    Ignore,
    Decide(ApprovalDecision),
    /// Deny and stop the whole turn (Ctrl+C).
    DenyAndCancel,
}

/// The approval prompt: the choices `ark_interact::approval_choices` gives (usually allow
/// once, allow for the session, deny), arrow keys or a shortcut to pick.
pub(crate) struct Menu {
    pub pending: Option<PendingApproval>,
    pub selected: usize,
    pub lines: usize,
    call_line: String,
}

impl Menu {
    pub(crate) fn new(pending: PendingApproval, call_line: String) -> Self {
        Menu {
            pending: Some(pending),
            selected: 0,
            lines: 0,
            call_line,
        }
    }

    fn choices(&self) -> Vec<ark_interact::Choice> {
        self.request()
            .map(ark_interact::approval_choices)
            .unwrap_or_default()
    }

    pub(crate) fn key(&mut self, k: KeyEvent) -> MenuKey {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let choices = self.choices();
        let n = choices.len().max(1);
        let offered = |d: ApprovalDecision| {
            if choices.iter().any(|c| c.decision == d) {
                MenuKey::Decide(d)
            } else {
                MenuKey::Ignore
            }
        };
        match k.code {
            KeyCode::Char('c') if ctrl => MenuKey::DenyAndCancel,
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + n - 1) % n;
                MenuKey::Redraw
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % n;
                MenuKey::Redraw
            }
            KeyCode::Char('k') => self.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            KeyCode::Char('j') => self.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            KeyCode::Enter => choices
                .get(self.selected)
                .map_or(MenuKey::Ignore, |c| MenuKey::Decide(c.decision)),
            KeyCode::Char(d @ '1'..='9') => choices
                .get(d as usize - '1' as usize)
                .map_or(MenuKey::Ignore, |c| MenuKey::Decide(c.decision)),
            KeyCode::Char('y') => offered(ApprovalDecision::AllowOnce),
            KeyCode::Char('a') => offered(ApprovalDecision::AllowSession),
            KeyCode::Char('n') | KeyCode::Esc => MenuKey::Decide(ApprovalDecision::Deny),
            _ => MenuKey::Ignore,
        }
    }

    /// Send the decision back to the gate.
    pub(crate) fn answer(&mut self, d: ApprovalDecision) {
        if let Some(p) = self.pending.take() {
            p.answer(d);
        }
    }

    pub(crate) fn request(&self) -> Option<&ApprovalRequest> {
        self.pending.as_ref().map(|p| &p.request)
    }

    /// The menu as lines, ready to print.
    pub(crate) fn render(&self, theme: &Theme) -> Vec<String> {
        let Some(req) = self.request() else {
            return Vec::new();
        };
        let labels: Vec<String> = ark_interact::approval_choices(req)
            .into_iter()
            .map(|c| match c.hint {
                Some(h) => format!("{} {}", c.label, theme.dim(&format!("({h})"))),
                None => c.label,
            })
            .collect();
        let mut lines = vec![
            String::new(),
            format!(
                "{} {}",
                theme.yellow(&theme.glyphs.warn),
                theme.bold(&ark_interact::approval_title(req))
            ),
            String::new(),
            format!("    {}", theme.accented(&self.call_line)),
            format!("    {}", theme.dim(&req.summary)),
            String::new(),
        ];
        for (i, label) in labels.iter().enumerate() {
            let marker = if i == self.selected {
                theme.accented("›")
            } else {
                " ".into()
            };
            let text = if i == self.selected {
                theme.bold(label)
            } else {
                label.clone()
            };
            lines.push(format!(
                "  {marker} {} {text}",
                theme.dim(&format!("{}.", i + 1))
            ));
        }
        lines.push(theme.dim("    ↑/↓ to choose · enter to confirm · esc to deny"));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::ChannelApprover;
    use ark_sdk::prelude::ark_core::RiskTier;
    use ark_sdk::prelude::Approver;
    use std::sync::Arc;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            action: "manuscript.save".into(),
            summary: "write text to a chapter".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({ "file": "ch4.md" }),
            class: "manuscript.save".into(),
            preview: "manuscript.save(file=\"ch4.md\")".into(),
            agent: None,
            task: None,
        }
    }

    /// A menu wired to a real approver, and the task waiting for its answer.
    async fn menu() -> (Menu, tokio::task::JoinHandle<ApprovalDecision>) {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = Arc::new(approver);
        let task = tokio::spawn(async move { approver.decide(&request()).await });
        let pending = rx.recv().await.unwrap();
        (
            Menu::new(pending, "manuscript.save(file=\"ch4.md\")".into()),
            task,
        )
    }

    #[tokio::test]
    async fn arrows_move_and_enter_confirms() {
        let (mut m, task) = menu().await;
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            MenuKey::Redraw
        );
        let MenuKey::Decide(d) = m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) else {
            panic!()
        };
        assert_eq!(d, ApprovalDecision::AllowSession);
        m.answer(d);
        assert_eq!(task.await.unwrap(), ApprovalDecision::AllowSession);
    }

    #[tokio::test]
    async fn shortcuts_and_escape() {
        let (mut m, _task) = menu().await;
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            MenuKey::Decide(ApprovalDecision::AllowOnce)
        );
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            MenuKey::Decide(ApprovalDecision::Deny)
        );
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            MenuKey::DenyAndCancel
        );
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            MenuKey::Redraw
        );
        assert_eq!(m.selected, 2, "up from the top wraps to Deny");
    }

    #[tokio::test]
    async fn a_request_to_connect_offers_connect_or_not_now() {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = Arc::new(approver);
        let mut req = request();
        req.action = "acquire_capability".into();
        req.class = "acquire_capability:mail".into();
        req.summary = "Connect Google so ARK can send email from your account.".into();
        let task = tokio::spawn(async move { approver.decide(&req).await });
        let mut m = Menu::new(rx.recv().await.unwrap(), "Connect Mail".into());
        let text = m.render(&Theme::default().with_color(false)).join("\n");
        assert!(text.contains("ARK needs this to finish:"), "{text}");
        assert!(
            text.contains("› 1. Connect") && text.contains("2. Not now"),
            "{text}"
        );
        assert!(!text.contains("session"), "{text}");
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            MenuKey::Ignore,
            "there is no session grant to give"
        );
        let MenuKey::Decide(d) = m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) else {
            panic!()
        };
        m.answer(d);
        assert_eq!(task.await.unwrap(), ApprovalDecision::AllowOnce);
    }

    #[tokio::test]
    async fn render_shows_call_and_choices() {
        let (m, _task) = menu().await;
        let theme = Theme::default().with_color(false);
        let text = m.render(&theme).join("\n");
        assert!(text.contains("The agent wants to run:"));
        assert!(text.contains("manuscript.save(file=\"ch4.md\")"));
        assert!(text.contains("› 1. Allow once"));
        assert!(text.contains("Allow for this session"));
        assert!(text.contains("3. Deny"));
    }
}
