//! A project's command finds the person's programs (ADR 0050). ARK Desktop starts from Finder
//! with `PATH=/usr/bin:/bin`: no cargo, npm or Homebrew. `process.start` reads the person's
//! login shell, so `cargo` is found there, and its exit code and output come back.
//!
//! The test binary runs itself as the child with that minimal environment, as a GUI app has.
//! It is skipped where the person's login shell itself has no cargo.

#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use ark_proc::{Processes, Shell, Start, State};

const CHILD: &str = "ARK_LOGIN_PATH_CHILD";

async fn child() {
    let dir = std::path::PathBuf::from(std::env::var("ARK_LOGIN_PATH_DIR").unwrap());
    let p = Processes::with_shell(dir.join("store"), Shell::person()).unwrap();
    let info = p
        .start(
            &dir,
            &Start {
                command: "cargo --version && exit 3".into(),
                ..Start::default()
            },
        )
        .await
        .expect("cargo is found through the login shell");
    let end = p
        .wait_end(&info.id, Duration::from_secs(60))
        .await
        .expect("it ends");
    assert_eq!(end.state, State::Exited, "{end:?}");
    assert_eq!(end.code, Some(3), "its exit code comes back");
    let out = p.output(&info.id, 2_000).unwrap_or_default();
    assert!(out.contains("cargo "), "its output comes back: {out}");
    std::fs::write(dir.join("passed"), b"").unwrap();
}

#[tokio::test]
async fn a_command_finds_cargo_through_the_login_shell_from_a_bare_path() {
    if std::env::var(CHILD).is_ok() {
        return child().await;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let login_has_cargo = Command::new(&shell)
        .args(["-i", "-l", "-c", "command -v cargo"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", std::env::var("HOME").unwrap())
        .output()
        .is_ok_and(|o| o.status.success());
    if !login_has_cargo {
        eprintln!("skipped: the login shell here has no cargo");
        return;
    }
    let bare = Command::new("/bin/sh")
        .args(["-c", "command -v cargo"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(!bare.status.success(), "the bare PATH has no cargo");

    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_command_finds_cargo_through_the_login_shell_from_a_bare_path",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, "1")
        .env("ARK_LOGIN_PATH_DIR", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", std::env::var("HOME").unwrap())
        .env("SHELL", &shell)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        dir.path().join("passed").exists(),
        "the child ran its checks"
    );
}
