//! Group chats (ADR 0113): the window calls and the driver that runs a person's message
//! through the members. The rules are in `ark_interact::groups`.

use super::*;
use ark_interact::ark_orchestrate::group::rule_set_name;
use ark_interact::ark_orchestrate::{AgentState, Orchestrator, TaskSpec, TaskState};
use ark_interact::groups::{
    brief, context, from_router, hand_on, route, router_prompt, sortable_id, Candidate, Group,
    GroupMessage, Member, Route, MAX_REPLIES, SCOPE,
};
use ark_model::Message;
use ark_sdk::prelude::ark_runtime::{Post, PostKind, Role, RuleBook};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

const SHOWN: usize = 200;
const ROUTER_WAIT: Duration = Duration::from_secs(15);

/// What the driver keeps in memory: a queue per group and who is answering.
#[derive(Default)]
pub(crate) struct GroupState {
    locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    working: std::sync::Mutex<HashMap<String, Vec<Member>>>,
}

fn short(reason: &str) -> String {
    let line = reason.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(120).collect();
    if out.is_empty() {
        out = "no reply".into();
    }
    out
}

impl Host {
    pub(crate) async fn group_call(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
    ) -> anyhow::Result<Value> {
        let text = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let members = || -> anyhow::Result<Vec<Member>> {
            serde_json::from_value(params["members"].clone())
                .map_err(|_| anyhow::anyhow!("members is a list of {{project, agent}}"))
        };
        match method {
            "groups" => {
                let mut rows = Vec::new();
                for g in self.group_list()? {
                    rows.push(self.group_row(&g)?);
                }
                rows.sort_by_key(|r| std::cmp::Reverse(r["updated_ms"].as_u64().unwrap_or(0)));
                Ok(Value::Array(rows))
            }
            "group_create" => {
                let name = text("name");
                anyhow::ensure!(!name.is_empty(), "a group needs a name");
                let members = self.live_members(members()?)?;
                let now = crate::notify::now_ms();
                let group = Group {
                    id: sortable_id("g-", now),
                    name,
                    members,
                    threads: Default::default(),
                    cursors: Default::default(),
                    created_ms: now,
                    updated_ms: now,
                };
                self.group_put(&group)?;
                self.group_row(&group)
            }
            "group_update" => {
                let mut group = self.group_get(&text("id"))?;
                if params.get("name").is_some() {
                    let name = text("name");
                    anyhow::ensure!(!name.is_empty(), "a group needs a name");
                    group.name = name;
                }
                if params.get("members").is_some() {
                    let before = std::mem::take(&mut group.members);
                    group.members = self.live_members(members()?)?;
                    let keys: Vec<String> = group.members.iter().map(Member::key).collect();
                    group.threads.retain(|k, _| keys.contains(k));
                    group.cursors.retain(|k, _| keys.contains(k));
                    let left: Vec<Member> = before
                        .into_iter()
                        .filter(|m| !group.members.contains(m))
                        .collect();
                    self.group_leave(&group.id, &left)?;
                }
                self.group_put(&group)?;
                self.emitter().emit("ark:group", json!({ "id": group.id }));
                self.group_row(&group)
            }
            "group_delete" => {
                let group = self.group_get(&text("id"))?;
                let store = &self.engine.store;
                // Contact and rules end first; a failure leaves the group to delete again.
                self.group_leave(&group.id, &group.members)?;
                RuleBook::new(store).remove(&rule_set_name(&group.id))?;
                for m in self.group_msgs(&group.id)? {
                    store.delete_record("group_msg", &m.id)?;
                }
                store.delete_record("group", &group.id)?;
                self.groups.locks.lock().unwrap().remove(&group.id);
                self.emitter().emit("ark:group", json!({ "id": group.id }));
                Ok(json!({}))
            }
            "group_messages" => {
                let group = self.group_get(&text("id"))?;
                let all = self.group_msgs(&group.id)?;
                let from = all.len().saturating_sub(SHOWN);
                let working = self.groups.working.lock().unwrap().get(&group.id).cloned();
                Ok(json!({ "messages": &all[from..], "working": working.unwrap_or_default() }))
            }
            "group_send" => self.group_send(&text("id"), &text("text")).await,
            _ => anyhow::bail!("unknown group call `{method}`"),
        }
    }

    fn group_list(&self) -> anyhow::Result<Vec<Group>> {
        Ok(self
            .engine
            .store
            .list_records("group", Some(SCOPE))?
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    fn group_get(&self, id: &str) -> anyhow::Result<Group> {
        let v = self
            .engine
            .store
            .get_record("group", id)?
            .ok_or_else(|| anyhow::anyhow!("there is no group `{id}`"))?;
        Ok(serde_json::from_value(v)?)
    }

    fn group_put(&self, group: &Group) -> anyhow::Result<()> {
        self.engine
            .store
            .put_record("group", &group.id, SCOPE, &serde_json::to_value(group)?)?;
        Ok(())
    }

    /// Take the group off these members: their contact and rules end with the membership.
    fn group_leave(&self, id: &str, members: &[Member]) -> anyhow::Result<()> {
        let mut by_project: HashMap<&str, Vec<String>> = HashMap::new();
        for m in members {
            by_project
                .entry(m.project.as_str())
                .or_default()
                .push(m.agent.clone());
        }
        for (project, agents) in by_project {
            // A project that is gone has no agent left to take it off.
            if let Ok(o) = self.group_orchestrator(project) {
                o.leave_group(id, &agents)?;
            }
        }
        Ok(())
    }

    fn group_edit(&self, id: &str, f: impl FnOnce(&mut Group)) -> anyhow::Result<()> {
        let mut group = self.group_get(id)?;
        f(&mut group);
        self.group_put(&group)
    }

    fn group_msgs(&self, group: &str) -> anyhow::Result<Vec<GroupMessage>> {
        let mut all: Vec<GroupMessage> = self
            .engine
            .store
            .list_records("group_msg", Some(group))?
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all)
    }

    fn group_store_msg(
        &self,
        group: &str,
        from: Option<Member>,
        text: &str,
        failed: bool,
    ) -> anyhow::Result<GroupMessage> {
        // A group deleted while a member was answering takes no more messages.
        self.group_get(group)?;
        let at_ms = crate::notify::now_ms();
        let msg = GroupMessage {
            id: sortable_id("m-", at_ms),
            group: group.into(),
            from,
            text: text.into(),
            at_ms,
            failed,
        };
        self.engine
            .store
            .put_record("group_msg", &msg.id, group, &serde_json::to_value(&msg)?)?;
        self.group_edit(group, |g| g.updated_ms = at_ms)?;
        Ok(msg)
    }

    fn group_orchestrator(&self, project: &str) -> anyhow::Result<Orchestrator> {
        let top = self
            .engine
            .store
            .get_project(&ProjectId(project.to_string()))?
            .filter(|p| p.parent.is_none())
            .ok_or_else(|| anyhow::anyhow!("there is no project `{project}` here"))?;
        self.siblings.project_orchestrator(&top.id)
    }

    /// The members that exist and are not ended, with their names and roles.
    fn live_roster(&self, members: &[Member]) -> Vec<Candidate> {
        let mut by_project: HashMap<String, Option<Vec<_>>> = HashMap::new();
        let mut out = Vec::new();
        for m in members {
            let agents = by_project
                .entry(m.project.clone())
                .or_insert_with(|| self.group_orchestrator(&m.project).ok().map(|o| o.agents()));
            let live = agents
                .iter()
                .flatten()
                .find(|a| a.name == m.agent && a.state != AgentState::Terminated);
            if let Some(a) = live {
                out.push(Candidate {
                    member: m.clone(),
                    name: if a.spec.title.is_empty() {
                        a.name.clone()
                    } else {
                        a.spec.title.clone()
                    },
                    role: a.spec.role.clone(),
                });
            }
        }
        out
    }

    /// A new member list: no duplicates, two or more live agents.
    fn live_members(&self, members: Vec<Member>) -> anyhow::Result<Vec<Member>> {
        let mut unique: Vec<Member> = Vec::new();
        for m in members {
            if !unique.contains(&m) {
                unique.push(m);
            }
        }
        let live: Vec<Member> = self
            .live_roster(&unique)
            .into_iter()
            .map(|c| c.member)
            .collect();
        anyhow::ensure!(
            live.len() == unique.len() && live.len() >= 2,
            "a group needs two or more live agents"
        );
        Ok(live)
    }

    fn group_row(&self, group: &Group) -> anyhow::Result<Value> {
        let members: Vec<Member> = self
            .live_roster(&group.members)
            .into_iter()
            .map(|c| c.member)
            .collect();
        let last = self
            .group_msgs(&group.id)?
            .pop()
            .map(|m| json!({ "from": m.from, "text": m.text }));
        let mut row = json!({
            "id": group.id,
            "name": group.name,
            "members": members,
            "updated_ms": group.updated_ms,
        });
        if let Some(last) = last {
            row["last"] = last;
        }
        Ok(row)
    }

    async fn group_send(self: &Arc<Self>, id: &str, text: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(!text.is_empty(), "a message needs text");
        let group = self.group_get(id)?;
        let roster = self.live_roster(&group.members);
        anyhow::ensure!(!roster.is_empty(), "this group has no live agents");
        let message = self.group_store_msg(id, None, text, false)?;
        self.emitter().emit("ark:group", json!({ "id": id }));
        let (to, by) = match route(text, &roster) {
            Route::Chosen(to, by) => (to, by),
            Route::AskRouter => {
                let messages = self.group_msgs(id)?;
                let last = messages.iter().rev().find_map(|m| m.from.clone());
                let answer = self
                    .ask_router(&roster, &messages)
                    .await
                    .unwrap_or_default();
                from_router(&answer, &roster, last.as_ref())
            }
        };
        let host = self.clone();
        let gid = id.to_string();
        let first = to.clone();
        tokio::spawn(async move { host.group_run(gid, first).await });
        Ok(json!({ "message": message, "to": to, "by": by }))
    }

    async fn ask_router(&self, roster: &[Candidate], messages: &[GroupMessage]) -> Option<String> {
        let o = self
            .group_orchestrator(&roster.first()?.member.project)
            .ok()?;
        let models = o.models().clone();
        let lane = "group-router";
        let resolved = models.resolve(lane, None).await.ok()?;
        let provider = resolved.router.provider(Role::Classify).clone();
        let (system, user) = router_prompt(roster, messages);
        let answer = tokio::time::timeout(
            ROUTER_WAIT,
            provider.complete(&system, &[Message::user(user)], &[]),
        )
        .await;
        models.close(lane).await;
        Some(answer.ok()?.ok()?.content)
    }

    /// One person message: the chosen members answer one at a time, then whoever a reply
    /// names, up to [`MAX_REPLIES`] replies.
    async fn group_run(self: Arc<Self>, gid: String, to: Vec<Member>) {
        let lock = self
            .groups
            .locks
            .lock()
            .unwrap()
            .entry(gid.clone())
            .or_default()
            .clone();
        let _turn = lock.lock().await;
        let mut queue: VecDeque<Member> = to.into();
        let mut replies = 0;
        while let Some(member) = queue.pop_front() {
            if replies >= MAX_REPLIES {
                break;
            }
            let Some(reply) = self.group_turn(&gid, &member).await else {
                continue;
            };
            replies += 1;
            let Ok(group) = self.group_get(&gid) else {
                return;
            };
            let roster = self.live_roster(&group.members);
            for next in hand_on(&reply, &roster, &member) {
                if !queue.contains(&next) {
                    queue.push_back(next);
                }
            }
        }
    }

    /// An answer a member handed to the background (`work.later`, ADR 0118) lands in the member's
    /// thread of a group chat: show it in the group as that member's next message. Waits for
    /// the group's driver so it does not race its cursors.
    pub(crate) async fn group_posted(self: Arc<Self>, chat: String, post: Post) {
        if post.kind != PostKind::Message || post.text.trim().is_empty() {
            return;
        }
        let Ok(groups) = self.group_list() else {
            return;
        };
        for group in groups {
            let Some(member) = group
                .threads
                .iter()
                .find(|(_, thread)| **thread == chat)
                .and_then(|(key, _)| group.members.iter().find(|m| m.key() == *key))
                .cloned()
            else {
                continue;
            };
            let lock = self
                .groups
                .locks
                .lock()
                .unwrap()
                .entry(group.id.clone())
                .or_default()
                .clone();
            let _turn = lock.lock().await;
            if self
                .group_store_msg(&group.id, Some(member), post.text.trim(), !post.ok)
                .is_ok()
            {
                self.emitter().emit("ark:group", json!({ "id": group.id }));
            }
        }
    }

    fn group_working(&self, gid: &str, member: &Member, on: bool) {
        let mut all = self.groups.working.lock().unwrap();
        let list = all.entry(gid.to_string()).or_default();
        list.retain(|m| m != member);
        if on {
            list.push(member.clone());
        }
        drop(all);
        self.emitter().emit("ark:group", json!({ "id": gid }));
    }

    /// One member's turn. The reply text, or `None` when it did not answer.
    async fn group_turn(&self, gid: &str, member: &Member) -> Option<String> {
        let group = self.group_get(gid).ok()?;
        let roster = self.live_roster(&group.members);
        if !roster.iter().any(|c| &c.member == member) {
            return None;
        }
        self.group_working(gid, member, true);
        let outcome = self.group_ask(&group, &roster, member).await;
        self.group_working(gid, member, false);
        match outcome {
            Ok(reply) => reply,
            Err(reason) => {
                let _ = self.group_store_msg(gid, Some(member.clone()), &short(&reason), true);
                self.emitter().emit("ark:group", json!({ "id": gid }));
                None
            }
        }
    }

    async fn group_ask(
        &self,
        group: &Group,
        roster: &[Candidate],
        member: &Member,
    ) -> Result<Option<String>, String> {
        let gid = &group.id;
        let key = member.key();
        let o = self
            .group_orchestrator(&member.project)
            .map_err(|e| e.to_string())?;
        let thread = match group.threads.get(&key) {
            Some(t) => t.clone(),
            None => {
                let t = o.start_thread(&member.agent).map_err(|e| e.to_string())?;
                let (k, v) = (key.clone(), t.clone());
                self.group_edit(gid, |g| {
                    g.threads.insert(k, v);
                })
                .map_err(|e| e.to_string())?;
                t
            }
        };
        let messages = self.group_msgs(gid).map_err(|e| e.to_string())?;
        // What this turn saw: a message that comes in while it runs reaches the next turn.
        let seen = messages.last().map(|m| m.id.clone());
        let objective = brief(
            roster,
            &messages,
            member,
            group.cursors.get(&key).map(String::as_str),
        );
        if objective.is_empty() {
            return Ok(None);
        }
        let task = o
            .submit(
                &member.agent,
                TaskSpec {
                    objective,
                    context: context(roster, member),
                    thread: Some(thread),
                    group_turn: true,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
        let record = o.wait(&task).await.map_err(|e| e.to_string())?;
        if record.state != TaskState::Done {
            return Err(record
                .error
                .unwrap_or_else(|| format!("the task ended {:?}", record.state)));
        }
        let reply = record.result.unwrap_or_default().trim().to_string();
        if reply.is_empty() {
            return Err("no reply".into());
        }
        let stored = self
            .group_store_msg(gid, Some(member.clone()), &reply, false)
            .map_err(|e| e.to_string())?;
        let _ = self.group_edit(gid, |g| {
            g.cursors.insert(key, seen.unwrap_or(stored.id));
        });
        self.emitter().emit("ark:group", json!({ "id": gid }));
        Ok(Some(reply))
    }
}
