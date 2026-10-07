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
        vec![LOOK, OPEN, CLICK, TYPE, KEYS, MENU, SEE, POINT, SCROLL]
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
    let typed =
        json!({ "app": "Notes", "ref": 1, "name": "Title", "text": "Plan", "submit": true });
    run(&a, TYPE, typed).await.unwrap();
    let log = screen.log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            "press Notes/Title",
            "open Notes",
            "keys cmd+a",
            "type \"Plan\"",
            "open Notes",
            "keys return"
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
