//! The Computer capability against a fake screen: one agent at a time, the person first, the
//! person can take it back, element numbers stay with their app, permission is asked once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_computer::fake::FakeScreen;
use ark_computer::*;
use ark_tools::{Adapter, BlobResolver, RunCtx};
use serde_json::json;

fn service(screen: &FakeScreen) -> Arc<ComputerService> {
    ComputerService::with_screen(Arc::new(screen.clone()))
        .timing(Duration::from_millis(300), Duration::from_millis(300))
}

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

async fn run(
    c: &Computer,
    action: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    c.run(action, input, &RunCtx::new(&NoBlobs))
        .await
        .map(|o| o.output)
        .map_err(|e| e.to_string())
}

fn notes() -> FakeScreen {
    FakeScreen::new()
        .app("Notes", &[("text field", "Title"), ("button", "Save")])
        .app("Mail", &[("button", "Send")])
}

#[tokio::test]
async fn an_uncertain_native_click_is_not_retried_with_the_pointer() {
    let screen = notes().presses_fail();
    let svc = service(&screen);
    let computer = svc.for_owner("test");
    run(&computer, LOOK, json!({"app":"Notes"})).await.unwrap();
    let error = run(
        &computer,
        CLICK,
        json!({"app":"Notes","ref":2,"name":"Save"}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("may have been delivered"), "{error}");
    assert_eq!(
        screen
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("press returned uncertain"))
            .count(),
        1
    );
    assert!(!screen
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains("pointer")));
}

#[tokio::test]
async fn replacing_a_field_replaces_existing_text_and_empty_text_clears_it() {
    let screen = notes();
    let svc = service(&screen);
    let computer = svc.for_owner("test");
    run(&computer, LOOK, json!({"app":"Notes"})).await.unwrap();
    for text in ["existing text", "different\nmultiline text", ""] {
        let look = run(
            &computer,
            TYPE,
            json!({"app":"Notes","ref":1,"name":"Title","text":text}),
        )
        .await
        .unwrap();
        assert_eq!(look["elements"][0]["value"], text);
    }
    assert!(!screen
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains("keys return")));
}

struct TestMessages;

impl MessageEvidence for TestMessages {
    fn context(
        &self,
        look: &Look,
        recipient: Option<&str>,
        text: Option<&str>,
        _bound: Option<(&str, &str)>,
    ) -> std::result::Result<Option<MessageContext>, String> {
        if look.elements.len() < 2 {
            return Ok(None);
        }
        let conversation = &look.elements[0];
        let composer = &look.elements[1];
        if recipient.is_some_and(|r| r != conversation.name) {
            return Ok(None);
        }
        let outgoing = text
            .map(|text| {
                look.elements
                    .iter()
                    .filter(|e| e.name == format!("Outgoing {text}"))
                    .map(|e| format!("{}:{}", e.number, e.name))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(MessageContext {
            window_identifier: look.window_identifier.clone().unwrap(),
            conversation_identifier: conversation.identifier.clone().unwrap(),
            conversation: conversation.name.clone(),
            conversation_frame: conversation.frame,
            composer_identifier: composer.identifier.clone().unwrap(),
            composer: composer.name.clone(),
            composer_value: composer.value.clone().unwrap_or_default(),
            composer_focused: true,
            composer_frame: composer.frame,
            outgoing,
        }))
    }
}

struct TestKey;
impl SafetyKey for TestKey {
    fn load_or_create(&self) -> std::result::Result<[u8; 32], String> {
        Ok([7; 32])
    }
}

fn message_service(screen: &FakeScreen, file: std::path::PathBuf) -> Arc<ComputerService> {
    let svc = service(screen)
        .messages(Arc::new(TestMessages))
        .message_safety(file, Arc::new(TestKey));
    svc.set_on(true).unwrap();
    svc
}

async fn draft(c: &Computer, text: &str) -> Result<serde_json::Value, String> {
    run(c, LOOK, json!({ "app": "WhatsApp" })).await?;
    run(
        c,
        DRAFT,
        json!({
            "app": "WhatsApp", "recipient": "Notas", "conversation_ref": 1,
            "conversation_name": "Notas", "composer_ref": 2,
            "composer_name": "Compose message", "text": text
        }),
    )
    .await
}

#[tokio::test]
async fn recovery_requires_every_possible_composer_to_be_known_and_empty() {
    for unknown in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let screen = FakeScreen::new().app(
            "WhatsApp",
            &[("button", "Notas"), ("text area", "Compose message")],
        );
        let svc = message_service(&screen, dir.path().join("hazard.json"));
        let computer = svc.for_owner("chat");
        draft(&computer, "staged").await.unwrap();
        screen.look("WhatsApp").unwrap().controls[1]
            .set_text("")
            .unwrap();
        if unknown {
            screen.unknown_recovery_window("second");
        } else {
            screen.recovery_window("second", &[("Compose message", "another draft")]);
        }
        assert!(svc.check_message_again().await.is_err());
        assert!(svc.message_hazard().is_some());
    }
}

#[tokio::test]
async fn recovery_of_a_legacy_window_record_checks_stable_composer_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hazard.json");
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, path.clone());
    let computer = svc.for_owner("chat");
    draft(&computer, "staged").await.unwrap();
    screen.look("WhatsApp").unwrap().controls[1]
        .set_text("")
        .unwrap();
    drop(computer);
    drop(svc);
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["hazard"]["window_identifier"] = json!("legacy-window-identifier");
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    screen.recovery_window("new-window", &[("Compose message", "")]);
    let reopened = message_service(&screen, path);
    reopened.check_message_again().await.unwrap();
    assert!(reopened.message_hazard().is_none());
    assert!(!screen
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "keys return"));
}

#[tokio::test]
async fn a_message_is_drafted_then_sent_only_when_the_outgoing_item_is_proved() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new()
        .app(
            "WhatsApp",
            &[("button", "Notas"), ("text area", "Compose message")],
        )
        .sends_on_return();
    let svc = message_service(&screen, dir.path().join("computer-hazard.json"));
    let c = svc.for_owner("chat");
    assert_eq!(draft(&c, "ARK test").await.unwrap()["status"], "drafted");
    assert_eq!(svc.message_hazard().unwrap().kind, HazardKind::DraftPresent);
    let sent = run(
        &c,
        SEND,
        json!({
            "app": "WhatsApp", "recipient": "Notas", "text": "ARK test"
        }),
    )
    .await
    .unwrap();
    assert_eq!(sent["status"], "confirmed");
    assert!(svc.message_hazard().is_none());
    assert_eq!(
        screen
            .value("WhatsApp", "Compose message")
            .unwrap_or_default(),
        ""
    );
}

#[tokio::test]
async fn an_unconfirmed_send_is_terminal_and_blocks_another_owner() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, dir.path().join("computer-hazard.json"));
    let c = svc.for_owner("chat");
    draft(&c, "ARK uncertain").await.unwrap();
    let sent = run(
        &c,
        SEND,
        json!({
            "app": "WhatsApp", "recipient": "Notas", "text": "ARK uncertain"
        }),
    )
    .await
    .unwrap();
    assert_eq!(sent["status"], "submitted_unconfirmed");
    assert_eq!(sent["effect"]["retry_allowed"], false);
    drop(c);
    let other = svc.for_owner("other");
    let refused = run(&other, KEYS, json!({ "app": "WhatsApp", "keys": "return" }))
        .await
        .unwrap_err();
    assert!(refused.contains("draft may still be present"), "{refused}");
    let open = run(&other, OPEN, json!({ "app": "WhatsApp" }))
        .await
        .unwrap_err();
    assert!(open.contains("draft may still be present"), "{open}");
    let retry = run(
        &other,
        SEND,
        json!({
            "app": "WhatsApp", "recipient": "Notas", "text": "ARK uncertain"
        }),
    )
    .await
    .unwrap_err();
    assert!(retry.contains("draft the message first"), "{retry}");
}

#[tokio::test]
async fn cancel_types_delete_and_clears_only_the_exact_draft() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "Messages",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, dir.path().join("computer-hazard.json"));
    let c = svc.for_owner("chat");
    run(&c, LOOK, json!({ "app": "Messages" })).await.unwrap();
    run(
        &c,
        DRAFT,
        json!({
            "app": "Messages", "recipient": "Notas", "conversation_ref": 1,
            "conversation_name": "Notas", "composer_ref": 2,
            "composer_name": "Compose message", "text": "clear me"
        }),
    )
    .await
    .unwrap();
    let cancelled = run(
        &c,
        CANCEL,
        json!({ "app": "Messages", "recipient": "Notas", "text": "clear me" }),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(svc.message_hazard().is_none());
    assert!(screen.log.lock().unwrap().ends_with(&[
        "press Messages/Compose message".into(),
        "open Messages".into(),
        "keys cmd+a".into(),
        "keys delete".into(),
    ]));
}

#[tokio::test]
async fn a_return_error_after_the_safety_boundary_is_unconfirmed_not_failed() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new()
        .app(
            "Messages",
            &[("button", "Notas"), ("text area", "Compose message")],
        )
        .return_fails();
    let svc = message_service(&screen, dir.path().join("computer-hazard.json"));
    let c = svc.for_owner("chat");
    run(&c, LOOK, json!({ "app": "Messages" })).await.unwrap();
    run(
        &c,
        DRAFT,
        json!({
            "app": "Messages", "recipient": "Notas", "conversation_ref": 1,
            "conversation_name": "Notas", "composer_ref": 2,
            "composer_name": "Compose message", "text": "ARK uncertain"
        }),
    )
    .await
    .unwrap();
    let sent = run(
        &c,
        SEND,
        json!({
            "app": "Messages", "recipient": "Notas", "text": "ARK uncertain"
        }),
    )
    .await
    .unwrap();
    assert_eq!(sent["status"], "submitted_unconfirmed");
    assert_eq!(sent["effect"]["state"], "unconfirmed");
    assert_eq!(sent["effect"]["retry_allowed"], false);
    assert_eq!(
        svc.message_hazard().unwrap().kind,
        HazardKind::SubmittedUnconfirmed
    );
}

#[tokio::test]
async fn send_posts_no_return_when_another_app_keeps_the_front() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new()
        .app(
            "Messages",
            &[("button", "Notas"), ("text area", "Compose message")],
        )
        .app("Notes", &[("text area", "Body")]);
    let svc = message_service(&screen, dir.path().join("computer-hazard.json"));
    let c = svc.for_owner("chat");
    run(&c, LOOK, json!({ "app": "Messages" })).await.unwrap();
    run(
        &c,
        DRAFT,
        json!({
            "app": "Messages", "recipient": "Notas", "conversation_ref": 1,
            "conversation_name": "Notas", "composer_ref": 2,
            "composer_name": "Compose message", "text": "do not misdirect"
        }),
    )
    .await
    .unwrap();
    screen.keep_front_on("Notes");
    let error = run(
        &c,
        SEND,
        json!({
            "app": "Messages", "recipient": "Notas", "text": "do not misdirect"
        }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("front"), "{error}");
    assert!(!screen
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "keys return"));
    assert_eq!(svc.message_hazard().unwrap().kind, HazardKind::DraftPresent);
}

#[tokio::test]
async fn send_never_posts_return_to_a_different_same_app_window() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "Messages",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, dir.path().join("hazard.json"));
    let computer = svc.for_owner("chat");
    run(&computer, LOOK, json!({"app":"Messages"}))
        .await
        .unwrap();
    run(&computer, DRAFT, json!({"app":"Messages","recipient":"Notas","conversation_ref":1,"conversation_name":"Notas","composer_ref":2,"composer_name":"Compose message","text":"bound draft"})).await.unwrap();
    screen.mismatch_input_window();
    let result = run(
        &computer,
        SEND,
        json!({"app":"Messages","recipient":"Notas","text":"bound draft"}),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "submitted_unconfirmed");
    assert!(!screen
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "keys return"));
    assert!(svc.message_hazard().is_some());
}

#[tokio::test]
async fn human_acknowledgement_is_bound_to_the_current_block_and_never_confirms_or_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hazard.json");
    let screen = FakeScreen::new()
        .app(
            "WhatsApp",
            &[("button", "Notas"), ("text area", "Compose message")],
        )
        .return_fails();
    let svc = message_service(&screen, path.clone());
    let computer = svc.for_owner("chat");
    draft(&computer, "private draft").await.unwrap();
    let previous = svc.review_message_block().unwrap();
    assert!(svc.acknowledge_message_block("wrong token").is_err());
    let replaced = svc.review_message_block().unwrap();
    assert_ne!(previous.token, replaced.token);
    assert!(svc.acknowledge_message_block(&previous.token).is_err());
    let sent = run(
        &computer,
        SEND,
        json!({"app":"WhatsApp","recipient":"Notas","text":"private draft"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["status"], "submitted_unconfirmed");
    assert!(svc.acknowledge_message_block(&previous.token).is_err());
    assert!(svc.acknowledge_message_block(&replaced.token).is_err());
    let stopped = svc.review_message_block().unwrap();
    svc.take_back();
    assert!(svc.acknowledge_message_block(&stopped.token).is_err());
    let review = svc.review_message_block().unwrap();
    let encoded = serde_json::to_string(&review).unwrap();
    assert!(!encoded.contains("Notas") && !encoded.contains("private draft"));
    assert_eq!(review.kind, HazardKind::SubmittedUnconfirmed);
    let before = screen.log.lock().unwrap().clone();
    let status = svc.acknowledge_message_block(&review.token).unwrap();
    assert_eq!(status.kind, HazardKind::SubmittedUnconfirmed);
    assert!(svc.message_hazard().is_none());
    assert_eq!(*screen.log.lock().unwrap(), before);
    assert!(run(
        &computer,
        SEND,
        json!({"app":"WhatsApp","recipient":"Notas","text":"private draft"})
    )
    .await
    .is_err());
    assert!(computer
        .actions()
        .iter()
        .all(|action| !action.name.contains("acknowledge")));
    assert!(message_service(&screen, path).message_hazard().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn acknowledgement_during_send_proof_preserves_uncertainty_and_the_next_draft() {
    struct PausedProof {
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        once: std::sync::atomic::AtomicBool,
    }
    impl MessageEvidence for PausedProof {
        fn context(
            &self,
            look: &Look,
            recipient: Option<&str>,
            text: Option<&str>,
            bound: Option<(&str, &str)>,
        ) -> std::result::Result<Option<MessageContext>, String> {
            let context = TestMessages.context(look, recipient, text, bound)?;
            if context.as_ref().is_some_and(|context| {
                context.composer_value.is_empty() && !context.outgoing.is_empty()
            }) && !self.once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.send(()).map_err(|error| error.to_string())?;
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| error.to_string())?;
            }
            Ok(context)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new()
        .app(
            "WhatsApp",
            &[("button", "Notas"), ("text area", "Compose message")],
        )
        .sends_on_return();
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let svc = service(&screen)
        .messages(Arc::new(PausedProof {
            entered,
            release: std::sync::Mutex::new(released),
            once: std::sync::atomic::AtomicBool::new(false),
        }))
        .message_safety(dir.path().join("hazard.json"), Arc::new(TestKey));
    svc.set_on(true).unwrap();
    let old = Arc::new(svc.for_owner("old"));
    draft(&old, "first draft").await.unwrap();
    let sender = old.clone();
    let send = tokio::spawn(async move {
        run(
            &sender,
            SEND,
            json!({"app":"WhatsApp","recipient":"Notas","text":"first draft"}),
        )
        .await
    });
    tokio::task::spawn_blocking(move || waiting.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .unwrap();
    let review = svc.review_message_block().unwrap();
    let acknowledged = svc.acknowledge_message_block(&review.token);
    if acknowledged.is_err() {
        release.send(()).unwrap();
    }
    acknowledged.unwrap();
    let next = svc.for_owner("next");
    let staged = draft(&next, "second draft").await;
    release.send(()).unwrap();
    staged.unwrap();
    let result = send.await.unwrap().unwrap();
    assert_eq!(result["status"], "submitted_unconfirmed");
    assert_eq!(result["effect"]["retry_allowed"], false);
    assert_eq!(svc.message_hazard().unwrap().kind, HazardKind::DraftPresent);
    run(
        &next,
        CANCEL,
        json!({"app":"WhatsApp","recipient":"Notas","text":"second draft"}),
    )
    .await
    .unwrap();
    assert!(svc.message_hazard().is_none());
    assert_eq!(
        screen
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|line| *line == "keys return")
            .count(),
        1
    );
}

#[tokio::test]
async fn saved_message_status_survives_activity_changes_and_service_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hazard.json");
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, path.clone());
    let mut status = svc.watch_message_status();
    let computer = svc.for_owner("chat");
    draft(&computer, "staged").await.unwrap();
    status.changed().await.unwrap();
    assert_eq!(
        status.borrow().as_ref().unwrap().kind,
        HazardKind::DraftPresent
    );
    run(&computer, LOOK, json!({"app":"WhatsApp"}))
        .await
        .unwrap();
    svc.take_back();
    assert_eq!(
        status.borrow().as_ref().unwrap().kind,
        HazardKind::DraftPresent
    );
    drop(computer);
    drop(svc);
    let reopened = message_service(&screen, path);
    assert_eq!(
        reopened.message_status().unwrap().unwrap().kind,
        HazardKind::DraftPresent
    );
}

#[tokio::test]
async fn successful_recovery_revokes_the_old_owner_and_pending_field_input() {
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let svc = message_service(&screen, dir.path().join("hazard.json"));
    let computer = svc.for_owner("chat");
    draft(&computer, "staged").await.unwrap();
    screen.look("WhatsApp").unwrap().controls[1]
        .set_text("")
        .unwrap();
    let pending = run(
        &computer,
        TYPE,
        json!({"app":"WhatsApp","ref":2,"name":"Compose message","text":"late input"}),
    );
    svc.check_message_again().await.unwrap();
    assert!(svc.message_hazard().is_none());
    assert!(svc.holder().is_none());
    assert!(pending
        .await
        .unwrap_err()
        .contains("took the computer back"));
    assert_eq!(
        screen.look("WhatsApp").unwrap().elements[1]
            .value
            .as_deref(),
        Some("")
    );
}

#[tokio::test]
async fn computer_status_and_recovery_never_load_the_safety_key_before_enablement() {
    struct Key(Arc<std::sync::atomic::AtomicUsize>);
    impl SafetyKey for Key {
        fn load_or_create(&self) -> std::result::Result<[u8; 32], String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok([19; 32])
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let path = dir.path().join("hazard.json");
    let svc = service(&FakeScreen::new())
        .messages(Arc::new(TestMessages))
        .message_safety(path.clone(), Arc::new(Key(calls.clone())))
        .switched(dir.path().join("computer.json"));
    assert!(!svc.is_on());
    assert!(svc.message_status().unwrap().is_none());
    assert!(svc.review_message_block().is_err());
    assert!(svc.acknowledge_message_block("old review").is_err());
    assert!(svc.check_message_again().await.is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!path.exists());
    svc.set_on(true).unwrap();
    assert!(svc.message_status().unwrap().is_none());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn simultaneous_send_requests_claim_the_draft_only_once() {
    struct Pause {
        log: ark_computer::fake::Log,
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        once: std::sync::atomic::AtomicBool,
    }
    impl MessageEvidence for Pause {
        fn context(
            &self,
            look: &Look,
            recipient: Option<&str>,
            text: Option<&str>,
            bound: Option<(&str, &str)>,
        ) -> std::result::Result<Option<MessageContext>, String> {
            let context = TestMessages.context(look, recipient, text, bound)?;
            if self
                .log
                .lock()
                .unwrap()
                .iter()
                .any(|line| line == "keys return")
                && !self.once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.send(()).map_err(|error| error.to_string())?;
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| error.to_string())?;
            }
            Ok(context)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hazard.json");
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let svc = service(&screen)
        .messages(Arc::new(Pause {
            log: screen.log.clone(),
            entered,
            release: std::sync::Mutex::new(released),
            once: std::sync::atomic::AtomicBool::new(false),
        }))
        .message_safety(path.clone(), Arc::new(TestKey));
    svc.set_on(true).unwrap();
    let computer = Arc::new(svc.for_owner("chat"));
    draft(&computer, "send once").await.unwrap();
    let first = computer.clone();
    let send = tokio::spawn(async move {
        run(
            &first,
            SEND,
            json!({"app":"WhatsApp","recipient":"Notas","text":"send once"}),
        )
        .await
    });
    tokio::task::spawn_blocking(move || waiting.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .unwrap();
    let before: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let second = run(
        &computer,
        SEND,
        json!({"app":"WhatsApp","recipient":"Notas","text":"send once"}),
    )
    .await;
    release.send(()).unwrap();
    assert!(second.unwrap_err().contains("already submitted"));
    let first = send.await.unwrap().unwrap();
    assert_eq!(first["status"], "submitted_unconfirmed");
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(before["hazard"]["effect_id"], after["hazard"]["effect_id"]);
    assert_eq!(after["hazard"]["effect_id"], first["effect_id"]);
    assert_eq!(
        screen
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|line| *line == "keys return")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_delayed_cancel_cannot_erase_a_newer_draft() {
    struct Pause {
        armed: Arc<std::sync::atomic::AtomicBool>,
        count: std::sync::atomic::AtomicUsize,
        entered: std::sync::mpsc::SyncSender<usize>,
        releases: [std::sync::Mutex<std::sync::mpsc::Receiver<()>>; 2],
    }
    impl MessageEvidence for Pause {
        fn context(
            &self,
            look: &Look,
            recipient: Option<&str>,
            text: Option<&str>,
            bound: Option<(&str, &str)>,
        ) -> std::result::Result<Option<MessageContext>, String> {
            let context = TestMessages.context(look, recipient, text, bound)?;
            if self.armed.load(std::sync::atomic::Ordering::SeqCst)
                && text == Some("old draft")
                && context
                    .as_ref()
                    .is_some_and(|context| context.composer_value == "old draft")
            {
                let index = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if index < 2 {
                    self.entered
                        .send(index)
                        .map_err(|error| error.to_string())?;
                    self.releases[index]
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok(context)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let (entered, waiting) = std::sync::mpsc::sync_channel(2);
    let (release0, receive0) = std::sync::mpsc::sync_channel(1);
    let (release1, receive1) = std::sync::mpsc::sync_channel(1);
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let svc = service(&screen)
        .messages(Arc::new(Pause {
            armed: armed.clone(),
            count: std::sync::atomic::AtomicUsize::new(0),
            entered,
            releases: [
                std::sync::Mutex::new(receive0),
                std::sync::Mutex::new(receive1),
            ],
        }))
        .message_safety(dir.path().join("hazard.json"), Arc::new(TestKey));
    svc.set_on(true).unwrap();
    let computer = Arc::new(svc.for_owner("chat"));
    draft(&computer, "old draft").await.unwrap();
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let one = computer.clone();
    let two = computer.clone();
    let mut first = tokio::spawn(async move {
        run(
            &one,
            CANCEL,
            json!({"app":"WhatsApp","recipient":"Notas","text":"old draft"}),
        )
        .await
    });
    let mut second = tokio::spawn(async move {
        run(
            &two,
            CANCEL,
            json!({"app":"WhatsApp","recipient":"Notas","text":"old draft"}),
        )
        .await
    });
    tokio::task::spawn_blocking(move || {
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    })
    .await
    .unwrap();
    release0.send(()).unwrap();
    let first_finished = tokio::select! { result = &mut first => { result.unwrap().unwrap(); true }, result = &mut second => { result.unwrap().unwrap(); false } };
    let staged = draft(&computer, "new draft").await;
    release1.send(()).unwrap();
    staged.unwrap();
    let stale = if first_finished {
        second.await
    } else {
        first.await
    }
    .unwrap()
    .unwrap_err();
    assert!(stale.contains("draft changed"), "{stale}");
    assert_eq!(
        screen.look("WhatsApp").unwrap().elements[1]
            .value
            .as_deref(),
        Some("new draft")
    );
    assert!(svc.message_hazard().is_some());
    run(
        &computer,
        CANCEL,
        json!({"app":"WhatsApp","recipient":"Notas","text":"new draft"}),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn cancel_completion_cannot_clear_a_submission_that_claimed_the_draft() {
    struct Pause {
        log: ark_computer::fake::Log,
        armed: Arc<std::sync::atomic::AtomicBool>,
        entered: std::sync::mpsc::SyncSender<usize>,
        releases: [std::sync::Mutex<std::sync::mpsc::Receiver<()>>; 2],
        seen: [std::sync::atomic::AtomicBool; 2],
    }
    impl MessageEvidence for Pause {
        fn context(
            &self,
            look: &Look,
            recipient: Option<&str>,
            text: Option<&str>,
            bound: Option<(&str, &str)>,
        ) -> std::result::Result<Option<MessageContext>, String> {
            let context = TestMessages.context(look, recipient, text, bound)?;
            let sent = self
                .log
                .lock()
                .unwrap()
                .iter()
                .any(|line| line == "keys return");
            let index = if context
                .as_ref()
                .is_some_and(|context| context.composer_value.is_empty())
                && !sent
            {
                Some(0)
            } else if sent {
                Some(1)
            } else {
                None
            };
            if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(index) = index.filter(|index| {
                    !self.seen[*index].swap(true, std::sync::atomic::Ordering::SeqCst)
                }) {
                    self.entered
                        .send(index)
                        .map_err(|error| error.to_string())?;
                    self.releases[index]
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok(context)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app(
        "WhatsApp",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let (entered, waiting) = std::sync::mpsc::sync_channel(2);
    let (release0, receive0) = std::sync::mpsc::sync_channel(1);
    let (release1, receive1) = std::sync::mpsc::sync_channel(1);
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let svc = service(&screen)
        .messages(Arc::new(Pause {
            log: screen.log.clone(),
            armed: armed.clone(),
            entered,
            releases: [
                std::sync::Mutex::new(receive0),
                std::sync::Mutex::new(receive1),
            ],
            seen: [
                std::sync::atomic::AtomicBool::new(false),
                std::sync::atomic::AtomicBool::new(false),
            ],
        }))
        .message_safety(dir.path().join("hazard.json"), Arc::new(TestKey));
    svc.set_on(true).unwrap();
    let computer = Arc::new(svc.for_owner("chat"));
    draft(&computer, "same draft").await.unwrap();
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let canceler = computer.clone();
    let cancel = tokio::spawn(async move {
        run(
            &canceler,
            CANCEL,
            json!({"app":"WhatsApp","recipient":"Notas","text":"same draft"}),
        )
        .await
    });
    let waiting = tokio::task::spawn_blocking(move || {
        assert_eq!(waiting.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
        waiting
    })
    .await
    .unwrap();
    screen.look("WhatsApp").unwrap().controls[1]
        .set_text("same draft")
        .unwrap();
    let sender = computer.clone();
    let send = tokio::spawn(async move {
        run(
            &sender,
            SEND,
            json!({"app":"WhatsApp","recipient":"Notas","text":"same draft"}),
        )
        .await
    });
    tokio::task::spawn_blocking(move || {
        assert_eq!(waiting.recv_timeout(Duration::from_secs(5)).unwrap(), 1)
    })
    .await
    .unwrap();
    release0.send(()).unwrap();
    let cancelled = cancel.await.unwrap();
    let hazard = svc.message_hazard();
    release1.send(()).unwrap();
    assert!(cancelled.unwrap_err().contains("already submitted"));
    assert_eq!(hazard.unwrap().kind, HazardKind::SubmittedUnconfirmed);
    let sent = send.await.unwrap().unwrap();
    assert_eq!(sent["status"], "submitted_unconfirmed");
    assert!(svc.message_hazard().is_some());
}

#[tokio::test]
async fn public_look_is_bounded_and_keeps_a_late_focused_field_addressable() {
    let names: Vec<_> = (0..220).map(|i| format!("Item {i}")).collect();
    let mut controls: Vec<_> = names.iter().map(|name| ("button", name.as_str())).collect();
    controls.push(("text field", "Late composer"));
    let screen = FakeScreen::new().app("Lumen", &controls);
    screen
        .look("Lumen")
        .unwrap()
        .controls
        .last()
        .unwrap()
        .press()
        .unwrap();
    let svc = service(&screen);
    let computer = svc.for_owner("chat");
    let look = run(&computer, LOOK, json!({"app":"Lumen"})).await.unwrap();
    assert_eq!(look["elements"].as_array().unwrap().len(), 150);
    assert_eq!(look["truncated"], true);
    assert!(look["elements"]
        .as_array()
        .unwrap()
        .iter()
        .any(|element| element["ref"] == 221));
    run(
        &computer,
        TYPE,
        json!({"app":"Lumen","ref":221,"name":"Late composer","text":"complete value"}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_draft_tombstone_survives_restart_and_blocks_another_chat() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("computer-hazard.json");
    let screen = FakeScreen::new().app(
        "Messages",
        &[("button", "Notas"), ("text area", "Compose message")],
    );
    let first = message_service(&screen, path.clone());
    let chat = first.for_owner("chat");
    run(&chat, LOOK, json!({ "app": "Messages" }))
        .await
        .unwrap();
    run(
        &chat,
        DRAFT,
        json!({
            "app": "Messages", "recipient": "Notas", "conversation_ref": 1,
            "conversation_name": "Notas", "composer_ref": 2,
            "composer_name": "Compose message", "text": "kept draft"
        }),
    )
    .await
    .unwrap();
    drop(chat);
    drop(first);

    let reopened = message_service(&screen, path);
    let other = reopened.for_owner("other");
    let refused = run(&other, KEYS, json!({ "app": "Messages", "keys": "return" }))
        .await
        .unwrap_err();
    assert!(refused.contains("draft may still be present"), "{refused}");
}

#[tokio::test]
async fn activity_is_sequenced_and_names_the_visible_target() {
    let screen = notes();
    let svc = service(&screen);
    let c = svc.for_owner("chat");
    run(&c, LOOK, json!({ "app": "Notes" })).await.unwrap();
    let mut activity = svc.watch_activity();
    let before = activity.borrow().as_ref().unwrap().sequence;
    let observed = tokio::spawn(async move {
        let mut seen = Vec::new();
        while activity.changed().await.is_ok() {
            if let Some(next) = activity.borrow().clone() {
                let done = next.sequence > before && next.phase == ActivityPhase::Idle;
                seen.push(next);
                if done {
                    return seen;
                }
            }
        }
        seen
    });
    run(
        &c,
        CLICK,
        json!({ "app": "Notes", "ref": 2, "name": "Save" }),
    )
    .await
    .unwrap();
    let seen = observed.await.unwrap();
    assert!(seen
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(seen.iter().any(|item| {
        item.action == CLICK && item.phase == ActivityPhase::Acting && item.target.is_some()
    }));
    assert_eq!(seen.last().unwrap().phase, ActivityPhase::Idle);
}

#[tokio::test]
async fn failed_and_stopped_actions_finish_with_terminal_activity() {
    let screen = notes();
    let svc = service(&screen);
    let c = svc.for_owner("chat");
    run(&c, LOOK, json!({ "app": "Notes" })).await.unwrap();
    let error = run(
        &c,
        CLICK,
        json!({ "app": "Notes", "ref": 99, "name": "Missing" }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("no element"), "{error}");
    assert_eq!(
        svc.watch_activity().borrow().as_ref().unwrap().phase,
        ActivityPhase::Failed
    );

    assert_eq!(svc.take_back().as_deref(), Some("chat"));
    assert_eq!(
        svc.watch_activity().borrow().as_ref().unwrap().phase,
        ActivityPhase::Stopped
    );
}

#[tokio::test]
async fn an_agent_reads_a_window_and_acts_on_it_by_number() {
    let screen = notes();
    let svc = service(&screen);
    let a = svc.for_owner("p/writer");
    let apps = run(&a, APPS, json!({})).await.unwrap();
    assert_eq!(apps["apps"].as_array().unwrap().len(), 2);
    assert_eq!(svc.holder(), None, "listing apps takes nothing");

    let look = run(&a, LOOK, json!({ "app": "notes" })).await.unwrap();
    assert_eq!(look["elements"][1]["name"], "Save");
    assert_eq!(svc.holder().as_deref(), Some("p/writer"));
    let after = run(
        &a,
        TYPE,
        json!({ "app": "Notes", "ref": 1, "name": "Title", "text": "Plan" }),
    )
    .await
    .unwrap();
    assert_eq!(
        after["elements"][0]["value"], "Plan",
        "the answer is the window after"
    );
    run(
        &a,
        CLICK,
        json!({ "app": "Notes", "ref": "[2]", "name": "“Save”" }),
    )
    .await
    .unwrap();
    assert_eq!(
        *screen.log.lock().unwrap(),
        vec![
            "press Notes/Title",
            "open Notes",
            "keys cmd+a",
            "type \"Plan\"",
            "press Notes/Save"
        ]
    );
    // The name must be the number's: what the approval showed is what is pressed.
    let e = run(
        &a,
        CLICK,
        json!({ "app": "Notes", "ref": 1, "name": "Save" }),
    )
    .await
    .unwrap_err();
    assert!(e.contains("is the text field “Title”, not “Save”"), "{e}");
    // Numbers belong to the last look and its app.
    let e = run(
        &a,
        CLICK,
        json!({ "app": "Mail", "ref": 1, "name": "Send" }),
    )
    .await
    .unwrap_err();
    assert!(e.contains("last look, at Notes"), "{e}");
    let e = run(&a, CLICK, json!({ "app": "Notes", "ref": 9, "name": "x" }))
        .await
        .unwrap_err();
    assert!(e.contains("no element [9]"), "{e}");
    // The card an agent reads.
    let (card, _) = distill(LOOK, &look).unwrap();
    assert!(card.contains("[2] button \"Save\""), "{card}");
    // Every action but the list asks first, and one answer covers them all
    // (`tests/asks.rs`).
    let risky: Vec<String> = a
        .actions()
        .into_iter()
        .filter(|d| !d.risk.auto_runs())
        .inspect(|d| assert!(!d.ask_each_time, "{}", d.name))
        .filter(|d| {
            d.grant
                .as_ref()
                .and_then(|g| g.class(&json!({})))
                .as_deref()
                == Some(GRANT)
        })
        .map(|d| d.name)
        .collect();
    assert_eq!(
        risky,
        vec![
            ACTIVE_WINDOW,
            LOOK,
            OPEN,
            CLICK,
            TYPE,
            KEYS,
            MENU,
            SEE,
            POINT,
            SCROLL,
            DRAFT,
            SEND,
            CANCEL,
        ]
    );
    drop(a);
    assert_eq!(svc.holder(), None, "the screen is free when the turn ends");
}

#[tokio::test]
async fn active_window_reads_only_remembered_window_metadata() {
    let screen = notes();
    screen.keep_front_on("Notes");
    let svc = service(&screen);
    assert_eq!(svc.remember_front_app().unwrap().as_deref(), Some("Notes"));
    assert_eq!(svc.context().app.as_deref(), Some("Notes"));

    let agent = svc.for_owner("p/writer");
    let metadata = run(&agent, ACTIVE_WINDOW, json!({})).await.unwrap();
    assert_eq!(metadata["app"], "Notes");
    assert_eq!(metadata["window"], "Notes window");
    assert_eq!(*screen.log.lock().unwrap(), vec!["metadata Notes"]);
    let (card, _) = distill(ACTIVE_WINDOW, &metadata).unwrap();
    assert!(card.contains("metadata only") && card.contains("untrusted"));

    let error = run(
        &agent,
        CLICK,
        json!({ "app": "Notes", "ref": 1, "name": "Title" }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("look at Notes first"), "{error}");

    svc.set_on(false).unwrap();
    assert_eq!(
        svc.context().app,
        None,
        "turning off clears the volatile app"
    );
}

#[test]
fn turning_computer_off_revokes_an_in_flight_ambient_sample() {
    use std::sync::{Arc, Barrier};

    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let screen = notes().apps_blocked(started.clone(), release.clone());
    screen.keep_front_on("Notes");
    let svc = service(&screen);
    let sampling = {
        let svc = svc.clone();
        std::thread::spawn(move || svc.remember_front_app())
    };

    started.wait();
    svc.set_on(false).unwrap();
    svc.set_on(true).unwrap();
    release.wait();

    assert_eq!(sampling.join().unwrap().unwrap(), None);
    assert_eq!(svc.context().app, None);
}

#[test]
fn ambient_context_has_truthful_empty_failure_and_permission_states() {
    let no_front = notes();
    let svc = service(&no_front);
    assert_eq!(svc.remember_front_app().unwrap(), None);
    let context = svc.context();
    assert!(context.on && context.app.is_none() && !context.sample_failed);

    let failed = FakeScreen::new().app("Notes", &[]).apps_fail();
    let svc = service(&failed);
    assert!(svc.remember_front_app().is_err());
    let context = svc.context();
    assert!(context.on && context.app.is_none() && context.sample_failed);

    let denied = FakeScreen::new()
        .app("Notes", &[])
        .not_allowed_until_asked(Duration::from_secs(60));
    let context = service(&denied).context();
    assert_eq!(context.permission, "not_yet");
    assert!(context.permission_help.is_some());
}

/// Keys go to whatever is in front, so they are sent only once the approved app is there.
#[tokio::test]
async fn typed_keys_go_only_to_the_approved_app_in_front() {
    let screen = notes().fields_refuse_text();
    let svc = service(&screen);
    let a = svc.for_owner("p/a");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    let typed = json!({ "app": "Notes", "ref": 1, "name": "Title", "text": "Plan" });
    run(&a, TYPE, typed).await.unwrap();
    let log = screen.log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            "press Notes/Title",
            "open Notes",
            "keys cmd+a",
            "type \"Plan\""
        ]
    );

    // Another window keeps the front: nothing is typed and the agent is told why.
    let screen = notes().fields_refuse_text().front_stays_elsewhere();
    let svc = service(&screen);
    let a = svc.for_owner("p/a");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    let typed = json!({ "app": "Notes", "ref": 1, "name": "Title", "text": "Plan" });
    let e = run(&a, TYPE, typed).await.unwrap_err();
    assert!(
        e.contains("did not come to the front, so nothing was typed"),
        "{e}"
    );
    let e = run(&a, KEYS, json!({ "app": "Notes", "keys": "cmd+s" }))
        .await
        .unwrap_err();
    assert!(e.contains("nothing was typed"), "{e}");
    let log = screen.log.lock().unwrap().clone();
    assert!(
        !log.iter()
            .any(|l| l.starts_with("type") || l.starts_with("keys")),
        "{log:?}"
    );
}

/// Menus are walked by name: a menu lists its items, an item is chosen, a greyed one refused.
#[tokio::test]
async fn an_agent_uses_the_menu_bar_by_name() {
    let screen = notes();
    let svc = service(&screen);
    let a = svc.for_owner("p/a");
    let bar = run(&a, MENU, json!({ "app": "Notes" })).await.unwrap();
    assert_eq!(bar["items"][0]["name"], "File");
    let file = run(&a, MENU, json!({ "app": "Notes", "path": "File" }))
        .await
        .unwrap();
    let (card, _) = distill(MENU, &file).unwrap();
    assert!(card.contains("- Save (cmd+s)"), "{card}");
    assert!(card.contains("- Export ›"), "{card}");
    let chosen = run(
        &a,
        MENU,
        json!({ "app": "Notes", "path": "File › Export > PDF…" }),
    )
    .await
    .unwrap();
    assert_eq!(chosen["chose"], "File > Export > PDF…");
    assert!(chosen["elements"].is_array(), "the window after");
    let e = run(&a, MENU, json!({ "app": "Notes", "path": "Edit > Undo" }))
        .await
        .unwrap_err();
    assert!(e.contains("not available now"), "{e}");
    let log = screen.log.lock().unwrap().clone();
    assert_eq!(
        log.iter()
            .filter(|l| l.starts_with("menu"))
            .collect::<Vec<_>>(),
        vec!["menu Notes/File > Export > PDF…"]
    );
}

/// Scrolling goes to the window, or to the part holding an element; the pointer opens and
/// shows menus.
#[tokio::test]
async fn an_agent_scrolls_and_double_clicks() {
    let screen = notes();
    let svc = service(&screen);
    let a = svc.for_owner("p/a");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    run(&a, SCROLL, json!({ "app": "Notes", "direction": "down" }))
        .await
        .unwrap();
    run(
        &a,
        SCROLL,
        json!({ "app": "Notes", "direction": "up", "pages": 2, "ref": 2, "name": "Save" }),
    )
    .await
    .unwrap();
    run(
        &a,
        CLICK,
        json!({ "app": "Notes", "ref": 2, "name": "Save", "double": true }),
    )
    .await
    .unwrap();
    let e = run(&a, SCROLL, json!({ "app": "Notes", "direction": "left" }))
        .await
        .unwrap_err();
    assert!(e.contains("up or down"), "{e}");
    let log: Vec<String> = screen
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("scroll") || l.starts_with("pointer"))
        .cloned()
        .collect();
    assert_eq!(
        log,
        vec![
            "scroll 1 at 400,300",
            "scroll -2 at 60,60",
            "pointer Double at 60,60"
        ]
    );
}

/// A picture of the window, and a click at a spot in it, mapped to the screen.
#[tokio::test]
async fn an_agent_sees_a_window_and_points_at_a_spot() {
    let screen = notes();
    let svc = service(&screen);
    let a = svc.for_owner("p/a");
    let e = run(&a, POINT, json!({ "app": "Notes", "x": 10, "y": 10 }))
        .await
        .unwrap_err();
    assert!(e.contains("take a picture of Notes first"), "{e}");
    let c = svc.for_owner("p/a");
    let out = c
        .run(SEE, json!({ "app": "Notes" }), &RunCtx::new(&NoBlobs))
        .await
        .unwrap();
    assert_eq!(out.pictures.len(), 1, "the picture goes to the model");
    assert_eq!(out.output["picture"]["width"], 400);
    let (card, _) = distill(SEE, &out.output).unwrap();
    assert!(card.contains("400 by 300 pixels"), "{card}");
    // The fake's picture is half the window's size: pixel (100, 50) is point (200, 100).
    run(
        &c,
        POINT,
        json!({ "app": "Notes", "x": 100, "y": 50, "what": "the Save button" }),
    )
    .await
    .unwrap();
    let e = run(&c, POINT, json!({ "app": "Notes", "x": 900, "y": 50 }))
        .await
        .unwrap_err();
    assert!(e.contains("outside the picture"), "{e}");
    let log = screen.log.lock().unwrap().clone();
    assert!(
        log.contains(&"pointer Click at 200,100".to_string()),
        "{log:?}"
    );
}

#[tokio::test]
async fn one_agent_at_a_time_and_the_next_waits_its_turn() {
    let screen = notes();
    let svc = service(&screen).timing(Duration::from_secs(30), Duration::from_millis(10));
    let a = svc.for_owner("p/a");
    let b = svc.for_owner("p/b");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    let waiting = tokio::spawn(async move {
        let t = Instant::now();
        let r = run(&b, LOOK, json!({ "app": "Mail" })).await;
        (r, t.elapsed(), b)
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "b waits while a holds the screen");
    drop(a);
    let (r, waited, b) = waiting.await.unwrap();
    r.unwrap();
    assert!(waited >= Duration::from_millis(250));
    assert_eq!(svc.holder().as_deref(), Some("p/b"));
    drop(b);
}

#[tokio::test]
async fn an_idle_holder_gives_way_and_must_look_again() {
    let screen = notes();
    let svc = ComputerService::with_screen(Arc::new(screen.clone()))
        .timing(Duration::from_millis(150), Duration::from_millis(10));
    let a = svc.for_owner("p/a");
    let b = svc.for_owner("p/b");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    run(&b, LOOK, json!({ "app": "Mail" })).await.unwrap();
    assert_eq!(svc.holder().as_deref(), Some("p/b"));
    drop(b);
    let e = run(
        &a,
        CLICK,
        json!({ "app": "Notes", "ref": 2, "name": "Save" }),
    )
    .await
    .unwrap_err();
    assert!(e.contains("look at Notes first"), "{e}");
}

#[tokio::test]
async fn the_person_comes_first_and_can_take_the_computer_back() {
    let screen = notes();
    let svc = service(&screen);
    let a = svc.for_owner("session:s1");
    screen.person_types();
    let t = Instant::now();
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    assert!(
        t.elapsed() >= Duration::from_millis(250),
        "waited for the person to pause"
    );

    let mut watch = svc.watch();
    assert_eq!(svc.take_back().as_deref(), Some("session:s1"));
    watch.changed().await.unwrap();
    assert_eq!(*watch.borrow(), None);
    let e = run(
        &a,
        CLICK,
        json!({ "app": "Notes", "ref": 2, "name": "Save" }),
    )
    .await
    .unwrap_err();
    assert!(e.contains("took the computer back"), "{e}");
    assert!(screen.log.lock().unwrap().is_empty(), "nothing was pressed");
    // A new turn may ask again.
    drop(a);
    let again = svc.for_owner("session:s1");
    run(&again, LOOK, json!({ "app": "Notes" })).await.unwrap();
}

/// The macOS screen works behind the person's window: a look does not wait for them to pause,
/// and keys reach an app that stays behind another one, where a foreground screen refuses.
#[tokio::test]
async fn in_the_background_the_person_keeps_working_and_the_app_stays_behind() {
    for background in [false, true] {
        let screen = if background {
            notes().front_stays_elsewhere().background()
        } else {
            notes().front_stays_elsewhere()
        };
        let svc = service(&screen);
        let a = svc.for_owner("session:s1");
        screen.person_types();
        let t = Instant::now();
        run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
        assert_eq!(
            t.elapsed() < Duration::from_millis(250),
            background,
            "only a foreground screen waits for the person to pause"
        );
        let keys = run(&a, KEYS, json!({ "app": "Notes", "keys": "cmd+s" })).await;
        if background {
            keys.unwrap();
            let log = screen.log.lock().unwrap();
            assert!(log.iter().any(|l| l == "keys cmd+s"), "{log:?}");
            assert!(
                !log.iter().any(|l| l.starts_with("open ")),
                "nothing was brought forward: {log:?}"
            );
        } else {
            let e = keys.unwrap_err();
            assert!(e.contains("did not come to the front"), "{e}");
        }
    }
}

#[tokio::test]
async fn permission_is_asked_once_and_the_action_goes_on() {
    let screen = notes().not_allowed_until_asked(Duration::from_millis(100));
    let svc = service(&screen);
    assert!(matches!(svc.access(), Access::NotYet(_)));
    let a = svc.for_owner("p/a");
    run(&a, LOOK, json!({ "app": "Notes" })).await.unwrap();
    assert_eq!(screen.log.lock().unwrap()[0], "asked for access");
    assert_eq!(svc.access(), Access::Allowed);
}

/// The real screen: lists the open apps. With `ARK_COMPUTER_LIVE=1`, and where ARK has the
/// person's permission, it also reads the window in front, without acting on it, and prints only
/// how many elements of each kind it found (never what the window shows).
#[tokio::test]
async fn the_native_screen_lists_apps_and_reads_the_front_window() {
    let Some(svc) = ComputerService::native() else {
        eprintln!("skipped: no native screen on this platform");
        return;
    };
    let a = svc.for_owner("test");
    let apps = run(&a, APPS, json!({})).await.unwrap();
    let list = apps["apps"].as_array().unwrap();
    assert!(!list.is_empty(), "no open apps listed");
    if std::env::var_os("ARK_COMPUTER_LIVE").is_none() || svc.access() != Access::Allowed {
        eprintln!("skipped reading a window: set ARK_COMPUTER_LIVE=1 where ARK may use apps");
        return;
    }
    let front = list
        .iter()
        .find(|a| a["front"] == json!(true))
        .unwrap_or(&list[0]);
    let name = front["name"].as_str().unwrap();
    let look = run(&a, LOOK, json!({ "app": name })).await.unwrap();
    assert_eq!(look["app"], name);
    let mut roles = std::collections::BTreeMap::new();
    for e in look["elements"].as_array().unwrap() {
        *roles
            .entry(e["role"].as_str().unwrap().to_string())
            .or_insert(0) += 1;
    }
    eprintln!("{} lines of text; elements: {roles:?}", look["lines"]);
}

/// The real screen, in the background: with `ARK_COMPUTER_LIVE=1` where ARK may use apps, it
/// types into a new TextEdit document behind the app in front, and that app stays in front.
/// The document is closed without saving at the end; TextEdit keeps its empty window until it
/// next comes forward (close it with cmd+w).
#[tokio::test]
async fn the_native_screen_types_into_an_app_behind_the_persons_window() {
    let Some(svc) = ComputerService::native() else {
        eprintln!("skipped: no native screen on this platform");
        return;
    };
    if std::env::var_os("ARK_COMPUTER_LIVE").is_none() || svc.access() != Access::Allowed {
        eprintln!("skipped: set ARK_COMPUTER_LIVE=1 where ARK may use apps");
        return;
    }
    let script = |source: &str| {
        let out = std::process::Command::new("osascript")
            .args(["-e", source])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let a = svc.for_owner("test");
    let front = || async {
        let apps = run(&a, APPS, json!({})).await.unwrap();
        apps["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|app| app["front"] == json!(true))
            .map(|app| app["name"].as_str().unwrap().to_string())
    };
    let before = front().await;
    assert_ne!(
        before.as_deref(),
        Some("TextEdit"),
        "put another app in front first"
    );
    script("tell application \"TextEdit\" to make new document with properties {name:\"ark-background-check\"}");
    std::thread::sleep(Duration::from_millis(800));
    let shown = run(&a, OPEN, json!({ "app": "TextEdit" })).await.unwrap();
    assert_eq!(
        front().await,
        before,
        "open left the app in front where it was"
    );
    let area = shown["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == "text area" || e["identifier"] == "First Text View")
        .unwrap_or_else(|| panic!("no document text in {shown}"))
        .clone();
    run(
        &a,
        TYPE,
        json!({ "app": "TextEdit", "ref": area["ref"], "name": area["name"], "text": "typed by ARK in the background" }),
    )
    .await
    .unwrap();
    assert_eq!(
        front().await,
        before,
        "typing left the app in front where it was"
    );
    let text = script("tell application \"TextEdit\" to get text of document 1");
    let closed = script("tell application \"TextEdit\" to close document 1 saving no");
    eprintln!("front before and after: {before:?}; closed: {closed:?}");
    assert_eq!(text, "typed by ARK in the background");
}

/// The real screen, in the background: with `ARK_COMPUTER_LIVE=1` where ARK may use apps, it
/// presses Calculator's 7 and sends it Escape while another app stays in front.
#[tokio::test]
async fn the_native_screen_presses_and_keys_an_app_behind_the_persons_window() {
    let Some(svc) = ComputerService::native() else {
        eprintln!("skipped: no native screen on this platform");
        return;
    };
    if std::env::var_os("ARK_COMPUTER_LIVE").is_none() || svc.access() != Access::Allowed {
        eprintln!("skipped: set ARK_COMPUTER_LIVE=1 where ARK may use apps");
        return;
    }
    let a = svc.for_owner("test");
    let front = || async {
        let apps = run(&a, APPS, json!({})).await.unwrap();
        apps["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|app| app["front"] == json!(true))
            .map(|app| app["name"].as_str().unwrap().to_string())
    };
    let before = front().await;
    assert_ne!(
        before.as_deref(),
        Some("Calculator"),
        "put another app in front first"
    );
    let shown = run(&a, OPEN, json!({ "app": "Calculator" })).await.unwrap();
    assert_eq!(
        front().await,
        before,
        "open left the app in front where it was"
    );
    let seven = shown["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == "button" && e["name"] == "7")
        .unwrap_or_else(|| panic!("no 7 in {shown}"))
        .clone();
    let after = run(
        &a,
        CLICK,
        json!({ "app": "Calculator", "ref": seven["ref"], "name": "7" }),
    )
    .await
    .unwrap();
    assert_eq!(
        front().await,
        before,
        "a press left the app in front where it was"
    );
    eprintln!(
        "after 7: {}",
        after["text"]
            .as_str()
            .unwrap_or("")
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ")
    );
    // Calculator has no focused field to post a key to, so Escape must bring it forward: that
    // waits for the person to pause, and refuses while they keep working.
    match run(&a, KEYS, json!({ "app": "Calculator", "keys": "escape" })).await {
        Ok(_) => eprintln!("escape: sent during a pause, the person's window given back"),
        Err(e) => {
            assert!(e.contains("try again when they pause"), "{e}");
            eprintln!("escape: refused while the person was working");
        }
    }
    assert_eq!(
        front().await,
        before,
        "a key left the app in front where it was"
    );
    eprintln!("front before and after: {before:?}");
}

fn switch_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ark-computer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("computer.json")
}

fn owner_of<'a>(
    name: &'a str,
    model: &'a Arc<dyn ark_model::Provider>,
) -> ark_runtime::per_agent::Owner<'a> {
    ark_runtime::per_agent::Owner { name, model }
}

/// ADR 0045: off until the person turns it on, asked for mid-task, remembered, and off again
/// stops an owner that still holds the tools at its next action.
#[tokio::test]
async fn the_computer_is_off_until_the_person_turns_it_on_and_remembers_it() {
    use ark_runtime::per_agent::PerAgent;
    let file = switch_file("switch");
    let screen = FakeScreen::new().app("Lumen", &[("button", "Go")]);
    let svc = service(&screen).switched(file.clone());
    assert!(!svc.is_on() && svc.switchable());

    let model: Arc<dyn ark_model::Provider> = Arc::new(ark_model::MockProvider::new("m", vec![]));
    let src = Computers(svc.clone());
    let owner = owner_of("session:1", &model);
    assert!(
        src.for_owner(&owner).actions().is_empty(),
        "no tools while off"
    );
    assert_eq!(
        src.listing().actions().len(),
        ACTIONS.len(),
        "the app still offers it"
    );

    let ask = src.acquisition(&owner).expect("an owner can ask for it");
    assert!(ask.active() && ask.asks_a_person("computer"));
    assert!(ask
        .offer()
        .unwrap()
        .contains("- computer: use the apps on this computer"));
    assert!(ask.find("computer").await.contains("is off"));

    let no = ask.acquire("computer", false).await;
    assert!(!no.changed && no.tools.is_empty() && !svc.is_on());

    let yes = ask.acquire("computer", true).await;
    assert!(yes.changed && svc.is_on(), "{}", yes.summary);
    assert_eq!(yes.tools[0].actions().len(), ACTIONS.len());
    assert!(!ask.active() && ask.offer().is_none());

    // Kept: a new process starts with it on.
    let again = service(&screen).switched(file.clone());
    assert!(again.is_on());

    // Off while an owner holds its tools: the next action refuses.
    let c = svc.for_owner("session:1");
    run(&c, LOOK, json!({ "app": "Lumen" })).await.unwrap();
    svc.set_on(false).unwrap();
    let err = run(&c, LOOK, json!({ "app": "Lumen" })).await.unwrap_err();
    assert!(err.contains("turned the Computer off"), "{err}");
    assert!(!service(&screen).switched(file).is_on());
}

/// Turning it on asks the system for its permission then, not at the first action.
#[tokio::test]
async fn turning_it_on_asks_for_the_permission_and_waits_for_it() {
    use ark_runtime::per_agent::PerAgent;
    let screen = FakeScreen::new()
        .app("Lumen", &[("button", "Go")])
        .not_allowed_until_asked(Duration::from_millis(100));
    let svc = service(&screen).switched(switch_file("permission"));
    let model: Arc<dyn ark_model::Provider> = Arc::new(ark_model::MockProvider::new("m", vec![]));
    let ask = Computers(svc.clone())
        .acquisition(&owner_of("session:1", &model))
        .unwrap();
    let got = ask.acquire("computer", true).await;
    assert!(got.changed && svc.is_on(), "{}", got.summary);
    assert_eq!(svc.access(), Access::Allowed);
}

#[test]
fn a_need_names_the_computer_by_whole_words() {
    for yes in [
        "computer",
        "use the apps on this computer",
        "native app control",
        "desktop application",
        "computer.open",
    ] {
        assert!(names_the_computer(yes), "{yes}");
    }
    for no in [
        "WhatsApp",
        "send mail",
        "calendar",
        "mapping",
        "screenshots",
    ] {
        assert!(!names_the_computer(no), "{no}");
    }
}

/// A service without a switch (an app's own, tests) is on, as before ADR 0045.
#[test]
fn a_service_without_a_switch_is_on_and_asks_nothing() {
    use ark_runtime::per_agent::PerAgent;
    let svc = service(&notes());
    let model: Arc<dyn ark_model::Provider> = Arc::new(ark_model::MockProvider::new("m", vec![]));
    assert!(svc.is_on() && !svc.switchable());
    assert!(Computers(svc).acquisition(&owner_of("x", &model)).is_none());
}
