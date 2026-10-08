//! A project's copy on another computer (ADR 0052): what is sent, what changed since, and
//! taking a copy's files in without trusting them.
//!
//! The copy is a second folder of the same project, on the computer that runs a chat. Both
//! sides run this code. The transport (a `tar` stream over SSH) is the caller's.
//!
//! | Step | Here |
//! |---|---|
//! | What goes | [`select`]: a git folder's `git ls-files -co --exclude-standard`, other folders all but the usual build and package folders |
//! | What the copy had | [`Manifest`]: path, size, mode, SHA-256 per file, kept per copy on both sides. `.git` is never in it |
//! | What changed since | [`changes`]: the folder now against the manifest, whatever made the change |
//! | A send | [`plan_send`], then [`pack`]: never a file the other side changed since the manifest |
//! | Taking one in | [`apply`] (and [`apply_first`], which also takes `.git`): trusts nothing in the stream |
//! | Changed on both sides | [`both_changed`]; the choice goes into the manifest ([`Manifest::record_choice`]) |
//!
//! A stream starts with `.ark-copy.json` (`{"deleted": [...], "version": 1}`), then each file
//! with its mode, links as links, and on the first send the `.git` folder.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ark_sdk::prelude::{ark_files, ark_os};
use ark_store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Above this many bytes, the person is asked before a send or a bring back.
pub const LIMIT_BYTES: u64 = 500 * 1024 * 1024;
/// Above this many files, the same.
pub const LIMIT_FILES: usize = 20_000;
/// The record kind a manifest is stored under.
pub const MANIFEST_KIND: &str = "copy_manifest";

/// Build and package folders a folder without git never sends, at any depth.
const SKIP: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];
/// The first entry of every stream.
const HEAD: &str = ".ark-copy.json";
const VERSION: u32 = 1;
/// The most the first entry may hold.
const MAX_HEAD: u64 = 64 * 1024 * 1024;

// ---- what goes --------------------------------------------------------------------------------

/// The files of a folder that a copy carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Relative, `/` separated, sorted. Links are listed; folders are not.
    pub files: Vec<String>,
    /// The size of the files, links not counted.
    pub bytes: u64,
    pub count: usize,
    /// The folder has a `.git` folder, and git listed the files.
    pub git: bool,
}

impl Selection {
    /// Too big to send without asking.
    pub fn over_limit(&self) -> bool {
        self.bytes > LIMIT_BYTES || self.count > LIMIT_FILES
    }
}

/// The files of `dir` a copy carries. A git folder (a `.git` here): what git tracks plus what it
/// does not ignore; submodules are left out. Other folders: every file except in the usual
/// build and package folders (`target`, `node_modules`, ...). Links to folders are never
/// followed. `.git` is never listed.
pub fn select(dir: &Path) -> anyhow::Result<Selection> {
    let listed = if dir.join(".git").exists() {
        git_files(dir)
    } else {
        None
    };
    let git = listed.is_some();
    let listed = match listed {
        Some(files) => files,
        None => {
            let mut out = Vec::new();
            walk(dir, "", &mut out)?;
            out
        }
    };
    let mut files = Vec::new();
    let mut bytes = 0;
    for rel in listed.into_iter().collect::<BTreeSet<_>>() {
        if is_git(&rel) || plain(&rel).is_err() {
            continue;
        }
        // Git also lists what was deleted from the folder but not from its index, and a
        // submodule as one path that is a folder here.
        let Ok(meta) = std::fs::symlink_metadata(dir.join(&rel)) else {
            continue;
        };
        if meta.is_file() {
            bytes += meta.len();
        } else if !meta.is_symlink() {
            continue;
        }
        files.push(rel);
    }
    Ok(Selection {
        count: files.len(),
        files,
        bytes,
        git,
    })
}

/// What git tracks and does not ignore in `dir`, or `None` when git cannot say.
fn git_files(dir: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        // The folder decides, not the caller's environment.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter_map(|p| String::from_utf8(p.to_vec()).ok())
            // An untracked folder that is a repository of its own shows as `name/`.
            .filter(|p| !p.is_empty() && !p.ends_with('/'))
            .collect(),
    )
}

/// Every file and link under `dir/prefix`, without following links.
fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir.join(prefix)) {
        Ok(e) => e,
        // A folder this user cannot read inside the project is left out, not the whole send.
        Err(_) if !prefix.is_empty() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for e in entries.flatten() {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Ok(kind) = e.file_type() else {
            continue;
        };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if name == ".git" {
            continue;
        }
        if kind.is_dir() {
            if !SKIP.contains(&name.as_str()) {
                walk(dir, &rel, out)?;
            }
        } else if kind.is_file() || kind.is_symlink() {
            out.push(rel);
        }
    }
    Ok(())
}

// ---- what the copy had ------------------------------------------------------------------------

/// What a copy had at the last send, bring back or choice: one entry per file. `.git` is never
/// in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
}

/// One file as a manifest knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Bytes; 0 for a link.
    #[serde(default)]
    pub size: u64,
    /// Permission bits (`0o755`); setuid, setgid and sticky are dropped. 0 for a link.
    #[serde(default)]
    pub mode: u32,
    /// Of the contents, or of a link's target text. Hex.
    #[serde(default)]
    pub sha256: String,
    /// A link's target, as written in the link.
    #[serde(default)]
    pub link: Option<String>,
}

impl Manifest {
    /// The manifest stored under `key` (one per copy, for example `<project>:<computer>`), or
    /// an empty one: nothing sent yet.
    pub fn load(store: &Store, key: &str) -> anyhow::Result<Manifest> {
        match store.get_record(MANIFEST_KIND, key)? {
            Some(v) => Ok(serde_json::from_value(v)?),
            None => Ok(Manifest::default()),
        }
    }

    /// Store it under `key`, replacing what was there.
    pub fn save(&self, store: &Store, key: &str) -> anyhow::Result<()> {
        store.put_record(MANIFEST_KIND, key, key, &serde_json::to_value(self)?)?;
        Ok(())
    }

    /// Take `paths` as they are in `dir` now: after a send (the plan's files and deletions) or
    /// a bring back (what was written and deleted). A path with no file here leaves the
    /// manifest.
    pub fn update(&mut self, dir: &Path, paths: &[String]) -> anyhow::Result<()> {
        for rel in paths {
            if is_git(rel) {
                continue;
            }
            match entry(dir, rel)? {
                Some(e) => self.entries.insert(rel.clone(), e),
                None => self.entries.remove(rel),
            };
        }
        Ok(())
    }

    /// A file changed on both sides was settled: `chosen` is the version both sides now have
    /// (`None`: no file). The file is not asked about again.
    pub fn record_choice(&mut self, path: &str, chosen: Option<Entry>) {
        match chosen {
            Some(e) => self.entries.insert(path.to_string(), e),
            None => self.entries.remove(path),
        };
    }
}

/// A manifest of `files` as they are in `dir`. Files not there are left out.
pub fn scan(dir: &Path, files: &[String]) -> anyhow::Result<Manifest> {
    let mut m = Manifest::default();
    m.update(dir, files)?;
    Ok(m)
}

/// File `rel` of `dir` as a manifest records it. `None` when there is no file or link there, or
/// when a folder on the way is a link (a file reached through a link is not the folder's).
pub fn entry(dir: &Path, rel: &str) -> anyhow::Result<Option<Entry>> {
    plain(rel).map_err(|why| anyhow::anyhow!("`{rel}`: {why}"))?;
    if parents(dir, rel).is_err() {
        return Ok(None);
    }
    let path = dir.join(rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.is_symlink() {
        let target = std::fs::read_link(&path)?.to_string_lossy().into_owned();
        return Ok(Some(Entry {
            size: 0,
            mode: 0,
            sha256: hex(&Sha256::digest(target.as_bytes())),
            link: Some(target),
        }));
    }
    if !meta.is_file() {
        return Ok(None);
    }
    Ok(Some(Entry {
        size: meta.len(),
        mode: mode_of(&meta),
        sha256: hash_file(&path)?,
        link: None,
    }))
}

// ---- what changed since -----------------------------------------------------------------------

/// A folder against its manifest. Each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    /// Contents or link target changed.
    pub changed: Vec<String>,
    /// New, and a send would carry them ([`select`]).
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    /// Only the mode changed.
    pub modes: Vec<String>,
    /// The size of the changed and new files.
    pub bytes: u64,
}

impl Changes {
    /// Every path in the four lists, sorted.
    pub fn all(&self) -> Vec<String> {
        let all: BTreeSet<&String> = self
            .changed
            .iter()
            .chain(&self.added)
            .chain(&self.deleted)
            .chain(&self.modes)
            .collect();
        all.into_iter().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.added.is_empty()
            && self.deleted.is_empty()
            && self.modes.is_empty()
    }

    /// Too big to bring back without asking.
    pub fn over_limit(&self) -> bool {
        self.bytes > LIMIT_BYTES || self.changed.len() + self.added.len() > LIMIT_FILES
    }
}

/// What changed in `dir` since `base`, whatever made the change (a chat, a program, a process,
/// the Terminal). A new file counts only when [`select`] would send it, so build output and
/// ignored files stay where they are.
pub fn changes(dir: &Path, base: &Manifest) -> anyhow::Result<Changes> {
    let mut c = Changes::default();
    for (rel, was) in &base.entries {
        if is_git(rel) {
            continue;
        }
        match entry(dir, rel)? {
            None => c.deleted.push(rel.clone()),
            Some(now) if now.sha256 != was.sha256 || now.link != was.link => {
                c.bytes += now.size;
                c.changed.push(rel.clone());
            }
            Some(now) if now.mode != was.mode => c.modes.push(rel.clone()),
            Some(_) => {}
        }
    }
    for rel in select(dir)?.files {
        if !base.entries.contains_key(&rel) {
            if let Ok(m) = std::fs::symlink_metadata(dir.join(&rel)) {
                if m.is_file() {
                    c.bytes += m.len();
                }
            }
            c.added.push(rel);
        }
    }
    Ok(c)
}

/// Files changed on both sides since `base`: this side's `local_dir` and the other side's
/// `remote` changes. A file deleted on both is not one. Each needs a choice: keep this side's
/// or take the other's.
pub fn both_changed(
    local_dir: &Path,
    remote: &Changes,
    base: &Manifest,
) -> anyhow::Result<Vec<String>> {
    let mine = changes(local_dir, base)?;
    let theirs: BTreeSet<String> = remote.all().into_iter().collect();
    Ok(mine
        .all()
        .into_iter()
        .filter(|p| theirs.contains(p))
        .filter(|p| !(mine.deleted.contains(p) && remote.deleted.contains(p)))
        .collect())
}

// ---- sending ----------------------------------------------------------------------------------

/// What a send carries. Each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendPlan {
    pub files: Vec<String>,
    pub deleted: Vec<String>,
    /// Changed here, but changed on the other side too: not sent, so the other side's work is
    /// never replaced.
    pub kept_back: Vec<String>,
}

/// What to send from `local_dir`: every file that differs from `base` (changed, new, mode) and
/// every deletion, except those in `remote_changed` (the other side's [`Changes::all`]). An
/// empty `base` is the first send.
pub fn plan_send(
    local_dir: &Path,
    base: &Manifest,
    remote_changed: &[String],
) -> anyhow::Result<SendPlan> {
    let c = changes(local_dir, base)?;
    let theirs: BTreeSet<&str> = remote_changed.iter().map(String::as_str).collect();
    let mut plan = SendPlan::default();
    let mut kept = BTreeSet::new();
    let files: BTreeSet<&String> = c.changed.iter().chain(&c.added).chain(&c.modes).collect();
    for rel in files {
        if theirs.contains(rel.as_str()) {
            kept.insert(rel.clone());
        } else {
            plan.files.push(rel.clone());
        }
    }
    for rel in &c.deleted {
        if theirs.contains(rel.as_str()) {
            kept.insert(rel.clone());
        } else {
            plan.deleted.push(rel.clone());
        }
    }
    plan.kept_back = kept.into_iter().collect();
    Ok(plan)
}

/// Write a stream to `out`: the head with `deleted`, then `files` of `dir` with their modes,
/// links as links, and with `with_git` the whole `.git` folder (the first send only). A file
/// gone since it was listed is left out.
pub fn pack(
    dir: &Path,
    files: &[String],
    deleted: &[String],
    with_git: bool,
    out: impl Write,
) -> anyhow::Result<()> {
    let mut b = tar::Builder::new(out);
    let head = serde_json::to_vec(&Head {
        deleted: deleted.to_vec(),
        version: VERSION,
    })?;
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(head.len() as u64);
    h.set_mode(0o644);
    b.append_data(&mut h, HEAD, head.as_slice())?;
    for rel in files {
        anyhow::ensure!(
            !is_git(rel),
            "`{rel}`: `.git` goes only with the first send"
        );
        plain(rel).map_err(|why| anyhow::anyhow!("`{rel}`: {why}"))?;
        // A file reached through a link in its folders is not the folder's: left out.
        if parents(dir, rel).is_err() {
            continue;
        }
        add(&mut b, dir, rel)?;
    }
    if with_git && dir.join(".git").is_dir() {
        add_tree(&mut b, dir, ".git")?;
    }
    b.into_inner()?.flush()?;
    Ok(())
}

/// One file or link of `dir` into the stream.
fn add<W: Write>(b: &mut tar::Builder<W>, dir: &Path, rel: &str) -> anyhow::Result<()> {
    let path = dir.join(rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut h = tar::Header::new_gnu();
    if meta.is_symlink() {
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        b.append_link(&mut h, rel, std::fs::read_link(&path)?)?;
    } else if meta.is_file() {
        let size = meta.len();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(size);
        h.set_mode(mode_of(&meta));
        if let Ok(t) = meta.modified() {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                h.set_mtime(d.as_secs());
            }
        }
        // Exactly `size` bytes even if the file changes while it is read: a file that grew is
        // cut, one that shrank is filled. The next comparison sees the difference.
        let data = std::fs::File::open(&path)?
            .take(size)
            .chain(std::io::repeat(0))
            .take(size);
        b.append_data(&mut h, rel, data)?;
    }
    Ok(())
}

/// A folder of `dir` with everything in it, folders too (git needs some empty ones).
fn add_tree<W: Write>(b: &mut tar::Builder<W>, dir: &Path, rel: &str) -> anyhow::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_size(0);
    h.set_mode(0o755);
    b.append_data(&mut h, rel, std::io::empty())?;
    let mut names: Vec<(String, std::fs::FileType)> = std::fs::read_dir(dir.join(rel))?
        .flatten()
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.file_type().ok()?)))
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in names {
        let rel = format!("{rel}/{name}");
        if kind.is_dir() {
            add_tree(b, dir, &rel)?;
        } else if kind.is_file() || kind.is_symlink() {
            add(b, dir, &rel)?;
        }
    }
    Ok(())
}

/// The first entry of a stream.
#[derive(Serialize, Deserialize)]
struct Head {
    #[serde(default)]
    deleted: Vec<String>,
    #[serde(default)]
    version: u32,
}

// ---- taking a stream in -----------------------------------------------------------------------

/// What [`apply`] did. Paths sorted as they came.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub written: Vec<String>,
    pub deleted: Vec<String>,
    /// A path and why it was not taken, in plain words.
    pub refused: Vec<(String, String)>,
}

/// Take a stream into `dir`, trusting nothing in it. Refused, each on its own: a path out of
/// the folder (absolute, `..`, or what `ark_files::inside` rejects), an empty or odd path,
/// `.git`, a link that points out of the folder, anything at or under an existing link here,
/// hard links, devices and pipes. Setuid, setgid and sticky bits are dropped. Each file is
/// written whole or not at all. `before(rel)` is called for every file here about to be
/// replaced or deleted, before it is touched, so its version can be kept; an error from it
/// refuses that file. Deletions in the head take the same checks.
pub fn apply(
    dir: &Path,
    tar: impl Read,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Applied> {
    take(dir, tar, false, before)
}

/// [`apply`] for the first send, which also carries the `.git` folder.
pub fn apply_first(
    dir: &Path,
    tar: impl Read,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Applied> {
    take(dir, tar, true, before)
}

/// What one entry did.
enum Took {
    Written,
    Nothing,
}

fn take(
    dir: &Path,
    tar: impl Read,
    first: bool,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Applied> {
    std::fs::create_dir_all(dir)?;
    let root = ark_os::dirs::canonicalize(dir)?;
    let mut archive = tar::Archive::new(tar);
    let mut entries = archive.entries()?;
    let head: Head = {
        let mut e = entries
            .next()
            .ok_or_else(|| anyhow::anyhow!("the copy is empty"))??;
        anyhow::ensure!(
            e.path_bytes().as_ref() == HEAD.as_bytes()
                && e.header().entry_type() == tar::EntryType::Regular,
            "this is not a project copy: it does not start with `{HEAD}`"
        );
        let mut text = Vec::new();
        (&mut e).take(MAX_HEAD).read_to_end(&mut text)?;
        serde_json::from_slice(&text)?
    };
    anyhow::ensure!(
        head.version == VERSION,
        "this copy is version {}; this ARK reads version {VERSION}",
        head.version
    );
    let mut done = Applied::default();
    let mut seq = 0u64;
    for e in entries {
        let mut e = e?;
        let raw = e.path_bytes().into_owned();
        let Ok(mut rel) = String::from_utf8(raw.clone()) else {
            let shown = String::from_utf8_lossy(&raw).into_owned();
            done.refused.push((shown, "the name is not text".into()));
            continue;
        };
        if e.header().entry_type() == tar::EntryType::Directory && rel.len() > 1 {
            rel = rel.strip_suffix('/').unwrap_or(&rel).to_string();
        }
        match take_entry(&root, &rel, &mut e, first, before, &mut seq) {
            Ok(Took::Written) => done.written.push(rel),
            Ok(Took::Nothing) => {}
            Err(why) => done.refused.push((rel, why)),
        }
    }
    for rel in head.deleted {
        match remove(&root, &rel, before) {
            Ok(true) => done.deleted.push(rel),
            Ok(false) => {}
            Err(why) => done.refused.push((rel, why)),
        }
    }
    Ok(done)
}

/// Where `rel` may be written in `root`, or why not.
fn checked(root: &Path, rel: &str, git_ok: bool) -> Result<PathBuf, String> {
    plain(rel)?;
    if is_git(rel) && !(git_ok && rel.split('/').next() == Some(".git")) {
        return Err("`.git` is taken only with the first copy".into());
    }
    if is_git(rel) && rel.split('/').skip(1).any(|p| p == ".git") {
        return Err("a `.git` folder inside the project is not taken".into());
    }
    parents(root, rel)?;
    ark_files::inside(root, rel).map_err(|e| e.to_string())?;
    Ok(root.join(rel))
}

fn take_entry<R: Read>(
    root: &Path,
    rel: &str,
    e: &mut tar::Entry<'_, R>,
    first: bool,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
    seq: &mut u64,
) -> Result<Took, String> {
    use tar::EntryType as T;
    let kind = e.header().entry_type();
    match kind {
        T::Regular | T::Continuous | T::Symlink | T::Directory => {}
        T::Link => return Err("hard links are not taken".into()),
        T::Char | T::Block => return Err("devices are not taken".into()),
        T::Fifo => return Err("pipes are not taken".into()),
        _ => return Err("this kind of entry is not taken".into()),
    }
    let dest = checked(root, rel, first)?;
    let here = std::fs::symlink_metadata(&dest).ok();
    if let Some(m) = &here {
        if m.is_symlink() && kind != T::Symlink {
            return Err("it is a link here; nothing is written through a link".into());
        }
        if m.is_dir() && kind != T::Directory {
            return Err("it is a folder here".into());
        }
        if !m.is_dir() && kind == T::Directory {
            return Err("it is a file here, not a folder".into());
        }
        if !(m.is_file() || m.is_dir() || m.is_symlink()) {
            return Err("it is not a file here".into());
        }
    }
    if kind == T::Directory {
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        return Ok(Took::Nothing);
    }
    let parent = dest
        .parent()
        .ok_or_else(|| "no folder for it".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    *seq += 1;
    let tmp = parent.join(format!(".ark-copy-{}-{}.tmp", std::process::id(), seq));
    if kind == T::Symlink {
        return take_link(root, rel, e, &dest, &tmp, here.as_ref(), before);
    }

    let mode = e.header().mode().unwrap_or(0o644) & 0o777;
    let new = write_new(&tmp, e, mode).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        err.to_string()
    })?;
    if let Some(m) = &here {
        // The same contents: at most the mode changes, and there is nothing to keep.
        if m.len() == new.size && hash_file(&dest).ok().as_deref() == Some(new.sha256.as_str()) {
            let _ = std::fs::remove_file(&tmp);
            if mode_of(m) == mode {
                return Ok(Took::Nothing);
            }
            set_mode(&dest, mode).map_err(|e| e.to_string())?;
            return Ok(Took::Written);
        }
        if let Err(err) = before(rel) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("its version here could not be kept: {err}"));
        }
    }
    std::fs::rename(&tmp, &dest).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        err.to_string()
    })?;
    Ok(Took::Written)
}

/// Write an entry's bytes to a new file `tmp` with `mode`. Answers what was written.
fn write_new<R: Read>(tmp: &Path, e: &mut tar::Entry<'_, R>, mode: u32) -> anyhow::Result<Entry> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut f = open.open(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = e.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        f.write_all(&buf[..n])?;
        size += n as u64;
    }
    f.flush()?;
    drop(f);
    set_mode(tmp, mode)?;
    Ok(Entry {
        size,
        mode,
        sha256: hex(&hasher.finalize()),
        link: None,
    })
}

/// A link entry: taken only when it points inside the folder.
fn take_link<R: Read>(
    root: &Path,
    rel: &str,
    e: &mut tar::Entry<'_, R>,
    dest: &Path,
    tmp: &Path,
    here: Option<&std::fs::Metadata>,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> Result<Took, String> {
    let target = e
        .link_name()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "a link with no target".to_string())?;
    let target = target
        .to_str()
        .ok_or_else(|| "the link's target is not text".to_string())?
        .to_string();
    link_inside(root, rel, &target)?;
    if here.is_some_and(|m| m.is_file()) {
        before(rel).map_err(|err| format!("its version here could not be kept: {err}"))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, tmp).map_err(|e| e.to_string())?;
        std::fs::rename(tmp, dest).map_err(|err| {
            let _ = std::fs::remove_file(tmp);
            err.to_string()
        })?;
        Ok(Took::Written)
    }
    #[cfg(not(unix))]
    {
        let _ = (tmp, dest);
        Err("links are not taken on this system".into())
    }
}

/// A link at `rel` pointing to `target` stays inside `root`, or why not.
fn link_inside(root: &Path, rel: &str, target: &str) -> Result<(), String> {
    let out = || "the link points outside the project".to_string();
    if target.is_empty() || target.starts_with('/') || target.contains('\\') {
        return Err(out());
    }
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop();
    for t in target.split('/') {
        match t {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or_else(out)?;
            }
            name => parts.push(name),
        }
    }
    // Through a link already here that points out, it would lead out too.
    ark_files::inside(root, &parts.join("/")).map_err(|_| out())?;
    Ok(())
}

/// Delete `rel` from `root` after the same checks as a write. Answers whether something was
/// there. Folders left empty go too.
fn remove(
    root: &Path,
    rel: &str,
    before: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> Result<bool, String> {
    let dest = checked(root, rel, false)?;
    let m = match std::fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if m.is_dir() {
        return Err("it is a folder here".into());
    }
    if m.is_file() {
        before(rel).map_err(|err| format!("its version here could not be kept: {err}"))?;
    }
    std::fs::remove_file(&dest).map_err(|e| e.to_string())?;
    let mut up = dest.parent();
    while let Some(p) = up {
        if p == root || std::fs::remove_dir(p).is_err() {
            break;
        }
        up = p.parent();
    }
    Ok(true)
}

// ---- paths and files --------------------------------------------------------------------------

/// `rel` is a plain relative path: names joined by `/`, no `.` or `..`, nothing odd.
fn plain(rel: &str) -> Result<(), String> {
    if rel.is_empty() {
        return Err("an empty path".into());
    }
    if rel.starts_with('/') || rel.as_bytes().get(1) == Some(&b':') {
        return Err("an absolute path".into());
    }
    if rel.contains('\\') || rel.chars().any(char::is_control) || rel.trim() != rel {
        return Err("an odd path".into());
    }
    for part in rel.split('/') {
        match part {
            ".." => return Err("the path leads out of the folder (`..`)".into()),
            "" | "." => return Err("an odd path".into()),
            _ => {}
        }
    }
    Ok(())
}

/// A path in a `.git` folder, at any depth.
fn is_git(rel: &str) -> bool {
    rel.split('/').any(|p| p == ".git")
}

/// Every folder on the way to `rel` in `root` is a real folder or not there yet.
fn parents(root: &Path, rel: &str) -> Result<(), String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut p = root.to_path_buf();
    for (i, part) in parts[..parts.len().saturating_sub(1)].iter().enumerate() {
        p.push(part);
        let shown = parts[..=i].join("/");
        match std::fs::symlink_metadata(&p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
            Ok(m) if m.is_symlink() => {
                return Err(format!(
                    "`{shown}` is a link here; nothing is written through a link"
                ))
            }
            Ok(m) if !m.is_dir() => return Err(format!("`{shown}` is a file here, not a folder")),
            Ok(_) => {}
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Permission bits only.
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}
