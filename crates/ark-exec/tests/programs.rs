//! An agent program's bounds against the real macOS sandbox (ADR 0074), with `sh` as the
//! program: what it writes and reads in a home folder ARK hides, and the network it reaches.
#![cfg(target_os = "macos")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use ark_exec::{Bounds, Network, Reads, Sandbox, Write};

struct Place {
    _dirs: Vec<tempfile::TempDir>,
    home: PathBuf,
    project: PathBuf,
    scratch: PathBuf,
}

fn place() -> Place {
    let (home, project, scratch) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let p = Place {
        home: home.path().canonicalize().unwrap(),
        project: project.path().canonicalize().unwrap(),
        scratch: scratch.path().canonicalize().unwrap(),
        _dirs: vec![home, project, scratch],
    };
    for d in ["state/keep", "other"] {
        std::fs::create_dir_all(p.home.join(d)).unwrap();
    }
    std::fs::write(p.home.join("secret"), "s").unwrap();
    std::fs::write(p.home.join("state.json"), "{}").unwrap();
    for d in [".git", ".ssh"] {
        std::fs::create_dir_all(p.project.join(d)).unwrap();
    }
    std::fs::write(p.project.join(".ssh/id"), "k").unwrap();
    p
}

fn program_bounds(p: &Place, network: Network) -> Bounds {
    let mut b = Bounds::run(p.project.clone(), p.scratch.clone(), vec![]);
    b.reads = Reads::Except {
        hidden: vec![p.home.clone()],
        allowed: vec![],
        never: vec![p.project.join(".ssh")],
    };
    b.writes = vec![
        Write::Folder(p.home.join("state")),
        Write::File(p.home.join("state.json")),
    ];
    b.kept = vec!["keep".into()];
    b.network = network;
    b
}

/// Run `script` with `sh` inside `bounds`, or uncontained with `None`; its standard output.
fn sh(bounds: Option<&Bounds>, cwd: &Path, script: &str) -> String {
    let args = vec!["-c".to_string(), script.to_string()];
    let (exe, argv) = match bounds {
        Some(b) => Sandbox::native()
            .unwrap()
            .wrap(b, Path::new("/bin/sh"), &args)
            .unwrap(),
        None => (PathBuf::from("/bin/sh"), args),
    };
    let out = Command::new(exe)
        .args(argv)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `name: ok` or `name: no` for each `(name, command)`.
fn tries(steps: &[(&str, String)]) -> String {
    steps
        .iter()
        .map(|(name, cmd)| {
            format!("if {cmd} 2>/dev/null; then echo {name}: ok; else echo {name}: no; fi")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_program_writes_only_its_project_and_its_state_and_reads_nothing_hidden() {
    let p = place();
    let (h, pr) = (p.home.display(), p.project.display());
    let script = tries(&[
        ("state", format!("echo x > {h}/state/a")),
        ("state-file", format!("echo x > {h}/state.json")),
        ("state-lock", format!("echo x > {h}/state.json.lock")),
        ("state-file-other", format!("echo x > {h}/state.json2")),
        ("kept", format!("echo x > {h}/state/keep/a")),
        ("other", format!("echo x > {h}/other/a")),
        ("secret", format!("cat {h}/secret")),
        ("project", format!("echo x > {pr}/a")),
        ("git", format!("echo x > {pr}/.git/a")),
        ("never", format!("cat {pr}/.ssh/id")),
    ]);
    let got = sh(
        Some(&program_bounds(&p, Network::None)),
        &p.project,
        &script,
    );
    assert_eq!(
        got,
        "state: ok\nstate-file: ok\nstate-lock: ok\nstate-file-other: no\nkept: no\nother: no\n\
         secret: no\nproject: ok\ngit: no\nnever: no\n"
    );
}

#[test]
fn a_program_reaches_no_port_but_443() {
    let p = place();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = tries(&[("local", format!("/usr/bin/nc -z -G 2 127.0.0.1 {port}"))]);
    assert_eq!(
        sh(None, &p.project, &script),
        "local: ok\n",
        "uncontained it connects"
    );
    for network in [Network::None, Network::Https] {
        let got = sh(Some(&program_bounds(&p, network)), &p.project, &script);
        assert_eq!(got, "local: no\n", "{network:?}");
    }
}
