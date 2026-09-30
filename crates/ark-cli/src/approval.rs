//! The terminal side of ARK's approval gate. The runtime's [`ApprovalGate`] decides *when* to
//! ask and remembers grants; this module only carries the question to the screen and the
//! answer back. [`TerminalApprover`] forwards each request over a channel to the turn loop,
//! which draws a [`Menu`] and replies when the human picks.
//!
//! [`ApprovalGate`]: ark_sdk::prelude::ApprovalGate

use ark_sdk::prelude::{ApprovalDecision, ApprovalRequest, Approver};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::{mpsc, oneshot};

use crate::theme::Theme;

/// A request waiting for a human.
pub(crate) struct Pending {
    pub request: ApprovalRequest,
    pub reply: oneshot::Sender<ApprovalDecision>,
}

/// An [`Approver`] that asks through the interactive shell.
pub(crate) struct TerminalApprover {
    tx: mpsc::UnboundedSender<Pending>,
}

impl TerminalApprover {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<Pending>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (TerminalApprover { tx }, rx)
    }
}

#[async_trait::async_trait]
impl Approver for TerminalApprover {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let (reply, answer) = oneshot::channel();
        if self
            .tx
            .send(Pending {
                request: request.clone(),
                reply,
            })
            .is_err()
        {
            return ApprovalDecision::Deny;
        }
        // A dropped reply (turn cancelled, shell gone) is a "no".
        answer.await.unwrap_or(ApprovalDecision::Deny)
    }
}

/// What a key press did to the menu.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Redraw,
    Ignore,
    Decide(ApprovalDecision),
    /// Deny and stop the whole turn (Ctrl+C).
    DenyAndCancel,
}

const CHOICES: [ApprovalDecision; 3] = [
    ApprovalDecision::AllowOnce,
    ApprovalDecision::AllowSession,
    ApprovalDecision::Deny,
];

/// The approval prompt: three choices, arrow keys or a shortcut to pick.
pub(crate) struct Menu {
    pub pending: Option<Pending>,
    pub selected: usize,
    pub lines: usize,
    call_line: String,
}

impl Menu {
    pub(crate) fn new(pending: Pending, call_line: String) -> Self {
        Menu {
            pending: Some(pending),
            selected: 0,
            lines: 0,
            call_line,
        }
    }

    pub(crate) fn key(&mut self, k: KeyEvent) -> MenuKey {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => MenuKey::DenyAndCancel,
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + CHOICES.len() - 1) % CHOICES.len();
                MenuKey::Redraw
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % CHOICES.len();
                MenuKey::Redraw
            }
            KeyCode::Char('k') => self.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            KeyCode::Char('j') => self.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            KeyCode::Enter => MenuKey::Decide(CHOICES[self.selected]),
            KeyCode::Char('1') | KeyCode::Char('y') => MenuKey::Decide(ApprovalDecision::AllowOnce),
            KeyCode::Char('2') | KeyCode::Char('a') => {
                MenuKey::Decide(ApprovalDecision::AllowSession)
            }
            KeyCode::Char('3') | KeyCode::Char('n') | KeyCode::Esc => {
                MenuKey::Decide(ApprovalDecision::Deny)
            }
            _ => MenuKey::Ignore,
        }
    }

    /// Send the decision back to the gate.
    pub(crate) fn answer(&mut self, d: ApprovalDecision) {
        if let Some(p) = self.pending.take() {
            let _ = p.reply.send(d);
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
        let class_hint = if req.class == req.action {
            format!("`{}`", req.action)
        } else {
            format!("`{}`", req.class)
        };
        let labels = [
            "Allow once".to_string(),
            format!(
                "Allow for this session {}",
                theme.dim(&format!("(don't ask again for {class_hint})"))
            ),
            "Deny".to_string(),
        ];
        let mut lines = vec![
            String::new(),
            format!(
                "{} {}",
                theme.yellow(&theme.glyphs.warn),
                theme.bold("The agent wants to run:")
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
    use ark_sdk::prelude::ark_core::RiskTier;

    fn menu() -> (Menu, oneshot::Receiver<ApprovalDecision>) {
        let (reply, rx) = oneshot::channel();
        let request = ApprovalRequest {
            action: "manuscript.save".into(),
            summary: "write text to a chapter".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({ "file": "ch4.md" }),
            class: "manuscript.save".into(),
            preview: "manuscript.save(file=\"ch4.md\")".into(),
        };
        (
            Menu::new(
                Pending { request, reply },
                "manuscript.save(file=\"ch4.md\")".into(),
            ),
            rx,
        )
    }

    #[test]
    fn arrows_move_and_enter_confirms() {
        let (mut m, mut rx) = menu();
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            MenuKey::Redraw
        );
        let MenuKey::Decide(d) = m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) else {
            panic!()
        };
        assert_eq!(d, ApprovalDecision::AllowSession);
        m.answer(d);
        assert_eq!(rx.try_recv().unwrap(), ApprovalDecision::AllowSession);
    }

    #[test]
    fn shortcuts_and_escape() {
        let (mut m, _rx) = menu();
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

    #[test]
    fn render_shows_call_and_choices() {
        let (m, _rx) = menu();
        let theme = Theme::default().with_color(false);
        let text = m.render(&theme).join("\n");
        assert!(text.contains("The agent wants to run:"));
        assert!(text.contains("manuscript.save(file=\"ch4.md\")"));
        assert!(text.contains("› 1. Allow once"));
        assert!(text.contains("Allow for this session"));
        assert!(text.contains("3. Deny"));
    }

    #[tokio::test]
    async fn dropped_reply_is_a_deny() {
        let (approver, mut rx) = TerminalApprover::channel();
        let req = menu().0.pending.unwrap().request;
        let task = tokio::spawn(async move { approver.decide(&req).await });
        let pending = rx.recv().await.unwrap();
        drop(pending);
        assert_eq!(task.await.unwrap(), ApprovalDecision::Deny);
    }
}
