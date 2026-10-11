//! Chats placed on another computer (ADR 0052). A chat has a place: the computer whose runtime
//! holds it and does its work. This module keeps both sides' records:
//!
//! | Side | Keeps | Why |
//! |---|---|---|
//! | This computer | a [`HeldChat`] per chat placed elsewhere | the chat stays listed after a restart and while its computer does not answer |
//! | The computer | the project each origin's project became there | a second chat for the same project lands in the same project |
//!
//! Every runtime has a store id ([`runtime_id`]), made once, which `hello` answers: a computer
//! is known by it, so renaming its link changes nothing stored.
//!
//! A chat held elsewhere is reached only through [`CHAT_CALLS`]: calls that name their chat and
//! never move the other runtime's own window (`Links::chat`).

use ark_sdk::prelude::*;
use serde::{Deserialize, Serialize};

/// The calls this runtime makes on another for a chat it holds there. Nothing else crosses: the
/// other computer's settings, capabilities, connections and other chats stay its own.
pub const CHAT_CALLS: &[&str] = &[
    "chat_new",
    "chat_send",
    "chat_cancel",
    "chat_steer",
    "chat_read",
    "chat_approve",
    "chat_model",
    "chat_context",
    "chat_trace",
    "chat_rename",
    "chat_resource",
    "project_resource_open",
    "project_resource",
    // Who answers there (ADR 0052, Who answers): its models, a program's install or sign-in in
    // a terminal the person answers, a key the person agreed to send.
    "chat_models",
    "chat_setup",
    "chat_key",
    // The project's copy there (ADR 0052, Project copy).
    "copy_state",
    "copy_put",
    "copy_pack",
    "copy_part",
    "copy_settle",
];

/// A chat this runtime shows whose place is another computer. Its id is the chat's own session
/// id there (ids are random, so they do not collide with this runtime's).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeldChat {
    #[serde(default)]
    pub session: String,
    /// The link's name the chat was placed with (what the person calls the computer).
    #[serde(default)]
    pub place: String,
    /// The computer's store id ([`runtime_id`] there).
    #[serde(default)]
    pub runtime: String,
    /// This runtime's project the chat belongs to.
    #[serde(default)]
    pub project: String,
    /// The project it works in on the computer.
    #[serde(default)]
    pub remote_project: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model: String,
    /// The model spec the person chose for it (`claude:sonnet`), sent with each message: the
    /// computer does not keep a chat's model between turns.
    #[serde(default)]
    pub spec: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<time::OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub updated_at: Option<time::OffsetDateTime>,
    #[serde(default)]
    pub turns: u32,
}

const HELD: &str = "held_chat";
const ORIGIN: &str = "held_origin";
const RUNTIME: &str = "runtime";

/// This runtime's store id: made the first time it is asked for, then kept.
pub fn runtime_id(engine: &Engine) -> String {
    let store = &engine.store;
    if let Ok(Some(v)) = store.get_record(RUNTIME, "self") {
        if let Some(id) = v["id"].as_str() {
            return id.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let _ = store.put_record(RUNTIME, "self", "", &serde_json::json!({ "id": id }));
    id
}

/// The chats held elsewhere: one project's (by id), or all, most recent first.
pub fn list(engine: &Engine, project: Option<&str>) -> Vec<HeldChat> {
    let mut out: Vec<HeldChat> = engine
        .store
        .list_records(HELD, project)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    out.sort_by_key(|c: &HeldChat| std::cmp::Reverse(c.updated_at));
    out
}

/// The held chat with this session id, if any.
pub fn get(engine: &Engine, session: &str) -> Option<HeldChat> {
    if session.is_empty() {
        return None;
    }
    engine
        .store
        .get_record(HELD, session)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Keep (or update) a held chat's record.
pub fn keep(engine: &Engine, chat: &HeldChat) -> anyhow::Result<()> {
    engine.store.put_record(
        HELD,
        &chat.session,
        &chat.project,
        &serde_json::to_value(chat)?,
    )?;
    Ok(())
}

/// Forget a held chat's record (the chat stays on its computer).
pub fn forget(engine: &Engine, session: &str) -> anyhow::Result<bool> {
    Ok(engine.store.delete_record(HELD, session)?)
}

const KEYS_SENT: &str = "keys_sent";

/// The keys this runtime sent to the computer `place`: provider to the key's SHA-256 (hex).
/// Removing the computer forgets them there, and only while the key there is still that one.
pub fn keys_sent(engine: &Engine, place: &str) -> std::collections::BTreeMap<String, String> {
    engine
        .store
        .get_record(KEYS_SENT, place)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// The key for `provider` whose SHA-256 is `sha256` went to `place`.
pub fn key_sent(engine: &Engine, place: &str, provider: &str, sha256: &str) {
    let mut sent = keys_sent(engine, place);
    sent.insert(provider.to_string(), sha256.to_string());
    let _ = engine
        .store
        .put_record(KEYS_SENT, place, place, &serde_json::json!(sent));
}

pub fn forget_keys_sent(engine: &Engine, place: &str) {
    let _ = engine.store.delete_record(KEYS_SENT, place);
}

/// On the computer: the project that `origin`'s project `project` works in here, made the
/// first time with `name` (unique among this runtime's projects).
pub fn origin_project(
    engine: &Engine,
    origin: &str,
    project: &str,
    name: &str,
) -> anyhow::Result<Project> {
    anyhow::ensure!(
        !origin.is_empty() && !project.is_empty(),
        "a chat placed here names where it comes from"
    );
    let key = format!("{origin}:{project}");
    if let Some(v) = engine.store.get_record(ORIGIN, &key)? {
        if let Some(id) = v["project"].as_str() {
            if let Some(p) = engine.store.get_project(&ProjectId(id.to_string()))? {
                return Ok(p);
            }
        }
    }
    let top = crate::interaction::top_projects(engine)?;
    let base = match name.trim() {
        "" => "project",
        n => n,
    };
    let p = engine.create_project(&crate::interaction::unique_name(base, &top), "")?;
    engine.store.put_record(
        ORIGIN,
        &key,
        origin,
        &serde_json::json!({ "project": p.id.as_str(), "origin": origin, "origin_project": project }),
    )?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(dir: &std::path::Path) -> std::sync::Arc<Engine> {
        Ark::builder()
            .store_root(dir)
            .provider(std::sync::Arc::new(
                ark_sdk::prelude::ark_model::MockProvider::new("m", vec![]),
            ))
            .build()
            .unwrap()
            .shared_engine()
    }

    #[test]
    fn a_runtime_keeps_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path());
        let id = runtime_id(&e);
        assert_eq!(id.len(), 32);
        assert_eq!(runtime_id(&e), id);
    }

    #[test]
    fn held_chats_are_listed_newest_first_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path());
        let at = |s| time::OffsetDateTime::from_unix_timestamp(s).unwrap();
        for (s, t) in [("a", 10), ("b", 20)] {
            keep(
                &e,
                &HeldChat {
                    session: s.into(),
                    place: "vps".into(),
                    project: "p1".into(),
                    updated_at: Some(at(t)),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let ids: Vec<_> = list(&e, Some("p1"))
            .into_iter()
            .map(|c| c.session)
            .collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(list(&e, Some("p2")).is_empty());
        assert_eq!(get(&e, "a").unwrap().place, "vps");
        assert!(forget(&e, "a").unwrap());
        assert!(get(&e, "a").is_none());
    }

    #[test]
    fn an_origin_project_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path());
        let a = origin_project(&e, "mac", "p1", "novel").unwrap();
        let again = origin_project(&e, "mac", "p1", "novel").unwrap();
        assert_eq!(a.id, again.id);
        let other = origin_project(&e, "laptop", "p1", "novel").unwrap();
        assert_ne!(other.id, a.id);
        assert_eq!(other.name, "novel 2");
        assert!(origin_project(&e, "", "p1", "x").is_err());
    }
}
