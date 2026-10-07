//! Runs one interactive turn. The agent works in a future; this loop multiplexes four inputs
//! around it without blocking any of them:
//!
//! ```text
//!   turn updates   ─┐  (events, approval questions, questions answered elsewhere)
//!   key presses    ─┼─► select! ─► TurnView ─► terminal
//!   spinner tick   ─┤              (esc → cancel, enter → steer, tab → queue)
//!   the turn's end ─┘
//! ```
//!
//! The runtime never sees the terminal. The turn runs here or on a runtime elsewhere
//! ([`crate::chat`]); the loop is the same.

use std::collections::VecDeque;
use std::time::Duration;

use ark_sdk::prelude::*;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::app::ShellState;
use crate::approval::{Menu, MenuKey};
use crate::chat::{Question, Update};
use crate::term::{Out, RawMode};
use crate::theme::Theme;
use crate::view::TurnView;

/// What the person set up for after the turn.
#[derive(Default)]
pub(crate) struct Next {
    /// Messages to send, in order: steering that arrived too late, then the queued one.
    pub submit: Vec<String>,
    /// Text typed but not sent; goes back into the editor.
    pub draft: String,
}

pub(crate) async fn run_turn(st: &mut ShellState, input: &str) -> Next {
    let cfg = st.cfg.clone();
    let mut view = TurnView::new(&cfg.theme, cfg.event_hook.as_ref(), st.chat.detail());
    view.live = true;

    let raw = RawMode::enable(true);
    let ShellState { chat, out, .. } = &mut *st;
    out.set_raw(raw.is_on());
    out.blank();

    let mut keys = if raw.is_on() {
        Some(EventStream::new())
    } else {
        None
    };
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut menu: Option<Menu> = None;
    // Questions that came while another was shown: one menu at a time, the next when it goes.
    let mut asks: VecDeque<Question> = VecDeque::new();
    let mut queued: Option<String> = None;

    let mut turn = chat.turn(input, true);
    let ctl = turn.control.clone();

    let end = loop {
        tokio::select! {
            biased;
            Some(u) = turn.updates.recv() => match u {
                Update::Event(ev, a) => view.event(ev, a, out),
                Update::Ask(q) if menu.is_some() => asks.push_back(q),
                Update::Ask(q) => menu = Some(show(q, &cfg.theme, out, &mut view)),
                // Answered in another window: take the menu away.
                Update::Closed(id) => {
                    if menu.as_ref().is_some_and(|m| m.id() == Some(id)) {
                        out.rewind(menu.take().map_or(0, |m| m.lines));
                        view.in_menu = false;
                    }
                    asks.retain(|q| q.id != id);
                }
            },
            Some(key) = next_key(&mut keys) => {
                if let Some(m) = menu.as_mut() {
                    match m.key(key) {
                        MenuKey::Redraw => {
                            out.rewind(m.lines);
                            let lines = m.render(&cfg.theme);
                            for l in &lines {
                                out.line(l);
                            }
                            m.lines = lines.len();
                        }
                        MenuKey::Decide(d) => {
                            out.rewind(m.lines);
                            m.answer(d);
                            menu = None;
                            view.in_menu = false;
                        }
                        MenuKey::DenyAndCancel => {
                            out.rewind(m.lines);
                            m.answer(ApprovalDecision::Deny);
                            menu = None;
                            view.in_menu = false;
                            ctl.cancel();
                            view.cancelling = true;
                        }
                        MenuKey::Ignore => {}
                    }
                    if menu.is_none() {
                        menu = next_open(&mut asks).map(|q| show(q, &cfg.theme, out, &mut view));
                    }
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('c') if ctrl => { ctl.cancel(); view.cancelling = true; }
                    KeyCode::Esc => { ctl.cancel(); view.cancelling = true; }
                    KeyCode::Enter if !view.typeahead.trim().is_empty() => {
                        let text = std::mem::take(&mut view.typeahead);
                        view.note(out, &format!("› {} (reaches the agent at its next step)", text.trim()));
                        ctl.steer(text.trim());
                    }
                    KeyCode::Tab if !view.typeahead.trim().is_empty() => {
                        queued = Some(std::mem::take(&mut view.typeahead).trim().to_string());
                        view.queued = queued.clone();
                    }
                    KeyCode::Backspace => { view.typeahead.pop(); }
                    KeyCode::Char(c) if !ctrl => view.typeahead.push(c),
                    _ => {}
                }
                view.tick(out);
            }
            _ = tick.tick() => {
                out.refresh_width();
                view.tick(out);
                if menu.is_none() {
                    menu = next_open(&mut asks).map(|q| show(q, &cfg.theme, out, &mut view));
                }
            }
            end = &mut turn.end => break end,
        }
    };
    let mut held = Vec::new();
    while let Ok(u) = turn.updates.try_recv() {
        match u {
            Update::Event(ev, a) => view.event(ev, a, out),
            Update::Ask(q) => held.extend(q.end_of_turn()),
            Update::Closed(_) => {}
        }
    }
    if let Some(q) = menu.take().and_then(|mut m| m.question.take()) {
        held.extend(q.end_of_turn());
    }
    held.extend(asks.into_iter().filter_map(Question::end_of_turn));
    drop(turn);
    chat.hold(held);
    let draft = std::mem::take(&mut view.typeahead);
    view.finish(out, &end);
    drop(keys);
    out.set_raw(false);
    drop(raw);

    let mut submit = end.undelivered.clone();
    submit.extend(queued);
    Next { submit, draft }
}

/// Draw a question's menu.
fn show(q: Question, theme: &Theme, out: &mut Out, view: &mut TurnView) -> Menu {
    let mut m = Menu::new(q);
    out.clear_status();
    let lines = m.render(theme);
    for l in &lines {
        out.line(l);
    }
    m.lines = lines.len();
    view.in_menu = true;
    m
}

/// The next queued question nobody has answered yet.
fn next_open(asks: &mut VecDeque<Question>) -> Option<Question> {
    while let Some(q) = asks.pop_front() {
        if !q.is_closed() {
            return Some(q);
        }
    }
    None
}

/// The next key press, or pend forever when there is no terminal to read.
async fn next_key(keys: &mut Option<EventStream>) -> Option<crossterm::event::KeyEvent> {
    let Some(stream) = keys.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        match stream.next().await {
            Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => return Some(k),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::ChannelApprover;
    use std::sync::Arc;

    /// Two questions in one turn: the second waits for the first, and one answered elsewhere
    /// meanwhile is skipped, never shown and never refused.
    #[tokio::test]
    async fn questions_wait_their_turn_and_answered_ones_are_skipped() {
        let (approver, mut rx) = ChannelApprover::channel();
        let approver = Arc::new(approver);
        let req = |n: &str| ApprovalRequest {
            action: n.into(),
            summary: n.into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({}),
            class: n.into(),
            preview: n.into(),
            agent: Some("scribe".into()),
            task: Some("t-1".into()),
            ask_each_time: false,
        };
        let mut tasks = Vec::new();
        let mut asks = VecDeque::new();
        for n in ["a.save", "b.save", "c.save"] {
            let a = approver.clone();
            let r = req(n);
            tasks.push(tokio::spawn(async move { a.decide(&r).await }));
            asks.push_back(Question::local(rx.recv().await.unwrap(), n.into()));
        }
        // "b" is answered in another window: its asker stops waiting.
        tasks.remove(1).abort();
        tokio::task::yield_now().await;
        assert_eq!(next_open(&mut asks).unwrap().line, "a.save");
        assert_eq!(next_open(&mut asks).unwrap().line, "c.save");
        assert!(next_open(&mut asks).is_none());
    }
}
