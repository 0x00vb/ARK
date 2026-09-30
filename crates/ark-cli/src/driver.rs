//! Runs one interactive turn. The agent works in a future; this loop multiplexes four inputs
//! around it without blocking any of them:
//!
//! ```text
//!   runtime events ─┐
//!   approval asks  ─┼─► select! ─► TurnView ─► terminal
//!   key presses    ─┤              (Ctrl+C/Esc → CancelToken)
//!   spinner tick   ─┘
//! ```
//!
//! The runtime never sees the terminal: it gets a `CancelToken`, an `ApprovalGate`, and a
//! callback that forwards events into a channel.

use std::collections::HashMap;
use std::time::Duration;

use ark_sdk::prelude::*;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::app::ShellState;
use crate::approval::{Menu, MenuKey};
use crate::term::RawMode;
use crate::view::TurnView;

/// Text typed while the agent was working.
pub(crate) struct TypeAhead {
    pub text: String,
    /// The user pressed Enter: send it as the next turn.
    pub submit: bool,
}

pub(crate) async fn run_turn(st: &mut ShellState, input: &str) -> Option<TypeAhead> {
    let cfg = st.cfg.clone();
    let names: HashMap<String, String> = st
        .ark
        .engine()
        .store
        .list_artifacts(&st.project.id)
        .map(|v| {
            v.into_iter()
                .map(|a| (a.id.as_str().to_string(), a.name))
                .collect()
        })
        .unwrap_or_default();

    let mut view = TurnView::new(
        &cfg.theme,
        &cfg.activity,
        cfg.event_hook.as_ref(),
        st.verbose,
    )
    .with_artifacts(names);
    view.live = true;

    let cancel = CancelToken::new();
    let opts = TurnOptions {
        cancel: Some(cancel.clone()),
        approvals: Some(st.gate.clone()),
        focus: None,
    };
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<HarnessEvent>();

    let raw = RawMode::enable(true);
    let (record, last_context, typeahead) = {
        let ShellState {
            ark,
            session,
            out,
            approvals,
            ..
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
        let mut typeahead = TypeAhead {
            text: String::new(),
            submit: false,
        };

        let turn = ark.ask_session(session, input, opts, move |ev| {
            let _ = ev_tx.send(ev);
        });
        tokio::pin!(turn);

        let result = loop {
            tokio::select! {
                biased;
                Some(ev) = ev_rx.recv() => view.event(ev, out),
                Some(pending) = approvals.recv() => {
                    let line = cfg.activity.approval_line(&pending.request);
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
                                cancel.cancel();
                                view.cancelling = true;
                            }
                            MenuKey::Ignore => {}
                        }
                        continue;
                    }
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Char('c') if ctrl => { cancel.cancel(); view.cancelling = true; }
                        KeyCode::Esc => { cancel.cancel(); view.cancelling = true; }
                        KeyCode::Enter if !typeahead.text.trim().is_empty() => typeahead.submit = true,
                        KeyCode::Backspace => { typeahead.text.pop(); typeahead.submit = false; }
                        KeyCode::Char(c) if !ctrl => { typeahead.text.push(c); typeahead.submit = false; }
                        _ => {}
                    }
                    view.typeahead = typeahead.text.clone();
                    view.tick(out);
                }
                _ = tick.tick() => {
                    out.refresh_width();
                    view.tick(out);
                }
                res = &mut turn => break res,
            }
        };
        while let Ok(ev) = ev_rx.try_recv() {
            view.event(ev, out);
        }
        if let Some(mut m) = menu.take() {
            m.answer(ApprovalDecision::Deny);
        }
        let last_context = view.last_context.take();
        let record = view.finish(out, input, &result);
        drop(keys);
        out.set_raw(false);
        (record, last_context, typeahead)
    };
    drop(raw);

    st.last_turn = Some(record);
    if last_context.is_some() {
        st.last_context = last_context;
    }
    if typeahead.text.trim().is_empty() {
        None
    } else {
        Some(typeahead)
    }
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
