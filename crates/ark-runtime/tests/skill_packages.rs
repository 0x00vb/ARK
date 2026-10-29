use ark_core::ProjectId;
use ark_runtime::skills::{
    self,
    packages::{self, PackagedSkill, SkillPackage},
};
use ark_runtime::Engine;
use ark_store::Store;
use ark_tools::Registry;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};

fn package(id: &str, skill: &str) -> SkillPackage {
    SkillPackage { id: id.into(), name: "Writing workflow".into(), summary: "Prepare editable release notes".into(), publisher: "Example".into(), version: "1.0".into(), skills: vec![PackagedSkill { name: skill.into(), files: BTreeMap::from([
        ("SKILL.md".into(), format!("---\nname: {skill}\ndescription: Write release notes from verified changes.\n---\nRead references/checklist.md.\n")),
        ("references/checklist.md".into(), "Use the actual changes.\n".into()),
    ]) }] }
}
fn engine(path: &std::path::Path, package: &SkillPackage) -> Engine {
    let mut engine = Engine::new(Store::open(path).unwrap(), Registry::new());
    engine.add_skill_package(package.clone()).unwrap();
    engine
}

#[test]
fn installed_content_is_discovered_after_restart_and_without_catalog_registration() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let project = ProjectId::new();
    {
        let e = engine(root.path(), &p);
        assert!(skills::discover(&skills::dirs(&e, &project)).is_empty());
        let row = packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
        assert!(row.installed);
        let all = skills::discover(&skills::dirs(&e, &project));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "release-notes");
        assert_eq!(
            fs::read_to_string(all[0].dir.join("references/checklist.md")).unwrap(),
            "Use the actual changes.\n"
        );
    }
    let e = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    assert_eq!(
        skills::discover(&skills::dirs(&e, &project))[0].name,
        "release-notes"
    );
    let row = &packages::list(&e).unwrap()[0];
    assert!(row.installed);
    assert!(!row.available);
    packages::remove(&e, &p.id).unwrap();
    assert!(skills::discover(&skills::dirs(&e, &project)).is_empty());
}

#[test]
fn reinstall_converges_without_recopy_and_a_stale_selection_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    assert!(packages::install(&e, &project, &p.id, "stale").is_err());
    assert!(!root.path().join("skill-packages").exists());
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    let dir = packages::directories(&e).unwrap()[0].1.clone();
    let before = fs::metadata(dir.join("release-notes/SKILL.md"))
        .unwrap()
        .modified()
        .unwrap();
    let count = fs::read_dir(root.path().join("skill-packages"))
        .unwrap()
        .count();
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    assert_eq!(
        fs::metadata(dir.join("release-notes/SKILL.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert_eq!(
        fs::read_dir(root.path().join("skill-packages"))
            .unwrap()
            .count(),
        count
    );
}

#[test]
fn removal_and_refused_reinstall_preserve_locally_modified_payload() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    let path = packages::directories(&e).unwrap()[0]
        .1
        .join("release-notes/SKILL.md");
    fs::write(&path, "my local work").unwrap();
    assert!(packages::list(&e).unwrap()[0].error.is_some());
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap()).is_err());
    packages::remove(&e, &p.id).unwrap();
    assert!(!packages::list(&e).unwrap()[0].installed);
    assert_eq!(fs::read_to_string(&path).unwrap(), "my local work");
    assert!(skills::discover(&skills::dirs(&e, &project)).is_empty());
}

#[test]
fn local_and_other_package_names_are_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let mut e = engine(root.path(), &p);
    let project = ProjectId::new();
    let local = e
        .workspace_dir(&project)
        .unwrap()
        .join(".agents/skills/release-notes");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("SKILL.md"), &p.skills[0].files["SKILL.md"]).unwrap();
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap()).is_err());
    assert_eq!(
        fs::read_to_string(local.join("SKILL.md")).unwrap(),
        p.skills[0].files["SKILL.md"]
    );
    let second = package("example.other", "another-skill");
    e.add_skill_package(second.clone()).unwrap();
    packages::install(&e, &project, &second.id, &second.digest().unwrap()).unwrap();
    let conflicting = package("example.conflict", "another-skill");
    e.add_skill_package(conflicting.clone()).unwrap();
    assert!(packages::install(
        &e,
        &project,
        &conflicting.id,
        &conflicting.digest().unwrap()
    )
    .is_err());
    assert!(
        packages::list(&e)
            .unwrap()
            .iter()
            .find(|r| r.id == second.id)
            .unwrap()
            .installed
    );
}

#[test]
fn partial_stages_are_inert_and_a_completed_unactivated_release_is_reused() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    let partial = root
        .path()
        .join("skill-packages/.stage-interrupted/release-notes");
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("SKILL.md"), "partial").unwrap();
    assert!(skills::discover(&skills::dirs(&e, &project)).is_empty());
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    let owned = packages::directories(&e).unwrap()[0].1.clone();
    packages::remove(&e, &p.id).unwrap();
    let stamp = fs::metadata(owned.join("release-notes/SKILL.md"))
        .unwrap()
        .modified()
        .unwrap();
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    assert_eq!(
        fs::metadata(owned.join("release-notes/SKILL.md"))
            .unwrap()
            .modified()
            .unwrap(),
        stamp
    );
    assert_eq!(
        fs::read_to_string(partial.join("SKILL.md")).unwrap(),
        "partial"
    );
}

#[test]
fn unsupported_paths_hooks_names_and_oversized_files_refuse_before_registration() {
    for path in [
        "../outside.md",
        "/outside.md",
        "reference\\outside.md",
        "secret:ads.md",
        "hooks/install.sh",
        ".hidden.md",
    ] {
        let mut p = package("example.notes", "release-notes");
        p.skills[0].files.insert(path.into(), "data".into());
        assert!(p.validate().is_err(), "{path}");
    }
    let mut p = package("example.notes", "release-notes");
    p.skills[0]
        .files
        .insert("huge.md".into(), "x".repeat(65537));
    assert!(p.validate().is_err());
    let mut p = package("example.notes", "release-notes");
    p.skills.push(p.skills[0].clone());
    assert!(p.validate().is_err());
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let mut e = engine(root.path(), &p);
    assert!(e.add_skill_package(p).is_err());
}

#[test]
fn another_store_operation_refuses_without_activating_any_package() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    let dir = root.path().join("skill-packages");
    fs::create_dir(&dir).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join("operation.lock"))
        .unwrap();
    held.try_lock().unwrap();
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap())
        .unwrap_err()
        .to_string()
        .contains("running"));
    assert!(!packages::list(&e).unwrap()[0].installed);
}

#[cfg(unix)]
#[test]
fn redirected_release_and_lock_preserve_the_external_target() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    let dir = root.path().join("skill-packages");
    fs::create_dir(&dir).unwrap();
    fs::write(outside.path().join("keep"), "original").unwrap();
    symlink(outside.path(), dir.join(p.digest().unwrap())).unwrap();
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap()).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("keep")).unwrap(),
        "original"
    );
    fs::remove_file(dir.join(p.digest().unwrap())).unwrap();
    fs::remove_file(dir.join("operation.lock")).unwrap();
    symlink(outside.path().join("keep"), dir.join("operation.lock")).unwrap();
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap()).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("keep")).unwrap(),
        "original"
    );
}

#[test]
fn updating_keeps_the_previous_release_and_one_damaged_package_does_not_hide_another() {
    let root = tempfile::tempdir().unwrap();
    let project = ProjectId::new();
    let first = package("example.notes", "release-notes");
    let old_digest = first.digest().unwrap();
    {
        let e = engine(root.path(), &first);
        packages::install(&e, &project, &first.id, &old_digest).unwrap();
    }
    let mut updated = first.clone();
    updated.version = "2".into();
    updated.skills[0].files.insert(
        "references/checklist.md".into(),
        "Revised instructions".into(),
    );
    let mut e = engine(root.path(), &updated);
    let row = &packages::list(&e).unwrap()[0];
    assert!(row.update_available);
    assert_eq!(row.installed_version.as_deref(), Some("1.0"));
    packages::install(&e, &project, &updated.id, &updated.digest().unwrap()).unwrap();
    assert!(root
        .path()
        .join("skill-packages")
        .join(old_digest)
        .join("release-notes/SKILL.md")
        .exists());
    let second = package("example.second", "another-skill");
    e.add_skill_package(second.clone()).unwrap();
    packages::install(&e, &project, &second.id, &second.digest().unwrap()).unwrap();
    let old = packages::directories(&e)
        .unwrap()
        .into_iter()
        .find(|(id, _)| id == &updated.id)
        .unwrap()
        .1;
    fs::write(old.join("release-notes/SKILL.md"), "local edit").unwrap();
    let names = skills::discover(&skills::dirs(&e, &project))
        .into_iter()
        .map(|s| s.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["another-skill"]);
    assert!(packages::list(&e)
        .unwrap()
        .into_iter()
        .find(|row| row.id == updated.id)
        .unwrap()
        .error
        .is_some());
}

#[test]
fn an_unreadable_local_skill_name_still_blocks_installation() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    let local = e
        .workspace_dir(&project)
        .unwrap()
        .join(".agents/skills/release-notes");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("SKILL.md"), "unfinished personal instructions").unwrap();
    assert!(packages::install(&e, &project, &p.id, &p.digest().unwrap()).is_err());
    assert_eq!(
        fs::read_to_string(local.join("SKILL.md")).unwrap(),
        "unfinished personal instructions"
    );
}

#[test]
fn adding_unowned_directories_and_ambiguous_file_paths_never_promotes_new_content() {
    let mut p = package("example.notes", "release-notes");
    p.skills[0]
        .files
        .insert("references//extra.md".into(), "data".into());
    assert!(p.validate().is_err());
    p.skills[0].files.remove("references//extra.md");
    p.skills[0]
        .files
        .insert("SKILL.md/extra.md".into(), "data".into());
    assert!(p.validate().is_err());
    p.skills[0].files.remove("SKILL.md/extra.md");
    let root = tempfile::tempdir().unwrap();
    let e = engine(root.path(), &p);
    let project = ProjectId::new();
    packages::install(&e, &project, &p.id, &p.digest().unwrap()).unwrap();
    let owned = packages::directories(&e).unwrap()[0].1.clone();
    fs::create_dir(owned.join("my-empty-work-folder")).unwrap();
    assert!(packages::list(&e).unwrap()[0].error.is_some());
    packages::remove(&e, &p.id).unwrap();
    assert!(owned.join("my-empty-work-folder").is_dir());
}

#[test]
fn receipt_capacity_refuses_before_writing_or_hiding_existing_packages() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.next", "release-notes");
    let e = engine(root.path(), &p);
    for index in 0..128 {
        let previous = package(&format!("example.previous-{index}"), "old-notes");
        let record = serde_json::json!({
            "schema_version": 1, "digest": previous.digest().unwrap(),
            "package": previous, "active": false,
        });
        e.store
            .put_record(
                "skill_package",
                &format!("example.previous-{index}"),
                "store",
                &record,
            )
            .unwrap();
    }
    assert!(packages::install(&e, &ProjectId::new(), &p.id, &p.digest().unwrap()).is_err());
    assert_eq!(
        e.store
            .list_records("skill_package", Some("store"))
            .unwrap()
            .len(),
        128
    );
    assert!(!root
        .path()
        .join("skill-packages")
        .join(p.digest().unwrap())
        .exists());
    assert_eq!(packages::list(&e).unwrap().len(), 1);
}

#[test]
fn private_import_preview_digest_restart_export_and_removal_share_ownership() {
    let root = tempfile::tempdir().unwrap();
    let p = package("private.notes", "private-notes");
    let source = serde_json::to_string(&p).unwrap();
    let project = ProjectId::new();
    let e = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    let preview = packages::preview(&source).unwrap();
    assert_eq!(preview, p);
    assert!(packages::list(&e).unwrap().is_empty());
    assert!(packages::import(&e, &project, &source, "stale").is_err());
    let digest = preview.digest().unwrap();
    assert!(
        packages::import(&e, &project, &source, &digest)
            .unwrap()
            .installed
    );
    assert_eq!(packages::export(&e, &p.id).unwrap(), p);
    drop(e);
    let e = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    assert_eq!(
        skills::discover(&skills::dirs(&e, &project))[0].name,
        "private-notes"
    );
    packages::remove(&e, &p.id).unwrap();
    assert!(skills::discover(&skills::dirs(&e, &project)).is_empty());
    assert_eq!(packages::export(&e, &p.id).unwrap(), p);
    assert!(root.path().join("skill-packages").join(digest).exists());
}

#[test]
fn private_import_rejects_hooks_paths_oversize_and_registered_identity_substitution() {
    let root = tempfile::tempdir().unwrap();
    let p = package("example.notes", "release-notes");
    let e = engine(root.path(), &p);
    let mut data = serde_json::to_value(&p).unwrap();
    data["hooks"] = serde_json::json!({"install":"bad"});
    assert!(packages::preview(&data.to_string()).is_err());
    data.as_object_mut().unwrap().remove("hooks");
    data["skills"][0]["hooks"] = serde_json::json!({"install":"bad"});
    assert!(packages::preview(&data.to_string()).is_err());
    assert!(packages::preview(&" ".repeat(1_048_577)).is_err());
    let mut changed = p.clone();
    changed.skills[0]
        .files
        .insert("../bad.md".into(), "bad".into());
    assert!(packages::preview(&serde_json::to_string(&changed).unwrap()).is_err());
    changed = p.clone();
    changed.summary = "Substitute".into();
    assert!(packages::import(
        &e,
        &ProjectId::new(),
        &serde_json::to_string(&changed).unwrap(),
        &changed.digest().unwrap()
    )
    .is_err());
    assert!(packages::list(&e).unwrap().iter().all(|p| !p.installed));
}
