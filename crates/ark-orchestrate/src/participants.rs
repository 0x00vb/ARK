use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Deserialize;

use crate::OrchError;

#[derive(Deserialize)]
struct Assignment {
    id: String,
    project: String,
    owner: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    report_to: Option<String>,
}

/// Agents assigned work for each verified chat, including delegated descendants.
/// Reads durable assignments, so completed work and removed agents retain their identity.
pub fn chat_participants(
    store: &ark_store::Store,
    project: &str,
    chats: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<String>>, OrchError> {
    let records = store.list_records(crate::orchestrator::TASK, Some(project))?;
    let assignments = records
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<Assignment>, _>>()?;
    project_participants(project, chats, assignments)
}

fn project_participants(
    project: &str,
    chats: &BTreeSet<String>,
    assignments: Vec<Assignment>,
) -> Result<BTreeMap<String, Vec<String>>, OrchError> {
    let mut index = BTreeMap::new();
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for assignment in assignments {
        if assignment.project != project || assignment.id.is_empty() || assignment.owner.is_empty()
        {
            return Err(OrchError::NotAllowed(
                "invalid participant assignment".into(),
            ));
        }
        if let Some(parent) = &assignment.parent {
            children
                .entry(parent.clone())
                .or_default()
                .push(assignment.id.clone());
        }
        if let Some(chat) = assignment
            .report_to
            .as_ref()
            .filter(|chat| chats.contains(*chat))
        {
            queue.push_back((assignment.id.clone(), chat.clone()));
        }
        if index.insert(assignment.id.clone(), assignment).is_some() {
            return Err(OrchError::NotAllowed(
                "duplicate participant assignment".into(),
            ));
        }
    }
    let mut visited = BTreeSet::new();
    let mut members: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    while let Some((task, chat)) = queue.pop_front() {
        if !visited.insert((task.clone(), chat.clone())) {
            continue;
        }
        let assignment = &index[&task];
        members
            .entry(chat.clone())
            .or_default()
            .insert(assignment.owner.clone());
        if let Some(descendants) = children.get(&task) {
            queue.extend(
                descendants
                    .iter()
                    .map(|child| (child.clone(), chat.clone())),
            );
        }
    }
    Ok(members
        .into_iter()
        .map(|(chat, names)| (chat, names.into_iter().collect()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(id: &str, owner: &str, parent: Option<&str>, chat: Option<&str>) -> Assignment {
        Assignment {
            id: id.into(),
            project: "p".into(),
            owner: owner.into(),
            parent: parent.map(str::to_owned),
            report_to: chat.map(str::to_owned),
        }
    }

    #[test]
    fn ancestry_preserves_both_reporting_chats_and_deduplicates_owners() {
        let chats = BTreeSet::from(["human".into(), "agent-chat".into()]);
        let result = project_participants(
            "p",
            &chats,
            vec![
                assignment("a", "Lead", None, Some("human")),
                assignment("b", "Writer", Some("a"), Some("agent-chat")),
                assignment("c", "Writer", Some("b"), None),
                assignment("d", "Unrelated", None, Some("elsewhere")),
            ],
        )
        .unwrap();
        assert_eq!(result["human"], ["Lead", "Writer"]);
        assert_eq!(result["agent-chat"], ["Writer"]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn cycles_missing_parents_and_legacy_records_do_not_invent_members() {
        let chats = BTreeSet::from(["chat".into()]);
        let result = project_participants(
            "p",
            &chats,
            vec![
                assignment("a", "A", Some("b"), Some("chat")),
                assignment("b", "B", Some("a"), None),
                assignment("c", "C", Some("missing"), Some("chat")),
                assignment("d", "Legacy", None, None),
            ],
        )
        .unwrap();
        assert_eq!(result["chat"], ["A", "B", "C"]);
        let mut foreign = assignment("a", "A", None, Some("chat"));
        foreign.project = "other".into();
        assert!(project_participants("p", &chats, vec![foreign]).is_err());
        assert!(project_participants(
            "p",
            &chats,
            vec![
                assignment("a", "A", None, None),
                assignment("a", "B", None, None)
            ]
        )
        .is_err());
    }

    #[test]
    fn deep_delegation_is_iterative() {
        let chats = BTreeSet::from(["chat".into()]);
        let tasks = (0..10_000)
            .map(|i| {
                assignment(
                    &i.to_string(),
                    "Writer",
                    (i > 0).then(|| (i - 1).to_string()).as_deref(),
                    (i == 0).then_some("chat"),
                )
            })
            .collect();
        assert_eq!(
            project_participants("p", &chats, tasks).unwrap()["chat"],
            ["Writer"]
        );
    }

    #[test]
    fn stored_assignments_survive_reopen_without_crossing_projects() {
        let root = tempfile::tempdir().unwrap();
        let write = |store: &ark_store::Store, project: &str, owner: &str| {
            store.put_record(crate::orchestrator::TASK, &format!("{project}-id"), project, &serde_json::json!({
                "id": format!("{project}-id"), "project": project, "owner": owner, "report_to": "chat"
            })).unwrap();
        };
        {
            let store = ark_store::Store::open(root.path()).unwrap();
            write(&store, "p", "Writer");
            write(&store, "other", "Foreign");
        }
        let store = ark_store::Store::open(root.path()).unwrap();
        let chats = BTreeSet::from(["chat".into()]);
        assert_eq!(
            chat_participants(&store, "p", &chats).unwrap()["chat"],
            ["Writer"]
        );
        store
            .put_record(
                crate::orchestrator::TASK,
                "broken",
                "p",
                &serde_json::json!({"id":"broken"}),
            )
            .unwrap();
        assert!(chat_participants(&store, "p", &chats).is_err());
    }
}
