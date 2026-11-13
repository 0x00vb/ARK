//! Groups of agents made from a person's chat (`groups.set`, ADR 0117).

use super::*;
use crate::group::{Group, Member, Outcome, Request};

/// What a call decided about one member, before anything is written.
enum Seat {
    Creates(Box<AgentSpec>),
    Joins(String),
    Stays,
}

/// What a group call that failed leaves on the agents it made and ended, so only the same
/// group's call makes them again; an agent the person ended keeps `group:<id>`.
fn undone(id: &str) -> String {
    format!("group-undone:{id}")
}

impl Inner {
    /// Make a group, or update the one this folder already has under that name: the rules
    /// become the call's list, the members listed are added, the others stay.
    pub(crate) async fn set_group(
        &self,
        from: &str,
        task: &str,
        request: Request,
    ) -> Result<Outcome, OrchError> {
        let book = RuleBook::new(&self.engine.store);
        let missing: Vec<String> = {
            let st = self.state.lock().unwrap();
            request
                .members
                .iter()
                .filter(|m| !st.agents.contains_key(&m.name))
                .filter_map(|m| m.selection.model.clone())
                .collect()
        };
        for model in missing {
            self.models.validate_selection(&model).await?;
        }
        let _guard = self.conversations.lock().await;
        let known = self.group_named(&request.name)?;
        let id = known
            .as_ref()
            .map_or_else(|| request.id(self.project.id.as_str()), |g| g.id.clone());
        let set_name = group::rule_set_name(&id);
        let current = book.get(&set_name, None)?;
        let held: Vec<String> = current
            .iter()
            .flat_map(|set| set.rules.iter().map(|r| r.text.clone()))
            .collect();
        let rules = request.rules.clone().unwrap_or_else(|| held.clone());
        let rules_changed = rules != held;
        let next = group::rule_set(&id, &request.name, &rules);
        let (seats, root, members) = {
            let st = self.state.lock().unwrap();
            let source = st
                .agents
                .get(from)
                .ok_or_else(|| OrchError::UnknownAgent(from.into()))?;
            let may = |action: &str| {
                source
                    .record
                    .spec
                    .tools
                    .iter()
                    .any(|p| action_matches(p, action))
            };
            if !may(tools::GROUPS_SET) {
                return Err(OrchError::NotAllowed(
                    "this agent may not make groups".into(),
                ));
            }
            let room = self.creation_room(&st, from, task)?;
            let mut members: Vec<Member> = known
                .iter()
                .flat_map(|g| g.members.iter().cloned())
                .collect();
            for m in &request.members {
                let member = Member::new(self.project.id.as_str(), &m.name);
                if !members.contains(&member) {
                    members.push(member);
                }
            }
            if members.len() < 2 {
                return Err(OrchError::NotAllowed(
                    "a group needs two or more agents".into(),
                ));
            }
            let mut seats = Vec::new();
            for m in &request.members {
                let Some(a) = st.agents.get(&m.name) else {
                    if m.role.is_empty() {
                        return Err(OrchError::NotAllowed(format!(
                            "`{}` is a new agent: give it a role",
                            m.name
                        )));
                    }
                    let mut spec =
                        source
                            .record
                            .spec
                            .colleague(&m.name, &m.role, "", &m.personality);
                    spec.model = m.selection.apply(Some(&room.inherited))?;
                    seats.push(Seat::Creates(Box::new(spec)));
                    continue;
                };
                if a.record.state == AgentState::Terminated {
                    // An agent a failed call of this group made and ended is made again.
                    if a.record.creation_request.as_deref() == Some(&undone(&id))
                        && !m.role.is_empty()
                    {
                        let mut spec =
                            source
                                .record
                                .spec
                                .colleague(&m.name, &m.role, "", &m.personality);
                        spec.model = m.selection.apply(Some(&room.inherited))?;
                        seats.push(Seat::Creates(Box::new(spec)));
                        continue;
                    }
                    return Err(OrchError::Terminated(m.name.clone()));
                }
                seats.push(if a.record.spec.groups.contains(&id) {
                    Seat::Stays
                } else {
                    Seat::Joins(m.name.clone())
                });
            }
            // Members already on the group's record (a group made in the window) join as well,
            // so every member has the same contacts and rules.
            for m in &members {
                if m.project != self.project.id.as_str()
                    || request.members.iter().any(|r| r.name == m.agent)
                {
                    continue;
                }
                if let Some(a) = st.agents.get(&m.agent) {
                    if a.record.state != AgentState::Terminated
                        && !a.record.spec.groups.contains(&id)
                    {
                        seats.push(Seat::Joins(m.agent.clone()));
                    }
                }
            }
            // Whoever follows the new rules: the group's members here and those already in it.
            for a in st.agents.values() {
                let listed = members
                    .iter()
                    .any(|m| m.project == self.project.id.as_str() && m.agent == a.record.name);
                if a.record.state == AgentState::Terminated
                    || !(listed || a.record.spec.groups.contains(&id))
                {
                    continue;
                }
                let mut count = rules.len();
                for other in a.record.spec.groups.iter().filter(|g| **g != id) {
                    count += book
                        .get(&group::rule_set_name(other), None)?
                        .map_or(0, |set| set.rules.len());
                }
                if count > group::MAX_AGENT_RULES {
                    return Err(OrchError::NotAllowed(format!(
                        "`{}` would follow {count} rules; at most {}",
                        a.record.name,
                        group::MAX_AGENT_RULES
                    )));
                }
            }
            let creates = seats
                .iter()
                .filter(|s| matches!(s, Seat::Creates(_)))
                .count();
            if creates > 0 && !may(tools::CREATE) {
                return Err(OrchError::NotAllowed(
                    "this agent may not create agents".into(),
                ));
            }
            if room.used + creates > room.limit {
                return Err(OrchError::NotAllowed(format!(
                    "this request may create {} agents",
                    room.limit
                )));
            }
            (seats, room.root, members)
        };

        let creation = format!("group:{id}");
        let mut outcome = Outcome {
            group: id.clone(),
            name: request.name.clone(),
            members: members.iter().map(|m| m.agent.clone()).collect(),
            created: Vec::new(),
            joined: Vec::new(),
            rules_changed,
        };
        // Everything that can still fail runs first: the new agents. If one cannot be made,
        // those made by this call end and nothing else is written.
        let mut joins = Vec::new();
        for seat in seats {
            match seat {
                Seat::Creates(mut spec) => {
                    spec.groups = vec![id.clone()];
                    let spec = *spec;
                    let name = spec.name.clone();
                    if let Err(e) = self
                        .spawn_record(spec, Some((task, &root)), Some(&creation), None, None)
                        .await
                    {
                        for made in &outcome.created {
                            match self.terminate(made).await {
                                Ok(()) => self.set_creation_request(made, &undone(&id)),
                                Err(undo) => {
                                    tracing::warn!(agent = %made, error = %undo, "could not end a member of a failed group call")
                                }
                            }
                        }
                        return Err(e);
                    }
                    self.set_creation_request(&name, &creation);
                    outcome.created.push(name);
                }
                Seat::Joins(name) => joins.push(name),
                Seat::Stays => {}
            }
        }
        for name in joins {
            self.edit_groups(&name, |groups| groups.push(id.clone()))?;
            outcome.joined.push(name);
        }
        if rules_changed {
            book.put(&next)?;
        }
        let now = now_ms();
        let mut record = known.unwrap_or_else(|| Group {
            id: id.clone(),
            name: request.name.clone(),
            members: Vec::new(),
            threads: Default::default(),
            cursors: Default::default(),
            created_ms: now,
            updated_ms: now,
        });
        if record.members != members {
            record.members = members;
            record.updated_ms = now;
            self.put_group(&record)?;
        }
        Ok(outcome)
    }

    /// The group with this name that has an agent of this folder, whoever made it.
    fn group_named(&self, name: &str) -> Result<Option<Group>, OrchError> {
        for value in self
            .engine
            .store
            .list_records(group::RECORD, Some(group::SCOPE))?
        {
            let Ok(found) = serde_json::from_value::<Group>(value) else {
                continue;
            };
            if found.name.eq_ignore_ascii_case(name)
                && found
                    .members
                    .iter()
                    .any(|m| m.project == self.project.id.as_str())
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    pub(crate) fn put_group(&self, group: &Group) -> Result<(), OrchError> {
        self.engine.store.put_record(
            group::RECORD,
            &group.id,
            group::SCOPE,
            &serde_json::to_value(group)?,
        )?;
        self.emit(EventKind::Group {
            group: group.id.clone(),
        });
        Ok(())
    }

    /// Change the group ids of an agent of this folder, and keep the record.
    pub(crate) fn edit_groups(
        &self,
        name: &str,
        edit: impl FnOnce(&mut Vec<String>),
    ) -> Result<(), OrchError> {
        let record = {
            let mut st = self.state.lock().unwrap();
            let a = st
                .agents
                .get_mut(name)
                .ok_or_else(|| OrchError::UnknownAgent(name.into()))?;
            edit(&mut a.record.spec.groups);
            a.record.updated_ms = now_ms();
            a.record.clone()
        };
        self.save_agent(&record);
        self.emit(EventKind::AgentSpawned {
            agent: name.to_string(),
            parent: record.spec.parent.clone(),
            model: record.model.clone(),
        });
        Ok(())
    }
}
