use ark_sdk::prelude::ark_core::{MemorySource, MemoryState, ProjectMemory};
use ark_sdk::prelude::ark_runtime;

use crate::{
    command::{Command, Reply},
    interaction::Interaction,
    report::{Report, Row, Table, Tone},
};

pub struct Memory;

fn state(memory: &ProjectMemory) -> &'static str {
    match memory.state {
        MemoryState::Pending => "Needs review",
        MemoryState::Active => "Remembered",
        MemoryState::Rejected => "Rejected",
        MemoryState::Superseded { .. } => "Replaced",
        MemoryState::Forgotten => "Forgotten",
    }
}

fn show(memory: &ProjectMemory) -> Report {
    let mut report = Report::titled("Project memory")
        .field("ID", memory.id.to_string())
        .field("State", state(memory));
    if memory.state == MemoryState::Forgotten {
        return report;
    }
    report = report
        .field("Topic", &memory.topic)
        .text(&memory.body)
        .field(
            "Source",
            match &memory.source {
                MemorySource::Person { session } => format!("Your correction in chat {session}"),
                MemorySource::Agent { invocation } => format!("Suggested from [inv:{invocation}]"),
            },
        );
    if let Some(id) = &memory.replaces {
        report = report.field("Replaces", id.to_string());
    }
    if let MemoryState::Superseded { by } = &memory.state {
        report = report.action("Show replacement", format!("/memory show {by}"));
    }
    if memory.state == MemoryState::Pending {
        report = report
            .action("Accept", format!("/memory accept {}", memory.id))
            .action("Reject", format!("/memory reject {}", memory.id));
    }
    report.action("Forget", format!("/memory forget {}", memory.id))
}

#[async_trait::async_trait]
impl Command for Memory {
    meta!("memory", "Review and remember project corrections");
    fn usage(&self) -> &str {
        "[show|accept|reject|forget <id> | remember <topic> | <correction> | replace <id> | <correction>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        use ark_runtime::memory::MemoryReview;
        let store = &ix.engine().store;
        let project = &ix.project().id;
        let reviewer = ix.session().id();
        let (verb, rest) = args
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((args.trim(), ""));
        match verb {
            "" | "list" => {
                let memories = store.project_memories(project)?;
                let mut table = Table::with_header(&["Topic", "State"]);
                for m in memories.iter().filter(|m| matches!(m.state, MemoryState::Active | MemoryState::Pending)) {
                    table.push(Row { cells: vec![m.topic.clone().into(), state(m).into()], tone: Tone::Normal, command: Some(format!("/memory show {}",m.id)) });
                }
                let mut report = Report::titled("Project memory");
                if table.rows.is_empty() { report = report.text("No project memories yet."); }
                else { report = report.table(table); }
                Ok(report.note("/memory remember topic | correction").into())
            }
            "show" => Ok(show(&store.project_memory(project, rest.trim())?).into()),
            "remember" | "replace" => {
                let (key, correction) = rest.split_once('|').ok_or_else(|| anyhow::anyhow!("Use /memory {verb} {} | correction", if verb == "remember" { "topic" } else { "id" }))?;
                let previous = if verb == "replace" { Some(store.project_memory(project, key.trim())?) } else { None };
                let topic = previous.as_ref().map(|m| m.topic.as_str()).unwrap_or(key.trim());
                let memory = store.record_memory(project, topic, correction, MemorySource::Person { session: reviewer.clone() }, previous.as_ref().map(|m| m.id.clone()))?;
                Ok(show(&memory).into())
            }
            "accept" | "reject" | "forget" => {
                let review = match verb { "accept" => MemoryReview::Accept, "reject" => MemoryReview::Reject, _ => MemoryReview::Forget };
                Ok(show(&store.review_memory(project, rest.trim(), review, reviewer)?).into())
            }
            _ => anyhow::bail!("Use /memory, /memory show id, /memory remember topic | correction, /memory replace id | correction, /memory accept id, /memory reject id, or /memory forget id"),
        }
    }
}
