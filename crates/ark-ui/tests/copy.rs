//! ADR 0052 step 2: a project's copy on another computer. Two folders stand for This Mac and
//! the computer; the stream between them is what `pack` writes and `apply` reads.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use ark_sdk::prelude::*;
use ark_ui::copy::{self, Applied, Changes, Manifest};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn mode(dir: &Path, rel: &str) -> u32 {
    std::fs::metadata(dir.join(rel))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn stream(dir: &Path, files: &[String], deleted: &[String], with_git: bool) -> Vec<u8> {
    let mut out = Vec::new();
    copy::pack(dir, files, deleted, with_git, &mut out).unwrap();
    out
}

fn take(dir: &Path, tar: &[u8]) -> Applied {
    copy::apply(dir, tar, &mut |_| Ok(())).unwrap()
}

/// The first send from `mac` to `vps`: both manifests written, as the lead does after a send.
fn first_send(mac: &Path, vps: &Path) -> (Manifest, Manifest) {
    let plan = copy::plan_send(mac, &Manifest::default(), &[]).unwrap();
    let applied = copy::apply_first(vps, &stream(mac, &plan.files, &[], true)[..], &mut |_| {
        Ok(())
    })
    .unwrap();
    assert!(applied.refused.is_empty(), "{:?}", applied.refused);
    (
        copy::scan(mac, &plan.files).unwrap(),
        copy::scan(vps, &plan.files).unwrap(),
    )
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A stream written by hand, as a computer that does not follow the rules could: a head, then
/// entries with any name, kind, link and mode.
struct Raw(tar::Builder<Vec<u8>>);

impl Raw {
    fn new(deleted: &[&str]) -> Raw {
        let mut b = tar::Builder::new(Vec::new());
        let head = serde_json::json!({ "deleted": deleted, "version": 1 }).to_string();
        let mut h = tar::Header::new_gnu();
        h.set_size(head.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, ".ark-copy.json", head.as_bytes())
            .unwrap();
        Raw(b)
    }

    fn entry(
        mut self,
        name: &str,
        kind: tar::EntryType,
        data: &[u8],
        link: &str,
        mode: u32,
    ) -> Raw {
        let mut h = tar::Header::new_gnu();
        let g = h.as_gnu_mut().unwrap();
        g.name[..name.len()].copy_from_slice(name.as_bytes());
        g.linkname[..link.len()].copy_from_slice(link.as_bytes());
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        self.0.append(&h, data).unwrap();
        self
    }

    fn file(self, name: &str, data: &str) -> Raw {
        self.entry(name, tar::EntryType::Regular, data.as_bytes(), "", 0o644)
    }

    fn link(self, name: &str, target: &str) -> Raw {
        self.entry(name, tar::EntryType::Symlink, b"", target, 0o777)
    }

    fn done(self) -> Vec<u8> {
        self.0.into_inner().unwrap()
    }
}

fn refused(a: &Applied) -> Vec<&str> {
    a.refused.iter().map(|(p, _)| p.as_str()).collect()
}

#[test]
fn the_first_send_keeps_modes_and_skips_ignored_files() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mac, vps) = (a.path(), b.path().join("copy"));
    git(mac, &["init", "-q"]);
    write(mac, ".gitignore", "secret.env\n/target\n");
    write(mac, "run.sh", "#!/bin/sh\necho hi\n");
    std::fs::set_permissions(mac.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    write(mac, "src/lib.rs", "fn main() {}\n");
    git(mac, &["add", "."]);
    git(mac, &["commit", "-qm", "first"]);
    write(mac, "notes.md", "not yet tracked\n");
    write(mac, "secret.env", "KEY=1\n");
    write(mac, "target/debug/app", "binary");

    let sel = copy::select(mac).unwrap();
    assert!(sel.git);
    assert_eq!(
        sel.files,
        paths(&[".gitignore", "notes.md", "run.sh", "src/lib.rs"])
    );
    assert!(!sel.over_limit());

    first_send(mac, &vps);
    assert_eq!(mode(&vps, "run.sh"), 0o755);
    assert_eq!(mode(&vps, "src/lib.rs") & 0o111, 0);
    assert_eq!(read(&vps, "notes.md"), "not yet tracked\n");
    assert!(!vps.join("secret.env").exists());
    assert!(!vps.join("target").exists());
    // The history and the branch came with it, and git sees the same working tree.
    assert!(git(&vps, &["log", "--oneline"]).contains("first"));
    assert_eq!(
        git(&vps, &["status", "--porcelain"]),
        git(mac, &["status", "--porcelain"])
    );
}

#[test]
fn a_size_over_the_limit_is_reported() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "small.txt", "x");
    assert!(!copy::select(d.path()).unwrap().over_limit());
    // Sparse: no disk is used.
    let big = std::fs::File::create(d.path().join("big.bin")).unwrap();
    big.set_len(copy::LIMIT_BYTES + 1).unwrap();
    let sel = copy::select(d.path()).unwrap();
    assert_eq!(sel.count, 2);
    assert!(sel.bytes > copy::LIMIT_BYTES);
    assert!(sel.over_limit());
}

#[test]
fn a_later_send_carries_only_changes_and_deletions() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mac, vps) = (a.path(), b.path());
    write(mac, "a.txt", "a");
    write(mac, "b.txt", "b");
    write(mac, "docs/c.txt", "c");
    write(mac, "docs/old/d.txt", "d");
    write(mac, "same.txt", "same");
    let (mut mac_m, _) = first_send(mac, vps);

    write(mac, "a.txt", "a2");
    std::fs::remove_file(mac.join("b.txt")).unwrap();
    std::fs::remove_file(mac.join("docs/old/d.txt")).unwrap();
    write(mac, "new.txt", "n");
    std::fs::set_permissions(
        mac.join("docs/c.txt"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let plan = copy::plan_send(mac, &mac_m, &[]).unwrap();
    assert_eq!(plan.files, paths(&["a.txt", "docs/c.txt", "new.txt"]));
    assert_eq!(plan.deleted, paths(&["b.txt", "docs/old/d.txt"]));
    assert!(plan.kept_back.is_empty());

    let tar = stream(mac, &plan.files, &plan.deleted, false);
    let applied = take(vps, &tar);
    assert_eq!(applied.written, plan.files);
    assert_eq!(applied.deleted, plan.deleted);
    assert_eq!(read(vps, "a.txt"), "a2");
    assert_eq!(mode(vps, "docs/c.txt"), 0o700);
    assert!(!vps.join("b.txt").exists());
    assert!(!vps.join("docs/old").exists(), "a folder left empty goes");

    // After the send the manifest takes what was sent, and the next send has nothing.
    let sent: Vec<String> = plan.files.iter().chain(&plan.deleted).cloned().collect();
    mac_m.update(mac, &sent).unwrap();
    let next = copy::plan_send(mac, &mac_m, &[]).unwrap();
    assert!(next.files.is_empty() && next.deleted.is_empty());
}

#[test]
fn a_file_the_computer_changed_is_never_replaced() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mac, vps) = (a.path(), b.path());
    write(mac, "shared.txt", "v1");
    write(mac, "mine.txt", "v1");
    write(mac, "gone.txt", "v1");
    let (mac_m, vps_m) = first_send(mac, vps);

    write(vps, "shared.txt", "the computer's");
    write(vps, "gone.txt", "the computer's");
    write(mac, "shared.txt", "this mac's");
    write(mac, "mine.txt", "this mac's");
    std::fs::remove_file(mac.join("gone.txt")).unwrap();

    let theirs = copy::changes(vps, &vps_m).unwrap();
    assert_eq!(theirs.changed, paths(&["gone.txt", "shared.txt"]));
    let plan = copy::plan_send(mac, &mac_m, &theirs.all()).unwrap();
    assert_eq!(plan.files, paths(&["mine.txt"]));
    assert!(plan.deleted.is_empty());
    assert_eq!(plan.kept_back, paths(&["gone.txt", "shared.txt"]));

    take(vps, &stream(mac, &plan.files, &plan.deleted, false));
    assert_eq!(read(vps, "mine.txt"), "this mac's");
    assert_eq!(read(vps, "shared.txt"), "the computer's");
    assert_eq!(read(vps, "gone.txt"), "the computer's");
}

#[test]
fn bring_back_finds_a_file_a_process_wrote_and_one_in_a_hidden_folder() {
    for with_git in [false, true] {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mac, vps) = (a.path(), b.path().join("copy"));
        write(mac, "README", "r");
        if with_git {
            git(mac, &["init", "-q"]);
            git(mac, &["add", "."]);
            git(mac, &["commit", "-qm", "first"]);
        }
        let (_, vps_m) = first_send(mac, &vps);

        // A process, not a tool, writes a file; another lands in a hidden folder.
        let ok = Command::new("sh")
            .arg("-c")
            .arg(
                "echo made > made.txt && mkdir -p .config && echo x > .config/x && chmod +x README",
            )
            .current_dir(&vps)
            .status()
            .unwrap();
        assert!(ok.success());
        let c = copy::changes(&vps, &vps_m).unwrap();
        assert_eq!(
            c.added,
            paths(&[".config/x", "made.txt"]),
            "git: {with_git}"
        );
        assert_eq!(c.modes, paths(&["README"]));
        assert!(c.changed.is_empty() && c.deleted.is_empty());

        // Brought back to This Mac, through the same stream.
        let back: Vec<String> = c.added.iter().chain(&c.modes).cloned().collect();
        let applied = take(mac, &stream(&vps, &back, &[], false));
        assert!(applied.refused.is_empty(), "{:?}", applied.refused);
        assert_eq!(read(mac, ".config/x"), "x\n");
        assert_eq!(mode(mac, "README") & 0o100, 0o100);
    }
}

#[test]
fn a_build_there_brings_back_nothing_from_target() {
    // A folder without git: the usual build folders at any depth.
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "Cargo.toml", "[package]");
    let m = copy::scan(d.path(), &copy::select(d.path()).unwrap().files).unwrap();
    for out in [
        "target/debug/app",
        "web/node_modules/x/index.js",
        "py/__pycache__/a.pyc",
        ".venv/bin/python",
        "site/dist/app.js",
        "build/out.o",
    ] {
        write(d.path(), out, "built");
    }
    assert!(copy::changes(d.path(), &m).unwrap().is_empty());

    // A git folder: what git ignores.
    let g = tempfile::tempdir().unwrap();
    git(g.path(), &["init", "-q"]);
    write(g.path(), ".gitignore", "/target\n");
    write(g.path(), "src/main.rs", "fn main() {}");
    let m = copy::scan(g.path(), &copy::select(g.path()).unwrap().files).unwrap();
    write(g.path(), "target/debug/app", "built");
    write(g.path(), "target/debug/.fingerprint/x", "built");
    assert!(copy::changes(g.path(), &m).unwrap().is_empty());
}

#[test]
fn a_link_out_of_the_project_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    write(&ssh, "id_ed25519", "private");
    let vps = home.path().join("work/project");
    let mac = tempfile::tempdir().unwrap();
    write(&vps, "src/real.txt", "real");
    std::os::unix::fs::symlink(&ssh, vps.join("keys")).unwrap();
    std::os::unix::fs::symlink("../../.ssh/id_ed25519", vps.join("key")).unwrap();
    std::os::unix::fs::symlink("src/real.txt", vps.join("inside")).unwrap();

    // The computer lists its links as links, never what they point to.
    let sel = copy::select(&vps).unwrap();
    assert_eq!(sel.files, paths(&["inside", "key", "keys", "src/real.txt"]));
    let applied = take(mac.path(), &stream(&vps, &sel.files, &[], false));
    assert_eq!(refused(&applied), ["key", "keys"]);
    assert!(std::fs::symlink_metadata(mac.path().join("keys")).is_err());
    assert!(std::fs::symlink_metadata(mac.path().join("key")).is_err());
    // A link that stays inside is taken as a link.
    assert_eq!(
        std::fs::read_link(mac.path().join("inside")).unwrap(),
        Path::new("src/real.txt")
    );
    assert_eq!(read(mac.path(), "inside"), "real");

    // Written by hand: an absolute target, one that climbs out, one through a link here that
    // points out.
    let tar = Raw::new(&[])
        .link("abs", &ssh.to_string_lossy())
        .link("up", "../.ssh")
        .link("deep/a", "../../x")
        .link("through", "keys-here/id_ed25519")
        .done();
    std::os::unix::fs::symlink(&ssh, mac.path().join("keys-here")).unwrap();
    let applied = take(mac.path(), &tar);
    assert_eq!(refused(&applied), ["abs", "up", "deep/a", "through"]);
    assert!(applied.written.is_empty());
}

#[test]
fn paths_out_of_the_folder_and_odd_ones_are_refused() {
    let base = tempfile::tempdir().unwrap();
    let mac = base.path().join("project");
    std::fs::create_dir(&mac).unwrap();
    let tar = Raw::new(&["../victim.txt", "/etc/hosts-copy"])
        .file("../x", "x")
        .file("/etc/x", "x")
        .file("a/../../x", "x")
        .file("a//b", "x")
        .file("./a", "x")
        .file(" a", "x")
        .file(".git/config", "x")
        .file("sub/.git/hooks/pre-commit", "x")
        .file("ok.txt", "ok")
        .done();
    write(base.path(), "victim.txt", "keep");
    let applied = take(&mac, &tar);
    assert_eq!(applied.written, paths(&["ok.txt"]));
    assert_eq!(
        refused(&applied),
        [
            "../x",
            "/etc/x",
            "a/../../x",
            "a//b",
            "./a",
            " a",
            ".git/config",
            "sub/.git/hooks/pre-commit",
            "../victim.txt",
            "/etc/hosts-copy",
        ]
    );
    assert!(!base.path().join("x").exists());
    assert_eq!(read(base.path(), "victim.txt"), "keep");
    assert!(!mac.join(".git").exists() && !mac.join("sub").exists());

    // A nested `.git` is refused even with the first copy.
    let first = Raw::new(&[]).file("sub/.git/hooks/x", "x").done();
    let a = copy::apply_first(&mac, &first[..], &mut |_| Ok(())).unwrap();
    assert_eq!(refused(&a), ["sub/.git/hooks/x"]);

    // A stream that does not start with the head is not a copy at all.
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(1);
    b.append_data(&mut h, "x", &b"x"[..]).unwrap();
    assert!(copy::apply(&mac, &b.into_inner().unwrap()[..], &mut |_| Ok(())).is_err());
}

#[test]
fn nothing_is_written_through_an_existing_link() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "target.txt", "outside");
    let d = tempfile::tempdir().unwrap();
    let mac = d.path();
    std::os::unix::fs::symlink(outside.path().join("target.txt"), mac.join("out.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path(), mac.join("sub")).unwrap();
    write(mac, "real/inner.txt", "inner");
    std::os::unix::fs::symlink("real", mac.join("inner-link")).unwrap();

    let tar = Raw::new(&["sub/target.txt"])
        .file("out.txt", "written")
        .file("sub/new.txt", "written")
        .file("inner-link/inner.txt", "written")
        .done();
    let applied = take(mac, &tar);
    assert!(applied.written.is_empty());
    assert_eq!(
        refused(&applied),
        [
            "out.txt",
            "sub/new.txt",
            "inner-link/inner.txt",
            "sub/target.txt"
        ]
    );
    assert_eq!(read(outside.path(), "target.txt"), "outside");
    assert!(!outside.path().join("new.txt").exists());
    assert_eq!(read(mac, "real/inner.txt"), "inner");
    assert!(std::fs::symlink_metadata(mac.join("out.txt"))
        .unwrap()
        .is_symlink());
}

#[test]
fn setuid_is_dropped_and_odd_entries_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let tar = Raw::new(&[])
        .entry("tool", tar::EntryType::Regular, b"#!/bin/sh\n", "", 0o6755)
        .entry("sticky", tar::EntryType::Regular, b"x", "", 0o1644)
        .entry("hard", tar::EntryType::Link, b"", "tool", 0o644)
        .entry("dev", tar::EntryType::Char, b"", "", 0o644)
        .entry("pipe", tar::EntryType::Fifo, b"", "", 0o644)
        .done();
    let applied = take(d.path(), &tar);
    assert_eq!(applied.written, paths(&["tool", "sticky"]));
    assert_eq!(refused(&applied), ["hard", "dev", "pipe"]);
    assert_eq!(mode(d.path(), "tool"), 0o755);
    assert_eq!(mode(d.path(), "sticky"), 0o644);
    assert!(!d.path().join("hard").exists() && !d.path().join("pipe").exists());
}

#[test]
fn before_runs_before_a_file_is_touched_and_its_error_refuses_it() {
    let d = tempfile::tempdir().unwrap();
    let mac = d.path();
    write(mac, "a.txt", "old a");
    write(mac, "b.txt", "old b");
    write(mac, "same.txt", "same");
    let tar = Raw::new(&["b.txt", "never-was.txt"])
        .file("a.txt", "new a")
        .file("same.txt", "same")
        .file("fresh.txt", "fresh")
        .done();
    let mut seen = Vec::new();
    let applied = copy::apply(mac, &tar[..], &mut |rel| {
        // The file is still as it was when its version is kept.
        seen.push((rel.to_string(), read(mac, rel)));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        seen,
        [
            ("a.txt".to_string(), "old a".to_string()),
            ("b.txt".to_string(), "old b".to_string())
        ]
    );
    assert_eq!(applied.written, paths(&["a.txt", "fresh.txt"]));
    assert_eq!(applied.deleted, paths(&["b.txt"]));

    // When the version cannot be kept, the file is left alone.
    write(mac, "b.txt", "old b");
    let tar = Raw::new(&["b.txt"]).file("a.txt", "newer a").done();
    let applied = copy::apply(mac, &tar[..], &mut |_| anyhow::bail!("the store is full")).unwrap();
    assert_eq!(refused(&applied), ["a.txt", "b.txt"]);
    assert!(applied.refused[0].1.contains("the store is full"));
    assert_eq!(read(mac, "a.txt"), "new a");
    assert_eq!(read(mac, "b.txt"), "old b");
    let left: Vec<_> = std::fs::read_dir(mac)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".ark-copy"))
        .collect();
    assert!(left.is_empty(), "no temporary file is left");
}

#[test]
fn changed_on_both_sides_keep_or_take_and_the_next_send_goes_on() {
    let (a, b, s) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (mac, vps) = (a.path(), b.path());
    write(mac, "take.txt", "v1");
    write(mac, "keep.txt", "v1");
    write(mac, "both-gone.txt", "v1");
    let (mut mac_m, mut vps_m) = first_send(mac, vps);
    for (dir, who) in [(mac, "mac"), (vps, "vps")] {
        write(dir, "take.txt", who);
        write(dir, "keep.txt", who);
        std::fs::remove_file(dir.join("both-gone.txt")).unwrap();
    }
    let theirs = copy::changes(vps, &vps_m).unwrap();
    assert_eq!(
        copy::both_changed(mac, &theirs, &mac_m).unwrap(),
        paths(&["keep.txt", "take.txt"])
    );

    // This Mac keeps its versions before in its store (ADR 0034).
    let engine = Engine::open(s.path()).unwrap();
    let mut project = Project::new("p", "");
    project.workspace = Some(mac.to_string_lossy().into_owned());
    engine.store.put_project(&project).unwrap();
    let pid = project.id.clone();

    // Take vps's: brought back, This Mac's version kept before it is replaced.
    let tar = stream(vps, &paths(&["take.txt"]), &[], false);
    let applied = copy::apply(mac, &tar[..], &mut |rel| {
        engine
            .keep_before(&pid, rel)
            .map(|_| ())
            .map_err(Into::into)
    })
    .unwrap();
    assert_eq!(applied.written, paths(&["take.txt"]));
    assert_eq!(read(mac, "take.txt"), "vps");
    let kept: Vec<Artifact> = engine
        .store
        .list_artifacts(&pid)
        .unwrap()
        .into_iter()
        .filter(|a| a.meta["before"] == true && a.meta["path"] == "take.txt")
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(engine.store.read_blob(&kept[0].id).unwrap(), b"mac");
    // One step undoes it.
    engine.restore_file(&pid, &kept[0].id).unwrap();
    assert_eq!(read(mac, "take.txt"), "mac");
    let tar = stream(vps, &paths(&["take.txt"]), &[], false);
    copy::apply(mac, &tar[..], &mut |_| Ok(())).unwrap();

    // Keep This Mac's: sent, the computer's version kept before it is replaced there.
    let mut there = Vec::new();
    let tar = stream(mac, &paths(&["keep.txt"]), &[], false);
    copy::apply(vps, &tar[..], &mut |rel| {
        there.push((rel.to_string(), read(vps, rel)));
        Ok(())
    })
    .unwrap();
    assert_eq!(there, [("keep.txt".to_string(), "vps".to_string())]);
    assert_eq!(read(vps, "keep.txt"), "mac");

    // Each choice goes into both manifests; a file gone on both sides leaves them.
    for rel in ["take.txt", "keep.txt", "both-gone.txt"] {
        let chosen = copy::entry(mac, rel).unwrap();
        mac_m.record_choice(rel, chosen.clone());
        vps_m.record_choice(rel, chosen);
    }
    let theirs = copy::changes(vps, &vps_m).unwrap();
    assert_eq!(theirs, Changes::default());
    assert!(copy::both_changed(mac, &theirs, &mac_m).unwrap().is_empty());
    write(mac, "later.txt", "later");
    let next = copy::plan_send(mac, &mac_m, &theirs.all()).unwrap();
    assert_eq!(next.files, paths(&["later.txt"]));
    assert!(next.kept_back.is_empty() && next.deleted.is_empty());

    // The manifest is a record of the store, read back as written.
    mac_m.save(&engine.store, "p:vps").unwrap();
    assert_eq!(Manifest::load(&engine.store, "p:vps").unwrap(), mac_m);
    assert_eq!(
        Manifest::load(&engine.store, "p:other").unwrap(),
        Manifest::default()
    );
}

#[test]
fn keep_before_keeps_a_file_and_refuses_one_outside() {
    let (s, w, outside) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let engine = Engine::open(s.path()).unwrap();
    let mut project = Project::new("p", "");
    project.workspace = Some(w.path().to_string_lossy().into_owned());
    engine.store.put_project(&project).unwrap();
    write(w.path(), "notes/a.md", "a");
    write(outside.path(), "secret", "s");
    std::os::unix::fs::symlink(outside.path().join("secret"), w.path().join("link")).unwrap();

    let id = engine
        .keep_before(&project.id, "notes/a.md")
        .unwrap()
        .unwrap();
    let art = engine
        .store
        .get_artifact(&project.id, &id)
        .unwrap()
        .unwrap();
    assert_eq!(
        art.meta,
        serde_json::json!({ "path": "notes/a.md", "before": true })
    );
    assert_eq!(art.name, "a.md");
    assert_eq!(engine.keep_before(&project.id, "none.md").unwrap(), None);
    assert!(engine.keep_before(&project.id, "link").is_err());
    assert!(engine.keep_before(&project.id, "../x").is_err());
}

#[test]
fn git_goes_only_with_the_first_send() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mac, vps) = (a.path(), b.path());
    git(mac, &["init", "-q"]);
    write(mac, "a.txt", "a");
    git(mac, &["add", "."]);
    git(mac, &["commit", "-qm", "first"]);
    let files = copy::select(mac).unwrap().files;
    assert!(!files.iter().any(|f| f.starts_with(".git/")));

    let names = |tar: &[u8]| -> Vec<String> {
        tar::Archive::new(tar)
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    let later = stream(mac, &files, &[], false);
    assert!(!names(&later).iter().any(|n| n.starts_with(".git")));
    let first = stream(mac, &files, &[], true);
    assert!(names(&first).iter().any(|n| n == ".git/HEAD"));
    assert_eq!(names(&first)[0], ".ark-copy.json");

    // A later stream that carries `.git` anyway: every `.git` entry is refused.
    let applied = take(vps, &first);
    assert_eq!(applied.written, paths(&["a.txt"]));
    assert!(applied.refused.iter().all(|(p, _)| p.starts_with(".git")));
    assert!(!vps.join(".git").exists());
    // A manifest never holds it.
    let m = copy::scan(mac, &paths(&["a.txt", ".git/HEAD"])).unwrap();
    assert_eq!(
        m.entries.keys().cloned().collect::<Vec<_>>(),
        paths(&["a.txt"])
    );
}

/// A file asked for through a link in its folders (a link to `/etc` made there) is not packed.
#[cfg(unix)]
#[test]
fn a_file_through_a_linked_folder_is_not_packed() {
    let src = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "s").unwrap();
    std::os::unix::fs::symlink(outside.path(), src.path().join("out")).unwrap();
    std::fs::write(src.path().join("a.txt"), "a").unwrap();
    let mut tar = Vec::new();
    ark_ui::copy::pack(
        src.path(),
        &["a.txt".into(), "out/secret".into()],
        &[],
        false,
        &mut tar,
    )
    .unwrap();
    let dst = tempfile::tempdir().unwrap();
    let applied = ark_ui::copy::apply(dst.path(), tar.as_slice(), &mut |_| Ok(())).unwrap();
    assert_eq!(applied.written, ["a.txt"]);
    assert!(!dst.path().join("out").exists());
}
