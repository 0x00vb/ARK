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

use std::time::Duration;

use ark_sdk::prelude::*;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::app::ShellState;
use crate::approval::{Menu, MenuKey};
use crate::chat::Update;
use crate::term::RawMode;
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
    let mut queued: Option<String> = None;

    let mut turn = chat.turn(input, true);
    let ctl = turn.control.clone();

    let end = loop {
        tokio::select! {
            biased;
            Some(u) = turn.updates.recv() => match u {
                Update::Event(ev, a) => view.event(ev, a, out),
                Update::Ask(q) => {
                    let mut m = Menu::new(q);
                    out.clear_status();
                    let lines = m.render(&cfg.theme);
                    for l in &lines {
                        out.line(l);
                    }
                    m.lines = lines.len();
                    view.in_menu = true;
                    menu = Some(m);
                }
                // Answered in another window: take the menu away.
                Update::Closed(id) => {
                    if menu.as_ref().is_some_and(|m| m.id() == Some(id)) {
                        out.rewind(menu.take().map_or(0, |m| m.lines));
                        view.in_menu = false;
                    }
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
