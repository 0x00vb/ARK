use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, Usage};
use ark_sdk::prelude::ark_runtime::skills::packages::{PackagedSkill, SkillPackage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

async fn host(dir: &std::path::Path) -> Arc<Host> {
    let model = MockProvider::new("m", vec![]).with_fallback(|_| Completion {
        content: "done".into(),
        tool_calls: vec![],
        usage: Usage::default(),
    });
    let mut ark = Ark::builder()
        .store_root(dir)
        .provider(Arc::new(model))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    ark.engine_mut().add_skill_package(SkillPackage { id:"example.notes".into(), name:"Release notes".into(), summary:"Write release notes".into(), publisher:"Example".into(), version:"1".into(), skills:vec![PackagedSkill { name:"release-notes".into(), files:BTreeMap::from([("SKILL.md".into(),"---\nname: release-notes\ndescription: Write release notes\n---\nUse actual changes.".into())]) }] }).unwrap();
    ArkUi::new(App::new("Test").ark(ark))
        .open()
        .await
        .unwrap()
        .0
}
async fn resource(
    host: &Arc<Host>,
    location: &Value,
    kind: &str,
    input: Value,
) -> anyhow::Result<Value> {
    host.call("resource",json!({"ref":{"location":location,"kind":"project","id":""},"operation":{"kind":kind,"input":input}})).await
}

#[tokio::test]
async fn scoped_install_is_visible_to_ui_and_cli_and_removal_keeps_the_payload() {
    let dir = tempfile::tempdir().unwrap();
    let h = host(dir.path()).await;
    let summary = h
        .call("add_project", json!({"name":"Package proof"}))
        .await
        .unwrap();
    let location = summary["location"].clone();
    let catalog = resource(&h, &location, "skill_packages", json!({}))
        .await
        .unwrap();
    let digest = catalog[0]["digest"].as_str().unwrap();
    assert_eq!(catalog[0]["installed"], false);
    resource(
        &h,
        &location,
        "skill_package_install",
        json!({"id":"example.notes","digest":digest}),
    )
    .await
    .unwrap();
    let found = resource(&h, &location, "skills", json!({})).await.unwrap();
    assert_eq!(found[0]["name"], "release-notes");
    assert_eq!(found[0]["package"], "example.notes");
    let command = h
        .call("command", json!({"line":"/skills available"}))
        .await
        .unwrap();
    assert!(command.to_string().contains("Installed"));
    resource(
        &h,
        &location,
        "skill_package_remove",
        json!({"id":"example.notes"}),
    )
    .await
    .unwrap();
    assert!(resource(&h, &location, "skills", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(dir
        .path()
        .join("skill-packages")
        .join(digest)
        .join("release-notes/SKILL.md")
        .exists());
}

#[tokio::test]
async fn stale_digest_foreign_computer_and_wrong_resource_cannot_install() {
    let dir = tempfile::tempdir().unwrap();
    let h = host(dir.path()).await;
    let summary = h
        .call("add_project", json!({"name":"Scope proof"}))
        .await
        .unwrap();
    let location = summary["location"].clone();
    assert!(resource(
        &h,
        &location,
        "skill_package_install",
        json!({"id":"example.notes","digest":"stale"})
    )
    .await
    .is_err());
    let mut wrong = location.clone();
    wrong["runtime"] = json!("another-computer");
    assert!(resource(
        &h,
        &wrong,
        "skill_package_install",
        json!({"id":"example.notes","digest":"stale"})
    )
    .await
    .is_err());
    assert!(h.call("resource",json!({"ref":{"location":location,"kind":"file","id":""},"operation":{"kind":"skill_package_install","input":{"id":"example.notes","digest":"stale"}}})).await.is_err());
    assert_eq!(
        resource(&h, &location, "skill_packages", json!({}))
            .await
            .unwrap()[0]["installed"],
        false
    );
}

#[tokio::test]
async fn private_source_preview_install_export_and_foreign_destination_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let h = host(dir.path()).await;
    let summary = h
        .call("add_project", json!({"name":"Import proof"}))
        .await
        .unwrap();
    let location = summary["location"].clone();
    let source = json!({"id":"private.notes","name":"Private notes","summary":"Private workflow","publisher":"Local author","version":"1","skills":[{"name":"private-notes","files":{"SKILL.md":"---\nname: private-notes\ndescription: Private notes\n---\nKeep evidence."}}]}).to_string();
    let preview = resource(&h, &location, "skill_packages", json!({"source":source}))
        .await
        .unwrap();
    assert_eq!(
        resource(&h, &location, "skills", json!({}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let mut foreign = location.clone();
    foreign["runtime"] = json!("foreign");
    assert!(resource(
        &h,
        &foreign,
        "skill_package_install",
        json!({"source":source,"digest":preview["digest"]})
    )
    .await
    .is_err());
    resource(
        &h,
        &location,
        "skill_package_install",
        json!({"source":source,"digest":preview["digest"]}),
    )
    .await
    .unwrap();
    let exported = resource(
        &h,
        &location,
        "skill_packages",
        json!({"id":"private.notes"}),
    )
    .await
    .unwrap();
    assert_eq!(exported, serde_json::from_str::<Value>(&source).unwrap());
    assert_eq!(
        resource(&h, &location, "skills", json!({})).await.unwrap()[0]["name"],
        "private-notes"
    );
}
