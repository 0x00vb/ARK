//! The project copy of a chat placed on another computer (ADR 0052): sent before a chat starts
//! there, brought back when the person asks. The files cross as a stream in parts through the
//! link, never with the system `tar`; [`crate::copy`] decides what goes and takes a stream in
//! trusting nothing.
//!
//! | Side | Calls | Keeps |
//! |---|---|---|
//! | This computer | `place_changes`, `place_bring` (the window's) | a manifest per copy, `<project>:<computer id>` |
//! | The computer | `copy_state`, `copy_put`, `copy_pack`, `copy_part`, `copy_settle` (`Links::chat`) | a manifest per origin's project, `<origin>:<project>` |
//!
//! Each side keeps a version before of every file the other side's stream replaces or deletes
//! (ADR 0034), so one step undoes it.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use ark_interact::held::{self, HeldChat};
use ark_sdk::prelude::*;
use base64::Engine as _;
use serde_json::{json, Value};

use super::Host;
use crate::copy::{self, Changes, Manifest};

/// One part of a stream, decoded: well under what one call may carry (`upload::MAX_CALL`), and
/// small enough for a slow link to carry within a call's time.
const PART: usize = 1024 * 1024;
const RECEIPT_KIND: &str = "copy_receipt";
const PACK_KIND: &str = "copy_pack";
const EPOCH_KIND: &str = "copy_epoch";
const BRING_PENDING_KIND: &str = "copy_bring_pending";

/// A stream's temporary file in the store (swept with the uploads after a day).
fn stream_file(store: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "a stream id is 8 to 64 letters, digits or `-`"
    );
    let dir = store.join("uploads");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("copy-{id}.tar")))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

impl Host {
    fn copy_lock(&self, side: &str, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let name = format!("{side}:{key}");
        self.copy_locks
            .lock()
            .unwrap()
            .entry(name)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn record_plan(manifest: &mut Manifest, plan: &Manifest, paths: &[String]) {
        for path in paths {
            manifest.record_choice(path, plan.entries.get(path).cloned());
        }
    }

    fn plan_matches(dir: &Path, plan: &Manifest, paths: &[String]) -> anyhow::Result<bool> {
        let current = copy::scan(dir, paths)?;
        Ok(paths
            .iter()
            .all(|path| current.entries.get(path) == plan.entries.get(path)))
    }

    fn copy_epoch(&self, key: &str) -> anyhow::Result<u64> {
        Ok(self
            .engine
            .store
            .get_record(EPOCH_KIND, key)?
            .and_then(|value| value["epoch"].as_u64())
            .unwrap_or(0))
    }

    fn advance_copy_epoch(&self, key: &str, epoch: u64) -> anyhow::Result<()> {
        self.engine.store.put_record(
            EPOCH_KIND,
            key,
            key,
            &json!({ "epoch": epoch.saturating_add(1) }),
        )?;
        Ok(())
    }

    // ---- on the computer ----------------------------------------------------------------------

    /// The project an origin's copy lives in here, its folder, and its manifest's key.
    fn copy_of(&self, params: &Value) -> anyhow::Result<(Project, PathBuf, String)> {
        let s = |k: &str| params[k].as_str().unwrap_or("").to_string();
        let (origin, project) = (s("origin"), s("origin_project"));
        let p = held::origin_project(&self.engine, &origin, &project, &s("name"))?;
        let dir = self.engine.workspace_dir(&p.id)?;
        std::fs::create_dir_all(&dir)?;
        Ok((p, dir, format!("{origin}:{project}")))
    }

    /// Whether a turn or an agent's task works in `project` now: a stream into its folder
    /// waits until it ends.
    fn works_in(&self, project: &ProjectId) -> bool {
        let chats = self
            .working()
            .iter()
            .any(|w| w["project"] == json!(project.as_str()) && w["place"].is_null());
        let tasks = self.orchs.list().iter().any(|o| {
            &o.project().id == project
                && o.tasks()
                    .iter()
                    .any(|t| t.state == ark_interact::ark_orchestrate::TaskState::Running)
        });
        chats || tasks
    }

    /// The calls of another runtime on the copy it keeps here.
    pub(super) async fn copy_call(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
    ) -> anyhow::Result<Value> {
        let replica = format!(
            "{}:{}",
            params["origin"].as_str().unwrap_or(""),
            params["origin_project"].as_str().unwrap_or("")
        );
        let lock = self.copy_lock("destination", &replica);
        let _guard = lock.lock().await;
        let store = self.engine.store.root().to_path_buf();
        match method {
            // What changed here since the last send or bring back, and whether one may go now.
            "copy_state" => {
                let (p, dir, key) = self.copy_of(params)?;
                let first = self
                    .engine
                    .store
                    .get_record(copy::MANIFEST_KIND, &key)?
                    .is_none();
                let base = Manifest::load(&self.engine.store, &key)?;
                let changes =
                    tokio::task::spawn_blocking(move || copy::changes(&dir, &base)).await??;
                Ok(json!({
                    "project": { "id": p.id, "name": p.name },
                    "first": first,
                    "changes": changes,
                    "busy": self.works_in(&p.id),
                    "epoch": self.copy_epoch(&key)?,
                }))
            }
            // A stream from the origin, in parts; the last one takes it into the folder.
            "copy_put" => {
                let (p, dir, key) = self.copy_of(params)?;
                anyhow::ensure!(
                    !self.works_in(&p.id),
                    "work is running in this project; the copy waits until it ends"
                );
                let stream = params["stream"].as_str().unwrap_or("");
                let epoch = params["epoch"].as_u64().unwrap_or(u64::MAX);
                let receipt = format!("{key}:{epoch}:{stream}");
                if let Some(applied) = self.engine.store.get_record(RECEIPT_KIND, &receipt)? {
                    return Ok(json!({ "done": true, "applied": applied }));
                }
                anyhow::ensure!(
                    epoch == self.copy_epoch(&key)?,
                    "the project copy changed during this transfer; retry its plan"
                );
                let file = stream_file(&store, stream)?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(params["data"].as_str().unwrap_or(""))?;
                anyhow::ensure!(data.len() <= crate::upload::MAX_CALL, "a part is too big");
                let offset = params["offset"].as_u64().unwrap_or(0);
                let current = file.metadata().map(|m| m.len()).unwrap_or(0);
                if current != offset {
                    return Ok(json!({ "done": false, "next": current }));
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file)?;
                f.write_all(&data)?;
                drop(f);
                if !params["last"].as_bool().unwrap_or(false) {
                    return Ok(json!({ "done": false, "next": offset + data.len() as u64 }));
                }
                let first = params["first"].as_bool().unwrap_or(false);
                let packed: Manifest = serde_json::from_value(params["manifest"].clone())?;
                let engine = self.engine.clone();
                let applied = tokio::task::spawn_blocking(move || {
                    let tar = std::fs::File::open(&file)?;
                    let mut before = |rel: &str| {
                        engine.keep_before(&p.id, rel)?;
                        anyhow::Ok(())
                    };
                    let applied = if first {
                        copy::apply_first(&dir, tar, &mut before)
                    } else {
                        copy::apply(&dir, tar, &mut before)
                    };
                    let _ = std::fs::remove_file(&file);
                    let applied = applied?;
                    let mut m = Manifest::load(&engine.store, &key)?;
                    let touched: Vec<String> = applied
                        .written
                        .iter()
                        .chain(&applied.deleted)
                        .cloned()
                        .collect();
                    Host::record_plan(&mut m, &packed, &touched);
                    m.save(&engine.store, &key)?;
                    let current = engine
                        .store
                        .get_record(EPOCH_KIND, &key)?
                        .and_then(|value| value["epoch"].as_u64())
                        .unwrap_or(0);
                    anyhow::ensure!(current == epoch, "the project copy changed during apply");
                    engine.store.put_record(
                        EPOCH_KIND,
                        &key,
                        &key,
                        &json!({ "epoch": epoch.saturating_add(1) }),
                    )?;
                    engine.store.put_record(
                        RECEIPT_KIND,
                        &receipt,
                        &key,
                        &serde_json::to_value(&applied)?,
                    )?;
                    anyhow::Ok(applied)
                })
                .await??;
                Ok(json!({ "done": true, "applied": applied }))
            }
            // A stream of these files (and deletions) for the origin to read in parts.
            "copy_pack" => {
                let (_, dir, key) = self.copy_of(params)?;
                let (files, deleted) = (strings(&params["files"]), strings(&params["deleted"]));
                let packed_manifest = copy::scan(&dir, &files)?;
                let epoch = self.copy_epoch(&key)?;
                use sha2::Digest as _;
                let identity = serde_json::to_vec(&json!({
                    "key": key,
                    "epoch": epoch,
                    "files": files,
                    "deleted": deleted,
                    "manifest": packed_manifest,
                }))?;
                let id = format!("{:x}", sha2::Sha256::digest(identity));
                let file = stream_file(&store, &id)?;
                let pack_key = format!("{key}:{id}");
                let completed = self.engine.store.get_record(PACK_KIND, &pack_key)?;
                let size = if completed.as_ref().is_some_and(|record| {
                    file.metadata().ok().map(|m| m.len()) == record["size"].as_u64()
                }) {
                    file.metadata()?.len()
                } else {
                    let _ = std::fs::remove_file(&file);
                    let partial =
                        file.with_extension(format!("{}.partial", uuid::Uuid::new_v4().simple()));
                    let (out_file, pack_files, pack_deleted) =
                        (partial.clone(), files.clone(), deleted.clone());
                    let size = tokio::task::spawn_blocking(move || {
                        let out = std::fs::File::create(&out_file)?;
                        copy::pack(
                            &dir,
                            &pack_files,
                            &pack_deleted,
                            false,
                            std::io::BufWriter::new(out),
                        )?;
                        anyhow::Ok(std::fs::metadata(&out_file)?.len())
                    })
                    .await??;
                    std::fs::rename(&partial, &file)?;
                    size
                };
                self.engine.store.put_record(
                    PACK_KIND,
                    &pack_key,
                    &key,
                    &json!({ "manifest": packed_manifest, "deleted": deleted, "epoch": epoch, "size": size }),
                )?;
                Ok(json!({
                    "stream": id, "size": size, "epoch": epoch,
                    "manifest": packed_manifest, "deleted": deleted,
                }))
            }
            "copy_part" => {
                let file = stream_file(&store, params["stream"].as_str().unwrap_or(""))?;
                let offset = params["offset"].as_u64().unwrap_or(0);
                let (data, done) = tokio::task::spawn_blocking(move || {
                    use std::io::{Read, Seek};
                    let mut f = std::fs::File::open(&file)?;
                    let size = f.metadata()?.len();
                    f.seek(std::io::SeekFrom::Start(offset))?;
                    let mut buf = vec![0; PART];
                    let n = f.read(&mut buf)?;
                    buf.truncate(n);
                    let done = offset + n as u64 >= size;
                    anyhow::Ok((buf, done))
                })
                .await??;
                Ok(json!({
                    "data": base64::engine::general_purpose::STANDARD.encode(data),
                    "done": done,
                }))
            }
            // The origin took these paths as they are here: they no longer differ.
            "copy_settle" => {
                let (_, _, key) = self.copy_of(params)?;
                let paths = strings(&params["paths"]);
                let stream = params["stream"].as_str().unwrap_or("");
                let receipt = format!("{key}:settle:{stream}");
                if self
                    .engine
                    .store
                    .get_record(RECEIPT_KIND, &receipt)?
                    .is_some()
                {
                    return Ok(json!({ "ok": true }));
                }
                let pack_key = format!("{key}:{stream}");
                let Some(pack) = self.engine.store.get_record(PACK_KIND, &pack_key)? else {
                    return Ok(json!({ "ok": false, "stale": true }));
                };
                let packed: Manifest = serde_json::from_value(pack["manifest"].clone())?;
                let deleted = strings(&pack["deleted"]);
                let epoch = pack["epoch"].as_u64().unwrap_or(u64::MAX);
                if self.copy_epoch(&key)? != epoch {
                    return Ok(json!({ "ok": false, "stale": true }));
                }
                let mut m = Manifest::load(&self.engine.store, &key)?;
                for path in &paths {
                    m.record_choice(path, packed.entries.get(path).cloned());
                }
                for path in deleted {
                    if paths.contains(&path) {
                        m.record_choice(&path, None);
                    }
                }
                m.save(&self.engine.store, &key)?;
                self.advance_copy_epoch(&key, epoch)?;
                self.engine.store.put_record(
                    RECEIPT_KIND,
                    &receipt,
                    &key,
                    &json!({ "paths": paths }),
                )?;
                let _ = self.engine.store.delete_record(PACK_KIND, &pack_key);
                if !stream.is_empty() {
                    let file = stream_file(&store, stream)?;
                    let _ = std::fs::remove_file(file);
                }
                Ok(json!({ "ok": true }))
            }
            other => anyhow::bail!("unknown method `{other}`"),
        }
    }

    // ---- on this computer ---------------------------------------------------------------------

    /// What identifies a project's copy on `place`, in the calls there.
    fn origin(&self, place: &str, project: &Project) -> Value {
        let _ = place;
        json!({
            "origin": held::runtime_id(&self.engine),
            "origin_project": project.id,
            "name": project.name,
        })
    }

    /// This side's manifest key for a project's copy on `place`.
    /// Known by the computer's store id, never its name: a renamed link keeps its copy.
    fn copy_key(&self, place: &str, project: &ProjectId) -> anyhow::Result<String> {
        let computer = self.links.runtime_id(place).ok_or_else(|| {
            anyhow::anyhow!("{place} runs an older version that cannot hold chats")
        })?;
        Ok(format!("{}:{computer}", project.as_str()))
    }

    /// Send a stream file to `place` in parts. Answers what it did with it there.
    async fn send_stream(
        &self,
        place: &str,
        project: &Project,
        file: &Path,
        first: bool,
        epoch: u64,
        manifest: &Manifest,
    ) -> anyhow::Result<copy::Applied> {
        let bytes = tokio::fs::read(file).await?;
        let _ = tokio::fs::remove_file(file).await;
        if bytes.is_empty() {
            anyhow::bail!("nothing to send");
        }
        use sha2::Digest as _;
        let stream = format!("{:x}", sha2::Sha256::digest(&bytes));
        let mut offset = 0usize;
        loop {
            let end = (offset + PART).min(bytes.len());
            let part = &bytes[offset..end];
            let mut p = self.origin(place, project);
            p["stream"] = json!(stream);
            p["data"] = json!(base64::engine::general_purpose::STANDARD.encode(part));
            p["offset"] = json!(offset);
            p["last"] = json!(end == bytes.len());
            p["first"] = json!(first);
            p["epoch"] = json!(epoch);
            p["manifest"] = serde_json::to_value(manifest)?;
            let answer = self.links.chat(place, "copy_put", p).await?;
            if answer["done"] == true {
                return Ok(serde_json::from_value(answer["applied"].clone())?);
            }
            let next = answer["next"].as_u64().ok_or_else(|| {
                anyhow::anyhow!("the receiving computer did not acknowledge the transfer")
            })? as usize;
            anyhow::ensure!(
                next <= bytes.len(),
                "the receiving computer acknowledged an invalid offset"
            );
            offset = next;
        }
    }

    /// Before a chat starts on `place`: send what changed in the project here since the last
    /// send. The first send carries everything a send carries (and `.git`); above the limit it
    /// waits for the person (`big`). A file the computer changed meanwhile is never replaced
    /// (`kept_back`). Answers `{ask}` for the size question, else what happened.
    pub(super) async fn copy_send(
        self: &Arc<Self>,
        place: &str,
        project: &Project,
        big: bool,
    ) -> anyhow::Result<Value> {
        let key = self.copy_key(place, &project.id)?;
        let lock = self.copy_lock("origin", &key);
        let _guard = lock.lock().await;
        let state = self
            .links
            .chat(place, "copy_state", self.origin(place, project))
            .await?;
        if state["busy"] == true {
            return Ok(json!({ "busy": true }));
        }
        let dir = self.engine.workspace_dir(&project.id)?;
        let base = Manifest::load(&self.engine.store, &key)?;
        let first = state["first"] == true;
        let remote: Changes = serde_json::from_value(state["changes"].clone()).unwrap_or_default();
        let d = dir.clone();
        let (files, deleted, kept_back, with_git, ask) = tokio::task::spawn_blocking(move || {
            if first {
                let sel = copy::select(&d)?;
                let ask = (sel.over_limit() && !big)
                    .then(|| json!({ "bytes": sel.bytes, "files": sel.count }));
                anyhow::Ok((sel.files, Vec::new(), Vec::new(), sel.git, ask))
            } else {
                let plan = copy::plan_send(&d, &base, &remote.all())?;
                anyhow::Ok((plan.files, plan.deleted, plan.kept_back, false, None))
            }
        })
        .await??;
        if let Some(ask) = ask {
            return Ok(json!({ "ask": ask }));
        }
        if !first && files.is_empty() && deleted.is_empty() {
            return Ok(json!({ "sent": 0, "kept_back": kept_back }));
        }
        let touched: Vec<String> = files.iter().chain(&deleted).cloned().collect();
        let sent_plan = copy::scan(&dir, &touched)?;
        let file = stream_file(
            self.engine.store.root(),
            &uuid::Uuid::new_v4().simple().to_string(),
        )?;
        let (d, f, fl, dl) = (dir.clone(), file.clone(), files.clone(), deleted.clone());
        tokio::task::spawn_blocking(move || {
            let out = std::fs::File::create(&f)?;
            copy::pack(&d, &fl, &dl, with_git, std::io::BufWriter::new(out))
        })
        .await??;
        if !Self::plan_matches(&dir, &sent_plan, &touched)? {
            let _ = std::fs::remove_file(&file);
            anyhow::bail!("the project changed while its transfer was packed; retry its plan");
        }
        let applied = self
            .send_stream(
                place,
                project,
                &file,
                first,
                state["epoch"].as_u64().unwrap_or(0),
                &sent_plan,
            )
            .await?;
        let mut m = if first {
            Manifest::default()
        } else {
            Manifest::load(&self.engine.store, &key)?
        };
        Self::record_plan(&mut m, &sent_plan, &touched);
        m.save(&self.engine.store, &key)?;
        Ok(json!({
            "sent": applied.written.len() + applied.deleted.len(),
            "kept_back": kept_back,
            "refused": applied.refused,
        }))
    }

    /// What changed on a held chat's computer since the last send or bring back, and which of
    /// those files changed here too (each needs a choice).
    pub(super) async fn place_changes(&self, chat: &HeldChat) -> anyhow::Result<Value> {
        let project = self.held_project(chat)?;
        let state = self
            .links
            .chat(
                &chat.place,
                "copy_state",
                self.origin(&chat.place, &project),
            )
            .await?;
        let remote: Changes = serde_json::from_value(state["changes"].clone()).unwrap_or_default();
        let dir = self.engine.workspace_dir(&project.id)?;
        let base = Manifest::load(
            &self.engine.store,
            &self.copy_key(&chat.place, &project.id)?,
        )?;
        let r = remote.clone();
        let both =
            tokio::task::spawn_blocking(move || copy::both_changed(&dir, &r, &base)).await??;
        Ok(json!({
            "count": remote.all().len(),
            "changes": remote,
            "both": both,
            "too_big": remote.over_limit(),
            "epoch": state["epoch"],
        }))
    }

    fn held_project(&self, chat: &HeldChat) -> anyhow::Result<Project> {
        self.engine
            .store
            .get_project(&ProjectId(chat.project.clone()))?
            .ok_or_else(|| anyhow::anyhow!("this chat's project is gone"))
    }

    /// Bring what changed on a held chat's computer to this computer's folder, keeping a
    /// version before of each file it replaces or deletes. A file changed on both sides waits
    /// for a choice (`choices`: path to `"mine"` or `"theirs"`); `"mine"` sends this computer's
    /// version there instead. Above the limit it waits for the person (`big`).
    pub(super) async fn place_bring(
        self: &Arc<Self>,
        chat: &HeldChat,
        choices: &HashMap<String, String>,
        big: bool,
    ) -> anyhow::Result<Value> {
        let project = self.held_project(chat)?;
        let place = chat.place.as_str();
        let dir = self.engine.workspace_dir(&project.id)?;
        let key = self.copy_key(place, &project.id)?;
        let lock = self.copy_lock("origin", &key);
        let _guard = lock.lock().await;
        if let Some(pending) = self.engine.store.get_record(BRING_PENDING_KIND, &key)? {
            let stream = pending["stream"].as_str().unwrap_or("");
            let receipt = format!("{key}:bring:{stream}");
            if let Some(done) = self.engine.store.get_record(RECEIPT_KIND, &receipt)? {
                let applied: copy::Applied = serde_json::from_value(done)?;
                let settled: Vec<String> = applied
                    .written
                    .iter()
                    .chain(&applied.deleted)
                    .cloned()
                    .collect();
                let plan: Manifest = match serde_json::from_value(pending["manifest"].clone()) {
                    Ok(plan) if pending["epoch"].is_u64() => plan,
                    _ => {
                        let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                        let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                        anyhow::bail!(
                            "the interrupted transfer used an obsolete plan; retry Bring"
                        );
                    }
                };
                if !Self::plan_matches(&dir, &plan, &settled)? {
                    let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                    let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                    anyhow::bail!(
                        "files changed here after the transfer was applied; retry Bring to choose"
                    );
                }
                let mut settle = self.origin(place, &project);
                settle["paths"] = json!(settled);
                settle["stream"] = json!(stream);
                let answer = self
                    .links
                    .chat(place, "copy_settle", settle)
                    .await
                    .with_context(|| {
                        "the interrupted transfer remains pending until its remote settlement is acknowledged"
                    })?;
                if answer["ok"] == true {
                    let mut manifest = Manifest::load(&self.engine.store, &key)?;
                    Self::record_plan(&mut manifest, &plan, &settled);
                    manifest.save(&self.engine.store, &key)?;
                    let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                    let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                    self.files_changed(&project.id, &settled);
                    return Ok(json!({
                        "written": applied.written, "deleted": applied.deleted,
                        "refused": applied.refused, "kept_mine": [], "waiting": [],
                    }));
                }
                anyhow::ensure!(answer["stale"] == true, "invalid settlement response");
                let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                // Fall through to the ordinary comparison, which exposes any conflict.
            }
            let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
        }
        let state = self.place_changes(chat).await?;
        if state["too_big"] == true && !big {
            return Ok(
                json!({ "ask": { "bytes": state["changes"]["bytes"], "files": state["count"] } }),
            );
        }
        let remote: Changes = serde_json::from_value(state["changes"].clone())?;
        let both = strings(&state["both"]);
        let choice = |p: &str| choices.get(p).map(String::as_str);
        let waits: Vec<String> = both
            .iter()
            .filter(|p| choice(p).is_none())
            .cloned()
            .collect();
        let skip = |p: &String| both.contains(p) && choice(p) != Some("theirs");
        let take: Vec<String> = remote
            .changed
            .iter()
            .chain(&remote.added)
            .chain(&remote.modes)
            .filter(|p| !skip(p))
            .cloned()
            .collect();
        let gone: Vec<String> = remote
            .deleted
            .iter()
            .filter(|p| !skip(p))
            .cloned()
            .collect();
        let mine: Vec<String> = both
            .iter()
            .filter(|p| choice(p) == Some("mine"))
            .cloned()
            .collect();
        let mut applied = copy::Applied::default();
        if !take.is_empty() || !gone.is_empty() {
            let mut p = self.origin(place, &project);
            p["files"] = json!(take);
            p["deleted"] = json!(gone);
            let packed = self.links.chat(place, "copy_pack", p).await?;
            let stream = packed["stream"].as_str().unwrap_or("").to_string();
            let epoch = packed["epoch"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("the remote transfer plan has no replica epoch"))?;
            let plan: Manifest = serde_json::from_value(packed["manifest"].clone())?;
            let receipt = format!("{key}:bring:{stream}");
            self.engine.store.put_record(
                BRING_PENDING_KIND,
                &key,
                &key,
                &json!({
                    "stream": stream, "epoch": epoch,
                    "manifest": plan, "deleted": packed["deleted"],
                }),
            )?;
            let file = stream_file(self.engine.store.root(), &stream)?;
            if let Some(done) = self.engine.store.get_record(RECEIPT_KIND, &receipt)? {
                applied = serde_json::from_value(done)?;
            } else {
                let mut out = tokio::fs::File::create(&file).await?;
                let mut offset = 0u64;
                loop {
                    let mut q = self.origin(place, &project);
                    q["stream"] = json!(stream);
                    q["offset"] = json!(offset);
                    let part = self.links.chat(place, "copy_part", q).await?;
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(part["data"].as_str().unwrap_or(""))?;
                    offset += data.len() as u64;
                    tokio::io::AsyncWriteExt::write_all(&mut out, &data).await?;
                    if part["done"] == true || data.is_empty() {
                        break;
                    }
                }
                drop(out);
                let (engine, pid, d, f) = (
                    self.engine.clone(),
                    project.id.clone(),
                    dir.clone(),
                    file.clone(),
                );
                applied = tokio::task::spawn_blocking(move || {
                    let tar = std::fs::File::open(&f)?;
                    let mut before = |rel: &str| {
                        engine.keep_before(&pid, rel)?;
                        anyhow::Ok(())
                    };
                    let r = copy::apply(&d, tar, &mut before);
                    let _ = std::fs::remove_file(&f);
                    r
                })
                .await??;
                self.engine.store.put_record(
                    RECEIPT_KIND,
                    &receipt,
                    &key,
                    &serde_json::to_value(&applied)?,
                )?;
            }
            let settled: Vec<String> = applied
                .written
                .iter()
                .chain(&applied.deleted)
                .cloned()
                .collect();
            if !Self::plan_matches(&dir, &plan, &settled)? {
                let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                anyhow::bail!(
                    "files changed here after the transfer was applied; retry Bring to choose"
                );
            }
            let mut s = self.origin(place, &project);
            s["paths"] = json!(settled);
            s["stream"] = json!(stream);
            let answer = self.links.chat(place, "copy_settle", s).await?;
            if answer["stale"] == true {
                let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
                let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
                anyhow::bail!("the remote replica changed before settlement; retry Bring");
            }
            anyhow::ensure!(answer["ok"] == true, "invalid settlement response");
            let mut m = Manifest::load(&self.engine.store, &key)?;
            Self::record_plan(&mut m, &plan, &settled);
            m.save(&self.engine.store, &key)?;
            let _ = self.engine.store.delete_record(RECEIPT_KIND, &receipt);
            let _ = self.engine.store.delete_record(BRING_PENDING_KIND, &key);
            self.files_changed(&project.id, &settled);
        }
        // This computer's version of a file changed on both sides goes there, where its own is
        // kept as a version before.
        if !mine.is_empty() {
            let (pack_here, pack_gone): (Vec<String>, Vec<String>) = mine
                .iter()
                .cloned()
                .partition(|path| dir.join(path).symlink_metadata().is_ok());
            let mine_plan = copy::scan(&dir, &mine)?;
            let d = dir.clone();
            let file = stream_file(
                self.engine.store.root(),
                &uuid::Uuid::new_v4().simple().to_string(),
            )?;
            let f = file.clone();
            tokio::task::spawn_blocking(move || {
                let out = std::fs::File::create(&f)?;
                copy::pack(
                    &d,
                    &pack_here,
                    &pack_gone,
                    false,
                    std::io::BufWriter::new(out),
                )
            })
            .await??;
            if !Self::plan_matches(&dir, &mine_plan, &mine)? {
                let _ = std::fs::remove_file(&file);
                anyhow::bail!("the project changed while its transfer was packed; retry Bring");
            }
            let remote_state = self
                .links
                .chat(place, "copy_state", self.origin(place, &project))
                .await?;
            self.send_stream(
                place,
                &project,
                &file,
                false,
                remote_state["epoch"].as_u64().unwrap_or(0),
                &mine_plan,
            )
            .await?;
            let mut m = Manifest::load(&self.engine.store, &key)?;
            Self::record_plan(&mut m, &mine_plan, &mine);
            m.save(&self.engine.store, &key)?;
        }
        if let Some(l) = self.held_live.lock().unwrap().get_mut(&chat.session) {
            l.changes = waits.len();
        }
        Ok(json!({
            "written": applied.written,
            "deleted": applied.deleted,
            "refused": applied.refused,
            "kept_mine": mine,
            "waiting": waits,
        }))
    }
}
