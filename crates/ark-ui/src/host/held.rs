//! Chats this runtime shows whose place is another computer (ADR 0052). The person's current
//! chat may be one of them: then the calls a window makes on its chat go to that computer
//! through the link, named by session (`chat_*`), and that computer's events for it come back
//! here, numbered into this runtime's own stream. A question from there gets the id
//! `<runtime id>:<id>`, so it never meets one of this runtime's.
//!
//! ```text
//! window ── send, steer, cancel, approve, model, transcript, trace, context ──► Host
//!   Host ── held chat shown? ── Links::chat(place, chat_*) ──► the computer's runtime
//!   the computer's events ── relay (one per place): its held chats only, ids rewritten ──► Feed
//! ```
//!
//! Each held chat has a record here (`ark_interact::held`), so it stays listed after a restart
//! and while its computer does not answer; then it reads only.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use ark_interact::held::{self, HeldChat};
use ark_interact::links::State;
use serde_json::{json, Value};

use super::Host;

/// What this runtime knows of a held chat while it runs: kept in memory only.
#[derive(Default)]
pub(super) struct HeldLive {
    /// The input of its running turn.
    running: Option<String>,
    /// What its computer last said `summary` is for it.
    summary: Value,
    /// Its running turn's events so far, as its computer sent them when it was read.
    live: Vec<Value>,
    /// Its open questions, by the id windows see (`<runtime>:<id>`).
    questions: Vec<Value>,
    /// How many files differ on its computer from what this computer has (after its last turn).
    pub(super) changes: usize,
}

/// How long a place's events may be silent (it pings every 15 s) before the stream counts as gone.
const IDLE: Duration = Duration::from_secs(40);

impl Host {
    /// The held chat windows show now, if any.
    pub(super) fn shown_held(&self) -> Option<HeldChat> {
        let id = self.held.lock().unwrap().clone()?;
        held::get(&self.engine, &id)
    }

    /// The held chat `session`, when it is one.
    fn held_chat(&self, session: &str) -> Option<HeldChat> {
        held::get(&self.engine, session)
    }

    /// The held chat a request is about: the one it names (`session` or `id`), else the one
    /// windows show.
    fn held_for(&self, params: &Value, key: &str) -> Option<HeldChat> {
        match params[key].as_str().filter(|s| !s.is_empty()) {
            Some(s) => self.held_chat(s),
            None => self.shown_held(),
        }
    }

    /// `{name, state}` of a held chat's place, as windows show it.
    fn place(&self, chat: &HeldChat) -> Value {
        let state = self.links.state(&chat.place);
        json!({
            "name": chat.place,
            "state": state.map(|s| json!(s)).unwrap_or(json!("gone")),
        })
    }

    /// Words for a held chat that cannot take a call now; `None` when it can.
    fn unreachable(&self, chat: &HeldChat) -> Option<String> {
        match self.links.state(&chat.place) {
            Some(State::Ok) => None,
            Some(State::Refused) => Some(format!(
                "{} signed this computer out: set it up again",
                chat.place
            )),
            Some(State::TooOld) => Some(format!(
                "{} runs an older version that cannot hold chats",
                chat.place
            )),
            Some(_) => Some(format!("{} is not answering", chat.place)),
            None => Some(format!("{} is not set up here", chat.place)),
        }
    }

    /// One call on a held chat's computer.
    async fn on_place(
        &self,
        chat: &HeldChat,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        if let Some(why) = self.unreachable(chat) {
            anyhow::bail!(why);
        }
        // The chat's model goes with every call: the computer opens a chat again on its own
        // current model otherwise.
        let mut params = params;
        if !chat.spec.is_empty() && params.get("model").is_none() {
            params["model"] = json!(chat.spec);
        }
        self.links.chat(&chat.place, method, params).await
    }

    /// What `summary` answers for a held chat: this runtime's summary of its project, with the
    /// chat's own model, session and grants from its computer, and its place.
    pub(super) fn held_summary(&self, chat: &HeldChat) -> Value {
        let mut v = self.summary();
        let live = self.held_live.lock().unwrap();
        let l = live.get(&chat.session);
        let theirs = l.map(|l| l.summary.clone()).unwrap_or(Value::Null);
        if let Ok(Some(p)) = self
            .engine
            .store
            .get_project(&ark_sdk::prelude::ProjectId(chat.project.clone()))
        {
            v["project"] = json!({
                "id": p.id, "name": p.name, "target": p.target, "current": true,
                "workspace": p.workspace,
            });
        }
        let project_name = v["project"]["name"].clone();
        let mut session = match &theirs["session"] {
            Value::Object(_) => theirs["session"].clone(),
            _ => json!({
                "id": chat.session,
                "short": chat.session.chars().take(8).collect::<String>(),
                "title": chat.title,
                "created_at": chat.created_at.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
                "updated_at": chat.updated_at.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
                "turns": chat.turns,
                "tokens": 0,
                "current": true,
            }),
        };
        session["project"] = json!(chat.project);
        session["project_name"] = project_name;
        v["session"] = session;
        if let Some(m) = theirs["model"].as_str() {
            v["model"] = json!(m);
        } else if !chat.model.is_empty() {
            v["model"] = json!(chat.model);
        }
        v["grants"] = theirs["grants"].clone();
        if v["grants"].is_null() {
            v["grants"] = json!([]);
        }
        v["running"] = json!(l.is_some_and(|l| l.running.is_some()));
        v["changes"] = json!(l.map(|l| l.changes).unwrap_or(0));
        v["place"] = self.place(chat);
        v["location"] = json!(crate::resource::ResourceLocation {
            runtime: chat.runtime.clone(),
            project: chat.remote_project.clone(),
            session: chat.session.clone(),
        });
        v
    }

    /// Tell windows the held chat they show changed.
    fn emit_held_summary(&self, chat: &HeldChat) {
        if self.held.lock().unwrap().as_deref() == Some(&chat.session) {
            self.emit(json!({ "kind": "summary", "summary": self.held_summary(chat) }));
        }
    }

    /// Windows leave a held chat for one of this runtime's. The chat this runtime shows
    /// underneath may have changed meanwhile: its views are read again.
    pub(super) async fn leave_held(&self) {
        if self.held.lock().unwrap().take().is_some() && !self.cur().is_running() {
            self.refresh().await;
        }
    }

    /// A new chat on the computer `place`, in the project windows show (`new_session` with a
    /// place). It becomes the chat windows show.
    pub(super) async fn held_new(
        self: &Arc<Self>,
        place: &str,
        model: &str,
        big: bool,
    ) -> anyhow::Result<Value> {
        let probe = HeldChat {
            place: place.to_string(),
            ..Default::default()
        };
        if let Some(why) = self.unreachable(&probe) {
            anyhow::bail!(why);
        }
        let project = self.project()?;
        // Its store id names its chats' questions and its copy: a computer that does not say it
        // runs an older version.
        let runtime = self.links.runtime_id(place).ok_or_else(|| {
            anyhow::anyhow!("{place} runs an older version that cannot hold chats")
        })?;
        self.ensure_relay(place).await;
        // The project goes first, so the chat finds it there.
        let sent = self.copy_send(place, &project, big).await?;
        if !sent["ask"].is_null() {
            return Ok(json!({ "ask": sent["ask"], "place": place }));
        }
        let v = self
            .links
            .chat(
                place,
                "chat_new",
                json!({
                    "origin": held::runtime_id(&self.engine),
                    "origin_project": project.id,
                    "name": project.name,
                    "model": model,
                }),
            )
            .await?;
        let session = v["session"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{place} did not start the chat"))?
            .to_string();
        let now = time::OffsetDateTime::now_utc();
        let chat = HeldChat {
            session: session.clone(),
            place: place.to_string(),
            runtime,
            project: project.id.as_str().to_string(),
            remote_project: v["project"]["id"].as_str().unwrap_or_default().to_string(),
            title: String::new(),
            model: v["summary"]["model"].as_str().unwrap_or(model).to_string(),
            spec: model.to_string(),
            created_at: Some(now),
            updated_at: Some(now),
            turns: 0,
        };
        held::keep(&self.engine, &chat)?;
        self.held_live.lock().unwrap().insert(
            session.clone(),
            HeldLive {
                summary: v["summary"].clone(),
                ..Default::default()
            },
        );
        *self.held.lock().unwrap() = Some(session);
        let mut summary = self.held_summary(&chat);
        // What the send did: files kept back because the computer changed them, or none sent
        // because work runs in the copy there.
        summary["copy"] = sent;
        self.emit(json!({ "kind": "summary", "summary": summary }));
        self.emit_working();
        Ok(summary)
    }

    /// Show the held chat `chat`: what its computer has of it now, or its record when that
    /// computer does not answer (then it reads only).
    pub(super) async fn held_open(self: &Arc<Self>, chat: HeldChat) -> anyhow::Result<Value> {
        // Windows show the chat's project: move this runtime's own chat there first.
        if self.project().ok().map(|p| p.id.as_str().to_string()) != Some(chat.project.clone()) {
            self.leave_held().await;
            let id = chat.project.clone();
            let _ = self
                .switch(super::Target::Change, move |ix| {
                    ix.open_project(&id).map(|_| ())
                })
                .await;
        }
        *self.held.lock().unwrap() = Some(chat.session.clone());
        self.ensure_relay(&chat.place).await;
        let _ = self.held_read(&chat).await;
        let summary = self.held_summary(&chat);
        self.emit(json!({ "kind": "summary", "summary": summary }));
        self.emit_working();
        Ok(summary)
    }

    /// Read a held chat's state from its computer into memory (and its record's title).
    async fn held_read(&self, chat: &HeldChat) -> anyhow::Result<Value> {
        let v = self
            .on_place(
                chat,
                "chat_read",
                json!({ "session": chat.session, "limit": 0 }),
            )
            .await?;
        let runtime = self.runtime_of(chat);
        {
            let mut live = self.held_live.lock().unwrap();
            let l = live.entry(chat.session.clone()).or_default();
            l.summary = v["summary"].clone();
            l.running = v["running"].as_str().map(str::to_string);
            l.live = v["live"].as_array().cloned().unwrap_or_default();
            l.questions = v["approvals"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|q| self.held_question(chat, &runtime, q))
                .collect();
        }
        let title = v["summary"]["session"]["title"].as_str().unwrap_or("");
        let turns = v["summary"]["session"]["turns"].as_u64().unwrap_or(0) as u32;
        if title != chat.title || turns != chat.turns {
            let mut c = chat.clone();
            c.title = title.to_string();
            c.turns = turns;
            let _ = held::keep(&self.engine, &c);
        }
        Ok(v)
    }

    /// The prefix of a held chat's question ids: its computer's store id, else its place.
    fn runtime_of(&self, chat: &HeldChat) -> String {
        if !chat.runtime.is_empty() {
            return chat.runtime.clone();
        }
        self.links
            .runtime_id(&chat.place)
            .unwrap_or_else(|| chat.place.clone())
    }

    /// A question from a held chat's computer, as windows here see it.
    fn held_question(&self, chat: &HeldChat, runtime: &str, q: &Value) -> Value {
        let mut v = only(
            q,
            &[
                "id", "request", "line", "title", "choices", "chat", "asked_ms",
            ],
        );
        v["id"] = json!(format!("{runtime}:{}", id_text(&q["id"])));
        v["remote_id"] = q["id"].clone();
        v["session"] = json!(chat.session);
        v["place"] = json!(chat.place);
        if let Ok(Some(p)) = self
            .engine
            .store
            .get_project(&ark_sdk::prelude::ProjectId(chat.project.clone()))
        {
            v["project"] = json!({ "id": p.id, "name": p.name });
        }
        if v["asked_ms"].is_null() {
            v["asked_ms"] = json!(crate::notify::now_ms());
        }
        v
    }

    /// Every held chat's open questions (for `hello` and the person's devices).
    pub(super) fn held_questions(&self) -> Vec<Value> {
        self.held_live
            .lock()
            .unwrap()
            .values()
            .flat_map(|l| l.questions.clone())
            .collect()
    }

    /// Held chats running a turn, as `working` lists them.
    pub(super) fn held_working(&self) -> Vec<Value> {
        let running: Vec<(String, String)> = self
            .held_live
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(s, l)| Some((s.clone(), l.running.clone()?)))
            .collect();
        running
            .into_iter()
            .filter_map(|(s, input)| {
                let c = self.held_chat(&s)?;
                Some(json!({
                    "session": s, "title": c.title, "project": c.project, "input": input,
                    "place": c.place,
                }))
            })
            .collect()
    }

    /// Rows for `sessions`: held chats in the same shape as this runtime's, with their place.
    pub(super) fn held_rows(&self, project: Option<&str>) -> Vec<Value> {
        let shown = self.held.lock().unwrap().clone();
        let names: HashMap<String, String> = self
            .engine
            .store
            .list_projects()
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.id.as_str().to_string(), p.name))
            .collect();
        let live = self.held_live.lock().unwrap();
        held::list(&self.engine, project)
            .into_iter()
            .map(|c| {
                let fmt = |t: Option<time::OffsetDateTime>| {
                    t.and_then(|t| {
                        t.format(&time::format_description::well_known::Rfc3339)
                            .ok()
                    })
                };
                let mut row = json!({
                    "id": c.session,
                    "short": c.session.chars().take(8).collect::<String>(),
                    "title": c.title,
                    "project": c.project,
                    "project_name": names.get(&c.project).cloned().unwrap_or_default(),
                    "created_at": fmt(c.created_at),
                    "updated_at": fmt(c.updated_at),
                    "turns": c.turns,
                    "tokens": 0,
                    "current": shown.as_deref() == Some(&c.session),
                    "place": self.place(&c),
                });
                if live.get(&c.session).is_some_and(|l| l.running.is_some()) {
                    row["working"] = json!(true);
                }
                row
            })
            .collect()
    }

    /// The calls a window makes on its chat, when that chat is held elsewhere. `None`: not a
    /// held chat's call; the host answers it as usual.
    pub(super) async fn held_route(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
    ) -> Option<anyhow::Result<Value>> {
        let s = |k: &str| params[k].as_str().unwrap_or("").to_string();
        match method {
            "dictation_status" | "dictation_begin" | "dictation_chunk" | "dictation_finish"
            | "dictation_cancel" => {
                // Empty means here, even if a held chat is now shown. Never follow focus.
                let place = match params.get("place") {
                    Some(Value::String(place)) => place,
                    _ => return Some(Err(anyhow::anyhow!("dictation needs its chosen computer"))),
                };
                if let Some(expected) = params["runtime"].as_str() {
                    let actual = if place.is_empty() {
                        Some(ark_interact::held::runtime_id(&self.engine))
                    } else {
                        self.links.runtime_id(place)
                    };
                    if actual.as_deref() != Some(expected) {
                        return Some(Err(anyhow::anyhow!(
                            "the recording's computer changed; try again"
                        )));
                    }
                }
                if place.is_empty() {
                    return None;
                }
                let mut forwarded = params.clone();
                forwarded["place"] = json!("");
                Some(self.links.chat(place, method, forwarded).await)
            }
            "resource" => {
                let reference: crate::resource::ResourceRef =
                    match serde_json::from_value(params["ref"].clone()) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e.into())),
                    };
                let here = ark_interact::held::runtime_id(&self.engine);
                if reference.location.runtime == here {
                    return None;
                }
                if let crate::resource::ResourceAuthority::Project { route } = &reference.authority
                {
                    let route = route.clone();
                    let kept = {
                        let routes = self.project_resource_routes.lock().unwrap();
                        routes.get(&route).cloned()
                    };
                    let Some(kept) = kept else {
                        return Some(Err(anyhow::anyhow!("that project Terminal route ended")));
                    };
                    if kept.runtime != reference.location.runtime
                        || kept.local_project != reference.location.project
                        || kept.kind != reference.kind
                        || kept.terminal != reference.id
                    {
                        return Some(Err(anyhow::anyhow!(
                            "the project Terminal route does not match that resource"
                        )));
                    }
                    let Some(place) = kept.place.as_deref() else {
                        return Some(Err(anyhow::anyhow!(
                            "that project Terminal route is not remote"
                        )));
                    };
                    if self.links.state(place) != Some(State::Ok) {
                        return Some(Err(anyhow::anyhow!("{place} is not answering")));
                    }
                    let closes = serde_json::from_value::<crate::resource::ResourceOperation>(
                        params["operation"].clone(),
                    )
                    .is_ok_and(|operation| {
                        operation.kind == crate::resource::ResourceOperationKind::TerminalClose
                    });
                    let answer = self
                        .links
                        .chat(
                            place,
                            "project_resource",
                            json!({
                                "origin": ark_interact::held::runtime_id(&self.engine),
                                "origin_project": kept.source_project,
                                "ref": reference,
                                "operation": params["operation"],
                            }),
                        )
                        .await;
                    if closes && answer.is_ok() {
                        self.end_project_terminal_routes(
                            &kept.runtime,
                            &kept.local_project,
                            &kept.terminal,
                        );
                    }
                    return Some(answer);
                }
                let Some(chat) = self.held_chat(&reference.location.session) else {
                    return Some(Err(anyhow::anyhow!("no linked chat owns that resource")));
                };
                if chat.runtime != reference.location.runtime
                    || chat.remote_project != reference.location.project
                {
                    return Some(Err(anyhow::anyhow!(
                        "the resource location does not match its chat"
                    )));
                }
                Some(
                    self.on_place(
                        &chat,
                        "chat_resource",
                        json!({
                            "session": chat.session,
                            "ref": reference,
                            "operation": params["operation"],
                        }),
                    )
                    .await,
                )
            }
            "summary" => {
                let chat = self.shown_held()?;
                Some(Ok(self.held_summary(&chat)))
            }
            // Ambient context belongs to the runtime that owns the chat. A held SSH or Cloud
            // chat must never fall back to the foreground app on this computer.
            "computer_context" => self.shown_held().map(|_| Ok(Value::Null)),
            "send" => {
                let chat = self.shown_held()?;
                let ask = params["ask"].as_bool().unwrap_or(true);
                let text = s("text");
                Some(
                    async {
                        anyhow::ensure!(!text.trim().is_empty(), "nothing to send");
                        self.ensure_relay(&chat.place).await;
                        self.on_place(
                            &chat,
                            "chat_send",
                            json!({ "session": chat.session, "text": text, "ask": ask, "model": chat.spec }),
                        )
                        .await
                    }
                    .await,
                )
            }
            "steer" | "cancel" => {
                let chat = self.held_for(params, "session")?;
                let call = if method == "steer" {
                    "chat_steer"
                } else {
                    "chat_cancel"
                };
                Some(
                    self.on_place(
                        &chat,
                        call,
                        json!({ "session": chat.session, "text": s("text") }),
                    )
                    .await,
                )
            }
            "approve" => {
                let id = params["id"].as_str()?;
                let q = self
                    .held_questions()
                    .into_iter()
                    .find(|q| q["id"].as_str() == Some(id))?;
                let chat = self.held_chat(q["session"].as_str().unwrap_or(""))?;
                let r = self
                    .on_place(
                        &chat,
                        "chat_approve",
                        json!({ "session": chat.session, "id": q["remote_id"], "decision": params["decision"] }),
                    )
                    .await;
                if r.is_ok() {
                    self.close_held_question(&chat.session, id);
                }
                Some(r.map(|_| json!({})))
            }
            "model" => {
                let chat = self.shown_held()?;
                Some(
                    async {
                        let v = self
                            .on_place(
                                &chat,
                                "chat_model",
                                json!({ "session": chat.session, "spec": s("spec") }),
                            )
                            .await?;
                        let mut c = chat.clone();
                        c.model = v["model"].as_str().unwrap_or_default().to_string();
                        c.spec = s("spec");
                        held::keep(&self.engine, &c)?;
                        if let Some(l) = self.held_live.lock().unwrap().get_mut(&chat.session) {
                            l.summary = v;
                        }
                        let summary = self.held_summary(&c);
                        self.emit(json!({ "kind": "summary", "summary": summary }));
                        Ok(summary)
                    }
                    .await,
                )
            }
            "rename_session" => {
                let chat = self.held_for(params, "id")?;
                Some(
                    async {
                        let v = self
                            .on_place(
                                &chat,
                                "chat_rename",
                                json!({ "session": chat.session, "title": s("title") }),
                            )
                            .await?;
                        let mut c = chat.clone();
                        c.title = s("title");
                        held::keep(&self.engine, &c)?;
                        if let Some(l) = self.held_live.lock().unwrap().get_mut(&chat.session) {
                            l.summary = v;
                        }
                        self.emit_held_summary(&c);
                        Ok(self.held_summary(&c))
                    }
                    .await,
                )
            }
            "transcript" => {
                let chat = self.held_for(params, "session")?;
                let mut p = json!({ "session": chat.session, "limit": params["limit"].as_u64().unwrap_or(30) });
                if let Some(b) = params["before"].as_u64() {
                    p["before"] = json!(b);
                }
                Some(
                    self.on_place(&chat, "chat_read", p)
                        .await
                        .map(|v| v["transcript"].clone()),
                )
            }
            "trace" | "call" => {
                let chat = self.shown_held()?;
                let mut p = json!({ "session": chat.session });
                if let Some(i) = params["index"].as_u64() {
                    p["index"] = json!(i);
                }
                let trace = self.on_place(&chat, "chat_trace", p).await;
                if method == "trace" {
                    return Some(trace);
                }
                Some(trace.and_then(|t| {
                    let k = params["n"].as_u64().unwrap_or(0) as usize;
                    let t: Option<ark_interact::Trace> = serde_json::from_value(t)?;
                    let t = t.ok_or_else(|| anyhow::anyhow!("no call #{k}"))?;
                    let call = t
                        .call(k)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("no call #{k}"))?;
                    let entries: Vec<_> = t
                        .entries
                        .into_iter()
                        .filter(|e| e.call == Some(k))
                        .collect();
                    // The call's stored invocation stays on its computer.
                    Ok(json!({ "call": call, "invocation": Value::Null, "entries": entries }))
                }))
            }
            "context" => {
                let chat = self.shown_held()?;
                Some(
                    self.on_place(&chat, "chat_context", json!({ "session": chat.session }))
                        .await,
                )
            }
            "command" => {
                let chat = self.shown_held()?;
                let word = s("line");
                let word = word.split_whitespace().next().unwrap_or("");
                match word {
                    // Moving to another chat or project leaves this one.
                    "/new" | "/resume" | "/project" => {
                        self.leave_held().await;
                        None
                    }
                    // Lists and help read this computer's records; this chat stays shown.
                    "/sessions" | "/chats" | "/help" | "/?" | "/waiting" => None,
                    _ => Some(Err(anyhow::anyhow!(
                        "this chat runs on {}; that command works in chats on this computer",
                        chat.place
                    ))),
                }
            }
            "new_session" => {
                let place = s("place");
                if place.is_empty() || place == "here" {
                    self.leave_held().await;
                    return None;
                }
                let big = params["big"].as_bool().unwrap_or(false);
                Some(self.held_new(&place, &s("model"), big).await)
            }
            "open_session" => match self.held_chat(&s("id")) {
                Some(chat) => Some(self.held_open(chat).await),
                None => {
                    self.leave_held().await;
                    None
                }
            },
            // The files that changed on a held chat's computer, and bringing them here.
            "place_changes" => {
                let chat = self.held_for(params, "session")?;
                Some(
                    async {
                        if let Some(why) = self.unreachable(&chat) {
                            anyhow::bail!(why);
                        }
                        self.place_changes(&chat).await
                    }
                    .await,
                )
            }
            "place_bring" => {
                let chat = self.held_for(params, "session")?;
                let choices: HashMap<String, String> =
                    serde_json::from_value(params["choices"].clone()).unwrap_or_default();
                let big = params["big"].as_bool().unwrap_or(false);
                Some(
                    async {
                        if let Some(why) = self.unreachable(&chat) {
                            anyhow::bail!(why);
                        }
                        let r = self.place_bring(&chat, &choices, big).await?;
                        self.emit_held_summary(&chat);
                        Ok(r)
                    }
                    .await,
                )
            }
            // Who answers on the chat's computer: its models, a program's step there.
            "models" => {
                let chat = self.shown_held()?;
                Some(self.on_place(&chat, "chat_models", json!({})).await)
            }
            "place_models" => {
                let place = s("place");
                if place.is_empty() {
                    return Some(Err(anyhow::anyhow!("which computer?")));
                }
                Some(self.links.chat(&place, "chat_models", json!({})).await)
            }
            "worker_offers" => {
                let chat = self.shown_held()?;
                Some(self.on_place(&chat, "chat_worker_offers", json!({})).await)
            }
            "worker_setup" => {
                let chat = self.shown_held()?;
                Some(
                    async {
                        let mut p = params.clone();
                        p["session"] = json!(chat.session);
                        self.on_place(&chat, "chat_worker_setup", p).await
                    }
                    .await,
                )
            }
            "place_setup" => {
                let chat = self.shown_held()?;
                Some(
                    async {
                        let mut p = params.clone();
                        p["session"] = json!(chat.session);
                        self.on_place(&chat, "chat_setup", p).await
                    }
                    .await,
                )
            }
            // A key for the chat's computer: the one typed, or the one saved here, which goes
            // only when the person asks for it.
            "place_key" => {
                let chat = self.shown_held();
                let place = chat
                    .as_ref()
                    .map(|chat| chat.place.clone())
                    .unwrap_or_else(|| s("place"));
                if place.is_empty() {
                    return Some(Err(anyhow::anyhow!("which computer?")));
                }
                Some(
                    async {
                        let provider = s("provider");
                        let key = match s("key") {
                            k if !k.trim().is_empty() => k,
                            _ => self
                                .api_keys()
                                .and_then(|k| k.saved_key(&provider))
                                .ok_or_else(|| {
                                    anyhow::anyhow!("no {provider} key is saved here")
                                })?,
                        };
                        let v = self
                            .links
                            .chat(
                                &place,
                                "chat_key",
                                json!({ "provider": provider, "key": key }),
                            )
                            .await?;
                        let hash = {
                            use sha2::Digest;
                            format!("{:x}", sha2::Sha256::digest(key.trim().as_bytes()))
                        };
                        held::key_sent(&self.engine, &place, &provider, &hash);
                        Ok(v)
                    }
                    .await,
                )
            }
            "switch_project" | "open_project" | "add_project" => {
                self.leave_held().await;
                None
            }
            // Explicitly window-local or relationship-wide. This is the complete fallthrough
            // set while a held chat is shown; adding a host method requires classifying it.
            "hello"
            | "places"
            | "computers"
            | "computer_hosts"
            | "computer_add"
            | "computer_remove"
            | "computer_terminal"
            | "working"
            | "projects"
            | "sessions"
            | "rename_project"
            | "project_folder"
            | "attention"
            | "attention_all"
            | "attention_answer"
            | "attention_go"
            | "attention_seen"
            | "push_key"
            | "push_add"
            | "push_remove"
            | "push_devices"
            | "push_here"
            | "devices_signed_in"
            | "device_sign_out"
            | "api_keys"
            | "api_key_save"
            | "api_key_forget"
            | "subscription_usage"
            | "capabilities"
            | "capability_connect"
            | "capability_turn"
            | "capability_disconnect"
            | "plugins"
            | "marketplace_plugins"
            | "marketplace_add"
            | "marketplace_remove"
            | "plugin_add"
            | "plugin_remove"
            | "plugin_restart"
            | "upload_project_parts"
            | "upload_project_finish"
            | "upload_project_cancel"
            | "ext" => None,
            other => self.shown_held().map(|chat| {
                Err(anyhow::anyhow!(
                    "`{other}` is not routed for resources on {}; nothing ran on this computer",
                    chat.place
                ))
            }),
        }
    }

    /// The computer `place` was removed: its chats leave the screen, their questions the
    /// tray, its terminals and its follower stop.
    pub(super) async fn forget_place(&self, place: &str, sessions: &[String]) {
        self.project_resource_routes
            .lock()
            .unwrap()
            .retain(|_, route| route.place.as_deref() != Some(place));
        // Remove aliases where the resources live before deleting our knowledge of the chats.
        // Failure is harmless: aliases are ephemeral and a disconnected runtime cannot emit to
        // this removed link, but an available runtime must not retain them.
        for session in sessions {
            let _ = self
                .links
                .chat(
                    place,
                    "chat_forget_resources",
                    json!({ "session": session }),
                )
                .await;
        }
        for session in sessions {
            self.resource_subscribers.remove_session(session);
        }
        let closed: Vec<String> = {
            let mut live = self.held_live.lock().unwrap();
            sessions
                .iter()
                .filter_map(|s| live.remove(s))
                .flat_map(|l| l.questions)
                .filter_map(|q| q["id"].as_str().map(str::to_string))
                .collect()
        };
        for id in closed {
            self.emit(json!({ "kind": "approval_closed", "id": id }));
        }
        if let Some((task, _)) = self.relays.lock().unwrap().remove(place) {
            task.abort();
        }
        let shown = self.held.lock().unwrap().clone();
        if shown.is_some_and(|s| sessions.contains(&s)) {
            self.leave_held().await;
            self.emit(json!({ "kind": "summary", "summary": self.summary() }));
        }
        self.emit_working();
    }

    fn close_held_question(&self, session: &str, id: &str) {
        let removed = {
            let mut live = self.held_live.lock().unwrap();
            live.get_mut(session).is_some_and(|l| {
                let n = l.questions.len();
                l.questions.retain(|q| q["id"].as_str() != Some(id));
                l.questions.len() != n
            })
        };
        if removed {
            self.emit(json!({ "kind": "approval_closed", "id": id }));
        }
    }

    /// Follow the events of every place that holds a chat for this runtime (at start).
    pub(super) fn start_relays(self: &Arc<Self>) {
        let mut places: Vec<String> = held::list(&self.engine, None)
            .into_iter()
            .map(|c| c.place)
            .collect();
        places.sort();
        places.dedup();
        for p in places {
            self.spawn_relay(&p);
        }
    }

    /// Follow `place`'s events, and wait a little for the stream to start so a turn sent next
    /// is not missed.
    pub(super) async fn ensure_relay(self: &Arc<Self>, place: &str) {
        let mut ready = self.spawn_relay(place);
        let _ = tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|r| *r)).await;
    }

    fn spawn_relay(self: &Arc<Self>, place: &str) -> tokio::sync::watch::Receiver<bool> {
        let mut relays = self.relays.lock().unwrap();
        if let Some((task, ready)) = relays.get(place) {
            if !task.is_finished() {
                return ready.clone();
            }
        }
        let (tx, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(relay(Arc::downgrade(self), place.to_string(), tx));
        relays.insert(place.to_string(), (task, rx.clone()));
        rx
    }

    /// One event from `place`: passed on when it is one of its held chats', and only a chat's
    /// kinds of event with their known fields: the computer is trusted with its chats, not with
    /// this computer's windows.
    fn held_event(self: &Arc<Self>, place: &str, ev: Value) {
        let kind = ev["kind"].as_str().unwrap_or("").to_string();
        let fields: &[&str] = match kind.as_str() {
            "text" => &["kind", "session", "text"],
            "event" => &["kind", "session", "event", "activity"],
            "turn_started" => &["kind", "session", "input"],
            "turn_finished" => &[
                "kind",
                "session",
                "ok",
                "cancelled",
                "error",
                "out_of_credit",
                "answer",
                "steps",
                "footer",
                "index",
                "resume",
                "elapsed_ms",
                "undelivered",
            ],
            "approval" => &[
                "kind", "session", "id", "request", "line", "title", "choices", "chat",
            ],
            "approval_closed" => &["kind", "id"],
            "term" => &[
                "kind",
                "session",
                "ref",
                "generation",
                "cursor",
                "data",
                "exit",
            ],
            "proc" => &["kind", "session", "ref", "event", "here"],
            "files_changed" => &["kind", "session", "ref", "project", "paths"],
            "orch" => &[
                "kind", "session", "ref", "event", "line", "tone", "activity",
            ],
            _ => return,
        };
        let mut ev = only(&ev, fields);
        if kind == "approval_closed" {
            let found = self.held_live.lock().unwrap().iter().find_map(|(s, l)| {
                l.questions
                    .iter()
                    .find(|q| q["remote_id"] == ev["id"] && q["place"] == json!(place))
                    .map(|q| (s.clone(), q["id"].as_str().unwrap_or("").to_string()))
            });
            if let Some((session, local)) = found {
                self.close_held_question(&session, &local);
            }
            return;
        }
        let Some(session) = ev["session"].as_str().map(str::to_string) else {
            return;
        };
        // A project resource event already carries its validated, session-bound alias.
        if matches!(kind.as_str(), "term" | "proc" | "orch" | "files_changed")
            && ev.get("ref").is_some()
        {
            let Ok(reference) =
                serde_json::from_value::<crate::resource::ResourceRef>(ev["ref"].clone())
            else {
                return;
            };
            if let crate::resource::ResourceAuthority::Project { route } = &reference.authority {
                let valid = self
                    .project_resource_routes
                    .lock()
                    .unwrap()
                    .get(route)
                    .is_some_and(|kept| {
                        kept.place.as_deref() == Some(place)
                            && kept.runtime == reference.location.runtime
                            && kept.local_project == reference.location.project
                            && kept.kind == reference.kind
                            && kept.terminal == reference.id
                    });
                if valid {
                    self.emit(ev);
                }
                return;
            }
            let Some(chat) = self
                .held_chat(&reference.location.session)
                .filter(|c| c.place == place)
            else {
                return;
            };
            if reference.location.runtime == chat.runtime
                && reference.location.project == chat.remote_project
            {
                self.emit(ev);
            }
            return;
        }
        let Some(chat) = self.held_chat(&session).filter(|c| c.place == place) else {
            return;
        };
        match kind.as_str() {
            "approval" => {
                let runtime = self.runtime_of(&chat);
                let q = self.held_question(&chat, &runtime, &ev);
                // Already listed from a read of the chat while the stream started.
                if self.held_questions().iter().any(|x| x["id"] == q["id"]) {
                    return;
                }
                let mut out = q.clone();
                out["kind"] = json!("approval");
                self.held_live
                    .lock()
                    .unwrap()
                    .entry(session)
                    .or_default()
                    .questions
                    .push(q);
                self.notify_soon();
                self.emit(out);
            }
            "turn_started" => {
                {
                    let mut live = self.held_live.lock().unwrap();
                    let l = live.entry(session).or_default();
                    l.running = ev["input"].as_str().map(str::to_string);
                    l.live.clear();
                }
                self.emit(ev);
                self.emit_working();
            }
            "turn_finished" => {
                let open: Vec<String> = {
                    let mut live = self.held_live.lock().unwrap();
                    let l = live.entry(session.clone()).or_default();
                    l.running = None;
                    l.live.clear();
                    l.questions
                        .drain(..)
                        .filter_map(|q| q["id"].as_str().map(str::to_string))
                        .collect()
                };
                for id in open {
                    self.emit(json!({ "kind": "approval_closed", "id": id }));
                }
                let mut c = chat.clone();
                c.updated_at = Some(time::OffsetDateTime::now_utc());
                if ev["index"].is_number() {
                    c.turns = c.turns.max(ev["index"].as_u64().unwrap_or(0) as u32 + 1);
                }
                let _ = held::keep(&self.engine, &c);
                let shown = self.held.lock().unwrap().as_deref() == Some(&session);
                ev["summary"] = if shown {
                    self.held_summary(&c)
                } else {
                    Value::Null
                };
                self.emit(ev);
                self.emit_working();
                // Its title and model as its computer has them after the turn.
                let me = self.clone();
                tokio::spawn(async move {
                    if me.held_read(&c).await.is_err() {
                        return;
                    }
                    // How many files differ there now: windows offer to bring them.
                    if let Ok(v) = me.place_changes(&c).await {
                        let n = v["count"].as_u64().unwrap_or(0) as usize;
                        if let Some(l) = me.held_live.lock().unwrap().get_mut(&c.session) {
                            l.changes = n;
                        }
                        me.emit(json!({
                            "kind": "place_changes", "session": c.session, "place": c.place,
                            "count": n,
                        }));
                    }
                    if let Some(c) = me.held_chat(&c.session) {
                        me.emit_held_summary(&c);
                    }
                });
            }
            _ => self.emit(ev),
        }
    }

    /// After `place`'s stream starts again: its held chats' state as it is now, since events
    /// may have been missed meanwhile.
    async fn held_resync(self: &Arc<Self>, place: &str) {
        let chats: Vec<HeldChat> = held::list(&self.engine, None)
            .into_iter()
            .filter(|c| c.place == place)
            .collect();
        let shown = self.held.lock().unwrap().clone();
        for c in chats {
            let known = self.held_live.lock().unwrap().contains_key(&c.session);
            if !known && shown.as_deref() != Some(&c.session) {
                continue;
            }
            let was = self
                .held_live
                .lock()
                .unwrap()
                .get(&c.session)
                .and_then(|l| l.running.clone());
            if self.held_read(&c).await.is_err() {
                continue;
            }
            let now = self
                .held_live
                .lock()
                .unwrap()
                .get(&c.session)
                .and_then(|l| l.running.clone());
            // A turn that ended while the stream was gone: windows read the chat again.
            if was.is_some() && now.is_none() {
                let summary = if shown.as_deref() == Some(&c.session) {
                    self.held_summary(&c)
                } else {
                    Value::Null
                };
                self.emit(json!({
                    "kind": "turn_finished", "session": c.session, "ok": true,
                    "resync": true, "summary": summary,
                }));
            }
            self.emit_held_summary(&c);
        }
        self.emit_working();
    }

    /// `hello`'s view of the held chat windows show: `(summary, running, live)`.
    pub(super) fn held_hello(&self, chat: &HeldChat) -> (Value, Option<String>, Vec<Value>) {
        let (_, fed) = self.feed.position(&chat.session);
        let live = self.held_live.lock().unwrap();
        let l = live.get(&chat.session);
        let running = l.and_then(|l| l.running.clone());
        // Events this runtime passed on since the turn started, else those its computer had.
        let events = if fed.is_empty() {
            l.map(|l| l.live.clone()).unwrap_or_default()
        } else {
            fed
        };
        drop(live);
        (self.held_summary(chat), running, events)
    }
}

/// A question id as text (`7` or `"q-1"`).
fn id_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Follow `place`'s events for its held chats, starting again with backoff when the stream
/// goes. Ends when the host does.
async fn relay(host: Weak<Host>, place: String, ready: tokio::sync::watch::Sender<bool>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let Some(h) = host.upgrade() else { return };
        let links = h.links();
        // Woken when the link changes (set, up again), not only by the clock.
        let mut changed = links.subscribe();
        let up = links.state(&place) == Some(State::Ok);
        drop(h);
        let rx = if up {
            links.chat_events(&place).await.ok()
        } else {
            None
        };
        drop(links);
        if let Some(mut rx) = rx {
            let _ = ready.send(true);
            backoff = Duration::from_secs(1);
            if let Some(h) = host.upgrade() {
                h.held_resync(&place).await;
            }
            while let Ok(Some(ev)) = tokio::time::timeout(IDLE, rx.recv()).await {
                let Some(h) = host.upgrade() else { return };
                h.held_event(&place, ev);
            }
            let _ = ready.send(false);
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = changed.recv() => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// `v` with only these fields.
fn only(v: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for f in fields {
        if let Some(x) = v.get(*f) {
            out.insert(f.to_string(), x.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_fields_are_kept() {
        let v = json!({ "kind": "text", "text": "hi", "seq": 3, "html": "<b>" });
        assert_eq!(
            only(&v, &["kind", "text"]),
            json!({ "kind": "text", "text": "hi" })
        );
    }
}
