//! Runs one interactive turn. The agent works in a future; this loop multiplexes four inputs
//! around it without blocking any of them:
//!
//! ```text
//!   runtime events ─┐
//!   approval asks  ─┼─► select! ─► TurnView ─► terminal
//!   key presses    ─┤              (esc → CancelToken, enter → Steering, tab → queue)
//!   spinner tick   ─┘
//! ```
//!
//! The runtime never sees the terminal. `Interaction::run_turn` gets a [`TurnControl`] and a
//! callback that forwards events into a channel.

use std::time::Duration;

use ark_interact::{Activity, TurnControl};
use ark_sdk::prelude::*;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::app::ShellState;
use crate::approval::{Menu, MenuKey};
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
    let mut view = TurnView::new(&cfg.theme, cfg.event_hook.as_ref(), st.ix.detail());
    view.live = true;

    let ctl = TurnControl::new();
    let (ev_tx, mut ev_rx) =
        tokio::sync::mpsc::unbounded_channel::<(HarnessEvent, Option<Activity>)>();

    let raw = RawMode::enable(true);
    let ShellState {
        ix, out, approvals, ..
    } = &mut *st;
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
    let labels = ix.labels().clone();

    let turn = ix.run_turn(input, &ctl, move |ev, a| {
        let _ = ev_tx.send((ev.clone(), a.cloned()));
    });
    tokio::pin!(turn);

    let end = loop {
        tokio::select! {
            biased;
            Some((ev, a)) = ev_rx.recv() => view.event(ev, a, out),
            Some(pending) = approvals.recv() => {
                let line = labels.approval_line(&pending.request);
                let mut m = Menu::new(pending, line);
                out.clear_status();
                let lines = m.render(&cfg.theme);
                for l in &lines {
                    out.line(l);
                }
                m.lines = lines.len();
                view.in_menu = true;
                menu = Some(m);
            }
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
                            ctl.cancel.cancel();
                            view.cancelling = true;
                        }
                        MenuKey::Ignore => {}
                    }
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('c') if ctrl => { ctl.cancel.cancel(); view.cancelling = true; }
                    KeyCode::Esc => { ctl.cancel.cancel(); view.cancelling = true; }
                    KeyCode::Enter if !view.typeahead.trim().is_empty() => {
                        let text = std::mem::take(&mut view.typeahead);
                        view.note(out, &format!("› {} (reaches the agent at its next step)", text.trim()));
                        ctl.steer.push(text.trim());
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
            end = &mut turn => break end,
        }
    };
    while let Ok((ev, a)) = ev_rx.try_recv() {
        view.event(ev, a, out);
    }
    if let Some(m) = menu.take() {
        let mut m = m;
        m.answer(ApprovalDecision::Deny);
    }
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
