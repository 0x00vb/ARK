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
}

impl Placed {
    fn key(project: &str, agent: &str) -> String {
        format!("{project}:{agent}")
    }
}

impl Host {
    fn placed_get(&self, project: &str, agent: &str) -> Option<Placed> {
        self.engine
            .store
            .get_record(PLACED, &Placed::key(project, agent))
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
    }

    fn placed_keep(&self, placed: &Placed) -> anyhow::Result<()> {
        self.engine.store.put_record(
            PLACED,
            &Placed::key(&placed.project, &placed.record.name),
            &placed.project,
            &serde_json::to_value(placed)?,
        )?;
        Ok(())
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
        for placed in self.placed_all() {
            let placed = self.placed_fresh(placed).await;
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
                "folder": project.workspace,
                "own_folder": false,
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

    /// `placed` as its computer says it is now; as remembered when it does not answer.
    async fn placed_fresh(&self, mut placed: Placed) -> Placed {
        let Some(chat) = self.held_chat(&placed.session) else {
            return placed;
        };
        if self.links.state(&chat.place) != Some(State::Ok) {
            return placed;
        }
        let Ok(Ok(agents)) = tokio::time::timeout(READ, self.place_agents(&chat)).await else {
            return placed;
        };
        if let Some(now) = agents.into_iter().find(|a| a.name == placed.record.name) {
            placed.record = now;
            let _ = self.placed_keep(&placed);
        }
        placed
    }

    /// `agent_move {agent, project, place}`: `place` is a link's name, or null for this
    /// computer. Asking for where the agent already is changes nothing.
    pub(super) async fn agent_move(self: &Arc<Self>, params: &Value) -> anyhow::Result<Value> {
        let agent = params["agent"].as_str().unwrap_or("").to_string();
        let project = self.top_project(params["project"].as_str().unwrap_or(""))?;
        let to = params["place"].as_str().filter(|p| !p.is_empty());
        let from = self.placed_get(project.id.as_str(), &agent);
        match (from, to) {
            (Some(at), Some(to)) if at.place == to => self.settle_away(&project, &agent).await?,
            (Some(at), Some(to)) => {
                self.bring_back(&project, at).await?;
                self.send_away(&project, &agent, to).await?;
            }
            (Some(at), None) => self.bring_back(&project, at).await?,
            (None, Some(to)) => self.send_away(&project, &agent, to).await?,
            (None, None) => {}
        }
        Ok(json!({ "agent": agent, "place": to }))
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
    ) -> anyhow::Result<()> {
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
            let spawned = self
                .place_op(
                    &chat,
                    ResourceOperationKind::SpawnAgent,
                    json!({ "spec": local.spec }),
                )
                .await?;
            there = vec![serde_json::from_value(spawned)?];
        }
        let record = there
            .into_iter()
            .find(|a| a.name == agent)
            .ok_or_else(|| anyhow::anyhow!("{place} did not start `{agent}`"))?;
        self.placed_keep(&Placed {
            project: project.id.as_str().to_string(),
            place: place.to_string(),
            session: chat.session,
            record,
        })?;
        self.settle_away(project, agent).await
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
        self.engine
            .store
            .delete_record(PLACED, &Placed::key(project.id.as_str(), &agent))?;
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
        self.engine
            .store
            .delete_record(PLACED, &Placed::key(project, agent))?;
        Ok(true)
    }
}
