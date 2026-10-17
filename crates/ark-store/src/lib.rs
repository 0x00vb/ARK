//! # ark-store
//!
//! Persistence for ARK: a content-addressed blob store for artifact bytes plus a SQLite
//! metadata database for projects, artifacts, invocations, notes, evidence cards, working
//! state and checkpoints, and an optional full-text search index.
//!
//! Layout of a workspace root:
//! ```text
//! <root>/
//!   objects/<blake3>      raw artifact bytes, deduplicated across projects
//!   meta.db               SQLite: projects, artifacts, invocations, notes, typed records,
//!                         event streams, …
//!   index/                full-text search index (best-effort)
//! ```
//!
//! Domain objects are stored as JSON blobs with a few promoted columns for ordering and
//! filtering. That keeps the schema stable while `ark-core` evolves, and keeps the store
//! completely agnostic to any domain.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ark_core::{
    Artifact, ArtifactId, EvidenceCard, Invocation, Note, Project, ProjectId, ResearchState,
};
use rusqlite::Connection;
use time::OffsetDateTime;

mod error;
mod index;
mod writer;
pub use error::StoreError;
pub use index::Hit;
pub use writer::LOCK_FILE;
pub type Result<T> = std::result::Result<T, StoreError>;

/// What a project holds, counted (see [`Store::project_stats`]).
#[derive(Clone, Debug, Default)]
pub struct ProjectStats {
    pub invocations: usize,
    /// Characters of stored tool output (compact JSON).
    pub output_chars: usize,
    pub cards: usize,
    /// Characters of distilled summaries and identifiers.
    pub card_chars: usize,
    pub artifacts: usize,
}

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
    /// Full-text index. `None` if it failed to open (search then unavailable).
    search: Option<index::SearchIndex>,
    /// This process is the store's one writer while any handle holds this.
    _writer: std::sync::Arc<writer::WriterLock>,
}

impl Store {
    /// Open (creating if needed) a workspace store rooted at `root`. One process writes a
    /// store: while another has it open this is [`StoreError::Busy`]; handles in this process
    /// share it.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(root.join("objects"))?;
        let writer = writer::acquire(&root)?;
        sweep_temp_blobs(&root.join("objects"));
        let db = Connection::open(root.join("meta.db"))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS projects(
                 id TEXT PRIMARY KEY, created_at TEXT NOT NULL, json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS artifacts(
                 project_id TEXT NOT NULL, id TEXT NOT NULL,
                 kind TEXT NOT NULL, name TEXT NOT NULL, created_at TEXT NOT NULL,
                 json TEXT NOT NULL, PRIMARY KEY(project_id, id));
             CREATE TABLE IF NOT EXISTS invocations(
                 project_id TEXT NOT NULL, id TEXT NOT NULL,
                 action TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL,
                 json TEXT NOT NULL, PRIMARY KEY(project_id, id));
             CREATE TABLE IF NOT EXISTS notes(
                 project_id TEXT NOT NULL, id TEXT NOT NULL,
                 kind TEXT NOT NULL, created_at TEXT NOT NULL,
                 json TEXT NOT NULL, PRIMARY KEY(project_id, id));
             CREATE TABLE IF NOT EXISTS research_state(
                 project_id TEXT PRIMARY KEY, updated_at TEXT NOT NULL, json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS evidence_cards(
                 project_id TEXT NOT NULL, invocation_id TEXT NOT NULL,
                 action TEXT NOT NULL, call_digest TEXT NOT NULL, created_at TEXT NOT NULL,
                 json TEXT NOT NULL, PRIMARY KEY(project_id, invocation_id));
             CREATE TABLE IF NOT EXISTS checkpoints(
                 project_id TEXT PRIMARY KEY, updated_at TEXT NOT NULL, json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions(
                 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, updated_at TEXT NOT NULL,
                 json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session_turns(
                 session_id TEXT NOT NULL, idx INTEGER NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(session_id, idx));
             CREATE TABLE IF NOT EXISTS turn_traces(
                 session_id TEXT NOT NULL, idx INTEGER NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(session_id, idx));
             -- Typed records a runtime layer owns (agents, tasks, rule sets, vendor
             -- sessions). `kind` names the type, `scope` groups them (usually a project).
             CREATE TABLE IF NOT EXISTS records(
                 kind TEXT NOT NULL, id TEXT NOT NULL, scope TEXT NOT NULL,
                 updated_at TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(kind, id));
             -- Append-only event streams. `seq` orders events across all streams.
             CREATE TABLE IF NOT EXISTS events(
                 seq INTEGER PRIMARY KEY AUTOINCREMENT, stream TEXT NOT NULL,
                 at TEXT NOT NULL, json TEXT NOT NULL);
             -- The lookups every turn makes; without them each one scans its table, so a
             -- turn gets slower as the project grows.
             CREATE INDEX IF NOT EXISTS cards_by_digest
                 ON evidence_cards(project_id, call_digest, created_at);
             CREATE INDEX IF NOT EXISTS cards_by_time ON evidence_cards(project_id, created_at);
             CREATE INDEX IF NOT EXISTS invocations_by_time ON invocations(project_id, created_at);
             CREATE INDEX IF NOT EXISTS invocations_by_status ON invocations(status);
             CREATE INDEX IF NOT EXISTS artifacts_by_time ON artifacts(project_id, created_at);
             CREATE INDEX IF NOT EXISTS sessions_by_project ON sessions(project_id, updated_at);
             CREATE INDEX IF NOT EXISTS notes_by_time ON notes(project_id, created_at);
             CREATE INDEX IF NOT EXISTS records_by_scope ON records(kind, scope, updated_at);
             CREATE INDEX IF NOT EXISTS events_by_stream ON events(stream, seq);",
        )?;
        migrate(&db)?;
        let search = match index::SearchIndex::open(&root.join("index")) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("search index unavailable: {e}");
                None
            }
        };
        Ok(Store {
            root,
            db: Mutex::new(db),
            search,
            _writer: writer,
        })
    }

    // ---- search index -----------------------------------------------------

    /// Index a text document (artifact body, note) for full-text search. No-op if the
    /// index is unavailable.
    pub fn index_text(
        &self,
        project: &ProjectId,
        artifact: &ArtifactId,
        name: &str,
        kind: &str,
        body: &str,
    ) {
        if let Some(s) = &self.search {
            if let Err(e) = s.add(project.as_str(), artifact.as_str(), name, kind, body) {
                tracing::warn!("indexing failed: {e}");
            }
        }
    }

    /// Full-text search within a project.
    pub fn search(&self, project: &ProjectId, query: &str, limit: usize) -> Result<Vec<Hit>> {
        match &self.search {
            Some(s) => Ok(s.search(project.as_str(), query, limit).unwrap_or_default()),
            None => Ok(Vec::new()),
        }
    }

    /// The workspace root this store lives in (used to site per-project scratch workspaces
    /// for tools that write files).
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ---- blob store -------------------------------------------------------

    fn object_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join("objects").join(id.as_str())
    }

    /// Store raw bytes, returning their content address. Idempotent.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<ArtifactId> {
        let id = ArtifactId::of_bytes(bytes);
        let path = self.object_path(&id);
        // An object of another size was cut short (an OS crash after the rename reached the
        // disk and before its bytes did): write it again.
        let whole = std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !whole {
            // Write to a temp file, flush it to the disk, then rename: a concurrent reader never
            // sees a partial blob, and a record that names it never outlives its bytes.
            use std::io::Write as _;
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            // Each writer its own temp file: two threads storing the same bytes never share one.
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
            let written = std::fs::File::create(&tmp).and_then(|mut f| {
                f.write_all(bytes)?;
                f.sync_all()?;
                // Replaces an object of the wrong size too, also on Windows.
                ark_os::fs::replace(&tmp, &path)
            });
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.into());
            }
            // The rename itself is durable once the folder is.
            #[cfg(unix)]
            if let Some(dir) = path.parent() {
                std::fs::File::open(dir)?.sync_all()?;
            }
        }
        Ok(id)
    }

    pub fn has_blob(&self, id: &ArtifactId) -> bool {
        self.object_path(id).exists()
    }

    pub fn read_blob(&self, id: &ArtifactId) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.object_path(id))?)
    }

    pub fn blob_path(&self, id: &ArtifactId) -> PathBuf {
        self.object_path(id)
    }

    // ---- projects ---------------------------------------------------------

    pub fn put_project(&self, p: &Project) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO projects(id, created_at, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![p.id.as_str(), rfc(p.created_at), serde_json::to_string(p)?],
        )?;
        Ok(())
    }

    pub fn get_project(&self, id: &ProjectId) -> Result<Option<Project>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM projects WHERE id=?1",
            [id.as_str()],
            |r| r.get::<_, String>(0),
        ))
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT json FROM projects ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    /// Find a project by exact name or by id prefix. Convenience for a CLI.
    pub fn resolve_project(&self, needle: &str) -> Result<Option<Project>> {
        Ok(self
            .list_projects()?
            .into_iter()
            .find(|p| p.name == needle || p.id.as_str().starts_with(needle)))
    }

    // ---- artifacts --------------------------------------------------------

    pub fn put_artifact(&self, project: &ProjectId, a: &Artifact) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO artifacts(project_id, id, kind, name, created_at, json)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                project.as_str(),
                a.id.as_str(),
                a.kind.as_tag(),
                a.name,
                rfc(a.created_at),
                serde_json::to_string(a)?
            ],
        )?;
        Ok(())
    }

    pub fn get_artifact(&self, project: &ProjectId, id: &ArtifactId) -> Result<Option<Artifact>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM artifacts WHERE project_id=?1 AND id=?2",
            [project.as_str(), id.as_str()],
            |r| r.get::<_, String>(0),
        ))
    }

    /// Resolve an artifact by full id, short id prefix, or exact name. When a name is
    /// ambiguous the most recently ingested match wins (list is newest-first).
    pub fn resolve_artifact(&self, project: &ProjectId, needle: &str) -> Result<Option<Artifact>> {
        Ok(self.list_artifacts(project)?.into_iter().find(|a| {
            a.id.as_str() == needle || a.id.as_str().starts_with(needle) || a.name == needle
        }))
    }

    pub fn list_artifacts(&self, project: &ProjectId) -> Result<Vec<Artifact>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT json FROM artifacts WHERE project_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt.query_map([project.as_str()], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    // ---- invocations ------------------------------------------------------

    pub fn put_invocation(&self, project: &ProjectId, inv: &Invocation) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO invocations(project_id, id, action, status, created_at, json, output_len)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                project.as_str(),
                inv.id.as_str(),
                inv.action,
                serde_json::to_string(&inv.status)?.trim_matches('"'),
                rfc(inv.created_at),
                serde_json::to_string(inv)?,
                serde_json::to_string(&inv.output)?.len() as i64
            ],
        )?;
        Ok(())
    }

    pub fn list_invocations(&self, project: &ProjectId) -> Result<Vec<Invocation>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT json FROM invocations WHERE project_id=?1 ORDER BY created_at ASC")?;
        let rows = stmt.query_map([project.as_str()], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    /// The invocations made for `work` (`Invocation::work`), oldest first. Only those rows are
    /// read.
    pub fn list_invocations_of_work(
        &self,
        project: &ProjectId,
        work: &str,
    ) -> Result<Vec<Invocation>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT json FROM invocations WHERE project_id=?1 AND json_extract(json, '$.work')=?2
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![project.as_str(), work], |r| {
            r.get::<_, String>(0)
        })?;
        collect(rows)
    }

    /// Counts and sizes for a project, computed in the database (no rows are loaded).
    pub fn project_stats(&self, project: &ProjectId) -> Result<ProjectStats> {
        let db = self.db.lock().unwrap();
        let p = project.as_str();
        let (invocations, output_chars): (i64, i64) = db.query_row(
            "SELECT COUNT(*), COALESCE(SUM(output_len), 0) FROM invocations WHERE project_id=?1",
            [p],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (cards, card_chars): (i64, i64) = db.query_row(
            "SELECT COUNT(*), COALESCE(SUM(text_len), 0) FROM evidence_cards WHERE project_id=?1",
            [p],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let artifacts: i64 = db.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE project_id=?1",
            [p],
            |r| r.get(0),
        )?;
        Ok(ProjectStats {
            invocations: invocations as usize,
            output_chars: output_chars as usize,
            cards: cards as usize,
            card_chars: card_chars as usize,
            artifacts: artifacts as usize,
        })
    }

    /// Characters of input and answer in a session's first `below` turns.
    pub fn session_text_chars(&self, session: &str, below: u32) -> Result<usize> {
        let db = self.db.lock().unwrap();
        let n: i64 = db.query_row(
            "SELECT COALESCE(SUM(text_len), 0) FROM session_turns WHERE session_id=?1 AND idx < ?2",
            rusqlite::params![session, below],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Invocations still marked running, in every project.
    pub fn list_running_invocations(&self) -> Result<Vec<(ProjectId, Invocation)>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT project_id, json FROM invocations WHERE status='running'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (p, json) = row?;
            out.push((ProjectId(p), serde_json::from_str(&json)?));
        }
        Ok(out)
    }

    /// Fetch one invocation by exact id — the anchor for on-demand evidence retrieval.
    pub fn get_invocation(&self, project: &ProjectId, id: &str) -> Result<Option<Invocation>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM invocations WHERE project_id=?1 AND id=?2",
            [project.as_str(), id],
            |r| r.get::<_, String>(0),
        ))
    }

    /// Resolve an invocation by full or short id prefix (as shown in `[inv:1a2b3c]`).
    pub fn resolve_invocation(
        &self,
        project: &ProjectId,
        needle: &str,
    ) -> Result<Option<Invocation>> {
        let needle = needle.trim_start_matches("inv:");
        if let Some(inv) = self.get_invocation(project, needle)? {
            return Ok(Some(inv));
        }
        Ok(self
            .list_invocations(project)?
            .into_iter()
            .find(|i| i.id.as_str().starts_with(needle)))
    }

    // ---- notes ------------------------------------------------------------

    pub fn put_note(&self, project: &ProjectId, n: &Note) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO notes(project_id, id, kind, created_at, json)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                project.as_str(),
                n.id.as_str(),
                serde_json::to_string(&n.kind)?.trim_matches('"'),
                rfc(n.created_at),
                serde_json::to_string(n)?
            ],
        )?;
        Ok(())
    }

    pub fn list_notes(&self, project: &ProjectId) -> Result<Vec<Note>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT json FROM notes WHERE project_id=?1 ORDER BY created_at ASC")?;
        let rows = stmt.query_map([project.as_str()], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    // ---- research / working state -----------------------------------------

    /// Persist the compact working state for a project (one row, replaced in place). This
    /// *is* the resume checkpoint for the structured record — a new session loads it
    /// instead of replaying history.
    pub fn put_research_state(&self, project: &ProjectId, state: &ResearchState) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO research_state(project_id, updated_at, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![project.as_str(), rfc(OffsetDateTime::now_utc()), serde_json::to_string(state)?],
        )?;
        Ok(())
    }

    pub fn get_research_state(&self, project: &ProjectId) -> Result<Option<ResearchState>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM research_state WHERE project_id=?1",
            [project.as_str()],
            |r| r.get::<_, String>(0),
        ))
    }

    // ---- evidence cards ---------------------------------------------------

    /// Store the compact, model-facing distillation of one invocation. Idempotent on the
    /// invocation id (a re-distilled card replaces the old one).
    pub fn put_evidence_card(&self, project: &ProjectId, card: &EvidenceCard) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO evidence_cards(project_id, invocation_id, action, call_digest, created_at, json, text_len)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                project.as_str(), card.invocation.as_str(), card.action,
                card.call_digest, rfc(card.created_at), serde_json::to_string(card)?,
                (card.summary.len() + card.identifiers.iter().map(|i| i.len()).sum::<usize>()) as i64
            ],
        )?;
        Ok(())
    }

    pub fn list_evidence_cards(&self, project: &ProjectId) -> Result<Vec<EvidenceCard>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT json FROM evidence_cards WHERE project_id=?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([project.as_str()], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    /// The card of one invocation, if it was distilled.
    pub fn get_evidence_card(
        &self,
        project: &ProjectId,
        invocation: &str,
    ) -> Result<Option<EvidenceCard>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM evidence_cards WHERE project_id=?1 AND invocation_id=?2",
            [project.as_str(), invocation],
            |r| r.get::<_, String>(0),
        ))
    }

    /// Find an existing card for the same (action, input) digest — the primitive the
    /// runtime uses to avoid re-running a tool call it already made.
    pub fn find_card_by_digest(
        &self,
        project: &ProjectId,
        call_digest: &str,
    ) -> Result<Option<EvidenceCard>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM evidence_cards WHERE project_id=?1 AND call_digest=?2 ORDER BY created_at DESC LIMIT 1",
            [project.as_str(), call_digest],
            |r| r.get::<_, String>(0),
        ))
    }

    // ---- checkpoints ------------------------------------------------------

    /// Persist an opaque runtime checkpoint (metrics, plan, cursor) as JSON, one row per
    /// project. Kept generic so the store stays agnostic to the runtime's shape.
    pub fn put_checkpoint(
        &self,
        project: &ProjectId,
        checkpoint: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO checkpoints(project_id, updated_at, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![
                project.as_str(),
                rfc(OffsetDateTime::now_utc()),
                serde_json::to_string(checkpoint)?
            ],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(&self, project: &ProjectId) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM checkpoints WHERE project_id=?1",
            [project.as_str()],
            |r| r.get::<_, String>(0),
        ))
    }
}

impl Store {
    // ---- sessions -----------------------------------------------------------

    /// Persist a conversation session record (opaque JSON owned by the runtime).
    pub fn put_session(
        &self,
        id: &str,
        project: &ProjectId,
        record: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO sessions(id, project_id, updated_at, json) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![id, project.as_str(), rfc(OffsetDateTime::now_utc()), serde_json::to_string(record)?],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        one(
            db.query_row("SELECT json FROM sessions WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            }),
        )
    }

    /// Sessions, most recently updated first; all projects when `project` is `None`.
    pub fn list_sessions(&self, project: Option<&ProjectId>) -> Result<Vec<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        match project {
            Some(p) => {
                let mut stmt = db.prepare("SELECT json FROM sessions WHERE project_id=?1 ORDER BY updated_at DESC, rowid DESC")?;
                let rows = stmt.query_map([p.as_str()], |r| r.get::<_, String>(0))?;
                collect(rows)
            }
            None => {
                let mut stmt =
                    db.prepare("SELECT json FROM sessions ORDER BY updated_at DESC, rowid DESC")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                collect(rows)
            }
        }
    }

    /// Sessions with a turn stored past the turns their header counts: one begun and not yet
    /// recorded (it runs, or the process running it stopped).
    pub fn sessions_with_begun_turns(&self) -> Result<Vec<String>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT s.id FROM sessions s JOIN session_turns t
               ON t.session_id = s.id AND t.idx >= json_extract(s.json, '$.turn_count')
             ORDER BY s.id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut ids: Vec<String> = rows.collect::<std::result::Result<_, _>>()?;
        ids.dedup();
        Ok(ids)
    }

    /// Resolve a session by full id or unique prefix.
    pub fn resolve_session(&self, needle: &str) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT json FROM sessions WHERE id LIKE ?1 || '%' ORDER BY updated_at DESC LIMIT 2",
        )?;
        let rows: Vec<serde_json::Value> =
            collect(stmt.query_map([needle], |r| r.get::<_, String>(0))?)?;
        Ok(if rows.len() == 1 {
            rows.into_iter().next()
        } else {
            None
        })
    }

    /// Append (or replace) one turn of a session's transcript.
    /// Delete a session's turn traces below index `below`.
    pub fn prune_turn_traces(&self, session: &str, below: u32) -> Result<usize> {
        let db = self.db.lock().unwrap();
        Ok(db.execute(
            "DELETE FROM turn_traces WHERE session_id=?1 AND idx < ?2",
            rusqlite::params![session, below],
        )?)
    }

    pub fn put_session_turn(
        &self,
        session: &str,
        index: u32,
        turn: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO session_turns(session_id, idx, json, text_len) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![
                session,
                index,
                serde_json::to_string(turn)?,
                (turn["input"].as_str().map_or(0, str::len) + turn["answer"].as_str().map_or(0, str::len)) as i64
            ],
        )?;
        Ok(())
    }

    /// A session's turns in order. `last` limits to the most recent N.
    pub fn list_session_turns(
        &self,
        session: &str,
        last: Option<usize>,
    ) -> Result<Vec<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let limit = last.map(|n| n as i64).unwrap_or(-1);
        let mut stmt = db.prepare(
            "SELECT json FROM (SELECT idx, json FROM session_turns WHERE session_id=?1 ORDER BY idx DESC LIMIT ?2) ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session, limit], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    /// Up to `limit` turns with index below `before` (all turns when `None`), oldest first.
    pub fn list_session_turns_before(
        &self,
        session: &str,
        before: Option<u32>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let before = before.map(i64::from).unwrap_or(i64::MAX);
        let mut stmt = db.prepare(
            "SELECT json FROM (SELECT idx, json FROM session_turns WHERE session_id=?1 AND idx < ?2 ORDER BY idx DESC LIMIT ?3) ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session, before, limit as i64], |r| {
            r.get::<_, String>(0)
        })?;
        collect(rows)
    }

    // ---- turn traces ----------------------------------------------------------

    /// Store the event trace of one turn (opaque JSON owned by the front-end layer). Traces
    /// are presentation history for inspecting past turns; the runtime never reads them.
    pub fn put_turn_trace(
        &self,
        session: &str,
        index: u32,
        trace: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO turn_traces(session_id, idx, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![session, index, serde_json::to_string(trace)?],
        )?;
        Ok(())
    }

    pub fn get_turn_trace(&self, session: &str, index: u32) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM turn_traces WHERE session_id=?1 AND idx=?2",
            rusqlite::params![session, index],
            |r| r.get::<_, String>(0),
        ))
    }
}

impl Store {
    // ---- records ------------------------------------------------------------

    /// Insert or replace a typed record (opaque JSON owned by the caller). `kind` names the
    /// type and `scope` groups records for listing, usually a project id.
    pub fn put_record(
        &self,
        kind: &str,
        id: &str,
        scope: &str,
        json: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO records(kind, id, scope, updated_at, json) VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![kind, id, scope, rfc(OffsetDateTime::now_utc()), serde_json::to_string(json)?],
        )?;
        Ok(())
    }

    /// Insert or replace several caller-owned records in one transaction.
    pub fn put_records(&self, records: &[(&str, &str, &str, &serde_json::Value)]) -> Result<()> {
        let encoded = records
            .iter()
            .map(|(kind, id, scope, value)| Ok((*kind, *id, *scope, serde_json::to_string(value)?)))
            .collect::<Result<Vec<_>>>()?;
        let mut db = self.db.lock().unwrap();
        let transaction = db.transaction()?;
        let updated_at = rfc(OffsetDateTime::now_utc());
        for (kind, id, scope, json) in encoded {
            transaction.execute(
                "INSERT OR REPLACE INTO records(kind, id, scope, updated_at, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![kind, id, scope, updated_at, json],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_record(&self, kind: &str, id: &str) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
            |r| r.get::<_, String>(0),
        ))
    }

    /// Records of one kind, oldest update first; every scope when `scope` is `None`.
    pub fn list_records(&self, kind: &str, scope: Option<&str>) -> Result<Vec<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        match scope {
            Some(sc) => {
                let mut stmt = db.prepare(
                    "SELECT json FROM records WHERE kind=?1 AND scope=?2 ORDER BY updated_at, rowid",
                )?;
                let rows =
                    stmt.query_map(rusqlite::params![kind, sc], |r| r.get::<_, String>(0))?;
                collect(rows)
            }
            None => {
                let mut stmt = db
                    .prepare("SELECT json FROM records WHERE kind=?1 ORDER BY updated_at, rowid")?;
                let rows = stmt.query_map([kind], |r| r.get::<_, String>(0))?;
                collect(rows)
            }
        }
    }

    /// Remove a record. Returns whether one existed.
    pub fn delete_record(&self, kind: &str, id: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        Ok(db.execute(
            "DELETE FROM records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
        )? > 0)
    }

    // ---- event streams --------------------------------------------------------

    /// Append an event to `stream`. Returns its sequence number, which increases across all
    /// streams, so a reader can resume after the last one it saw.
    pub fn append_event(&self, stream: &str, json: &serde_json::Value) -> Result<u64> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO events(stream, at, json) VALUES(?1, ?2, ?3)",
            rusqlite::params![
                stream,
                rfc(OffsetDateTime::now_utc()),
                serde_json::to_string(json)?
            ],
        )?;
        Ok(db.last_insert_rowid() as u64)
    }

    /// The sequence number of the newest event in `stream` (0 when it has none).
    pub fn last_event(&self, stream: &str) -> Result<u64> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE stream=?1",
            [stream],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Up to `limit` events of `stream` with a sequence number above `after`, oldest first.
    pub fn list_events(
        &self,
        stream: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, serde_json::Value)>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT seq, json FROM events WHERE stream=?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![stream, after as i64, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (seq, json) = r?;
            out.push((seq, serde_json::from_str(&json)?));
        }
        Ok(out)
    }
}

fn rfc(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn one<T: for<'de> serde::Deserialize<'de>>(r: rusqlite::Result<String>) -> Result<Option<T>> {
    match r {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn collect<T: for<'de> serde::Deserialize<'de>>(
    rows: impl Iterator<Item = rusqlite::Result<String>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for r in rows {
        out.push(serde_json::from_str(&r?)?);
    }
    Ok(out)
}

/// Bring an older store up to the current schema. Each step is idempotent.
fn migrate(db: &Connection) -> Result<()> {
    // Sizes kept next to each row, so the counts shown after every turn are a sum over an
    // integer column instead of a parse of every stored row. Rows written before this get
    // their size once, here.
    add_column(
        db,
        "invocations",
        "output_len",
        "UPDATE invocations SET output_len = COALESCE(LENGTH(json_extract(json, '$.output')), 0)",
    )?;
    add_column(
        db,
        "evidence_cards",
        "text_len",
        "UPDATE evidence_cards SET text_len = COALESCE(LENGTH(json_extract(json, '$.summary')), 0)
             + COALESCE(LENGTH(json_extract(json, '$.identifiers')), 0)",
    )?;
    add_column(
        db,
        "session_turns",
        "text_len",
        "UPDATE session_turns SET text_len = COALESCE(LENGTH(json_extract(json, '$.input')), 0)
             + COALESCE(LENGTH(json_extract(json, '$.answer')), 0)",
    )?;
    Ok(())
}

fn add_column(db: &Connection, table: &str, column: &str, backfill: &str) -> Result<()> {
    let exists = db
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1"
        ))?
        .exists([column])?;
    if !exists {
        db.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0; {backfill};"
        ))?;
    }
    Ok(())
}

/// Remove temp files a crash left in `objects` (an object written but never renamed). Only
/// old ones: another process on the store may be writing one now.
fn sweep_temp_blobs(objects: &Path) {
    let Ok(entries) = std::fs::read_dir(objects) else {
        return;
    };
    let hour = std::time::Duration::from_secs(3600);
    for e in entries.flatten() {
        let temp = e
            .path()
            .extension()
            .is_some_and(|x| x.to_string_lossy().starts_with("tmp"));
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > hour);
        if temp && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_older_store_gains_size_columns_with_its_old_rows_counted() {
        let dir = tempfile::tempdir().unwrap();
        // A store as an earlier version wrote it: no size columns.
        {
            let db = Connection::open(dir.path().join("meta.db")).unwrap();
            db.execute_batch(
                "CREATE TABLE invocations(project_id TEXT NOT NULL, id TEXT NOT NULL,
                     action TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL,
                     json TEXT NOT NULL, PRIMARY KEY(project_id, id));
                 CREATE TABLE evidence_cards(project_id TEXT NOT NULL, invocation_id TEXT NOT NULL,
                     action TEXT NOT NULL, call_digest TEXT NOT NULL, created_at TEXT NOT NULL,
                     json TEXT NOT NULL, PRIMARY KEY(project_id, invocation_id));
                 CREATE TABLE session_turns(session_id TEXT NOT NULL, idx INTEGER NOT NULL,
                     json TEXT NOT NULL, PRIMARY KEY(session_id, idx));
                 INSERT INTO invocations VALUES('p', 'i1', 'a.b', 'ok', '2026-01-01T00:00:00Z',
                     '{\"output\": {\"k\": \"abcdef\"}}');
                 INSERT INTO session_turns VALUES('s', 0, '{\"input\": \"hello\", \"answer\": \"world!\"}');",
            )
            .unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        let stats = store.project_stats(&ProjectId("p".into())).unwrap();
        assert_eq!(stats.invocations, 1);
        assert_eq!(stats.output_chars, r#"{"k":"abcdef"}"#.len());
        assert_eq!(store.session_text_chars("s", 1).unwrap(), 11);
        // Opening again is a no-op.
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store
                .project_stats(&ProjectId("p".into()))
                .unwrap()
                .invocations,
            1
        );
    }

    use super::*;
    use ark_core::{ArtifactKind, Invocation, NoteKind, Provenance, RiskTier};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        (dir, s)
    }

    #[test]
    fn sessions_and_turns_roundtrip() {
        let (_d, s) = store();
        let p = Project::new("p", "");
        s.put_session("abc123", &p.id, &serde_json::json!({ "id": "abc123" }))
            .unwrap();
        s.put_session("abd999", &p.id, &serde_json::json!({ "id": "abd999" }))
            .unwrap();
        assert_eq!(s.list_sessions(Some(&p.id)).unwrap().len(), 2);
        assert!(
            s.resolve_session("ab").unwrap().is_none(),
            "ambiguous prefix"
        );
        assert_eq!(s.resolve_session("abc").unwrap().unwrap()["id"], "abc123");
        for i in 0..5 {
            s.put_session_turn("abc123", i, &serde_json::json!({ "index": i }))
                .unwrap();
        }
        let last2 = s.list_session_turns("abc123", Some(2)).unwrap();
        assert_eq!(
            last2
                .iter()
                .map(|t| t["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(s.list_session_turns("abc123", None).unwrap().len(), 5);
    }

    #[test]
    fn records_and_event_streams() {
        let (_d, s) = store();
        s.put_record("task", "t1", "p", &serde_json::json!({ "n": 1 }))
            .unwrap();
        s.put_record("task", "t2", "q", &serde_json::json!({ "n": 2 }))
            .unwrap();
        s.put_record("task", "t1", "p", &serde_json::json!({ "n": 3 }))
            .unwrap();
        assert_eq!(s.get_record("task", "t1").unwrap().unwrap()["n"], 3);
        assert!(s.get_record("agent", "t1").unwrap().is_none());
        assert_eq!(s.list_records("task", Some("p")).unwrap().len(), 1);
        assert_eq!(s.list_records("task", None).unwrap().len(), 2);
        assert!(s.delete_record("task", "t2").unwrap());
        assert!(!s.delete_record("task", "t2").unwrap());

        let a = s
            .append_event("run", &serde_json::json!({ "e": "a" }))
            .unwrap();
        s.append_event("other", &serde_json::json!({ "e": "x" }))
            .unwrap();
        let b = s
            .append_event("run", &serde_json::json!({ "e": "b" }))
            .unwrap();
        assert!(b > a);
        let all = s.list_events("run", 0, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(s.last_event("run").unwrap(), b);
        assert_eq!(s.last_event("none").unwrap(), 0);
        let after = s.list_events("run", a, 10).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].1["e"], "b");
    }

    #[test]
    fn a_blob_cut_short_by_a_crash_is_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let bytes = b"the whole chapter".to_vec();
        let id = s.put_blob(&bytes).unwrap();
        // An OS crash after the rename reached the disk and before the bytes did.
        std::fs::write(s.object_path(&id), b"").unwrap();
        assert_eq!(s.put_blob(&bytes).unwrap(), id);
        assert_eq!(s.read_blob(&id).unwrap(), bytes);
        // No temp files are left behind.
        let left: Vec<_> = std::fs::read_dir(dir.path().join("objects"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(left.is_empty());
    }

    #[test]
    fn blobs_are_deduplicated() {
        let (_d, s) = store();
        let a = s.put_blob(b"same bytes").unwrap();
        let b = s.put_blob(b"same bytes").unwrap();
        assert_eq!(a, b);
        assert!(s.has_blob(&a));
        assert_eq!(s.read_blob(&a).unwrap(), b"same bytes");
    }

    #[test]
    fn project_roundtrip_and_resolve() {
        let (_d, s) = store();
        let p = Project::new("acme-audit", "acme.example.com");
        s.put_project(&p).unwrap();
        assert_eq!(s.get_project(&p.id).unwrap().unwrap().name, "acme-audit");
        assert_eq!(s.resolve_project("acme-audit").unwrap().unwrap().id, p.id);
        assert_eq!(s.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn artifact_and_invocation_persist() {
        let (_d, s) = store();
        let p = Project::new("t", "");
        s.put_project(&p).unwrap();

        let bytes = b"the quick brown fox";
        let id = s.put_blob(bytes).unwrap();
        let art = Artifact::new(
            id.clone(),
            ArtifactKind::Text,
            "notes.txt",
            bytes.len() as u64,
            Provenance::Ingested {
                path: "/x/notes.txt".into(),
            },
        );
        s.put_artifact(&p.id, &art).unwrap();
        assert_eq!(
            s.resolve_artifact(&p.id, id.short()).unwrap().unwrap().name,
            "notes.txt"
        );

        let mut inv = Invocation::new("fs.read", RiskTier::Read, serde_json::json!({}));
        inv.consumed.push(id.clone());
        inv.mark_ok(serde_json::json!({"lines": 1}));
        s.put_invocation(&p.id, &inv).unwrap();
        let list = s.list_invocations(&p.id).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].output["lines"], 1);

        let note = Note::new(
            NoteKind::Finding,
            "one line of text",
            ark_core::Author::Agent,
        );
        s.put_note(&p.id, &note).unwrap();
        assert_eq!(s.list_notes(&p.id).unwrap().len(), 1);
    }

    #[test]
    fn research_state_and_cards_persist_and_dedup() {
        use ark_core::{Claim, ClaimType, EvidenceCard, InvocationId, ResearchState};
        let (_d, s) = store();
        let p = Project::new("t", "");
        s.put_project(&p).unwrap();

        // working state round-trips and replaces in place
        assert!(s.get_research_state(&p.id).unwrap().is_none());
        let mut st = ResearchState::new("find the root cause");
        st.findings
            .push(Claim::new("cache evicted early", ClaimType::Fact));
        s.put_research_state(&p.id, &st).unwrap();
        st.questions.push("which key first?".into());
        s.put_research_state(&p.id, &st).unwrap();
        let back = s.get_research_state(&p.id).unwrap().unwrap();
        assert_eq!(back.goal, "find the root cause");
        assert_eq!(back.findings.len(), 1);
        assert_eq!(back.questions.len(), 1);

        // evidence cards persist and are findable by call digest (dedup primitive)
        let card = EvidenceCard::new(InvocationId::new(), "search.query", true, "412 hits")
            .with_call_digest("search.query|{\"q\":\"x\"}");
        s.put_evidence_card(&p.id, &card).unwrap();
        assert_eq!(s.list_evidence_cards(&p.id).unwrap().len(), 1);
        let hit = s
            .find_card_by_digest(&p.id, "search.query|{\"q\":\"x\"}")
            .unwrap();
        assert!(hit.is_some());
        assert!(s.find_card_by_digest(&p.id, "nope").unwrap().is_none());

        // checkpoint round-trips as opaque json
        s.put_checkpoint(&p.id, &serde_json::json!({"steps": 3, "tokens": 1200}))
            .unwrap();
        assert_eq!(s.get_checkpoint(&p.id).unwrap().unwrap()["steps"], 3);
    }

    #[test]
    fn search_finds_indexed_body_scoped_to_project() {
        let (_d, s) = store();
        let p1 = Project::new("p1", "");
        let p2 = Project::new("p2", "");
        s.put_project(&p1).unwrap();
        s.put_project(&p2).unwrap();

        let id = ArtifactId::of_bytes(b"doc");
        s.index_text(
            &p1.id,
            &id,
            "notes.txt",
            "text",
            "the cache layer evicted entries early under load",
        );
        let other = ArtifactId::of_bytes(b"other");
        s.index_text(
            &p2.id,
            &other,
            "notes.txt",
            "text",
            "the cache in another project",
        );

        let hits = s.search(&p1.id, "evicted", 10).unwrap();
        assert_eq!(hits.len(), 1, "match only within p1");
        assert_eq!(hits[0].name, "notes.txt");

        // project scoping: p2's doc does not leak into p1 results
        let cross = s.search(&p1.id, "another", 10).unwrap();
        assert!(cross.is_empty());
    }
}
