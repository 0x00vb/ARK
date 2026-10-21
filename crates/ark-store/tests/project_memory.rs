use ark_core::{Invocation, MemorySource, MemoryState, Project, ProjectId, RiskTier, SessionId};
use ark_store::{MemoryReview, Store};
use serde_json::json;
use std::sync::{Arc, Barrier};

fn project(store: &Store, name: &str) -> (ProjectId, SessionId) {
    let p = Project::new(name, "");
    store.put_project(&p).unwrap();
    let session = SessionId::new();
    store
        .create_session(session.as_str(), &p.id, &json!({}))
        .unwrap();
    (p.id, session)
}
fn person(session: &SessionId) -> MemorySource {
    MemorySource::Person {
        session: session.clone(),
    }
}
fn agent(store: &Store, project: &ProjectId) -> MemorySource {
    let invocation = Invocation::new("memory.suggest", RiskTier::Mutate, json!({}));
    store.put_invocation(project, &invocation).unwrap();
    MemorySource::Agent {
        invocation: invocation.id,
    }
}

#[test]
fn corrections_survive_restart_and_replacements_retire_the_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let old = store
        .record_memory(&p, "report format", "Use a table.", person(&s), None)
        .unwrap();
    let suggestion = store
        .record_memory(
            &p,
            "report format",
            "Use prose.",
            agent(&store, &p),
            Some(old.id.clone()),
        )
        .unwrap();
    assert_eq!(suggestion.state, MemoryState::Pending);
    assert_eq!(
        store.project_memory(&p, old.id.as_str()).unwrap().state,
        MemoryState::Active
    );
    let accepted = store
        .review_memory(&p, suggestion.id.as_str(), MemoryReview::Accept, &s)
        .unwrap();
    assert_eq!(accepted.reviewed_by, Some(s.clone()));
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        store
            .project_memory(&p, suggestion.id.as_str())
            .unwrap()
            .state,
        MemoryState::Active
    );
    assert_eq!(
        store.project_memory(&p, old.id.as_str()).unwrap().state,
        MemoryState::Superseded { by: suggestion.id }
    );
}

#[test]
fn every_lookup_and_transition_refuses_foreign_ids_and_sources() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (a, sa) = project(&store, "a");
    let (b, sb) = project(&store, "b");
    let m = store
        .record_memory(&a, "deploy", "Use staging.", person(&sa), None)
        .unwrap();
    let missing = store.project_memory(&b, "unknown").unwrap_err().to_string();
    assert_eq!(
        store
            .project_memory(&b, m.id.as_str())
            .unwrap_err()
            .to_string(),
        missing
    );
    for review in [
        MemoryReview::Accept,
        MemoryReview::Reject,
        MemoryReview::Forget,
    ] {
        assert!(store.review_memory(&b, m.id.as_str(), review, &sb).is_err());
    }
    assert!(store
        .record_memory(&b, "deploy", "Other.", person(&sb), Some(m.id.clone()))
        .is_err());
    assert!(store
        .record_memory(&b, "deploy", "Other.", person(&sa), None)
        .is_err());
    assert!(store
        .record_memory(&ProjectId::new(), "deploy", "Other.", person(&sa), None)
        .is_err());
    assert!(store
        .record_memory(&b, "deploy", "Other.", agent(&store, &a), None)
        .is_err());
    assert!(store.project_memories(&b).unwrap().is_empty());
    assert_eq!(
        store.project_memory(&a, m.id.as_str()).unwrap().body,
        "Use staging."
    );
}

#[test]
fn concurrent_replacements_cannot_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let (p, s) = project(&store, "one");
    let old = store
        .record_memory(&p, "export", "Use CSV.", person(&s), None)
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for correction in ["Use XLSX.", "Use TSV."] {
        let (store, p, s, old, barrier) = (
            store.clone(),
            p.clone(),
            s.clone(),
            old.id.clone(),
            barrier.clone(),
        );
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store.record_memory(&p, "export", correction, person(&s), Some(old))
        }));
    }
    barrier.wait();
    let winners = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(Result::is_ok)
        .count();
    assert_eq!(winners, 1);
    assert_eq!(
        store
            .project_memories(&p)
            .unwrap()
            .iter()
            .filter(|m| m.state == MemoryState::Active)
            .count(),
        1
    );
}

#[test]
fn dedupe_does_not_resurrect_rejected_forgotten_or_superseded_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let source = agent(&store, &p);
    let m = store
        .record_memory(&p, "tests", "Run unit tests.", source.clone(), None)
        .unwrap();
    assert_eq!(
        store
            .record_memory(&p, "tests", "Run unit tests.", source.clone(), None)
            .unwrap()
            .id,
        m.id
    );
    store
        .review_memory(&p, m.id.as_str(), MemoryReview::Reject, &s)
        .unwrap();
    let next = store
        .record_memory(&p, "tests", "Run unit tests.", source, None)
        .unwrap();
    assert_ne!(next.id, m.id);
    store
        .review_memory(&p, next.id.as_str(), MemoryReview::Accept, &s)
        .unwrap();
    let replacement = store
        .record_memory(
            &p,
            "tests",
            "Run integration tests.",
            person(&s),
            Some(next.id.clone()),
        )
        .unwrap();
    assert!(store
        .review_memory(&p, next.id.as_str(), MemoryReview::Accept, &s)
        .is_err());
    let forgotten = store
        .review_memory(&p, replacement.id.as_str(), MemoryReview::Forget, &s)
        .unwrap();
    assert!(forgotten.topic.is_empty() && forgotten.body.is_empty());
    assert!(store
        .review_memory(&p, forgotten.id.as_str(), MemoryReview::Accept, &s)
        .is_err());
    assert_eq!(
        store.project_memory(&p, m.id.as_str()).unwrap().state,
        MemoryState::Rejected
    );
}

#[test]
fn stale_candidate_review_fails_without_partial_activation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let old = store
        .record_memory(&p, "report", "Tables.", person(&s), None)
        .unwrap();
    let pending = store
        .record_memory(
            &p,
            "report",
            "Bullets.",
            agent(&store, &p),
            Some(old.id.clone()),
        )
        .unwrap();
    let newest = store
        .record_memory(&p, "report", "Prose.", person(&s), Some(old.id.clone()))
        .unwrap();
    assert!(store
        .review_memory(&p, pending.id.as_str(), MemoryReview::Accept, &s)
        .is_err());
    assert_eq!(
        store.project_memory(&p, pending.id.as_str()).unwrap().state,
        MemoryState::Pending
    );
    assert_eq!(
        store.project_memory(&p, newest.id.as_str()).unwrap().state,
        MemoryState::Active
    );
}

#[test]
fn invalid_or_conflicting_content_never_changes_existing_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    for (topic, body) in [
        ("", "body"),
        ("topic", ""),
        ("multi\nline", "body"),
        ("bad|topic", "body"),
        ("topic", &"x".repeat(2001)),
        (&"x".repeat(129), "body"),
    ] {
        assert!(store
            .record_memory(&p, topic, body, person(&s), None)
            .is_err());
    }
    let m = store
        .record_memory(&p, "report", "Use tables.", person(&s), None)
        .unwrap();
    assert!(store
        .record_memory(&p, "REPORT", "Use prose.", person(&s), None)
        .is_err());
    assert_eq!(store.project_memories(&p).unwrap().len(), 1);
    assert_eq!(
        store.project_memory(&p, m.id.as_str()).unwrap().body,
        "Use tables."
    );
}

#[test]
fn repeated_suggestions_from_distinct_calls_keep_first_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let first_source = agent(&store, &p);
    let first = store
        .record_memory(&p, "report", "Use a table.", first_source.clone(), None)
        .unwrap();
    let second = store
        .record_memory(&p, "report", "Use a table.", agent(&store, &p), None)
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.source, first_source);
    let confirmed = store
        .record_memory(&p, "report", "Use a table.", person(&s), None)
        .unwrap();
    assert_eq!(confirmed.id, first.id);
    assert_eq!(confirmed.source, first_source);
    assert_eq!(confirmed.reviewed_by, Some(s));
    assert_eq!(confirmed.state, MemoryState::Active);
}

#[test]
fn remembering_an_old_pending_suggestion_cannot_duplicate_an_active_topic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let pending = store
        .record_memory(&p, "report", "Use a table.", agent(&store, &p), None)
        .unwrap();
    let active = store
        .record_memory(&p, "report", "Use prose.", person(&s), None)
        .unwrap();
    assert!(store
        .record_memory(&p, "report", "Use a table.", person(&s), None)
        .is_err());
    assert_eq!(
        store.project_memory(&p, pending.id.as_str()).unwrap().state,
        MemoryState::Pending
    );
    assert_eq!(
        store.project_memory(&p, active.id.as_str()).unwrap().state,
        MemoryState::Active
    );
}

#[test]
fn forgetting_frees_capacity_and_replacement_at_capacity_does_not_disable_recall() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (p, s) = project(&store, "one");
    let mut first = None;
    for n in 0..512 {
        let memory = store
            .record_memory(&p, &format!("topic {n}"), "A correction.", person(&s), None)
            .unwrap();
        if n == 0 {
            first = Some(memory);
        }
    }
    assert!(store
        .record_memory(&p, "overflow", "A correction.", person(&s), None)
        .is_err());
    let first = first.unwrap();
    let replaced = store
        .record_memory(
            &p,
            &first.topic,
            "New correction.",
            person(&s),
            Some(first.id),
        )
        .unwrap();
    assert_eq!(store.project_memories(&p).unwrap().len(), 512);
    store
        .review_memory(&p, replaced.id.as_str(), MemoryReview::Forget, &s)
        .unwrap();
    store
        .record_memory(&p, "available", "A correction.", person(&s), None)
        .unwrap();
    assert_eq!(store.project_memories(&p).unwrap().len(), 512);
}
