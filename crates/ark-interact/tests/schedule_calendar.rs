use ark_interact::orchestration::schedule_calendar;
use ark_orchestrate::{AgentSpec, Orchestrator, ScheduleChange, When};
use ark_sdk::prelude::*;

#[tokio::test]
async fn calendar_reads_persisted_occurrences_dst_revisions_and_pause_without_starting_runs() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let project = ark.create_project("Calendar", "test").unwrap();
    let when = When::daily_at("01:30")
        .in_zone("America/New_York")
        .normalized()
        .unwrap();
    let stored: ark_orchestrate::ScheduleRecord = serde_json::from_value(serde_json::json!({
        "id":"s-fixed", "project":project.id, "agent":"helper", "task":{"objective":"Review draft"},
        "when":when, "next_ms":1793424600000u64, "created_ms":1793420000000u64
    }))
    .unwrap();
    ark.engine()
        .store
        .put_record(
            "orch_schedule",
            &stored.id,
            project.id.as_str(),
            &serde_json::to_value(&stored).unwrap(),
        )
        .unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    o.spawn(AgentSpec::new("helper")).await.unwrap();
    let rec = stored;
    let ms = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond() as u64;
    let from = ms("2026-11-01T00:00:00Z");
    let to = ms("2026-11-03T00:00:00Z");
    let first = schedule_calendar(&o, from, to, "America/New_York").unwrap();
    let events = first["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["start"], "2026-11-01T05:30:00Z");
    assert_eq!(events[1]["start"], "2026-11-02T06:30:00Z");
    assert_eq!(events[0]["state"], "planned");
    assert_eq!(events[0]["end_known"], false);
    assert_eq!(events[0]["delivery"], "agent_run");
    assert_eq!(events[0]["source_record"], rec.id);
    let again = schedule_calendar(&o, from, to, "America/New_York").unwrap();
    assert_eq!(first["events"], again["events"]);
    o.change_schedule(
        &rec.id,
        ScheduleChange {
            objective: Some("Changed draft".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let changed = schedule_calendar(&o, from, to, "America/New_York").unwrap();
    assert_eq!(events[0]["id"], changed["events"][0]["id"]);
    assert_ne!(
        events[0]["source_revision"],
        changed["events"][0]["source_revision"]
    );
    o.pause_schedule(&rec.id).unwrap();
    assert!(
        schedule_calendar(&o, from, to, "America/New_York").unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(o.tasks().is_empty());
    assert!(schedule_calendar(&o, from, from + 63 * 86_400_000, "UTC").is_err());
    assert!(schedule_calendar(&o, from, to, "Bad/Zone").is_err());
}

#[tokio::test]
async fn completed_runs_survive_their_removed_schedule_and_dense_ranges_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let project = ark.create_project("History", "test").unwrap();
    let at = "2026-11-01T05:30:00Z"
        .parse::<jiff::Timestamp>()
        .unwrap()
        .as_millisecond() as u64;
    let task = serde_json::json!({"id":"t-history", "project":project.id, "owner":"helper", "objective":"😀".repeat(240), "origin":{"by":"external"}, "state":"done", "created_ms":at, "started_ms":at, "finished_ms":at+30000, "schedule":"s-removed"});
    let parsed: ark_orchestrate::TaskRecord = serde_json::from_value(task.clone()).unwrap();
    assert_eq!(parsed.objective.encode_utf16().count(), 480);
    ark.engine()
        .store
        .put_record("orch_task", "t-history", project.id.as_str(), &task)
        .unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    assert!(o.schedules().is_empty());
    let snapshot = schedule_calendar(&o, at - 1, at + 60000, "UTC").unwrap();
    assert_eq!(snapshot["events"][0]["state"], "completed");
    assert_eq!(snapshot["events"][0]["end_known"], true);
    assert_eq!(
        snapshot["events"][0]["title"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count(),
        240
    );
    assert_eq!(snapshot["events"][0]["source_record"], "s-removed");
    assert_eq!(snapshot["events"][0]["end"], "2026-11-01T05:30:30Z");
}

#[tokio::test]
async fn automation_calendar_reads_other_writers_and_keeps_frozen_results_in_their_project() {
    use ark_orchestrate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let project = ark.create_project("Automations", "test").unwrap();
    let observer = Orchestrator::builder(&ark, &project).build().unwrap();
    let writer = Orchestrator::builder(&ark, &project).build().unwrap();
    let draft: Draft = serde_json::from_value(serde_json::json!({
        "name":"Frozen manual result","trigger":{"kind":"manual"},"inputs":[],
        "steps":[{"id":"answer","name":"Result","action":{"kind":"result","inputs":[{"kind":"literal","text":"Saved answer"}]}}]
    })).unwrap();
    let saved = writer
        .save_automation("calendar", None, None, draft.clone())
        .unwrap();
    let run = writer
        .admit_manual_automation_run(ark_orchestrate::automation::ManualStart {
            automation: saved.automation.clone(),
            expected: 1,
            previous: None,
            request: "calendar-run".into(),
            inputs: Default::default(),
        })
        .await
        .unwrap();
    let from = run.created_ms.saturating_sub(1);
    let to = run.created_ms + 60_000;
    let pending = schedule_calendar(&observer, from, to, "UTC").unwrap();
    assert_eq!(pending["events"].as_array().unwrap().len(), 1);
    assert_eq!(pending["events"][0]["id"], format!("automation-{}", run.id));
    writer.execute_automation_run(&run.id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while writer.automation_run(&run.id).unwrap().phase != RunPhase::Completed {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut changed = draft;
    changed.name = "Later draft name".into();
    writer
        .save_automation("rename", Some(&saved.automation), Some(1), changed)
        .unwrap();
    let completed = schedule_calendar(&observer, from, to, "UTC").unwrap();
    assert_eq!(completed["events"][0]["title"], "Frozen manual result");
    assert_eq!(completed["events"][0]["state"], "completed");
    assert_eq!(completed["events"][0]["end_known"], true);
    let other = ark.create_project("Other", "").unwrap();
    let other = Orchestrator::builder(&ark, &other).build().unwrap();
    assert!(
        schedule_calendar(&other, from, to, "UTC").unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        schedule_calendar(&observer, to, to + 60_000, "UTC").unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
