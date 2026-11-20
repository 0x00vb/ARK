//! Real processes through `/bin/sh`: ready, ended, stopped with their children, the same
//! command once, the limit, a missing program, and the sweep after a crash.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use ark_proc::{Event, Processes, Refused, Shell, Start, State};

fn env_with(path: &str) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> =
        std::env::vars_os().filter(|(k, _)| k != "PATH").collect();
    env.push(("PATH".into(), path.into()));
    env
}

fn service(dir: &Path) -> Processes {
    let path = std::env::var("PATH").unwrap_or_default();
    Processes::with_shell(dir.join("store"), Shell::fixed("/bin/sh", env_with(&path))).unwrap()
}

fn start(cmd: &str) -> Start {
    Start {
        command: cmd.into(),
        ..Start::default()
    }
}

fn has_python() -> bool {
    std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn group_alive(pgid: u32) -> bool {
    std::process::Command::new("pgrep")
        .args(["-g", &pgid.to_string()])
        .output()
        .is_ok_and(|o| !o.stdout.is_empty())
}

#[tokio::test]
async fn a_server_is_ready_at_the_address_it_prints_and_its_page_opens() {
    if !has_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let mut events = p.subscribe();
    let req = Start {
        command: "python3 -u -m http.server 0 --bind 127.0.0.1".into(),
        open: Some("/home".into()),
        ..Start::default()
    };
    let started = p.start(dir.path(), &req).await.unwrap();
    let now = p
        .settle(&started.id, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(now.state, State::Ready, "{:?}", p.output(&started.id, 2000));
    let url = now.url.clone().unwrap();
    assert!(url.starts_with("http://localhost:"), "{url}");

    let mut opened = None;
    while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
        if let Event::Changed { open: Some(o), .. } = ev {
            opened = Some(o);
            break;
        }
    }
    assert_eq!(opened.unwrap(), format!("{}home", url));
    p.stop(&started.id).await;
}

#[tokio::test]
async fn a_port_the_agent_names_makes_it_ready_when_nothing_is_printed() {
    if !has_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let port = free_port();
    let req = Start {
        command: format!(
            "python3 -c \"import socket,time;s=socket.socket();s.bind(('127.0.0.1',{port}));s.listen();time.sleep(60)\""
        ),
        port: Some(port),
        ..Start::default()
    };
    let started = p.start(dir.path(), &req).await.unwrap();
    let now = p
        .settle(&started.id, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(now.state, State::Ready);
    assert_eq!(
        now.url.as_deref(),
        Some(format!("http://localhost:{port}/").as_str())
    );
    p.stop(&started.id).await;
}

#[tokio::test]
async fn a_process_that_ends_early_says_how_with_its_output() {
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let started = p
        .start(dir.path(), &start("echo boom >&2; exit 3"))
        .await
        .unwrap();
    let now = p
        .settle(&started.id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(now.state, State::Exited);
    assert_eq!(now.code, Some(3));
    assert!(p.output(&started.id, 100).unwrap().contains("boom"));
}

#[tokio::test]
async fn stop_ends_the_process_and_everything_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let started = p
        .start(dir.path(), &start("sleep 300 & sleep 300"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(group_alive(started.pid));
    let stopped = p.stop(&started.id).await.unwrap();
    assert_eq!(stopped.state, State::Stopped);
    assert!(
        !group_alive(started.pid),
        "the background sleep outlived the stop"
    );
    // Stopped stays stopped once the shell's exit is seen.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(p.get(&started.id).unwrap().state, State::Stopped);
}

#[tokio::test]
async fn the_same_command_in_the_same_folder_is_one_process() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("web")).unwrap();
    let p = service(dir.path());
    let a = p.start(dir.path(), &start("sleep 300")).await.unwrap();
    let b = p.start(dir.path(), &start("  sleep 300 ")).await.unwrap();
    assert_eq!(a.id, b.id);
    let in_web = Start {
        folder: "./web/".into(),
        ..start("sleep 300")
    };
    let c = p.start(dir.path(), &in_web).await.unwrap();
    assert_ne!(a.id, c.id);
    assert_eq!(c.folder, "web");
    p.stop_all().await;
    assert!(p.list().iter().all(|i| i.state == State::Stopped));
}

#[tokio::test]
async fn at_most_eight_run_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    for i in 0..8 {
        p.start(dir.path(), &start(&format!("sleep 30{i}")))
            .await
            .unwrap();
    }
    assert!(matches!(
        p.start(dir.path(), &start("sleep 309")).await,
        Err(Refused::Limit(8))
    ));
    p.stop_all().await;
    assert!(p.start(dir.path(), &start("sleep 309")).await.is_ok());
    p.stop_all().await;
}

#[tokio::test]
async fn a_missing_program_is_not_run_and_names_its_install() {
    let dir = tempfile::tempdir().unwrap();
    let p = Processes::with_shell(
        dir.path().join("store"),
        Shell::fixed("/bin/sh", env_with("/usr/bin:/bin")),
    )
    .unwrap();
    match p.start(dir.path(), &start("npm run dev")).await {
        Err(Refused::Missing {
            program, install, ..
        }) => {
            assert_eq!(program, "npm");
            assert_eq!(install.as_deref(), Some("node"));
        }
        other => panic!("{other:?}"),
    }
    match p
        .start(dir.path(), &start("PORT=1 no-such-program-ark"))
        .await
    {
        Err(e @ Refused::Missing { install: None, .. }) => {
            assert!(e.to_string().contains("does not know how to install"))
        }
        other => panic!("{other:?}"),
    }
    assert!(p.list().is_empty());
}

#[tokio::test]
async fn a_failed_install_is_reported_instead_of_asked_again() {
    let dir = tempfile::tempdir().unwrap();
    let p = Processes::with_shell(
        dir.path().join("store"),
        Shell::fixed("/bin/sh", env_with("/usr/bin:/bin")),
    )
    .unwrap();
    p.record_install_failure(
        "node",
        "it ended with exit 7. The end of its output:\nno disk".into(),
    );
    let e = p
        .start(dir.path(), &start("npm run dev"))
        .await
        .unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("exit 7") && text.contains("no disk"),
        "{text}"
    );
    assert!(
        !text.contains("Call acquire_capability with need"),
        "{text}"
    );
}

#[tokio::test]
async fn a_crash_leaves_groups_that_the_next_start_ends() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let mut left = std::process::Command::new("sleep")
        .arg("300")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut other = std::process::Command::new("sleep")
        .arg("300")
        .process_group(0)
        .spawn()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let records = serde_json::json!([
        { "pid": left.id(), "command": "sleep 300", "started": now, "recorded": now },
        // Recorded long ago: that id now belongs to another process, which is left alone.
        { "pid": other.id(), "command": "sleep 300", "started": now - 86_400, "recorded": now - 86_400 },
    ]);
    std::fs::write(store.join("running.json"), records.to_string()).unwrap();

    let _p =
        Processes::with_shell(&store, Shell::fixed("/bin/sh", env_with("/usr/bin:/bin"))).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if left.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(ended.is_ok(), "the recorded group still runs");
    assert!(
        other.try_wait().unwrap().is_none(),
        "an unknown group was ended"
    );
    assert!(!store.join("running.json").exists());
    other.kill().unwrap();
    let _ = other.wait();
}

#[tokio::test]
async fn running_processes_are_recorded_until_they_stop() {
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let a = p.start(dir.path(), &start("sleep 300")).await.unwrap();
    let file = dir.path().join("store").join("running.json");
    let rec: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(rec[0]["pid"], a.pid);
    p.stop(&a.id).await;
    assert!(!file.exists());
}

#[tokio::test]
async fn dropping_the_service_ends_what_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let p = service(dir.path());
    let a = p
        .start(dir.path(), &start("sleep 300 & sleep 300"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(group_alive(a.pid));
    drop(p);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!group_alive(a.pid), "a process outlived its runtime");
    assert!(!dir.path().join("store").join("running.json").exists());
}

/// An agent's browser may open only what a process of the project serves itself (ADR 0046):
/// not a port another program listens on, not another project's, not a stopped one.
#[tokio::test]
async fn only_a_port_the_process_itself_listens_on_is_served() {
    if !has_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (project, other) = (dir.path().join("p"), dir.path().join("q"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let p = service(dir.path());

    // Something else listens on a port; a process names it and serves nothing.
    let theirs = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let their_port = theirs.local_addr().unwrap().port();
    let borrow = Start {
        command: format!("echo http://localhost:{their_port}/; sleep 60"),
        port: Some(their_port),
        ..Start::default()
    };
    let lender = p.start(&project, &borrow).await.unwrap();
    let now = p.settle(&lender.id, Duration::from_secs(20)).await.unwrap();
    assert_eq!(
        now.state,
        State::Ready,
        "it answers, so the person may see it"
    );
    assert!(
        p.serving(&project).is_empty(),
        "but it is not the process's own"
    );

    // A server the process runs itself.
    let port = free_port();
    let req = Start {
        command: format!("python3 -u -m http.server {port} --bind 127.0.0.1"),
        port: Some(port),
        ..Start::default()
    };
    let server = p.start(&project, &req).await.unwrap();
    let now = p.settle(&server.id, Duration::from_secs(20)).await.unwrap();
    assert_eq!(now.state, State::Ready, "{:?}", p.output(&server.id, 2000));
    assert_eq!(p.serving(&project), vec![port]);
    assert!(p.serving(&other).is_empty(), "another project's process");

    p.stop(&server.id).await;
    p.stop(&lender.id).await;
    assert!(p.serving(&project).is_empty(), "a stopped process");
    drop(theirs);
}

#[tokio::test]
async fn an_installed_program_names_the_action_that_runs_it() {
    use ark_runtime::AcquisitionProvider;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pnpm = bin.join("pnpm");
    std::fs::write(&pnpm, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&pnpm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = Processes::with_shell(
        dir.path().join("store"),
        Shell::fixed(
            "/bin/sh",
            env_with(&format!("{}:/usr/bin:/bin", bin.display())),
        ),
    )
    .unwrap();
    let found = ark_proc::Installs::new(p).find("pnpm").await;
    assert!(found.contains("is installed"), "{found}");
    assert!(found.contains("process.start"), "{found}");
}

#[test]
fn starting_a_command_counts_as_looking_at_what_it_did() {
    use ark_core::{Reuse, RiskTier};
    use ark_tools::Adapter;
    let dir = tempfile::tempdir().unwrap();
    let tool = ark_proc::ProcessTool(service(dir.path()));
    let start = tool
        .actions()
        .into_iter()
        .find(|d| d.name == ark_proc::START)
        .unwrap();
    // Still a change that asks where approvals ask, and its answer is the command's output:
    // the harness counts it as a look, so tests run after an edit check that edit.
    assert_eq!(start.risk, RiskTier::Mutate);
    assert_eq!(start.reuse, Reuse::Volatile);
}
