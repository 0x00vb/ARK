use ark_core::Project;
use ark_runtime::Engine;
use ark_store::Store;
use ark_tools::Registry;

#[test]
fn an_agent_works_in_its_own_folder_else_its_projects() {
    let root = tempfile::tempdir().unwrap();
    let shared = tempfile::tempdir().unwrap();
    let own = tempfile::tempdir().unwrap();
    let e = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    let top = Project::new("Work", "");
    e.store.put_project(&top).unwrap();
    e.set_workspace(&top.id, shared.path()).unwrap();
    let scout = Project::new("scout", "").with_parent(top.id.clone());
    let writer = Project::new("writer", "").with_parent(top.id.clone());
    e.store.put_project(&scout).unwrap();
    e.store.put_project(&writer).unwrap();
    let canon = |p: &std::path::Path| p.canonicalize().unwrap();

    assert_eq!(
        canon(&e.workspace_dir(&scout.id).unwrap()),
        canon(shared.path())
    );
    e.set_workspace(&scout.id, own.path()).unwrap();
    assert_eq!(
        canon(&e.workspace_dir(&scout.id).unwrap()),
        canon(own.path())
    );
    assert_eq!(
        canon(&e.workspace_dir(&writer.id).unwrap()),
        canon(shared.path()),
        "the other agent keeps the project's folder"
    );
}

#[test]
fn an_agents_folder_that_is_gone_is_an_error_not_a_new_folder() {
    let root = tempfile::tempdir().unwrap();
    let own = tempfile::tempdir().unwrap();
    let e = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    let top = Project::new("Work", "");
    e.store.put_project(&top).unwrap();
    let scout = Project::new("scout", "").with_parent(top.id.clone());
    e.store.put_project(&scout).unwrap();
    e.set_workspace(&scout.id, own.path()).unwrap();
    let gone = own.path().to_path_buf();
    drop(own);
    assert!(e.workspace_dir(&scout.id).is_err());
    assert!(!gone.exists(), "no folder made in its place");
}
