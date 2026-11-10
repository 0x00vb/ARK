//! Agents that run on another computer the person picked (ADR 0114).
//!
//! An agent has one home. Moving it spawns its spec on the target computer, keeps a record of
//! where it went, and ends it here (its old chats stay readable here). Operations on it then
//! cross the link as resource operations of a chat held there: the chat only anchors the
//! project replica, so no new route exists.

use super::held::Made;
use super::*;
use crate::resource::{
    ResourceAuthority, ResourceKind, ResourceLocation, ResourceOperationKind, ResourceRef,
};
use ark_interact::ark_orchestrate::{AgentRecord, AgentState};
use ark_interact::held::HeldChat;
use ark_interact::links::State;

const PLACED: &str = "placed_agent";

/// How long a sidebar read waits for a computer's answer before showing what is remembered.
const READ: std::time::Duration = std::time::Duration::from_secs(3);

/// An agent of this runtime's project that runs on another computer.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Placed {
    pub project: String,
    pub place: String,
    /// The chat held on that computer that names its replica of the project.
    pub session: String,
    /// The agent as that computer last said it was.
    pub record: AgentRecord,
    /// The agent's folder on that computer, as it last said.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub own_folder: bool,
}

impl Placed {
    fn key(project: &str, agent: &str) -> String {
        format!("{project}:{agent}")
    }
}

impl Host {
    fn placed_get(&self, project: &str, agent: &str) -> Option<Placed> {
        placed_read(&self.engine.store, project, agent)
    }

    fn placed_keep(&self, placed: &Placed) -> anyhow::Result<()> {
        let _held = placed_lock();
        placed_write(&self.engine.store, placed)
    }

    fn placed_forget(&self, project: &str, agent: &str) -> anyhow::Result<()> {
        placed_delete(&self.engine.store, project, agent)
    }

    fn placed_all(&self) -> Vec<Placed> {
        self.engine
            .store
            .list_records(PLACED, None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    /// One operation on the project replica a held chat names.
    async fn place_op(
        &self,
        chat: &HeldChat,
        kind: ResourceOperationKind,
        input: Value,
    ) -> anyhow::Result<Value> {
        let reference = ResourceRef {
            location: ResourceLocation {
                runtime: chat.runtime.clone(),
                project: chat.remote_project.clone(),
                session: chat.session.clone(),
            },
            kind: ResourceKind::Project,
            id: String::new(),
            authority: ResourceAuthority::Chat,
        };
        self.on_place(
            chat,
            "chat_resource",
            json!({
                "session": chat.session,
                "ref": reference,
                "operation": { "kind": kind, "input": input },
            }),
        )
        .await
    }

    /// An agent's folder on the computer that holds it: `change` as in `agent_folder_of`.
    async fn place_folder(
        &self,
        chat: &HeldChat,
        agent: &str,
        change: Option<Option<&str>>,
    ) -> anyhow::Result<(String, bool)> {
        let mut input = json!({ "agent": agent });
        if let Some(path) = change {
            input["path"] = json!(path);
        }
        let v = self
            .place_op(chat, ResourceOperationKind::AgentFolder, input)
            .await
            .map_err(|e| folder_error(&chat.place, e))?;
        let folder = v["folder"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{} did not say the folder", chat.place))?;
        Ok((
            folder.to_string(),
            v["own_folder"].as_bool().unwrap_or(false),
        ))
    }

    /// `agent_folder` for an agent that runs on another computer: the folder is there. `None`
    /// when the agent runs here.
    pub(super) async fn placed_folder(
        &self,
        project: &str,
        agent: &str,
        path: Option<&str>,
    ) -> anyhow::Result<Option<Value>> {
        let Some(placed) = self.placed_get(project, agent) else {
            return Ok(None);
        };
        let chat = self
            .held_chat(&placed.session)
            .ok_or_else(|| anyhow::anyhow!("{} no longer holds `{agent}`", placed.place))?;
        let (folder, own) = self.place_folder(&chat, agent, Some(path)).await?;
        self.placed_remember(&placed, None, Some((folder.clone(), own)))?;
        Ok(Some(json!({ "folder": folder, "own_folder": own })))
    }

    /// Whether the computer offers the model an agent runs with. When it does not answer in
    /// time the model stays: the move is not the place to guess.
    async fn place_offers(&self, chat: &HeldChat, model: Option<&str>) -> bool {
        let Some(spec) = model.filter(|m| !m.is_empty()) else {
            return true;
        };
        let call = if spec.starts_with("agent:") {
            "chat_worker_offers"
        } else {
            "chat_models"
        };
        let Ok(Ok(list)) = tokio::time::timeout(READ, self.on_place(chat, call, json!({}))).await
        else {
            return true;
        };
        spec_offered(spec, &list)
    }

    async fn place_agents(&self, chat: &HeldChat) -> anyhow::Result<Vec<AgentRecord>> {
        let v = self
            .place_op(chat, ResourceOperationKind::Agents, json!({}))
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// The chat held on `place` that names `project`'s replica there: one that exists, else a
    /// new one. A project too large to send without asking stops the move.
    async fn place_anchor(
        self: &Arc<Self>,
        place: &str,
        project: &Project,
    ) -> anyhow::Result<HeldChat> {
        if let Some(chat) = ark_interact::held::list(&self.engine, Some(project.id.as_str()))
            .into_iter()
            .find(|c| c.place == place && !c.runtime.is_empty() && !c.remote_project.is_empty())
        {
            return Ok(chat);
        }
        match self.held_make(place, project, "", false).await? {
            Made::Chat(made) => Ok(made.chat),
            Made::Ask(_) => anyhow::bail!(
                "this project is large: start a chat on {place} first to send it there"
            ),
        }
    }

    fn top_project(&self, project: &str) -> anyhow::Result<Project> {
        self.engine
            .store
            .get_project(&ProjectId(project.to_string()))?
            .filter(|p| p.parent.is_none())
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"))
    }

    /// `agents {all}`: this runtime's agents, then those it placed on other computers. A
    /// computer that does not answer shows its agents as last remembered.
    pub(super) async fn all_agents_placed(&self) -> anyhow::Result<Value> {
        let mut rows = self.all_agents()?;
        let list = rows.as_array_mut().expect("a list");
        // Each computer is asked at once: a slow one costs its bound once, not once per agent.
        let fresh =
            futures::future::join_all(self.placed_all().into_iter().map(|p| self.placed_fresh(p)))
                .await;
        for placed in fresh.into_iter().flatten() {
            let Some(project) = self
                .engine
                .store
                .get_project(&ProjectId(placed.project.clone()))
                .ok()
                .flatten()
            else {
                continue;
            };
            let agent = &placed.record;
            if agent.state == AgentState::Terminated {
                continue;
            }
            let runtime = self.links.runtime_id(&placed.place).unwrap_or_default();
            let chat = self.held_chat(&placed.session);
            list.push(json!({
                "project": project.id,
                "project_name": project.name,
                "name": agent.name,
                "title": agent.spec.title,
                "look": agent.spec.look,
                "role": agent.spec.role,
                "state": agent.state,
                "asking": false,
                "running": agent.running,
                "updated_ms": agent.updated_ms,
                "preview": "",
                "program": agent.on_program(),
                "model": agent.spec.model,
                "description": agent.spec.description,
                "folder": placed.folder,
                "own_folder": placed.own_folder,
                "place": placed.place,
                "session": placed.session,
                "location": chat.map(|c| ResourceLocation {
                    runtime: if c.runtime.is_empty() { runtime } else { c.runtime },
                    project: c.remote_project,
                    session: c.session,
                }),
            }));
        }
        list.sort_by_key(|row| std::cmp::Reverse(row["updated_ms"].as_u64().unwrap_or(0)));
        Ok(rows)
    }

    /// `placed` as its computer says it is now; as remembered when it does not answer. `None`
    /// when the agent is no longer placed here (ended or moved back while this ran).
    async fn placed_fresh(&self, placed: Placed) -> Option<Placed> {
        let Some(chat) = self.held_chat(&placed.session) else {
            return Some(placed);
        };
        if self.links.state(&chat.place) != Some(State::Ok) {
            return Some(placed);
        }
        let name = placed.record.name.clone();
        let (agents, folder) = tokio::join!(
            tokio::time::timeout(READ, self.place_agents(&chat)),
            tokio::time::timeout(READ, self.place_folder(&chat, &name, None)),
        );
        let record = match agents {
            Ok(Ok(agents)) => agents.into_iter().find(|a| a.name == name),
            _ => None,
        };
        let folder = match folder {
            Ok(Ok(folder)) => Some(folder),
            Ok(Err(error)) => {
                tracing::warn!(%error, place = %placed.place, "could not read a placed agent's folder");
                None
            }
            Err(_) => {
                tracing::warn!(place = %placed.place, "a placed agent's folder was not answered in time");
                None
            }
        };
        if record.is_none() && folder.is_none() {
            return Some(placed);
        }
        match self.placed_remember(&placed, record, folder) {
            Ok(next) => next,
            Err(error) => {
                tracing::warn!(%error, place = %placed.place, "could not keep a refreshed placed agent");
                Some(placed)
            }
        }
    }

    /// Keep what a computer said about `seen` (its record, its folder), on the record as it is
    /// now: an agent ended or moved while the computer answered is not brought back. The
    /// stored record, or `None` when it is gone.
    fn placed_remember(
        &self,
        seen: &Placed,
        record: Option<AgentRecord>,
        folder: Option<(String, bool)>,
    ) -> anyhow::Result<Option<Placed>> {
        remember_in(&self.engine.store, seen, record, folder)
    }

    /// `agent_move {agent, project, place}`: `place` is a link's name, or null for this
    /// computer. Asking for where the agent already is changes nothing.
    pub(super) async fn agent_move(self: &Arc<Self>, params: &Value) -> anyhow::Result<Value> {
        let agent = params["agent"].as_str().unwrap_or("").to_string();
        let project = self.top_project(params["project"].as_str().unwrap_or(""))?;
        let to = params["place"].as_str().filter(|p| !p.is_empty());
        let from = self.placed_get(project.id.as_str(), &agent);
        let mut session = None;
        match (from, to) {
            (Some(at), Some(to)) if at.place == to => {
                self.settle_away(&project, &agent).await?;
                session = Some(at.session);
            }
            (Some(at), Some(to)) => {
                self.bring_back(&project, at).await?;
                session = Some(self.send_away(&project, &agent, to).await?);
            }
            (Some(at), None) => self.bring_back(&project, at).await?,
            (None, Some(to)) => session = Some(self.send_away(&project, &agent, to).await?),
            (None, None) => {}
        }
        Ok(json!({ "agent": agent, "place": to, "session": session }))
    }

    async fn local_agent(&self, project: &Project, agent: &str) -> anyhow::Result<AgentRecord> {
        self.siblings
            .project_orchestrator(&project.id)?
            .agents()
            .into_iter()
            .find(|a| a.name == agent)
            .ok_or_else(|| anyhow::anyhow!("no agent `{agent}` in `{}`", project.id))
    }

    /// End the agent here once a computer holds it (also finishes a move that stopped there).
    async fn settle_away(&self, project: &Project, agent: &str) -> anyhow::Result<()> {
        let o = self.siblings.project_orchestrator(&project.id)?;
        if self.local_agent(project, agent).await?.state != AgentState::Terminated {
            self.control_on(
                &o,
                Control::Terminate {
                    agent: agent.to_string(),
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn send_away(
        self: &Arc<Self>,
        project: &Project,
        agent: &str,
        place: &str,
    ) -> anyhow::Result<String> {
        let local = self.local_agent(project, agent).await?;
        anyhow::ensure!(
            local.state != AgentState::Terminated,
            "no agent `{agent}` in `{}`",
            project.id
        );
        let chat = self.place_anchor(place, project).await?;
        let mut there = self.place_agents(&chat).await?;
        if !there
            .iter()
            .any(|a| a.name == agent && a.state != AgentState::Terminated)
        {
            let mut spec = local.spec.clone();
            if !self.place_offers(&chat, spec.model.as_deref()).await {
                spec.model = None;
            }
            let spawned = self
                .place_op(
                    &chat,
                    ResourceOperationKind::SpawnAgent,
                    json!({ "spec": spec }),
                )
                .await?;
            there = vec![serde_json::from_value(spawned)?];
        }
        let record = there
            .into_iter()
            .find(|a| a.name == agent)
            .ok_or_else(|| anyhow::anyhow!("{place} did not start `{agent}`"))?;
        let folder = match self.place_folder(&chat, agent, None).await {
            Ok(folder) => Some(folder),
            Err(error) => {
                tracing::warn!(%error, %place, "could not read a moved agent's folder");
                None
            }
        };
        self.placed_keep(&Placed {
            project: project.id.as_str().to_string(),
            place: place.to_string(),
            session: chat.session.clone(),
            record,
            own_folder: folder.as_ref().is_some_and(|f| f.1),
            folder: folder.map(|f| f.0),
        })?;
        self.settle_away(project, agent).await?;
        Ok(chat.session)
    }

    async fn bring_back(&self, project: &Project, placed: Placed) -> anyhow::Result<()> {
        let agent = placed.record.name.clone();
        let chat = self
            .held_chat(&placed.session)
            .ok_or_else(|| anyhow::anyhow!("{} no longer holds `{agent}`", placed.place))?;
        let there = self
            .place_agents(&chat)
            .await?
            .into_iter()
            .find(|a| a.name == agent && a.state != AgentState::Terminated);
        let spec = there
            .as_ref()
            .map_or(placed.record.spec.clone(), |a| a.spec.clone());
        let o = self.siblings.project_orchestrator(&project.id)?;
        let here = self.local_agent(project, &agent).await;
        if here.map_or(true, |a| a.state == AgentState::Terminated) {
            self.control_on(&o, Control::Spawn(Box::new(spec))).await?;
        }
        if there.is_some() {
            self.place_op(
                &chat,
                ResourceOperationKind::TerminateAgent,
                json!({ "agent": agent }),
            )
            .await?;
        }
        self.placed_forget(project.id.as_str(), &agent)?;
        Ok(())
    }

    /// Delete an agent that runs on another computer: it ends there and is forgotten here.
    pub(super) async fn placed_end(&self, project: &str, agent: &str) -> anyhow::Result<bool> {
        let Some(placed) = self.placed_get(project, agent) else {
            return Ok(false);
        };
        let chat = self
            .held_chat(&placed.session)
            .ok_or_else(|| anyhow::anyhow!("{} no longer holds `{agent}`", placed.place))?;
        self.place_op(
            &chat,
            ResourceOperationKind::TerminateAgent,
            json!({ "agent": agent }),
        )
        .await?;
        self.placed_forget(project, agent)?;
        Ok(true)
    }
}

/// Every write to and delete of a placed record holds this, so that a refresh's read, check
/// and write cannot interleave with an agent being ended.
static PLACED_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The lock guards no data, so a panic while it was held leaves nothing half written to recover.
fn placed_lock() -> std::sync::MutexGuard<'static, ()> {
    PLACED_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn placed_read(store: &ark_store::Store, project: &str, agent: &str) -> Option<Placed> {
    store
        .get_record(PLACED, &Placed::key(project, agent))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Callers hold [`placed_lock`].
fn placed_write(store: &ark_store::Store, placed: &Placed) -> anyhow::Result<()> {
    store.put_record(
        PLACED,
        &Placed::key(&placed.project, &placed.record.name),
        &placed.project,
        &serde_json::to_value(placed)?,
    )?;
    Ok(())
}

fn placed_delete(store: &ark_store::Store, project: &str, agent: &str) -> anyhow::Result<()> {
    let _held = placed_lock();
    store.delete_record(PLACED, &Placed::key(project, agent))?;
    Ok(())
}

fn remember_in(
    store: &ark_store::Store,
    seen: &Placed,
    record: Option<AgentRecord>,
    folder: Option<(String, bool)>,
) -> anyhow::Result<Option<Placed>> {
    let _held = placed_lock();
    let now = placed_read(store, &seen.project, &seen.record.name);
    let Some(next) = refreshed(now, seen, record, folder) else {
        return Ok(None);
    };
    placed_write(store, &next)?;
    Ok(Some(next))
}

/// `now` (the record as it is stored) with what the computer said, or `None` when the agent
/// is no longer placed where `seen` says: ended, moved back or moved elsewhere meanwhile.
fn refreshed(
    now: Option<Placed>,
    seen: &Placed,
    record: Option<AgentRecord>,
    folder: Option<(String, bool)>,
) -> Option<Placed> {
    let mut next = now.filter(|n| n.place == seen.place && n.session == seen.session)?;
    if let Some(record) = record {
        next.record = record;
    }
    if let Some((path, own)) = folder {
        next.folder = Some(path);
        next.own_folder = own;
    }
    Some(next)
}

/// A computer that does not know the folder operation answers with a decoding error: say what
/// the person can do instead.
fn folder_error(place: &str, error: anyhow::Error) -> anyhow::Error {
    if error.to_string().contains("unknown variant") {
        anyhow::anyhow!("Update ARK on {place} to change its folders")
    } else {
        error
    }
}

/// Whether `list`, a computer's answer to `chat_models` (for `provider:model`) or
/// `chat_worker_offers` (for `agent:` programs), offers `spec`. A model's effort (`@medium`)
/// is not part of the name.
fn spec_offered(spec: &str, list: &Value) -> bool {
    let Some(rows) = list.as_array() else {
        return true;
    };
    if spec.starts_with("agent:") {
        return rows.iter().any(|offer| {
            offer["state"] == "ready"
                && (offer["spec"] == spec
                    || offer["models"]
                        .as_array()
                        .is_some_and(|m| m.iter().any(|m| m["spec"] == spec)))
        });
    }
    let spec = spec.split('@').next().unwrap_or(spec);
    let (provider, model) = spec.split_once(':').unwrap_or((spec, ""));
    rows.iter().any(|backend| {
        backend["provider"] == provider
            && backend["ready"] == true
            && (model.is_empty()
                || backend["models"].as_array().is_some_and(|models| {
                    models
                        .iter()
                        .filter_map(|m| m.as_str())
                        .any(|m| m == model || m.strip_suffix(":latest") == Some(model))
                }))
    })
}

#[cfg(test)]
mod tests {
    use super::{folder_error, refreshed, spec_offered, Placed};
    use ark_interact::ark_orchestrate::{AgentRecord, AgentSpec, AgentState};
    use serde_json::json;

    fn placed(place: &str, session: &str) -> Placed {
        let record: AgentRecord = serde_json::from_value(json!({
            "name": "scout", "project": "p", "scope": "p", "created_ms": 0, "spec": AgentSpec::new("scout"),
            "state": AgentState::Idle,
        }))
        .unwrap();
        Placed {
            project: "p".into(),
            place: place.into(),
            session: session.into(),
            record,
            folder: None,
            own_folder: false,
        }
    }

    #[test]
    fn a_refresh_never_brings_back_an_agent_that_left_meanwhile() {
        let seen = placed("vps", "s1");
        assert!(refreshed(None, &seen, None, Some(("/a".into(), true))).is_none());
        assert!(refreshed(Some(placed("vps", "s2")), &seen, None, None).is_none());
        assert!(refreshed(Some(placed("mac", "s1")), &seen, None, None).is_none());
        let kept = refreshed(
            Some(placed("vps", "s1")),
            &seen,
            None,
            Some(("/a".into(), true)),
        )
        .unwrap();
        assert_eq!(kept.folder.as_deref(), Some("/a"));
        assert!(kept.own_folder);
    }

    #[test]
    fn an_old_computer_is_told_to_update_for_folders() {
        // What a computer without the operation says: its own decoding of the operation.
        let real = serde_json::from_value::<crate::resource::ResourceOperation>(
            json!({ "kind": "agent_folder_from_the_future", "input": {} }),
        )
        .unwrap_err();
        let old = anyhow::Error::new(real);
        assert!(old.to_string().contains("unknown variant"), "{old}");
        assert_eq!(
            folder_error("vps", old).to_string(),
            "Update ARK on vps to change its folders"
        );
        let other = anyhow::anyhow!("vps is not answering");
        assert_eq!(
            folder_error("vps", other).to_string(),
            "vps is not answering"
        );
    }

    #[test]
    fn a_refresh_after_an_end_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ark_store::Store::open(dir.path()).unwrap();
        let seen = placed("vps", "s1");
        super::placed_write(&store, &seen).unwrap();
        let kept = super::remember_in(&store, &seen, None, Some(("/a".into(), true)))
            .unwrap()
            .expect("still placed");
        assert_eq!(kept.folder.as_deref(), Some("/a"));

        super::placed_delete(&store, "p", "scout").unwrap();
        let gone = super::remember_in(&store, &seen, None, Some(("/b".into(), true))).unwrap();
        assert!(gone.is_none());
        assert!(
            super::placed_read(&store, "p", "scout").is_none(),
            "not re-inserted"
        );
    }

    #[test]
    fn a_model_is_offered_only_when_its_backend_is_ready_and_lists_it() {
        let models = json!([
            {"provider":"codex","ready":true,"models":["gpt-6-luna"]},
            {"provider":"ollama","ready":true,"models":["qwen3:4b-instruct:latest"]},
            {"provider":"claude","ready":false,"models":["sonnet"]},
        ]);
        assert!(spec_offered("codex:gpt-6-luna@medium", &models));
        assert!(spec_offered("codex", &models));
        assert!(spec_offered("ollama:qwen3:4b-instruct", &models));
        assert!(!spec_offered("codex:gpt-9", &models));
        assert!(!spec_offered("claude:sonnet", &models));
        assert!(!spec_offered("openai:gpt-5", &models));
        assert!(spec_offered("codex:anything", &json!({"not": "a list"})));
    }

    #[test]
    fn a_program_is_offered_when_ready() {
        let offers = json!([
            {"spec":"agent:test","state":"ready","models":[{"spec":"agent:test:big"}]},
            {"spec":"agent:other","state":"missing","models":[]},
        ]);
        assert!(spec_offered("agent:test", &offers));
        assert!(spec_offered("agent:test:big", &offers));
        assert!(!spec_offered("agent:other", &offers));
    }
}
