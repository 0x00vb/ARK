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

struct TestMessages;

impl MessageEvidence for TestMessages {
    fn context(
        &self,
        look: &Look,
        recipient: Option<&str>,
        text: Option<&str>,
        _bound: Option<(&str, &str)>,
    ) -> std::result::Result<Option<MessageContext>, String> {
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
    // Every action but the list asks first, each time: no earlier answer covers it.
    let risky: Vec<String> = a
        .actions()
        .into_iter()
        .filter(|d| !d.risk.auto_runs() && d.ask_each_time)
        .map(|d| d.name)
        .collect();
    assert_eq!(
        risky,
        vec![LOOK, OPEN, CLICK, TYPE, KEYS, MENU, SEE, POINT, SCROLL, DRAFT, SEND, CANCEL,]
    );
    drop(a);
    assert_eq!(svc.holder(), None, "the screen is free when the turn ends");
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
