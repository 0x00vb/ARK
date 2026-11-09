//! `conversations.search` and `conversations.read`: a scope's other conversations, bounded,
//! never the current one, never a work or execution conversation, never another scope.
use ark_core::{ArtifactId, ProjectId, SessionId};
use ark_runtime::conversations::{has_other_conversations, ConversationsTool, READ, SEARCH};
use ark_runtime::{CarryPolicy, Engine, SessionRecord};
use ark_tools::{Adapter, AdapterError, BlobResolver, Registry, RunCtx};
use serde_json::{json, Value};

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(AdapterError::ArtifactNotFound(id.to_string()))
    }
}

/// A stored conversation in `project` with these (input, answer) turns.
fn chat(
    engine: &Engine,
    project: &ProjectId,
    title: &str,
    turns: &[(&str, &str)],
    shape: impl FnOnce(&mut SessionRecord),
) -> SessionId {
    let id = SessionId::new();
    let mut record =
        SessionRecord::empty(id.clone(), project.clone(), CarryPolicy::default(), None);
    record.title = title.into();
    record.turn_count = turns.len() as u32;
    shape(&mut record);
    engine
        .store
        .create_session(
            id.as_str(),
            project,
            &serde_json::to_value(&record).unwrap(),
        )
        .unwrap();
    for (i, (input, answer)) in turns.iter().enumerate() {
        engine
            .store
            .put_session_turn(
                id.as_str(),
                i as u32,
                &json!({"index": i, "input": input, "answer": answer, "at": "2026-11-09T10:00:00Z"}),
            )
            .unwrap();
    }
    id
}

async fn call(tool: &ConversationsTool, action: &str, input: Value) -> ark_tools::Result<Value> {
    let blobs = NoBlobs;
    tool.run(action, input, &RunCtx::new(&blobs))
        .await
        .map(|o| o.output)
}

fn ids(out: &Value) -> Vec<String> {
    out["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["conversation"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn search_finds_a_fact_from_another_conversation_and_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let p = engine.create_project("home", "").unwrap();
    let filler = "warehouse inventory notes ".repeat(200);
    for i in 0..12 {
        chat(
            &engine,
            &p.id,
            &format!("warehouse {i}"),
            &[("warehouse inventory", filler.as_str())],
            |_| {},
        );
    }
    let fact = chat(
        &engine,
        &p.id,
        "door codes",
        &[
            ("hello", "hi"),
            (
                "what is the warehouse gate code",
                "The warehouse gate code is 4471, changed in March.",
            ),
        ],
        |_| {},
    );
    let current = chat(
        &engine,
        &p.id,
        "today",
        &[("warehouse gate code again", "gate code 9999")],
        |_| {},
    );
    let tool = ConversationsTool::new(engine.store.clone(), p.id.clone(), current.clone());
    let out = call(&tool, SEARCH, json!({"query": "warehouse gate code"}))
        .await
        .unwrap();
    let hits = out["hits"].as_array().unwrap();
    assert_eq!(hits[0]["conversation"], fact.as_str());
    assert_eq!(hits[0]["turn"], 1);
    assert!(hits[0]["snippet"].as_str().unwrap().contains("4471"));
    assert!(!ids(&out).contains(&current.as_str().to_string()));
    assert!(hits.len() <= 5 && hits.len() >= 2, "{}", hits.len());
    for h in hits {
        assert!(h["snippet"].as_str().unwrap().chars().count() <= 400);
    }
    assert!(out.to_string().chars().count() <= 2_000, "{}", out);
    assert!(call(&tool, SEARCH, json!({"query": "the of"}))
        .await
        .is_err());
    assert!(call(&tool, SEARCH, json!({"query": "x", "scope": "all"}))
        .await
        .is_err());
}

#[tokio::test]
async fn only_this_scopes_own_conversations_are_searched() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let p = engine.create_project("home", "").unwrap();
    let other = engine.create_project("other", "").unwrap();
    let said = [(
        "where is the blue ledger",
        "The blue ledger is in drawer seven.",
    )];
    let own = chat(&engine, &p.id, "ledger", &said, |_| {});
    let foreign = chat(&engine, &other.id, "ledger", &said, |_| {});
    let work = chat(&engine, &p.id, "ledger", &said, |r| {
        r.work_item = Some("w-1".into())
    });
    let managed = chat(&engine, &p.id, "ledger", &said, |r| {
        r.execution_managed = true
    });
    let scoped = chat(&engine, &p.id, "ledger", &said, |r| {
        r.execution_scope = Some(other.id.clone())
    });
    let current = chat(&engine, &p.id, "now", &said, |_| {});
    let tool = ConversationsTool::new(engine.store.clone(), p.id.clone(), current.clone());
    let out = call(&tool, SEARCH, json!({"query": "blue ledger drawer"}))
        .await
        .unwrap();
    assert_eq!(ids(&out), vec![own.as_str().to_string()]);

    for refused in [&foreign, &work, &managed, &scoped, &current] {
        let err = call(&tool, READ, json!({"conversation": refused.as_str()}))
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::BadInput { .. }), "{err:?}");
    }
    assert!(
        call(&tool, READ, json!({"conversation": "no-such-conversation"}))
            .await
            .is_err()
    );
    let read = call(
        &tool,
        READ,
        json!({"conversation": own.as_str(), "turn": 0}),
    )
    .await
    .unwrap();
    assert!(read["text"].as_str().unwrap().contains("drawer seven"));
    assert!(call(
        &tool,
        READ,
        json!({"conversation": own.as_str(), "turn": 3})
    )
    .await
    .is_err());
}

#[tokio::test]
async fn read_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let p = engine.create_project("home", "").unwrap();
    let long = "a long answer about the harbour ".repeat(400);
    let turns: Vec<(&str, &str)> = (0..10).map(|_| ("tell me", long.as_str())).collect();
    let big = chat(&engine, &p.id, "harbour", &turns, |r| {
        r.gists = (0..50)
            .map(|i| format!("gist {i} {}", "word ".repeat(40)))
            .collect();
    });
    let current = chat(&engine, &p.id, "now", &[], |_| {});
    let tool = ConversationsTool::new(engine.store.clone(), p.id, current);
    for input in [
        json!({"conversation": big.as_str()}),
        json!({"conversation": big.as_str(), "turn": 4}),
    ] {
        let out = call(&tool, READ, input).await.unwrap();
        assert!(out.to_string().chars().count() <= 4_000, "{}", out);
        assert!(out["text"].as_str().unwrap().contains("harbour"));
    }
    let overview = call(&tool, READ, json!({"conversation": big.as_str()}))
        .await
        .unwrap();
    let text = overview["text"].as_str().unwrap();
    assert!(text.contains("gist 49"), "the newest gist is kept");
    assert!(text.contains("[turn 9]"), "the last turn is kept");
}

#[test]
fn offered_only_when_another_conversation_has_turns() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
    let p = engine.create_project("home", "").unwrap();
    let other = engine.create_project("other", "").unwrap();
    let current = chat(&engine, &p.id, "now", &[("hi", "hello")], |_| {});
    let check = || has_other_conversations(&engine.store, &p.id, &current).unwrap();
    assert!(!check());
    chat(&engine, &p.id, "empty", &[], |_| {});
    chat(&engine, &other.id, "elsewhere", &[("hi", "hello")], |_| {});
    chat(&engine, &p.id, "work", &[("hi", "hello")], |r| {
        r.work_item = Some("w".into())
    });
    assert!(!check());
    chat(&engine, &p.id, "earlier", &[("hi", "hello")], |_| {});
    assert!(check());
}

#[test]
fn the_model_sees_the_excerpts_and_the_chat_it_read_whole() {
    use ark_runtime::conversations::{distiller, READ, SEARCH};
    let d = distiller();
    let found = serde_json::json!({"hits": [{"conversation": "c-1", "title": "Garden guide", "date": "2026-11-09", "snippet": "the printed copy costs 7 euros and the launch is on March 14", "turn": 0}]});
    let (text, ids) = d(SEARCH, &found).expect("search is shown");
    assert!(
        text.contains("7 euros") && text.contains("March 14") && text.contains("c-1"),
        "{text}"
    );
    assert_eq!(ids, ["c-1"]);
    let read = serde_json::json!({"conversation": "c-1", "title": "Garden guide", "date": "2026-11-09", "turns": 1, "text": "[turn 0] Person: the printed copy costs 7 euros"});
    let (text, _) = d(READ, &read).expect("read is shown");
    assert!(
        text.ends_with("Person: the printed copy costs 7 euros"),
        "{text}"
    );
    let (none, _) = d(SEARCH, &serde_json::json!({"hits": []})).unwrap();
    assert_eq!(none, "No earlier chat matches.");
}
