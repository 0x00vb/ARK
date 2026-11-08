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
mod memory;
mod records;
mod scrub;
mod writer;
pub use error::StoreError;
pub use index::Hit;
pub use memory::MemoryReview;
pub use records::RecordTransaction;
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

/// A running invocation and the exact stored value observed by recovery.
pub struct ObservedInvocation {
    project: ProjectId,
    invocation: Invocation,
    raw_json: String,
}

impl ObservedInvocation {
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }
}

/// A scoped record replacement and its expected previous value.
pub struct RecordUpdate<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub scope: &'a str,
    pub expected: Option<&'a serde_json::Value>,
    pub value: &'a serde_json::Value,
}

/// A scoped record comparison that does not modify the record.
pub struct RecordCheck<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub scope: &'a str,
    pub expected: Option<&'a serde_json::Value>,
}

/// Require that no ordinary record of a kind exists in a scope.
pub struct EmptyRecordScope<'a> {
    pub kind: &'a str,
    pub scope: &'a str,
}

pub struct RecordCompareLimits {
    pub max_bytes: usize,
    pub max_total_bytes: usize,
    pub max_records: usize,
}

/// A derived scoped record maintained in the same transaction as guarded updates.
pub struct DerivedRecord<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub scope: &'a str,
    pub value: &'a serde_json::Value,
}

/// An immutable scoped byte record inserted only when its identity is unused.
pub struct ImmutableRecord<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub scope: &'a str,
    pub bytes: &'a [u8],
}

pub struct ImmutableEpoch<'a> {
    pub scope: &'a str,
    pub value: u64,
}

/// A scoped ordinary-record clock with the kinds that must remain watched.
pub struct RecordMutationEpoch<'a> {
    pub scope: &'a str,
    pub kinds: &'a [&'a str],
    pub value: u64,
}

pub struct RecordCompareGuards<'a> {
    pub immutable_epoch: Option<ImmutableEpoch<'a>>,
    pub record_epoch: RecordMutationEpoch<'a>,
}

/// Verified inputs for one invocation's atomic output publication.
pub struct PreparedOutputPublication<'a> {
    store: &'a Store,
    project: ProjectId,
    running_json: String,
    finished: Invocation,
    finished_json: String,
    output_len: usize,
    outputs: Vec<(Artifact, String)>,
    blobs: Vec<PreparedBlob>,
}

struct PreparedBlob {
    file: std::fs::File,
    path: PathBuf,
    identity: BlobIdentity,
}

#[cfg(unix)]
#[derive(PartialEq, Eq)]
struct BlobIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[cfg(not(unix))]
struct BlobIdentity;

#[cfg(unix)]
fn blob_identity(metadata: &std::fs::Metadata) -> Result<BlobIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(StoreError::ImmutableConflict);
    }
    Ok(BlobIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

impl PreparedBlob {
    #[cfg(unix)]
    fn verify(&self) -> Result<()> {
        if blob_identity(&self.file.metadata()?)? != self.identity
            || blob_identity(&std::fs::symlink_metadata(&self.path)?)? != self.identity
        {
            return Err(StoreError::ImmutableConflict);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify(&self) -> Result<()> {
        Err(std::io::Error::other("output publication requires protected blob handles").into())
    }
}

pub struct OutputPublicationCommit<'a> {
    pub limits: RecordCompareLimits,
    pub guards: RecordCompareGuards<'a>,
    pub publication: &'a PreparedOutputPublication<'a>,
    pub receipt_keys: &'a [(&'a str, &'a str)],
}

/// The recorded result of a completed publication, including its receipt values.
pub struct CommittedOutputPublication {
    pub invocation: Invocation,
    pub outputs: Vec<Artifact>,
    /// Absent when the child no longer has access to the original owner scope.
    pub record_scope: Option<String>,
    /// Owner-scoped receipts are withheld after that access is lost.
    pub receipts: Option<Vec<(String, String, serde_json::Value)>>,
}

type PublicationReceiptRows = Vec<(String, String, String)>;
type PublicationManifest = (String, String, Vec<String>, PublicationReceiptRows);

fn publication_scope_matches(db: &Connection, project: &str, scope: &str) -> Result<bool> {
    use rusqlite::OptionalExtension as _;
    if project == scope {
        return Ok(true);
    }
    let mut current = project.to_owned();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..8 {
        if !seen.insert(current.clone()) {
            return Ok(false);
        }
        let json: Option<Option<String>> = db.query_row(
            "SELECT CASE WHEN octet_length(json)<=1048576 THEN json ELSE NULL END FROM projects WHERE id=?1",
            [current.as_str()], |row| row.get(0),
        ).optional()?;
        let Some(json) = json else {
            return Ok(false);
        };
        let row: Project = serde_json::from_str(&json.ok_or(StoreError::RecordTooLarge)?)?;
        if row.id.as_str() != current {
            return Err(StoreError::ImmutableConflict);
        }
        match row.parent {
            Some(parent) => current = parent.as_str().to_owned(),
            None => return Ok(current == scope),
        }
    }
    Ok(false)
}

fn bounded_json<T: serde::Serialize>(value: &T, limit: usize) -> Result<String> {
    struct BoundedWriter {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("publication exceeds its size limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| StoreError::RecordTooLarge)?;
    String::from_utf8(writer.bytes).map_err(|error| {
        StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
}

struct ComparePolicy<'a> {
    max_bytes: usize,
    max_total_bytes: usize,
    max_records: usize,
    count_identity_bytes: bool,
    epoch: Option<ImmutableEpoch<'a>>,
    record_epoch: Option<RecordMutationEpoch<'a>>,
    publication: Option<&'a PreparedOutputPublication<'a>>,
    publication_receipts: Option<&'a [(&'a str, &'a str)]>,
    publication_scope: Option<&'a str>,
}

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
    #[cfg(unix)]
    location: Option<StoreLocation>,
    /// Full-text index. `None` if it failed to open (search then unavailable).
    search: Option<index::SearchIndex>,
    _writer: std::sync::Arc<writer::WriterLock>,
}

#[cfg(unix)]
struct StoreLocation {
    root: std::fs::File,
    database_identity: (u64, u64),
}

#[cfg(unix)]
impl StoreLocation {
    fn database_identity(root: &std::fs::File) -> Result<(u64, u64)> {
        use std::os::fd::AsRawFd;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        let status = unsafe {
            libc::fstatat(
                root.as_raw_fd(),
                c"meta.db".as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG || metadata.st_nlink != 1 {
            return Err(std::io::Error::other("invalid database location").into());
        }
        #[cfg(target_os = "linux")]
        let device = metadata.st_dev;
        #[cfg(not(target_os = "linux"))]
        let device = metadata.st_dev as u64;
        Ok((device, metadata.st_ino))
    }

    fn verify(&self, root: &std::path::Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let old_root = self.root.metadata()?;
        let current_root = std::fs::metadata(root)?;
        if !old_root.is_dir()
            || old_root.dev() != current_root.dev()
            || old_root.ino() != current_root.ino()
            || Self::database_identity(&self.root)? != self.database_identity
        {
            return Err(std::io::Error::other("store location changed").into());
        }
        Ok(())
    }

    fn verify_connection(db: &Connection) -> Result<()> {
        let mut moved: std::ffi::c_int = 1;
        let status = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                db.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut std::ffi::c_int).cast(),
            )
        };
        if status != rusqlite::ffi::SQLITE_OK || moved != 0 {
            return Err(std::io::Error::other("database location cannot be verified").into());
        }
        Ok(())
    }
}

impl Store {
    /// Open (creating if needed) a workspace store rooted at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), false)
    }

    /// Open an existing store for guarded record access without touching blob or search files.
    #[cfg(unix)]
    pub fn open_guarded(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), true)
    }

    fn open_inner(root: &Path, guarded: bool) -> Result<Self> {
        let root = root.to_path_buf();
        if !guarded {
            std::fs::create_dir_all(root.join("objects"))?;
        }
        #[cfg(unix)]
        let root = if guarded { root.canonicalize()? } else { root };
        let writer = writer::acquire(&root)?;
        if !guarded {
            sweep_temp_blobs(&root.join("objects"));
        }
        #[cfg(unix)]
        let location = if guarded {
            let root_file = std::fs::File::open(&root)?;
            let database_identity = StoreLocation::database_identity(&root_file)?;
            Some(StoreLocation {
                root: root_file,
                database_identity,
            })
        } else {
            None
        };
        #[cfg(unix)]
        let db = if guarded {
            Connection::open_with_flags(
                root.join("meta.db"),
                (rusqlite::OpenFlags::default() - rusqlite::OpenFlags::SQLITE_OPEN_CREATE)
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?
        } else {
            Connection::open(root.join("meta.db"))?
        };
        #[cfg(not(unix))]
        let db = {
            let _ = guarded;
            Connection::open(root.join("meta.db"))?
        };
        #[cfg(unix)]
        if let Some(location) = &location {
            location.verify(&root)?;
            StoreLocation::verify_connection(&db)?;
        }
        if guarded {
            let journal: String = db.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
            if journal != "wal" || synchronous < 2 {
                return Err(std::io::Error::other(
                    "guarded records require WAL and full synchronization",
                )
                .into());
            }
        }
        if !guarded {
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
             CREATE TABLE IF NOT EXISTS invocation_outputs(
                 project_id TEXT NOT NULL, invocation_id TEXT NOT NULL,
                 ordinal INTEGER NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(project_id, invocation_id, ordinal));
             CREATE TABLE IF NOT EXISTS invocation_publications(
                 project_id TEXT NOT NULL, invocation_id TEXT NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(project_id, invocation_id));
             CREATE TABLE IF NOT EXISTS notes(
                 project_id TEXT NOT NULL, id TEXT NOT NULL,
                 kind TEXT NOT NULL, created_at TEXT NOT NULL,
                 json TEXT NOT NULL, PRIMARY KEY(project_id, id));
             CREATE TABLE IF NOT EXISTS research_state(
                 project_id TEXT NOT NULL, session_id TEXT NOT NULL DEFAULT '',
                 updated_at TEXT NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(project_id, session_id));
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
             CREATE TABLE IF NOT EXISTS quota_windows(
                 quota_key TEXT NOT NULL, window_start INTEGER NOT NULL, used INTEGER NOT NULL,
                 PRIMARY KEY(quota_key, window_start));
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
             CREATE INDEX IF NOT EXISTS records_by_scope_id ON records(kind, scope, id);
             CREATE INDEX IF NOT EXISTS events_by_stream ON events(stream, seq);",
            )?;
            migrate_research_state(&db)?;
            migrate_immutable(&db)?;
            migrate_record_watches(&db)?;
            migrate(&db)?;
        }
        #[cfg(unix)]
        if let Some(location) = &location {
            location.verify(&root)?;
            StoreLocation::verify_connection(&db)?;
        }
        let search = if guarded {
            None
        } else {
            match index::SearchIndex::open(&root.join("index")) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("search index unavailable: {e}");
                    None
                }
            }
        };
        Ok(Store {
            root,
            db: Mutex::new(db),
            #[cfg(unix)]
            location,
            search,
            _writer: writer,
        })
    }

    /// Refuse use if the directory or database path no longer names the opened store.
    #[cfg(unix)]
    pub fn verify_location(&self) -> Result<()> {
        self.location
            .as_ref()
            .ok_or_else(|| std::io::Error::other("store has no location guard"))?
            .verify(&self.root)?;
        StoreLocation::verify_connection(&self.db.lock().unwrap())
    }

    /// Check that SQLite still addresses the database path it opened.
    #[cfg(unix)]
    pub fn verify_connection_location(&self) -> Result<()> {
        StoreLocation::verify_connection(&self.db.lock().unwrap())
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

    /// Hold while changing project locations or publishing files into a project workspace.
    #[cfg(unix)]
    pub fn try_workspace_fence(&self) -> Result<std::fs::File> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(self.root.join("workspace.fence"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(std::io::Error::other("workspace fence is not private").into());
        }
        match file.try_lock() {
            Ok(()) => {
                self.verify_workspace_fence(&file)?;
                Ok(file)
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::ProjectBusy),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Check that a held workspace fence has not been unlinked or replaced.
    #[cfg(unix)]
    pub fn verify_workspace_fence(&self, fence: &std::fs::File) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = fence.metadata()?;
        let path = std::fs::symlink_metadata(self.root.join("workspace.fence"))?;
        if !path.is_file()
            || held.nlink() != 1
            || path.nlink() != 1
            || held.dev() != path.dev()
            || held.ino() != path.ino()
            || path.uid() != unsafe { libc::geteuid() }
            || path.mode() & 0o077 != 0
        {
            return Err(std::io::Error::other("workspace fence changed").into());
        }
        Ok(())
    }

    // ---- blob store -------------------------------------------------------

    fn object_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join("objects").join(id.as_str())
    }

    fn blob_matches(path: &Path, bytes: &[u8]) -> Result<bool> {
        match (|| -> std::io::Result<bool> {
            use std::io::Read as _;
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            options.write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let mut file = options.open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "blob is not a regular file",
                ));
            }
            if metadata.len() != bytes.len() as u64 {
                return Ok(false);
            }
            let mut buffer = [0u8; 8192];
            for expected in bytes.chunks(buffer.len()) {
                file.read_exact(&mut buffer[..expected.len()])?;
                if &buffer[..expected.len()] != expected {
                    return Ok(false);
                }
            }
            if file.read(&mut buffer[..1])? != 0 {
                return Ok(false);
            }
            file.sync_all()?;
            Ok(true)
        })() {
            Ok(whole) => Ok(whole),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Store raw bytes, returning their content address. Idempotent.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<ArtifactId> {
        let id = ArtifactId::of_bytes(bytes);
        let path = self.object_path(&id);
        let whole = Self::blob_matches(&path, bytes)?;
        if !whole {
            // Write to a temp file, flush it to the disk, then rename: a concurrent reader never
            // sees a partial blob, and a record that names it never outlives its bytes.
            use std::io::Write as _;
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            // Each writer its own temp file: two threads storing the same bytes never share one.
            let mut created = None;
            for _ in 0..128 {
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)
                {
                    Ok(file) => {
                        created = Some((tmp, file));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            let (tmp, mut file) = created
                .ok_or_else(|| std::io::Error::other("cannot allocate blob temporary file"))?;
            let written = (|| {
                file.write_all(bytes)?;
                file.sync_all()?;
                drop(file);
                ark_os::fs::replace(&tmp, &path)
            })();
            if let Err(e) = written {
                if let Err(cleanup) = std::fs::remove_file(&tmp) {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("blob write failed: {e}; temporary file cleanup failed: {cleanup}"),
                    )
                    .into());
                }
                return Err(e.into());
            }
        }
        // Another writer may have renamed the same object but not synced the folder yet.
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(id)
    }

    pub fn has_blob(&self, id: &ArtifactId) -> bool {
        self.object_path(id).exists()
    }

    pub fn read_blob(&self, id: &ArtifactId) -> Result<Vec<u8>> {
        self.read_blob_bounded(id, usize::MAX)
    }

    /// Read and verify an object while refusing data larger than the caller's limit.
    pub fn read_blob_bounded(&self, id: &ArtifactId, limit: usize) -> Result<Vec<u8>> {
        use std::io::Read as _;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(self.object_path(id))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "blob is not a regular file",
            )
            .into());
        }
        if metadata.len() > limit as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "blob exceeds the size limit",
            )
            .into());
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "blob exceeds the size limit",
            )
            .into());
        }
        if &ArtifactId::of_bytes(&bytes) != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "blob does not match its content address",
            )
            .into());
        }
        Ok(bytes)
    }

    pub fn blob_path(&self, id: &ArtifactId) -> PathBuf {
        self.object_path(id)
    }

    // ---- projects ---------------------------------------------------------

    pub fn put_project(&self, p: &Project) -> Result<()> {
        self.put_project_after_read(p, || {})
    }

    fn put_project_after_read(&self, p: &Project, after_read: impl FnOnce()) -> Result<()> {
        #[cfg(unix)]
        let location_changed = self
            .get_project(&p.id)?
            .is_some_and(|old| old.workspace != p.workspace || old.parent != p.parent);
        #[cfg(unix)]
        let _fence = if location_changed {
            Some(self.try_workspace_fence()?)
        } else {
            None
        };
        #[cfg(not(unix))]
        let location_changed = true;
        after_read();
        let db = self.db.lock().unwrap();
        let changed = db.execute(
            "INSERT OR REPLACE INTO projects(id, created_at, json)
             SELECT ?1, ?2, ?3 WHERE (?4 IS NOT NULL OR EXISTS (
               SELECT 1 FROM projects WHERE id = ?1 AND json_extract(json, '$.name') = ?5
               AND json_extract(json, '$.parent') IS ?4) OR NOT EXISTS (
               SELECT 1 FROM projects WHERE id != ?1
               AND json_extract(json, '$.parent') IS NULL
               AND json_extract(json, '$.name') = ?5)) AND (?6 OR NOT EXISTS (
               SELECT 1 FROM projects WHERE id = ?1 AND (json_extract(json, '$.workspace') IS NOT ?7
               OR json_extract(json, '$.parent') IS NOT ?4)))",
            rusqlite::params![
                p.id.as_str(),
                rfc(p.created_at),
                serde_json::to_string(p)?,
                p.parent.as_ref().map(|id| id.as_str()),
                p.name,
                location_changed,
                p.workspace.as_deref()
            ],
        )?;
        if changed == 0 {
            let current: Option<Project> = one(db.query_row(
                "SELECT json FROM projects WHERE id=?1",
                [p.id.as_str()],
                |row| row.get::<_, String>(0),
            ))?;
            if !location_changed
                && current.is_some_and(|old| old.workspace != p.workspace || old.parent != p.parent)
            {
                return Err(StoreError::ProjectLocationChanged);
            }
            return Err(StoreError::ProjectNameConflict);
        }
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

    #[cfg(unix)]
    fn prepare_publication_blob(&self, id: &ArtifactId, size: usize) -> Result<PreparedBlob> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.object_path(id);
        let initial = blob_identity(&std::fs::symlink_metadata(&path)?)?;
        if initial.size != size as u64 {
            return Err(StoreError::ImmutableConflict);
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let identity = blob_identity(&file.metadata()?)?;
        if identity.size != size as u64 {
            return Err(StoreError::ImmutableConflict);
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take((size as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() != size
            || ArtifactId::of_bytes(&bytes) != *id
            || blob_identity(&file.metadata()?)? != identity
            || blob_identity(&std::fs::symlink_metadata(&path)?)? != identity
        {
            return Err(StoreError::ImmutableConflict);
        }
        file.sync_all()?;
        std::fs::File::open(path.parent().ok_or(StoreError::ImmutableConflict)?)?.sync_all()?;
        Ok(PreparedBlob {
            file,
            path,
            identity,
        })
    }

    #[cfg(not(unix))]
    fn prepare_publication_blob(&self, _id: &ArtifactId, _size: usize) -> Result<PreparedBlob> {
        Err(std::io::Error::other("output publication requires protected blob handles").into())
    }

    /// Verify staged blobs before entering a cancellation-sensitive commit fence.
    pub fn prepare_output_publication<'a>(
        &'a self,
        project: &ProjectId,
        running: &Invocation,
        finished: &Invocation,
        outputs: &[Artifact],
    ) -> Result<PreparedOutputPublication<'a>> {
        const MAX_RECORD: usize = 128 * 1024;
        const MAX_METADATA: usize = 1024 * 1024;
        const MAX_OUTPUTS: usize = 64;
        const MAX_BLOB: usize = 64 * 1024 * 1024;
        const MAX_BLOBS: usize = 256 * 1024 * 1024;
        if project.as_str().is_empty()
            || project.as_str().len() > 128
            || running.id.as_str().is_empty()
            || running.id.as_str().len() > 96
            || running.status != ark_core::InvocationStatus::Running
            || finished.status != ark_core::InvocationStatus::Ok
            || finished.finished_at.is_none()
            || finished.error.is_some()
            || outputs.is_empty()
            || outputs.len() > MAX_OUTPUTS
            || finished.produced.len() != outputs.len()
        {
            return Err(StoreError::ImmutableConflict);
        }
        let running_json = bounded_json(running, MAX_RECORD)?;
        let finished_json = bounded_json(finished, MAX_RECORD)?;
        let output_len = bounded_json(&finished.output, MAX_RECORD)?.len();
        let mut normalized = finished.clone();
        normalized.status = running.status;
        normalized.output = running.output.clone();
        normalized.command = running.command.clone();
        normalized.produced = running.produced.clone();
        normalized.effect = running.effect.clone();
        normalized.error = running.error.clone();
        normalized.finished_at = running.finished_at;
        if bounded_json(&normalized, MAX_RECORD)? != running_json {
            return Err(StoreError::ImmutableConflict);
        }
        let mut total = 0usize;
        let mut metadata = running_json.len().saturating_add(finished_json.len());
        let mut encoded = Vec::with_capacity(outputs.len());
        let mut blobs = Vec::with_capacity(outputs.len());
        for (ordinal, artifact) in outputs.iter().enumerate() {
            if artifact.id != finished.produced[ordinal]
                || artifact.id.as_str().len() != 64
                || !artifact
                    .id
                    .as_str()
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || artifact.name.is_empty()
                || artifact.name.len() > 512
                || !matches!(&artifact.provenance, ark_core::Provenance::Produced { invocation } if invocation == &running.id)
            {
                return Err(StoreError::ImmutableConflict);
            }
            let size = usize::try_from(artifact.size).map_err(|_| StoreError::RecordTooLarge)?;
            total = total
                .checked_add(size)
                .filter(|n| *n <= MAX_BLOBS)
                .ok_or(StoreError::RecordTooLarge)?;
            if size > MAX_BLOB {
                return Err(StoreError::RecordTooLarge);
            }
            blobs.push(self.prepare_publication_blob(&artifact.id, size)?);
            let json = bounded_json(artifact, MAX_RECORD)?;
            metadata = metadata
                .checked_add(json.len())
                .filter(|n| *n <= MAX_METADATA)
                .ok_or(StoreError::RecordTooLarge)?;
            encoded.push((artifact.clone(), json));
        }
        Ok(PreparedOutputPublication {
            store: self,
            project: project.clone(),
            running_json,
            finished: finished.clone(),
            finished_json,
            output_len,
            outputs: encoded,
            blobs,
        })
    }

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
        use rusqlite::OptionalExtension;
        let db = self.db.lock().unwrap();
        let value: Option<String> = db.query_row("SELECT json FROM artifacts WHERE project_id=?1 AND (id=?2 OR substr(id,1,length(?2))=?2 OR json_extract(json,'$.name')=?2) ORDER BY created_at DESC LIMIT 1", rusqlite::params![project.as_str(), needle], |row| row.get(0)).optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn list_artifacts(&self, project: &ProjectId) -> Result<Vec<Artifact>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT json FROM artifacts WHERE project_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt.query_map([project.as_str()], |r| r.get::<_, String>(0))?;
        collect(rows)
    }

    // ---- invocations ------------------------------------------------------

    pub fn create_invocation(&self, project: &ProjectId, invocation: &Invocation) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("INSERT INTO invocations(project_id,id,action,status,created_at,json,output_len) VALUES(?1,?2,?3,?4,?5,?6,?7)", rusqlite::params![project.as_str(),invocation.id.as_str(),&invocation.action,serde_json::to_string(&invocation.status)?.trim_matches('"'),rfc(invocation.created_at),serde_json::to_string(invocation)?,serde_json::to_string(&invocation.output)?.len() as i64])?;
        Ok(())
    }

    pub fn transition_invocation(
        &self,
        project: &ProjectId,
        expected: &Invocation,
        next: &Invocation,
    ) -> Result<bool> {
        use rusqlite::OptionalExtension;
        if expected.id != next.id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "an invocation transition cannot change identity",
            )
            .into());
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT json FROM invocations WHERE project_id=?1 AND id=?2",
                rusqlite::params![project.as_str(), expected.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(false);
        };
        let current: Invocation = serde_json::from_str(&stored)?;
        if serde_json::to_value(current)? != serde_json::to_value(expected)? {
            return Ok(false);
        }
        tx.execute(
            "UPDATE invocations SET status=?1,json=?2,output_len=?3 WHERE project_id=?4 AND id=?5",
            rusqlite::params![
                serde_json::to_string(&next.status)?.trim_matches('"'),
                serde_json::to_string(next)?,
                serde_json::to_string(&next.output)?.len() as i64,
                project.as_str(),
                next.id.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

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

    /// Mark an observed running invocation interrupted only if it has not changed.
    pub fn interrupt_invocation_if_unchanged(
        &self,
        observed: &ObservedInvocation,
        message: &str,
    ) -> Result<bool> {
        if observed.invocation.status != ark_core::InvocationStatus::Running {
            return Ok(false);
        }
        let mut interrupted = observed.invocation.clone();
        interrupted.mark_failed(message);
        let json = serde_json::to_string(&interrupted)?;
        let db = self.db.lock().unwrap();
        let changed = db.execute(
            "UPDATE invocations SET status=?1, json=?2, output_len=?3
             WHERE project_id=?4 AND id=?5 AND status='running' AND json=?6",
            rusqlite::params![
                "failed",
                json,
                serde_json::to_string(&interrupted.output)?.len() as i64,
                observed.project.as_str(),
                observed.invocation.id.as_str(),
                observed.raw_json,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Fail a freshly recorded running invocation only if that exact row is still current.
    pub fn fail_running_invocation_if_unchanged(
        &self,
        project: &ProjectId,
        running: &Invocation,
        message: &str,
    ) -> Result<bool> {
        let observed = ObservedInvocation {
            project: project.clone(),
            invocation: running.clone(),
            raw_json: serde_json::to_string(running)?,
        };
        self.interrupt_invocation_if_unchanged(&observed, message)
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

    /// Newest invocations whose work belongs to one conversation.
    pub fn list_invocations_with_work_prefix(
        &self,
        project: &ProjectId,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<Invocation>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT json FROM invocations
             WHERE project_id=?1
               AND substr(json_extract(json, '$.work'), 1, length(?2))=?2
               AND json_array_length(json_extract(json, '$.produced')) > 0
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project.as_str(), prefix, limit as i64],
            |row| row.get::<_, String>(0),
        )?;
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
        Ok(self
            .list_running_invocations_for_recovery()?
            .into_iter()
            .map(|row| (row.project, row.invocation))
            .collect())
    }

    pub fn list_running_invocations_for_recovery(&self) -> Result<Vec<ObservedInvocation>> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT project_id, id, json FROM invocations WHERE status='running'")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (p, id, json) = row?;
            let invocation: Invocation = serde_json::from_str(&json)?;
            if invocation.id.as_str() != id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invocation identity mismatch",
                )
                .into());
            }
            out.push(ObservedInvocation {
                project: ProjectId(p),
                invocation,
                raw_json: json,
            });
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
        let mut note_json = serde_json::to_value(n)?;
        scrub::scrub_all(&mut note_json);
        let note_json = note_json.to_string();
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO notes(project_id, id, kind, created_at, json)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                project.as_str(),
                n.id.as_str(),
                serde_json::to_string(&n.kind)?.trim_matches('"'),
                rfc(n.created_at),
                note_json
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

    /// Persist the compact working state for a chat of a project (one row, replaced in
    /// place; `""` is a run with no chat). Secrets are taken out of every string first. This
    /// *is* the resume checkpoint for the structured record — a new session loads it
    /// instead of replaying history.
    pub fn put_research_state(
        &self,
        project: &ProjectId,
        session: &str,
        state: &ResearchState,
    ) -> Result<()> {
        let mut json = serde_json::to_value(state)?;
        scrub::scrub_all(&mut json);
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO research_state(project_id, session_id, updated_at, json) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![project.as_str(), session, rfc(OffsetDateTime::now_utc()), json.to_string()],
        )?;
        Ok(())
    }

    pub fn get_research_state(
        &self,
        project: &ProjectId,
        session: &str,
    ) -> Result<Option<ResearchState>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM research_state WHERE project_id=?1 AND session_id=?2",
            [project.as_str(), session],
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

    /// Persist a new conversation session without replacing an existing one.
    pub fn create_session(
        &self,
        id: &str,
        project: &ProjectId,
        record: &serde_json::Value,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO sessions(id, project_id, updated_at, json) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![
                id,
                project.as_str(),
                rfc(OffsetDateTime::now_utc()),
                serde_json::to_string(record)?
            ],
        )?;
        Ok(())
    }

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
        let turn = &scrub::turn(turn);
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
        let mut trace = trace.clone();
        scrub::scrub_all(&mut trace);
        let trace = &trace;
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

    /// Atomically replace scoped records only when every previous value still matches.
    /// A missing expectation requires the key to be absent in every scope.
    /// Return the mutation epoch for immutable records in one scope.
    pub fn immutable_epoch(&self, scope: &str) -> Result<u64> {
        let db = self.db.lock().unwrap();
        ensure_immutable_schema(&db)?;
        let value: rusqlite::Result<i64> = db.query_row(
            "SELECT epoch FROM immutable_epochs WHERE scope=?1",
            [scope],
            |row| row.get(0),
        );
        match value {
            Ok(value) => u64::try_from(value)
                .map_err(|_| std::io::Error::other("invalid immutable epoch").into()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically add immutable kind registrations to a scoped mutation clock.
    pub fn register_record_watch(&self, scope: &str, kinds: &[&str]) -> Result<()> {
        validate_record_watch(scope, kinds)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_record_watch_schema(&tx)?;
        let incomplete: i64 = tx.query_row("SELECT EXISTS(SELECT 1 FROM watched_record_kinds WHERE scope=?1) AND NOT EXISTS(SELECT 1 FROM watched_record_epochs WHERE scope=?1)", [scope], |row| row.get(0))?;
        if incomplete != 0 {
            return Err(std::io::Error::other("missing watched epoch").into());
        }
        tx.execute("INSERT INTO watched_record_epochs(scope,value,exhausted) SELECT ?1,0,0 WHERE NOT EXISTS(SELECT 1 FROM watched_record_epochs WHERE scope=?1)", [scope])?;
        for kind in kinds {
            tx.execute("INSERT INTO watched_record_kinds(scope,kind) SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM watched_record_kinds WHERE scope=?1 AND kind=?2)", rusqlite::params![scope, kind])?;
        }
        watched_record_epoch_in(&tx, scope, kinds)?;
        tx.commit()?;
        Ok(())
    }

    /// Records in the selected states, optionally restricted to a recent numeric timestamp.
    pub fn records_in_states(
        &self,
        kind: &str,
        field: &str,
        states: &[&str],
        time_field: &str,
        since_ms: u64,
    ) -> Result<Vec<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare("SELECT json FROM records WHERE kind=?1 AND json_extract(json,?2) IN (SELECT value FROM json_each(?3)) AND json_extract(json,?4)>=?5 ORDER BY updated_at,id")?;
        let rows = statement.query_map(
            rusqlite::params![
                kind,
                format!("$.{field}"),
                serde_json::to_string(states)?,
                format!("$.{time_field}"),
                since_ms.min(i64::MAX as u64) as i64
            ],
            |row| row.get::<_, String>(0),
        )?;
        collect(rows)
    }

    /// Read one consistent watched-kind registration and its scoped mutation clock.
    pub fn watched_record_epoch(&self, scope: &str, kinds: &[&str]) -> Result<u64> {
        validate_record_watch(scope, kinds)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let value = watched_record_epoch_in(&tx, scope, kinds)?;
        tx.commit()?;
        Ok(value)
    }

    /// Read a scoped immutable record without allocating a value above the caller's limit.
    pub fn get_immutable_record(
        &self,
        kind: &str,
        id: &str,
        scope: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>> {
        let db = self.db.lock().unwrap();
        ensure_immutable_schema(&db)?;
        let limit = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        let metadata: rusqlite::Result<(String, i64)> = db.query_row(
            "SELECT scope, length(bytes) FROM immutable_records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match metadata {
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok((actual, _)) if actual != scope => return Err(StoreError::ImmutableConflict),
            Ok((_, length)) if length > limit || length < 0 => {
                return Err(StoreError::RecordTooLarge)
            }
            Ok(_) => {}
        }
        let bytes = db.query_row(
            "SELECT bytes FROM immutable_records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
            |row| row.get(0),
        )?;
        Ok(Some(bytes))
    }

    /// Insert immutable records atomically; identical existing records are idempotent.
    pub fn put_immutable_records(
        &self,
        records: &[ImmutableRecord<'_>],
        max_records: usize,
        max_total_bytes: usize,
    ) -> Result<()> {
        if max_records > 1024 || records.len() > max_records {
            return Err(StoreError::RecordTooLarge);
        }
        let mut keys = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for record in records {
            if [record.kind, record.id, record.scope].iter().any(|part| {
                part.is_empty() || part.len() > 256 || part.chars().any(char::is_control)
            }) || !keys.insert((record.kind, record.id))
            {
                return Err(StoreError::ImmutableConflict);
            }
            total = total
                .checked_add(record.kind.len())
                .and_then(|n| n.checked_add(record.id.len()))
                .and_then(|n| n.checked_add(record.scope.len()))
                .and_then(|n| n.checked_add(record.bytes.len()))
                .filter(|n| *n <= max_total_bytes)
                .ok_or(StoreError::RecordTooLarge)?;
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_immutable_schema(&tx)?;
        for record in records {
            let existing: rusqlite::Result<i64> = tx.query_row(
                "SELECT CASE WHEN scope=?3 AND bytes=?4 THEN 1 ELSE 0 END FROM immutable_records WHERE kind=?1 AND id=?2",
                rusqlite::params![record.kind, record.id, record.scope, record.bytes], |row| row.get(0),
            );
            match existing {
                Ok(1) => {}
                Ok(_) => return Err(StoreError::ImmutableConflict),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    tx.execute(
                        "INSERT INTO immutable_records(kind,id,scope,bytes) VALUES(?1,?2,?3,?4)",
                        rusqlite::params![record.kind, record.id, record.scope, record.bytes],
                    )?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Comparisons use the JSON encoding written by this store. Include a monotonic revision
    /// in the value when an intervening change back to the same value must be detected.
    pub fn compare_put_records(
        &self,
        updates: &[RecordUpdate<'_>],
        max_bytes: usize,
        max_total_bytes: usize,
    ) -> Result<bool> {
        self.compare_put_records_with_derived(updates, &[], max_bytes, max_total_bytes)
    }

    /// Replace derived records after the guarded updates match. A conflicting foreign scope is
    /// left untouched; derived data is advisory and must never authorize a mutation.
    pub fn compare_put_records_with_derived(
        &self,
        updates: &[RecordUpdate<'_>],
        derived: &[DerivedRecord<'_>],
        max_bytes: usize,
        max_total_bytes: usize,
    ) -> Result<bool> {
        self.compare_put_records_impl(
            updates,
            &[],
            &[],
            derived,
            ComparePolicy {
                max_bytes,
                max_total_bytes,
                max_records: 1024,
                count_identity_bytes: false,
                epoch: None,
                record_epoch: None,
                publication: None,
                publication_receipts: None,
                publication_scope: None,
            },
        )
    }

    /// Perform a bounded larger transaction when a caller needs many atomic claims.
    /// The total byte limit includes record identities, scopes and encoded values.
    pub fn compare_put_records_with_derived_limit(
        &self,
        updates: &[RecordUpdate<'_>],
        derived: &[DerivedRecord<'_>],
        max_bytes: usize,
        max_total_bytes: usize,
        max_records: usize,
    ) -> Result<bool> {
        self.compare_put_records_impl(
            updates,
            &[],
            &[],
            derived,
            ComparePolicy {
                max_bytes,
                max_total_bytes,
                max_records,
                count_identity_bytes: true,
                epoch: None,
                record_epoch: None,
                publication: None,
                publication_receipts: None,
                publication_scope: None,
            },
        )
    }

    /// Compare ordinary records and an immutable-record epoch in one transaction.
    pub fn compare_put_records_at_immutable_epoch(
        &self,
        updates: &[RecordUpdate<'_>],
        derived: &[DerivedRecord<'_>],
        max_bytes: usize,
        max_total_bytes: usize,
        epoch: ImmutableEpoch<'_>,
    ) -> Result<bool> {
        self.compare_put_records_impl(
            updates,
            &[],
            &[],
            derived,
            ComparePolicy {
                max_bytes,
                max_total_bytes,
                max_records: 1024,
                count_identity_bytes: true,
                epoch: Some(epoch),
                record_epoch: None,
                publication: None,
                publication_receipts: None,
                publication_scope: None,
            },
        )
    }

    /// Compare additional records without writing them, in the update transaction.
    pub fn compare_put_records_checked(
        &self,
        updates: &[RecordUpdate<'_>],
        checks: &[RecordCheck<'_>],
        derived: &[DerivedRecord<'_>],
        max_bytes: usize,
        max_total_bytes: usize,
        epoch: Option<ImmutableEpoch<'_>>,
    ) -> Result<bool> {
        self.compare_put_records_checked_limit(
            updates,
            checks,
            &[],
            derived,
            RecordCompareLimits {
                max_bytes,
                max_total_bytes,
                max_records: 1024,
            },
            epoch,
        )
    }

    /// Compare records and perform a bounded larger checked transaction.
    pub fn compare_put_records_checked_limit(
        &self,
        updates: &[RecordUpdate<'_>],
        checks: &[RecordCheck<'_>],
        empty_scopes: &[EmptyRecordScope<'_>],
        derived: &[DerivedRecord<'_>],
        limits: RecordCompareLimits,
        epoch: Option<ImmutableEpoch<'_>>,
    ) -> Result<bool> {
        self.compare_put_records_impl(
            updates,
            checks,
            empty_scopes,
            derived,
            ComparePolicy {
                max_bytes: limits.max_bytes,
                max_total_bytes: limits.max_total_bytes,
                max_records: limits.max_records,
                count_identity_bytes: true,
                epoch,
                record_epoch: None,
                publication: None,
                publication_receipts: None,
                publication_scope: None,
            },
        )
    }

    /// Compare the watched ordinary-record clock and optional immutable clock atomically.
    pub fn compare_put_records_with_guards(
        &self,
        updates: &[RecordUpdate<'_>],
        checks: &[RecordCheck<'_>],
        empty_scopes: &[EmptyRecordScope<'_>],
        derived: &[DerivedRecord<'_>],
        limits: RecordCompareLimits,
        guards: RecordCompareGuards<'_>,
    ) -> Result<bool> {
        self.compare_put_records_impl(
            updates,
            checks,
            empty_scopes,
            derived,
            ComparePolicy {
                max_bytes: limits.max_bytes,
                max_total_bytes: limits.max_total_bytes,
                max_records: limits.max_records,
                count_identity_bytes: true,
                epoch: guards.immutable_epoch,
                record_epoch: Some(guards.record_epoch),
                publication: None,
                publication_receipts: None,
                publication_scope: None,
            },
        )
    }

    /// Commit guarded records and a prepared invocation's outputs together.
    /// The caller must hold its cancellation fence for this short transaction.
    pub fn compare_put_records_with_publication(
        &self,
        updates: &[RecordUpdate<'_>],
        checks: &[RecordCheck<'_>],
        empty_scopes: &[EmptyRecordScope<'_>],
        derived: &[DerivedRecord<'_>],
        commit: OutputPublicationCommit<'_>,
    ) -> Result<bool> {
        let OutputPublicationCommit {
            limits,
            guards,
            publication,
            receipt_keys,
        } = commit;
        if !std::ptr::eq(self, publication.store)
            || receipt_keys.is_empty()
            || receipt_keys.len() > 64
        {
            return Err(StoreError::ImmutableConflict);
        }
        let record_scope = guards.record_epoch.scope;
        self.compare_put_records_impl(
            updates,
            checks,
            empty_scopes,
            derived,
            ComparePolicy {
                max_bytes: limits.max_bytes,
                max_total_bytes: limits.max_total_bytes,
                max_records: limits.max_records,
                count_identity_bytes: true,
                epoch: guards.immutable_epoch,
                record_epoch: Some(guards.record_epoch),
                publication: Some(publication),
                publication_receipts: Some(receipt_keys),
                publication_scope: Some(record_scope),
            },
        )
    }

    /// Verify an exact committed publication and its caller-owned receipt records.
    pub fn verify_output_publication(
        &self,
        publication: &PreparedOutputPublication<'_>,
        receipts: &[RecordUpdate<'_>],
    ) -> Result<bool> {
        if !std::ptr::eq(self, publication.store) || receipts.is_empty() || receipts.len() > 64 {
            return Err(StoreError::ImmutableConflict);
        }
        let record_scope = receipts[0].scope;
        let mut total = publication.finished_json.len();
        let mut encoded = Vec::with_capacity(receipts.len());
        let mut keys = std::collections::BTreeSet::new();
        for receipt in receipts {
            if receipt.scope != record_scope
                || receipt.kind.is_empty()
                || receipt.id.is_empty()
                || receipt.kind.len() > 256
                || receipt.id.len() > 256
                || !keys.insert((receipt.kind, receipt.id))
            {
                return Err(StoreError::ImmutableConflict);
            }
            let value = bounded_json(receipt.value, 128 * 1024)?;
            total = total
                .checked_add(value.len())
                .and_then(|n| n.checked_add(receipt.kind.len()))
                .and_then(|n| n.checked_add(receipt.id.len()))
                .ok_or(StoreError::RecordTooLarge)?;
            encoded.push((receipt, value));
        }
        for (_, json) in &publication.outputs {
            total = total
                .checked_add(json.len())
                .ok_or(StoreError::RecordTooLarge)?;
        }
        if total > 1024 * 1024 {
            return Err(StoreError::RecordTooLarge);
        }
        let manifest = bounded_json(
            &(
                record_scope,
                &publication.finished_json,
                publication
                    .outputs
                    .iter()
                    .map(|(_, json)| json)
                    .collect::<Vec<_>>(),
                encoded
                    .iter()
                    .map(|(receipt, value)| (receipt.kind, receipt.id, value))
                    .collect::<Vec<_>>(),
            ),
            1024 * 1024,
        )?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if !publication_scope_matches(&tx, publication.project.as_str(), record_scope)? {
            return Err(StoreError::ImmutableConflict);
        }
        let found: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM invocations WHERE project_id=?1 AND id=?2 AND status='ok' AND json=?3)",
            rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), publication.finished_json],
            |row| row.get(0),
        )?;
        if !found {
            return Ok(false);
        }
        let found: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM invocation_publications WHERE project_id=?1 AND invocation_id=?2 AND json=?3)",
            rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), manifest], |row| row.get(0),
        )?;
        if !found {
            return Ok(false);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2",
            rusqlite::params![
                publication.project.as_str(),
                publication.finished.id.as_str()
            ],
            |row| row.get(0),
        )?;
        if count != publication.outputs.len() as i64 {
            return Ok(false);
        }
        for (ordinal, (_, json)) in publication.outputs.iter().enumerate() {
            let found: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2 AND ordinal=?3 AND json=?4)",
                rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), ordinal as i64, json], |row| row.get(0),
            )?;
            if !found {
                return Ok(false);
            }
        }
        for (receipt, value) in encoded {
            let found: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE kind=?1 AND id=?2 AND scope=?3 AND json=?4)",
                rusqlite::params![receipt.kind, receipt.id, receipt.scope, value], |row| row.get(0),
            )?;
            if !found {
                return Ok(false);
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Recover the original committed result after an acknowledgement is lost.
    /// The caller must compare its request identity with the returned invocation and receipts,
    /// and verify the referenced blobs before using their bytes. Invocation and outputs remain
    /// child-owned history after reparenting; unavailable owner receipts are not verified.
    pub fn load_output_publication(
        &self,
        project: &ProjectId,
        invocation_id: &str,
    ) -> Result<Option<CommittedOutputPublication>> {
        use rusqlite::OptionalExtension as _;
        if project.as_str().is_empty()
            || project.as_str().len() > 128
            || invocation_id.is_empty()
            || invocation_id.len() > 96
        {
            return Err(StoreError::ImmutableConflict);
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let manifest: Option<Option<String>> = tx.query_row(
            "SELECT CASE WHEN octet_length(json)<=1048576 THEN json ELSE NULL END FROM invocation_publications WHERE project_id=?1 AND invocation_id=?2",
            rusqlite::params![project.as_str(), invocation_id], |row| row.get(0),
        ).optional()?;
        let Some(manifest) = manifest else {
            return Ok(None);
        };
        let manifest = manifest.ok_or(StoreError::RecordTooLarge)?;
        let mut total = manifest.len();
        let (record_scope, committed_invocation, committed_outputs, entries): PublicationManifest =
            serde_json::from_str(&manifest)?;
        if record_scope.is_empty()
            || record_scope.len() > 256
            || entries.is_empty()
            || entries.len() > 64
        {
            return Err(StoreError::ImmutableConflict);
        }
        let receipts_authorized = publication_scope_matches(&tx, project.as_str(), &record_scope)?;
        let invocation_json: Option<String> = tx.query_row(
            "SELECT CASE WHEN octet_length(json)<=131072 THEN json ELSE NULL END FROM invocations WHERE project_id=?1 AND id=?2 AND status='ok'",
            rusqlite::params![project.as_str(), invocation_id], |row| row.get(0),
        )?;
        let invocation_json = invocation_json.ok_or(StoreError::RecordTooLarge)?;
        if invocation_json != committed_invocation {
            return Err(StoreError::ImmutableConflict);
        }
        total = total
            .checked_add(invocation_json.len())
            .filter(|n| *n <= 1024 * 1024)
            .ok_or(StoreError::RecordTooLarge)?;
        let invocation: Invocation = serde_json::from_str(&invocation_json)?;
        if invocation.id.as_str() != invocation_id
            || invocation.status != ark_core::InvocationStatus::Ok
            || invocation.produced.is_empty()
            || invocation.produced.len() > 64
        {
            return Err(StoreError::ImmutableConflict);
        }
        let mut outputs = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT ordinal, CASE WHEN octet_length(json)<=131072 THEN json ELSE NULL END FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2 ORDER BY ordinal LIMIT 65",
            )?;
            let mut rows = stmt.query(rusqlite::params![project.as_str(), invocation_id])?;
            while let Some(row) = rows.next()? {
                let ordinal: i64 = row.get(0)?;
                let json: Option<String> = row.get(1)?;
                if ordinal != outputs.len() as i64 || outputs.len() >= invocation.produced.len() {
                    return Err(StoreError::ImmutableConflict);
                }
                let json = json.ok_or(StoreError::RecordTooLarge)?;
                if committed_outputs.get(outputs.len()) != Some(&json) {
                    return Err(StoreError::ImmutableConflict);
                }
                total = total
                    .checked_add(json.len())
                    .filter(|n| *n <= 1024 * 1024)
                    .ok_or(StoreError::RecordTooLarge)?;
                let artifact: Artifact = serde_json::from_str(&json)?;
                if artifact.id != invocation.produced[outputs.len()]
                    || !matches!(&artifact.provenance, ark_core::Provenance::Produced { invocation: origin } if origin == &invocation.id)
                {
                    return Err(StoreError::ImmutableConflict);
                }
                outputs.push(artifact);
            }
        }
        if outputs.len() != invocation.produced.len() || outputs.len() != committed_outputs.len() {
            return Err(StoreError::ImmutableConflict);
        }
        let mut keys = std::collections::BTreeSet::new();
        let mut receipts = Vec::with_capacity(entries.len());
        for (kind, id, json) in entries {
            if kind.is_empty()
                || kind.len() > 256
                || id.is_empty()
                || id.len() > 256
                || json.len() > 128 * 1024
                || !keys.insert((kind.clone(), id.clone()))
            {
                return Err(StoreError::ImmutableConflict);
            }
            if receipts_authorized {
                let found: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM records WHERE kind=?1 AND id=?2 AND scope=?3 AND json=?4)",
                    rusqlite::params![kind, id, record_scope, json], |row| row.get(0),
                )?;
                if !found {
                    return Err(StoreError::ImmutableConflict);
                }
                receipts.push((kind, id, serde_json::from_str(&json)?));
            }
        }
        tx.commit()?;
        Ok(Some(CommittedOutputPublication {
            invocation,
            outputs,
            record_scope: receipts_authorized.then_some(record_scope),
            receipts: receipts_authorized.then_some(receipts),
        }))
    }

    fn compare_put_records_impl(
        &self,
        updates: &[RecordUpdate<'_>],
        checks: &[RecordCheck<'_>],
        empty_scopes: &[EmptyRecordScope<'_>],
        derived: &[DerivedRecord<'_>],
        policy: ComparePolicy<'_>,
    ) -> Result<bool> {
        let ComparePolicy {
            max_bytes,
            max_total_bytes,
            max_records,
            count_identity_bytes,
            epoch,
            record_epoch,
            publication,
            publication_receipts,
            publication_scope,
        } = policy;
        if publication.is_some() && (max_total_bytes > 1024 * 1024 || max_records > 128) {
            return Err(StoreError::RecordTooLarge);
        }
        if let Some(epoch) = &epoch {
            if epoch.scope.is_empty()
                || epoch.scope.len() > 256
                || epoch.scope.chars().any(char::is_control)
                || updates.is_empty()
                || updates.iter().any(|update| update.scope != epoch.scope)
                || checks.iter().any(|check| check.scope != epoch.scope)
                || empty_scopes.iter().any(|check| check.scope != epoch.scope)
                || derived.iter().any(|record| record.scope != epoch.scope)
            {
                return Err(StoreError::ImmutableConflict);
            }
        }
        if let Some(guard) = &record_epoch {
            validate_record_watch(guard.scope, guard.kinds)?;
            if updates.is_empty()
                || updates.iter().any(|update| update.scope != guard.scope)
                || checks.iter().any(|check| check.scope != guard.scope)
                || empty_scopes.iter().any(|check| check.scope != guard.scope)
                || derived.iter().any(|record| record.scope != guard.scope)
            {
                return Err(StoreError::ImmutableConflict);
            }
        }
        struct Bounded {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl std::io::Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                    return Err(std::io::Error::other("record exceeds its write limit"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn encode(value: &serde_json::Value, limit: usize) -> Result<String> {
            let mut out = Bounded {
                bytes: Vec::new(),
                limit,
            };
            serde_json::to_writer(&mut out, value).map_err(|_| StoreError::RecordTooLarge)?;
            String::from_utf8(out.bytes).map_err(|e| {
                StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })
        }
        if max_records > 16_384
            || updates
                .len()
                .saturating_add(checks.len())
                .saturating_add(empty_scopes.len())
                .saturating_add(derived.len())
                > max_records
            || (!derived.is_empty() && updates.is_empty())
        {
            return Err(StoreError::RecordTooLarge);
        }
        let mut keys = std::collections::BTreeSet::new();
        let mut encoded = Vec::with_capacity(updates.len());
        let mut total = epoch.as_ref().map_or(0, |epoch| epoch.scope.len());
        if let Some(guard) = &record_epoch {
            total = guard
                .kinds
                .iter()
                .try_fold(total.saturating_add(guard.scope.len()), |sum, kind| {
                    sum.checked_add(kind.len())
                })
                .ok_or(StoreError::RecordTooLarge)?;
        }
        if total > max_total_bytes {
            return Err(StoreError::RecordTooLarge);
        }
        let mut scope_keys = std::collections::BTreeSet::new();
        for check in empty_scopes {
            if check.kind.is_empty()
                || check.scope.is_empty()
                || check.kind.len() > 256
                || check.scope.len() > 256
                || check.kind.chars().any(char::is_control)
                || check.scope.chars().any(char::is_control)
                || !scope_keys.insert((check.kind, check.scope))
            {
                return Err(StoreError::RecordTooLarge);
            }
            total = total
                .checked_add(check.kind.len())
                .and_then(|n| n.checked_add(check.scope.len()))
                .filter(|n| *n <= max_total_bytes)
                .ok_or(StoreError::RecordTooLarge)?;
        }
        let mut encoded_checks = Vec::with_capacity(checks.len());
        for check in checks {
            if !keys.insert((check.kind, check.id)) {
                return Ok(false);
            }
            if count_identity_bytes {
                total = total
                    .checked_add(check.kind.len())
                    .and_then(|size| size.checked_add(check.id.len()))
                    .and_then(|size| size.checked_add(check.scope.len()))
                    .filter(|size| *size <= max_total_bytes)
                    .ok_or(StoreError::RecordTooLarge)?;
            }
            let expected = check
                .expected
                .map(|value| encode(value, max_bytes.min(max_total_bytes.saturating_sub(total))))
                .transpose()?;
            total = total
                .checked_add(expected.as_ref().map_or(0, String::len))
                .ok_or(StoreError::RecordTooLarge)?;
            encoded_checks.push((check, expected));
        }
        for update in updates {
            if !keys.insert((update.kind, update.id)) {
                return Ok(false);
            }
            if count_identity_bytes {
                total = total
                    .checked_add(update.kind.len())
                    .and_then(|size| size.checked_add(update.id.len()))
                    .and_then(|size| size.checked_add(update.scope.len()))
                    .filter(|size| *size <= max_total_bytes)
                    .ok_or(StoreError::RecordTooLarge)?;
            }
            let remaining = max_total_bytes.saturating_sub(total);
            let expected = update
                .expected
                .map(|value| encode(value, max_bytes.min(remaining)))
                .transpose()?;
            total = total
                .checked_add(expected.as_ref().map_or(0, String::len))
                .ok_or(StoreError::RecordTooLarge)?;
            let value = encode(
                update.value,
                max_bytes.min(max_total_bytes.saturating_sub(total)),
            )?;
            total = total
                .checked_add(value.len())
                .ok_or(StoreError::RecordTooLarge)?;
            encoded.push((update, expected, value));
        }
        let mut derived_encoded = Vec::with_capacity(derived.len());
        for record in derived {
            if !keys.insert((record.kind, record.id)) {
                return Ok(false);
            }
            if count_identity_bytes {
                total = total
                    .checked_add(record.kind.len())
                    .and_then(|size| size.checked_add(record.id.len()))
                    .and_then(|size| size.checked_add(record.scope.len()))
                    .filter(|size| *size <= max_total_bytes)
                    .ok_or(StoreError::RecordTooLarge)?;
            }
            let value = encode(
                record.value,
                max_bytes.min(max_total_bytes.saturating_sub(total)),
            )?;
            total = total
                .checked_add(value.len())
                .ok_or(StoreError::RecordTooLarge)?;
            derived_encoded.push((record, value));
        }
        let mut publication_manifest = None;
        if let Some(publication) = publication {
            if updates
                .len()
                .saturating_add(checks.len())
                .saturating_add(empty_scopes.len())
                .saturating_add(derived.len())
                .saturating_add(publication.outputs.len())
                .saturating_add(1)
                > max_records
            {
                return Err(StoreError::RecordTooLarge);
            }
            total = total
                .checked_add(publication.running_json.len())
                .and_then(|n| n.checked_add(publication.finished_json.len()))
                .ok_or(StoreError::RecordTooLarge)?;
            for (_, json) in &publication.outputs {
                total = total
                    .checked_add(json.len())
                    .ok_or(StoreError::RecordTooLarge)?;
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut manifest = Vec::new();
            for &(kind, id) in publication_receipts.ok_or(StoreError::ImmutableConflict)? {
                if !seen.insert((kind, id)) {
                    return Err(StoreError::ImmutableConflict);
                }
                let (update, _, value) = encoded
                    .iter()
                    .find(|(update, _, _)| update.kind == kind && update.id == id)
                    .ok_or(StoreError::ImmutableConflict)?;
                if update.expected.is_some() {
                    return Err(StoreError::ImmutableConflict);
                }
                manifest.push((kind, id, value));
            }
            let manifest = bounded_json(
                &(
                    publication_scope.ok_or(StoreError::ImmutableConflict)?,
                    &publication.finished_json,
                    publication
                        .outputs
                        .iter()
                        .map(|(_, json)| json)
                        .collect::<Vec<_>>(),
                    manifest,
                ),
                1024 * 1024,
            )?;
            total = total
                .checked_add(manifest.len())
                .ok_or(StoreError::RecordTooLarge)?;
            if total > max_total_bytes {
                return Err(StoreError::RecordTooLarge);
            }
            publication_manifest = Some(manifest);
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(publication) = publication {
            if !publication_scope_matches(
                &tx,
                publication.project.as_str(),
                publication_scope.ok_or(StoreError::ImmutableConflict)?,
            )? {
                return Err(StoreError::ImmutableConflict);
            }
        }
        let limit = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        if let Some(epoch) = epoch {
            ensure_immutable_schema(&tx)?;
            let expected = i64::try_from(epoch.value)
                .map_err(|_| std::io::Error::other("invalid immutable epoch"))?;
            let current: i64 = tx.query_row(
                "SELECT COALESCE((SELECT epoch FROM immutable_epochs WHERE scope=?1), 0)",
                [epoch.scope],
                |row| row.get(0),
            )?;
            if current != expected {
                return Ok(false);
            }
        }
        if let Some(guard) = record_epoch {
            let current = watched_record_epoch_in(&tx, guard.scope, guard.kinds)?;
            if current != guard.value {
                return Ok(false);
            }
        }
        if let Some(publication) = publication {
            for blob in &publication.blobs {
                blob.verify()?;
            }
            let matches: rusqlite::Result<i64> = tx.query_row(
                "SELECT 1 FROM invocations WHERE project_id=?1 AND id=?2 AND status='running' AND json=?3",
                rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), publication.running_json],
                |row| row.get(0),
            );
            match matches {
                Ok(_) => (),
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
                Err(error) => return Err(error.into()),
            }
            let existing: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2)",
                rusqlite::params![publication.project.as_str(), publication.finished.id.as_str()],
                |row| row.get(0),
            )?;
            if existing != 0 {
                return Ok(false);
            }
        }
        for check in empty_scopes {
            let found: rusqlite::Result<i64> = tx.query_row(
                "SELECT 1 FROM records WHERE kind=?1 AND scope=?2 LIMIT 1",
                rusqlite::params![check.kind, check.scope],
                |row| row.get(0),
            );
            match found {
                Err(rusqlite::Error::QueryReturnedNoRows) => (),
                Ok(_) => return Ok(false),
                Err(error) => return Err(error.into()),
            }
        }
        let matches_record = |kind: &str,
                              id: &str,
                              scope: &str,
                              expected: &Option<String>|
         -> Result<bool> {
            if let Some(want) = expected {
                let expected_len = i64::try_from(want.len()).unwrap_or(i64::MAX);
                let row: rusqlite::Result<i64> = tx.query_row(
                    "SELECT CASE WHEN scope != ?3 THEN 0 WHEN octet_length(json) > ?4 THEN -1 WHEN octet_length(json) != ?5 THEN 0 WHEN json = ?6 THEN 1 ELSE 0 END FROM records WHERE kind=?1 AND id=?2",
                    rusqlite::params![kind, id, scope, limit, expected_len, want],
                    |row| row.get(0),
                );
                match row {
                    Ok(1) => Ok(true),
                    Ok(-1) => Err(StoreError::RecordTooLarge),
                    Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
                    Err(error) => Err(error.into()),
                }
            } else {
                let row: rusqlite::Result<i64> = tx.query_row(
                    "SELECT 1 FROM records WHERE kind=?1 AND id=?2",
                    rusqlite::params![kind, id],
                    |row| row.get(0),
                );
                match row {
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
                    Ok(_) => Ok(false),
                    Err(error) => Err(error.into()),
                }
            }
        };
        for (check, expected) in &encoded_checks {
            if !matches_record(check.kind, check.id, check.scope, expected)? {
                return Ok(false);
            }
        }
        for (update, expected, _) in &encoded {
            if !matches_record(update.kind, update.id, update.scope, expected)? {
                return Ok(false);
            }
        }
        let updated_at = rfc(OffsetDateTime::now_utc());
        for (update, _, value) in encoded {
            tx.execute(
                "INSERT OR REPLACE INTO records(kind, id, scope, updated_at, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![update.kind, update.id, update.scope, updated_at, value],
            )?;
        }
        for (record, value) in derived_encoded {
            tx.execute(
                "INSERT INTO records(kind, id, scope, updated_at, json) VALUES(?1, ?2, ?3, ?4, ?5) ON CONFLICT(kind, id) DO UPDATE SET updated_at=excluded.updated_at, json=excluded.json WHERE records.scope=excluded.scope",
                rusqlite::params![record.kind, record.id, record.scope, updated_at, value],
            )?;
        }
        if let Some(publication) = publication {
            tx.execute(
                "INSERT INTO invocation_publications(project_id,invocation_id,json) VALUES(?1,?2,?3)",
                rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), publication_manifest.as_ref().ok_or(StoreError::ImmutableConflict)?],
            )?;
            for (ordinal, (artifact, json)) in publication.outputs.iter().enumerate() {
                tx.execute(
                    "INSERT OR IGNORE INTO artifacts(project_id,id,kind,name,created_at,json) VALUES(?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![publication.project.as_str(), artifact.id.as_str(), artifact.kind.as_tag(), artifact.name, rfc(artifact.created_at), json],
                )?;
                tx.execute(
                    "INSERT INTO invocation_outputs(project_id,invocation_id,ordinal,json) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![publication.project.as_str(), publication.finished.id.as_str(), ordinal as i64, json],
                )?;
            }
            let changed = tx.execute(
                "UPDATE invocations SET status='ok', json=?1, output_len=?2 WHERE project_id=?3 AND id=?4 AND status='running' AND json=?5",
                rusqlite::params![publication.finished_json, publication.output_len as i64, publication.project.as_str(), publication.finished.id.as_str(), publication.running_json],
            )?;
            if changed != 1 {
                return Ok(false);
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn get_record(&self, kind: &str, id: &str) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        one(db.query_row(
            "SELECT json FROM records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
            |r| r.get::<_, String>(0),
        ))
    }

    /// Check whether a globally addressed record exists without loading its value.
    pub fn record_exists(&self, kind: &str, id: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let row: rusqlite::Result<i64> = db.query_row(
            "SELECT 1 FROM records WHERE kind=?1 AND id=?2",
            rusqlite::params![kind, id],
            |row| row.get(0),
        );
        match row {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Read a caller-owned record only from its expected scope and within a byte limit.
    pub fn get_record_in_scope_bounded(
        &self,
        kind: &str,
        id: &str,
        scope: &str,
        max_bytes: usize,
    ) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let limit = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        let result: rusqlite::Result<Option<String>> = db.query_row(
            "SELECT CASE WHEN octet_length(json) <= ?4 THEN json ELSE NULL END FROM records WHERE kind=?1 AND id=?2 AND scope=?3",
            rusqlite::params![kind, id, scope, limit],
            |row| row.get(0),
        );
        match result {
            Ok(Some(json)) => Ok(Some(serde_json::from_str(&json)?)),
            Ok(None) => Err(StoreError::RecordTooLarge),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Read the next key in one scope, checking key and JSON sizes before allocation.
    /// A caller that needs a stable scan must guard concurrent writes separately.
    pub fn next_record_in_scope_bounded(
        &self,
        kind: &str,
        scope: &str,
        after: Option<&str>,
        max_id_bytes: usize,
        max_json_bytes: usize,
    ) -> Result<Option<(String, serde_json::Value)>> {
        let db = self.db.lock().unwrap();
        let id_limit = i64::try_from(max_id_bytes).unwrap_or(i64::MAX);
        let json_limit = i64::try_from(max_json_bytes).unwrap_or(i64::MAX);
        let sql = if after.is_some() {
            "SELECT CASE WHEN octet_length(id) <= ?4 THEN id ELSE NULL END, \
             CASE WHEN octet_length(json) <= ?5 THEN json ELSE NULL END \
             FROM records INDEXED BY records_by_scope_id WHERE kind=?1 AND scope=?2 AND id>?3 ORDER BY id LIMIT 1"
        } else {
            "SELECT CASE WHEN octet_length(id) <= ?4 THEN id ELSE NULL END, \
             CASE WHEN octet_length(json) <= ?5 THEN json ELSE NULL END \
             FROM records INDEXED BY records_by_scope_id WHERE kind=?1 AND scope=?2 ORDER BY id LIMIT 1"
        };
        let result: rusqlite::Result<(Option<String>, Option<String>)> = db.query_row(
            sql,
            rusqlite::params![kind, scope, after, id_limit, json_limit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        drop(db);
        match result {
            Ok((Some(id), Some(json))) => Ok(Some((id, serde_json::from_str(&json)?))),
            Ok(_) => Err(StoreError::RecordTooLarge),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
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
fn validate_record_watch(scope: &str, kinds: &[&str]) -> Result<()> {
    if scope.is_empty()
        || scope.len() > 256
        || scope.chars().any(char::is_control)
        || kinds.is_empty()
        || kinds.len() > 32
    {
        return Err(StoreError::ImmutableConflict);
    }
    let mut unique = std::collections::BTreeSet::new();
    for kind in kinds {
        if kind.is_empty()
            || kind.len() > 256
            || kind.chars().any(char::is_control)
            || !unique.insert(*kind)
        {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

fn ensure_record_watch_schema(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row(
        "SELECT version FROM watched_record_schema WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let triggers: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN ('watched_epoch_no_replace','watched_epoch_monotonic','watched_epoch_no_delete','watched_kind_no_replace','watched_kind_no_update','watched_kind_no_delete','watched_kind_advance','watched_schema_no_replace','watched_schema_no_update','watched_schema_no_delete','watched_record_before_insert','watched_record_after_insert','watched_record_before_update','watched_record_after_update','watched_record_after_delete')", [], |row| row.get(0))?;
    if version != 1 || triggers != 15 {
        return Err(std::io::Error::other("watched record schema is unavailable").into());
    }
    Ok(())
}

fn watched_record_epoch_in(db: &Connection, scope: &str, kinds: &[&str]) -> Result<u64> {
    validate_record_watch(scope, kinds)?;
    ensure_record_watch_schema(db)?;
    for kind in kinds {
        let present: rusqlite::Result<i64> = db.query_row(
            "SELECT 1 FROM watched_record_kinds WHERE scope=?1 AND kind=?2",
            rusqlite::params![scope, kind],
            |row| row.get(0),
        );
        match present {
            Ok(1) => {}
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(std::io::Error::other("record watch is unavailable").into())
            }
            Ok(_) => return Err(std::io::Error::other("invalid record watch").into()),
            Err(error) => return Err(error.into()),
        }
    }
    let (value, exhausted): (i64, i64) = db.query_row(
        "SELECT value, exhausted FROM watched_record_epochs WHERE scope=?1",
        [scope],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if exhausted == 1 {
        return Err(StoreError::RecordEpochExhausted);
    }
    if exhausted != 0 || value < 0 || value == i64::MAX {
        return Err(std::io::Error::other("invalid record mutation clock").into());
    }
    Ok(value as u64)
}

fn migrate_record_watches(db: &Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS watched_record_schema(id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL CHECK(typeof(version)='integer'));
        CREATE TABLE IF NOT EXISTS watched_record_epochs(scope TEXT NOT NULL PRIMARY KEY CHECK(typeof(scope)='text' AND length(scope)>0), value INTEGER NOT NULL CHECK(typeof(value)='integer' AND value>=0), exhausted INTEGER NOT NULL CHECK(typeof(exhausted)='integer' AND exhausted IN (0,1)));
        CREATE TABLE IF NOT EXISTS watched_record_kinds(scope TEXT NOT NULL CHECK(typeof(scope)='text' AND length(scope)>0), kind TEXT NOT NULL CHECK(typeof(kind)='text' AND length(kind)>0), PRIMARY KEY(scope,kind));")?;
    let version: rusqlite::Result<i64> = tx.query_row(
        "SELECT version FROM watched_record_schema WHERE id=1",
        [],
        |row| row.get(0),
    );
    match version {
        Ok(1) => ensure_record_watch_schema(&tx)?,
        Ok(_) => return Err(std::io::Error::other("unsupported watched record schema").into()),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let rows: i64 = tx.query_row("SELECT (SELECT COUNT(*) FROM watched_record_epochs)+(SELECT COUNT(*) FROM watched_record_kinds)", [], |row| row.get(0))?;
            if rows != 0 {
                return Err(std::io::Error::other("incomplete watched record schema").into());
            }
            tx.execute(
                "INSERT INTO watched_record_schema(id,version) VALUES(1,1)",
                [],
            )?;
        }
        Err(error) => return Err(error.into()),
    }
    tx.execute_batch("CREATE TRIGGER IF NOT EXISTS watched_epoch_no_replace BEFORE INSERT ON watched_record_epochs
          WHEN NEW.value != 0 OR NEW.exhausted != 0 OR EXISTS(SELECT 1 FROM watched_record_epochs WHERE scope=NEW.scope)
          BEGIN SELECT RAISE(ABORT, 'invalid watched epoch'); END;
        CREATE TRIGGER IF NOT EXISTS watched_epoch_monotonic BEFORE UPDATE ON watched_record_epochs
          WHEN NEW.scope != OLD.scope OR OLD.exhausted != 0 OR OLD.value >= 9223372036854775807
            OR NEW.value != OLD.value+1 OR NEW.exhausted != CASE WHEN NEW.value=9223372036854775807 THEN 1 ELSE 0 END
          BEGIN SELECT RAISE(ABORT, 'invalid watched epoch'); END;
        CREATE TRIGGER IF NOT EXISTS watched_epoch_no_delete BEFORE DELETE ON watched_record_epochs BEGIN SELECT RAISE(ABORT, 'watched epoch'); END;
        CREATE TRIGGER IF NOT EXISTS watched_kind_no_replace BEFORE INSERT ON watched_record_kinds
          WHEN EXISTS(SELECT 1 FROM watched_record_kinds WHERE scope=NEW.scope AND kind=NEW.kind)
          BEGIN SELECT RAISE(ABORT, 'watched kind'); END;
        CREATE TRIGGER IF NOT EXISTS watched_kind_no_update BEFORE UPDATE ON watched_record_kinds BEGIN SELECT RAISE(ABORT, 'watched kind'); END;
        CREATE TRIGGER IF NOT EXISTS watched_kind_no_delete BEFORE DELETE ON watched_record_kinds BEGIN SELECT RAISE(ABORT, 'watched kind'); END;
        CREATE TRIGGER IF NOT EXISTS watched_kind_advance AFTER INSERT ON watched_record_kinds BEGIN
          SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM watched_record_epochs WHERE scope=NEW.scope) THEN RAISE(ABORT, 'missing watched epoch') END;
          UPDATE watched_record_epochs SET value=value+1, exhausted=CASE WHEN value=9223372036854775806 THEN 1 ELSE 0 END WHERE scope=NEW.scope AND exhausted=0;
        END;
        CREATE TRIGGER IF NOT EXISTS watched_schema_no_replace BEFORE INSERT ON watched_record_schema
          WHEN EXISTS(SELECT 1 FROM watched_record_schema WHERE id=NEW.id) BEGIN SELECT RAISE(ABORT, 'watched schema'); END;
        CREATE TRIGGER IF NOT EXISTS watched_schema_no_update BEFORE UPDATE ON watched_record_schema BEGIN SELECT RAISE(ABORT, 'watched schema'); END;
        CREATE TRIGGER IF NOT EXISTS watched_schema_no_delete BEFORE DELETE ON watched_record_schema BEGIN SELECT RAISE(ABORT, 'watched schema'); END;")?;
    for (name, timing, scope, kind) in [
        (
            "watched_record_before_insert",
            "BEFORE INSERT",
            "(SELECT scope FROM records WHERE kind=NEW.kind AND id=NEW.id)",
            "NEW.kind",
        ),
        (
            "watched_record_after_insert",
            "AFTER INSERT",
            "NEW.scope",
            "NEW.kind",
        ),
        (
            "watched_record_before_update",
            "BEFORE UPDATE",
            "OLD.scope",
            "OLD.kind",
        ),
        (
            "watched_record_after_update",
            "AFTER UPDATE",
            "NEW.scope",
            "NEW.kind",
        ),
        (
            "watched_record_after_delete",
            "AFTER DELETE",
            "OLD.scope",
            "OLD.kind",
        ),
    ] {
        tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS {name} {timing} ON records BEGIN
            SELECT CASE WHEN EXISTS(SELECT 1 FROM watched_record_kinds WHERE scope={scope} AND kind={kind})
              AND NOT EXISTS(SELECT 1 FROM watched_record_epochs WHERE scope={scope})
              THEN RAISE(ABORT, 'missing watched epoch') END;
            UPDATE watched_record_epochs SET value=value+1, exhausted=CASE WHEN value=9223372036854775806 THEN 1 ELSE 0 END
              WHERE scope={scope} AND exhausted=0 AND EXISTS(SELECT 1 FROM watched_record_kinds WHERE scope={scope} AND kind={kind});
          END;"))?;
    }
    ensure_record_watch_schema(&tx)?;
    tx.commit()?;
    Ok(())
}

fn ensure_immutable_schema(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row(
        "SELECT version FROM immutable_schema WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let triggers: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN ('immutable_records_no_replace','immutable_records_no_update','immutable_records_no_delete','immutable_records_epoch','immutable_epochs_no_replace','immutable_epochs_monotonic','immutable_epochs_no_delete')", [], |row| row.get(0))?;
    if version != 1 || triggers != 7 {
        return Err(std::io::Error::other("immutable record schema is unavailable").into());
    }
    Ok(())
}

fn migrate_research_state(db: &Connection) -> Result<()> {
    let keyed_by_session = db
        .prepare("SELECT 1 FROM pragma_table_info('research_state') WHERE name='session_id'")?
        .exists([])?;
    if keyed_by_session {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "ALTER TABLE research_state RENAME TO research_state_by_project;
        CREATE TABLE research_state(
            project_id TEXT NOT NULL, session_id TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL, json TEXT NOT NULL,
            PRIMARY KEY(project_id, session_id));
        INSERT INTO research_state(project_id, session_id, updated_at, json)
            SELECT project_id, '#legacy', updated_at, json FROM research_state_by_project;
        DROP TABLE research_state_by_project;",
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate_immutable(db: &Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS immutable_records(
            kind TEXT NOT NULL, id TEXT NOT NULL, scope TEXT NOT NULL,
            bytes BLOB NOT NULL, PRIMARY KEY(kind, id));
        CREATE TABLE IF NOT EXISTS immutable_epochs(
            scope TEXT PRIMARY KEY, epoch INTEGER NOT NULL CHECK(epoch >= 0));
        CREATE TABLE IF NOT EXISTS immutable_schema(
            id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL);",
    )?;
    let version: rusqlite::Result<i64> = tx.query_row(
        "SELECT version FROM immutable_schema WHERE id=1",
        [],
        |row| row.get(0),
    );
    let fresh = match version {
        Ok(1) => false,
        Ok(_) => return Err(std::io::Error::other("unsupported immutable record schema").into()),
        Err(rusqlite::Error::QueryReturnedNoRows) => true,
        Err(error) => return Err(error.into()),
    };
    tx.execute_batch("DROP TRIGGER IF EXISTS immutable_records_no_replace;
                DROP TRIGGER IF EXISTS immutable_records_no_update;
                DROP TRIGGER IF EXISTS immutable_records_no_delete;
                DROP TRIGGER IF EXISTS immutable_records_epoch;
                DROP TRIGGER IF EXISTS immutable_epochs_no_replace;
                DROP TRIGGER IF EXISTS immutable_epochs_monotonic;
                DROP TRIGGER IF EXISTS immutable_epochs_no_delete;
                CREATE TRIGGER immutable_epochs_no_replace BEFORE INSERT ON immutable_epochs
                    WHEN NEW.epoch != 0 OR EXISTS(SELECT 1 FROM immutable_epochs WHERE scope=NEW.scope)
                    BEGIN SELECT RAISE(ABORT, 'invalid immutable epoch'); END;
                CREATE TRIGGER immutable_epochs_monotonic BEFORE UPDATE ON immutable_epochs
                    WHEN NEW.scope != OLD.scope OR OLD.epoch >= 9223372036854775807 OR NEW.epoch != OLD.epoch+1
                    BEGIN SELECT RAISE(ABORT, 'invalid immutable epoch'); END;
                CREATE TRIGGER immutable_epochs_no_delete BEFORE DELETE ON immutable_epochs
                    BEGIN SELECT RAISE(ABORT, 'immutable epoch'); END;
                CREATE TRIGGER immutable_records_no_replace BEFORE INSERT ON immutable_records
                    WHEN EXISTS(SELECT 1 FROM immutable_records WHERE kind=NEW.kind AND id=NEW.id)
                    BEGIN SELECT RAISE(ABORT, 'immutable record'); END;
                CREATE TRIGGER immutable_records_no_update BEFORE UPDATE ON immutable_records
                    BEGIN SELECT RAISE(ABORT, 'immutable record'); END;
                CREATE TRIGGER immutable_records_no_delete BEFORE DELETE ON immutable_records
                    BEGIN SELECT RAISE(ABORT, 'immutable record'); END;
                CREATE TRIGGER immutable_records_epoch AFTER INSERT ON immutable_records BEGIN
                    INSERT INTO immutable_epochs(scope, epoch) SELECT NEW.scope, 0
                        WHERE NOT EXISTS(SELECT 1 FROM immutable_epochs WHERE scope=NEW.scope);
                    UPDATE immutable_epochs SET epoch=CASE WHEN epoch=9223372036854775807
                        THEN RAISE(ABORT, 'immutable epoch exhausted') ELSE epoch+1 END WHERE scope=NEW.scope;
                END;")?;
    if fresh {
        tx.execute("INSERT INTO immutable_schema(id,version) VALUES(1,1)", [])?;
    }
    tx.commit()?;
    Ok(())
}

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
    fn guarded_output_publication_keeps_each_invocation_association() {
        let (dir, store) = store();
        let other = Store::open(dir.path()).unwrap();
        let project = ProjectId("publication-test".into());
        let kinds = ["authority"];
        store
            .register_record_watch(project.as_str(), &kinds)
            .unwrap();
        let blob = store.put_blob(b"same output").unwrap();
        let original = Artifact::new(
            blob.clone(),
            ArtifactKind::Binary,
            "original",
            11,
            Provenance::Ingested {
                path: "original".into(),
            },
        );
        store.put_artifact(&project, &original).unwrap();
        for n in 0..2 {
            let running = Invocation::new(
                "fixture.export",
                RiskTier::Mutate,
                serde_json::json!({"n": n}),
            );
            let mut running = running;
            running.status = ark_core::InvocationStatus::Running;
            store.put_invocation(&project, &running).unwrap();
            let artifact = Artifact::new(
                blob.clone(),
                ArtifactKind::Binary,
                format!("output-{n}"),
                11,
                Provenance::Produced {
                    invocation: running.id.clone(),
                },
            );
            let mut finished = running.clone();
            finished.produced.push(blob.clone());
            finished.mark_ok(serde_json::json!({"n": n}));
            let prepared = store
                .prepare_output_publication(&project, &running, &finished, &[artifact])
                .unwrap();
            let receipt = serde_json::json!({"n": n});
            let receipt_id = format!("receipt-{n}");
            let updates = [RecordUpdate {
                kind: "receipt",
                id: &receipt_id,
                scope: project.as_str(),
                expected: None,
                value: &receipt,
            }];
            let epoch = store
                .watched_record_epoch(project.as_str(), &kinds)
                .unwrap();
            let guards = || RecordCompareGuards {
                immutable_epoch: None,
                record_epoch: RecordMutationEpoch {
                    scope: project.as_str(),
                    kinds: &kinds,
                    value: epoch,
                },
            };
            let limits = || RecordCompareLimits {
                max_bytes: 8192,
                max_total_bytes: 32768,
                max_records: 8,
            };
            assert!(!store
                .verify_output_publication(&prepared, &updates)
                .unwrap());
            assert!(other
                .compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: limits(),
                        guards: guards(),
                        publication: &prepared,
                        receipt_keys: &[("receipt", &receipt_id)],
                    }
                )
                .is_err());
            assert!(store
                .compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: limits(),
                        guards: guards(),
                        publication: &prepared,
                        receipt_keys: &[("receipt", &receipt_id)],
                    }
                )
                .unwrap());
            assert!(store
                .verify_output_publication(&prepared, &updates)
                .unwrap());
            let recovered = other
                .load_output_publication(&project, running.id.as_str())
                .unwrap()
                .unwrap();
            assert_eq!(recovered.invocation.input, running.input);
            assert_eq!(recovered.outputs[0].id, blob);
            assert_eq!(recovered.receipts.as_ref().unwrap()[0].2, receipt);
            if n == 0 {
                let original_json: String = other.db.lock().unwrap().query_row(
                    "SELECT json FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2 AND ordinal=0",
                    rusqlite::params![project.as_str(), running.id.as_str()], |row| row.get(0),
                ).unwrap();
                let mut altered: serde_json::Value = serde_json::from_str(&original_json).unwrap();
                altered["name"] = serde_json::json!("altered");
                other.db.lock().unwrap().execute(
                    "UPDATE invocation_outputs SET json=?1 WHERE project_id=?2 AND invocation_id=?3 AND ordinal=0",
                    rusqlite::params![serde_json::to_string(&altered).unwrap(), project.as_str(), running.id.as_str()],
                ).unwrap();
                assert!(store
                    .load_output_publication(&project, running.id.as_str())
                    .is_err());
                other.db.lock().unwrap().execute(
                    "UPDATE invocation_outputs SET json=?1 WHERE project_id=?2 AND invocation_id=?3 AND ordinal=0",
                    rusqlite::params![original_json, project.as_str(), running.id.as_str()],
                ).unwrap();
            }
            assert!(!store
                .compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: limits(),
                        guards: guards(),
                        publication: &prepared,
                        receipt_keys: &[("receipt", &receipt_id)],
                    }
                )
                .unwrap());
            assert_eq!(
                store
                    .get_invocation(&project, running.id.as_str())
                    .unwrap()
                    .unwrap()
                    .status,
                ark_core::InvocationStatus::Ok
            );
        }
        assert_eq!(
            store.get_artifact(&project, &blob).unwrap().unwrap().name,
            "original"
        );
        let db = store.db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM invocation_outputs WHERE project_id=?1",
                [project.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn guarded_output_publication_refuses_revocation_and_interruption() {
        let (_dir, store) = store();
        let project = ProjectId("publication-revoke".into());
        let kinds = ["authority"];
        store
            .register_record_watch(project.as_str(), &kinds)
            .unwrap();
        let blob = store.put_blob(b"output").unwrap();
        for revoked in [true, false] {
            let mut running = Invocation::new(
                "fixture.export",
                RiskTier::Mutate,
                serde_json::json!({"revoked": revoked}),
            );
            running.status = ark_core::InvocationStatus::Running;
            store.put_invocation(&project, &running).unwrap();
            let artifact = Artifact::new(
                blob.clone(),
                ArtifactKind::Binary,
                "output",
                6,
                Provenance::Produced {
                    invocation: running.id.clone(),
                },
            );
            let mut finished = running.clone();
            finished.produced.push(blob.clone());
            finished.mark_ok(serde_json::json!({}));
            let prepared = store
                .prepare_output_publication(&project, &running, &finished, &[artifact])
                .unwrap();
            let id = format!("receipt-{revoked}");
            let receipt = serde_json::json!({"ok": true});
            let updates = [RecordUpdate {
                kind: "receipt",
                id: &id,
                scope: project.as_str(),
                expected: None,
                value: &receipt,
            }];
            let epoch = store
                .watched_record_epoch(project.as_str(), &kinds)
                .unwrap();
            if revoked {
                store
                    .put_record(
                        "authority",
                        "gate",
                        project.as_str(),
                        &serde_json::json!("revoked"),
                    )
                    .unwrap();
            } else {
                let observed = store
                    .list_running_invocations_for_recovery()
                    .unwrap()
                    .into_iter()
                    .find(|row| row.invocation().id == running.id)
                    .unwrap();
                assert!(store
                    .interrupt_invocation_if_unchanged(&observed, "interrupted")
                    .unwrap());
            }
            let accepted = store
                .compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: RecordCompareLimits {
                            max_bytes: 8192,
                            max_total_bytes: 32768,
                            max_records: 8,
                        },
                        guards: RecordCompareGuards {
                            immutable_epoch: None,
                            record_epoch: RecordMutationEpoch {
                                scope: project.as_str(),
                                kinds: &kinds,
                                value: epoch,
                            },
                        },
                        publication: &prepared,
                        receipt_keys: &[("receipt", &id)],
                    },
                )
                .unwrap();
            assert!(!accepted);
            assert!(store
                .load_output_publication(&project, running.id.as_str())
                .unwrap()
                .is_none());
            assert!(store.get_record("receipt", &id).unwrap().is_none());
        }
    }

    #[test]
    fn output_publication_rejects_unrelated_and_reassigned_project_scopes() {
        let (_dir, store) = store();
        let owner = Project::new("owner", "");
        let other = Project::new("other", "");
        let mut child = Project::new("child", "").with_parent(owner.id.clone());
        for project in [&owner, &other, &child] {
            store.put_project(project).unwrap();
        }
        let kinds = ["authority"];
        for scope in [&owner.id, &other.id] {
            store.register_record_watch(scope.as_str(), &kinds).unwrap();
        }
        let mut running =
            Invocation::new("fixture.export", RiskTier::Mutate, serde_json::json!({}));
        running.status = ark_core::InvocationStatus::Running;
        store.put_invocation(&child.id, &running).unwrap();
        let blob = store.put_blob(b"output").unwrap();
        let artifact = Artifact::new(
            blob.clone(),
            ArtifactKind::Binary,
            "output",
            6,
            Provenance::Produced {
                invocation: running.id.clone(),
            },
        );
        let mut finished = running.clone();
        finished.produced.push(blob);
        finished.mark_ok(serde_json::json!({}));
        let prepared = store
            .prepare_output_publication(&child.id, &running, &finished, &[artifact])
            .unwrap();
        let receipt = serde_json::json!({"ok": true});
        let attempt = |scope: &ProjectId| {
            let updates = [RecordUpdate {
                kind: "receipt",
                id: "scope",
                scope: scope.as_str(),
                expected: None,
                value: &receipt,
            }];
            let epoch = store.watched_record_epoch(scope.as_str(), &kinds).unwrap();
            store.compare_put_records_with_publication(
                &updates,
                &[],
                &[],
                &[],
                OutputPublicationCommit {
                    limits: RecordCompareLimits {
                        max_bytes: 8192,
                        max_total_bytes: 32768,
                        max_records: 8,
                    },
                    guards: RecordCompareGuards {
                        immutable_epoch: None,
                        record_epoch: RecordMutationEpoch {
                            scope: scope.as_str(),
                            kinds: &kinds,
                            value: epoch,
                        },
                    },
                    publication: &prepared,
                    receipt_keys: &[("receipt", "scope")],
                },
            )
        };
        assert!(attempt(&other.id).is_err());
        child.parent = Some(other.id.clone());
        store.put_project(&child).unwrap();
        assert!(attempt(&owner.id).is_err());
        assert!(store.get_record("receipt", "scope").unwrap().is_none());
        assert!(store
            .load_output_publication(&child.id, running.id.as_str())
            .unwrap()
            .is_none());
    }

    #[test]
    fn output_publication_observes_a_concurrent_reparent_before_commit() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static BUSY: AtomicBool = AtomicBool::new(false);
        fn busy(count: i32) -> bool {
            BUSY.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
            count < 5000
        }
        BUSY.store(false, Ordering::SeqCst);
        let (dir, store) = store();
        let other = Store::open(dir.path()).unwrap();
        let owner = Project::new("owner", "");
        let replacement = Project::new("replacement", "");
        let mut child = Project::new("child", "").with_parent(owner.id.clone());
        for project in [&owner, &replacement, &child] {
            store.put_project(project).unwrap();
        }
        let kinds = ["authority"];
        store
            .register_record_watch(owner.id.as_str(), &kinds)
            .unwrap();
        let epoch = store
            .watched_record_epoch(owner.id.as_str(), &kinds)
            .unwrap();
        let mut running =
            Invocation::new("fixture.export", RiskTier::Mutate, serde_json::json!({}));
        running.status = ark_core::InvocationStatus::Running;
        store.put_invocation(&child.id, &running).unwrap();
        let blob = store.put_blob(b"output").unwrap();
        let artifact = Artifact::new(
            blob.clone(),
            ArtifactKind::Binary,
            "output",
            6,
            Provenance::Produced {
                invocation: running.id.clone(),
            },
        );
        let mut finished = running.clone();
        finished.produced.push(blob);
        finished.mark_ok(serde_json::json!({}));
        let prepared = store
            .prepare_output_publication(&child.id, &running, &finished, &[artifact])
            .unwrap();
        store.db.lock().unwrap().busy_handler(Some(busy)).unwrap();
        let mut other_db = other.db.lock().unwrap();
        let tx = other_db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        child.parent = Some(replacement.id.clone());
        tx.execute(
            "UPDATE projects SET json=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&child).unwrap(), child.id.as_str()],
        )
        .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let receipt = serde_json::json!({"ok": true});
                let updates = [RecordUpdate {
                    kind: "receipt",
                    id: "race",
                    scope: owner.id.as_str(),
                    expected: None,
                    value: &receipt,
                }];
                store.compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: RecordCompareLimits {
                            max_bytes: 8192,
                            max_total_bytes: 32768,
                            max_records: 8,
                        },
                        guards: RecordCompareGuards {
                            immutable_epoch: None,
                            record_epoch: RecordMutationEpoch {
                                scope: owner.id.as_str(),
                                kinds: &kinds,
                                value: epoch,
                            },
                        },
                        publication: &prepared,
                        receipt_keys: &[("receipt", "race")],
                    },
                )
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !BUSY.load(Ordering::SeqCst) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "publication did not reach the held database lock"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            tx.commit().unwrap();
            assert!(matches!(
                worker.join().unwrap(),
                Err(StoreError::ImmutableConflict)
            ));
        });
        assert_eq!(
            store.get_project(&child.id).unwrap().unwrap().parent,
            Some(replacement.id)
        );
        assert_eq!(
            store
                .get_invocation(&child.id, running.id.as_str())
                .unwrap()
                .unwrap()
                .status,
            ark_core::InvocationStatus::Running
        );
        assert!(store.get_record("receipt", "race").unwrap().is_none());
        assert!(store
            .load_output_publication(&child.id, running.id.as_str())
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn guarded_output_publication_refuses_replaced_blob() {
        let (_dir, store) = store();
        let project = ProjectId("publication-blob".into());
        let kinds = ["authority"];
        store
            .register_record_watch(project.as_str(), &kinds)
            .unwrap();
        let blob = store.put_blob(b"output").unwrap();
        let mut running =
            Invocation::new("fixture.export", RiskTier::Mutate, serde_json::json!({}));
        running.status = ark_core::InvocationStatus::Running;
        store.put_invocation(&project, &running).unwrap();
        let artifact = Artifact::new(
            blob.clone(),
            ArtifactKind::Binary,
            "output",
            6,
            Provenance::Produced {
                invocation: running.id.clone(),
            },
        );
        let mut finished = running.clone();
        finished.produced.push(blob.clone());
        finished.mark_ok(serde_json::json!({}));
        let prepared = store
            .prepare_output_publication(&project, &running, &finished, &[artifact])
            .unwrap();
        let path = store.object_path(&blob);
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, b"badxxx").unwrap();
        std::fs::rename(replacement, path).unwrap();
        let receipt = serde_json::json!({"ok": true});
        let updates = [RecordUpdate {
            kind: "receipt",
            id: "blob",
            scope: project.as_str(),
            expected: None,
            value: &receipt,
        }];
        let epoch = store
            .watched_record_epoch(project.as_str(), &kinds)
            .unwrap();
        assert!(store
            .compare_put_records_with_publication(
                &updates,
                &[],
                &[],
                &[],
                OutputPublicationCommit {
                    limits: RecordCompareLimits {
                        max_bytes: 8192,
                        max_total_bytes: 32768,
                        max_records: 8
                    },
                    guards: RecordCompareGuards {
                        immutable_epoch: None,
                        record_epoch: RecordMutationEpoch {
                            scope: project.as_str(),
                            kinds: &kinds,
                            value: epoch
                        }
                    },
                    publication: &prepared,
                    receipt_keys: &[("receipt", "blob")],
                },
            )
            .is_err());
        assert!(store
            .load_output_publication(&project, running.id.as_str())
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_first_blob_writers_share_the_same_object() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let bytes = std::sync::Arc::new(vec![42u8; 2 * 1024 * 1024]);
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let a_bytes = bytes.clone();
        let a_start = start.clone();
        let a = std::thread::spawn(move || {
            a_start.wait();
            first.put_blob(&a_bytes)
        });
        let b_bytes = bytes.clone();
        let b_start = start.clone();
        let b = std::thread::spawn(move || {
            b_start.wait();
            second.put_blob(&b_bytes)
        });
        start.wait();
        let a = a.join().unwrap().unwrap();
        let b = b.join().unwrap().unwrap();
        assert_eq!(a, b);
        assert_eq!(
            Store::open(dir.path()).unwrap().read_blob(&a).unwrap(),
            *bytes
        );
    }

    #[test]
    fn output_publication_exit_child() {
        let Ok(root) = std::env::var("ARK_TEST_OUTPUT_EXIT_ROOT") else {
            return;
        };
        let mode = std::env::var("ARK_TEST_OUTPUT_EXIT_MODE").unwrap();
        let invocation_id = std::env::var("ARK_TEST_OUTPUT_EXIT_INVOCATION").unwrap();
        let store = Store::open(root).unwrap();
        let project = ProjectId("exit-publication".into());
        let running = store
            .get_invocation(&project, &invocation_id)
            .unwrap()
            .unwrap();
        let blob = store.put_blob(b"durable-output").unwrap();
        let artifact = Artifact::new(
            blob.clone(),
            ArtifactKind::Binary,
            "output",
            14,
            Provenance::Produced {
                invocation: running.id.clone(),
            },
        );
        let mut finished = running.clone();
        finished.produced.push(blob);
        finished.mark_ok(serde_json::json!({"ok": true}));
        let prepared = store
            .prepare_output_publication(&project, &running, &finished, &[artifact])
            .unwrap();
        if mode == "before" {
            std::process::exit(0);
        }
        let receipt = serde_json::json!({"request": "exact"});
        let updates = [RecordUpdate {
            kind: "receipt",
            id: "exit",
            scope: project.as_str(),
            expected: None,
            value: &receipt,
        }];
        let kinds = ["authority"];
        let epoch = store
            .watched_record_epoch(project.as_str(), &kinds)
            .unwrap();
        assert!(store
            .compare_put_records_with_publication(
                &updates,
                &[],
                &[],
                &[],
                OutputPublicationCommit {
                    limits: RecordCompareLimits {
                        max_bytes: 8192,
                        max_total_bytes: 32768,
                        max_records: 8
                    },
                    guards: RecordCompareGuards {
                        immutable_epoch: None,
                        record_epoch: RecordMutationEpoch {
                            scope: project.as_str(),
                            kinds: &kinds,
                            value: epoch
                        }
                    },
                    publication: &prepared,
                    receipt_keys: &[("receipt", "exit")],
                },
            )
            .unwrap());
        std::process::exit(0);
    }

    #[test]
    fn output_publication_recovers_before_and_after_process_exit() {
        for mode in ["before", "after"] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            let project = ProjectId("exit-publication".into());
            store
                .register_record_watch(project.as_str(), &["authority"])
                .unwrap();
            let mut running = Invocation::new(
                "fixture.export",
                RiskTier::Mutate,
                serde_json::json!({"request": "exact"}),
            );
            running.status = ark_core::InvocationStatus::Running;
            store.put_invocation(&project, &running).unwrap();
            drop(store);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::output_publication_exit_child",
                    "--nocapture",
                ])
                .env("ARK_TEST_OUTPUT_EXIT_ROOT", dir.path())
                .env("ARK_TEST_OUTPUT_EXIT_MODE", mode)
                .env("ARK_TEST_OUTPUT_EXIT_INVOCATION", running.id.as_str())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("publication child timed out");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let child = child.wait_with_output().unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stderr)
            );
            let reopened = Store::open(dir.path()).unwrap();
            let loaded = reopened
                .load_output_publication(&project, running.id.as_str())
                .unwrap();
            if mode == "before" {
                assert!(loaded.is_none());
                assert_eq!(
                    reopened
                        .get_invocation(&project, running.id.as_str())
                        .unwrap()
                        .unwrap()
                        .status,
                    ark_core::InvocationStatus::Running
                );
                assert!(reopened.get_record("receipt", "exit").unwrap().is_none());
                assert!(reopened
                    .get_artifact(&project, &ArtifactId::of_bytes(b"durable-output"))
                    .unwrap()
                    .is_none());
                let count: i64 = reopened.db.lock().unwrap().query_row(
                    "SELECT count(*) FROM invocation_outputs WHERE project_id=?1 AND invocation_id=?2",
                    rusqlite::params![project.as_str(), running.id.as_str()], |row| row.get(0),
                ).unwrap();
                assert_eq!(count, 0);
            } else {
                let loaded = loaded.unwrap();
                assert_eq!(loaded.invocation.input, running.input);
                assert_eq!(
                    loaded.receipts.as_ref().unwrap()[0].2,
                    serde_json::json!({"request": "exact"})
                );
                assert_eq!(
                    reopened
                        .read_blob_bounded(&loaded.outputs[0].id, 64)
                        .unwrap(),
                    b"durable-output"
                );
            }
        }
    }

    #[test]
    fn watched_record_epoch_fences_other_connections_and_replacements() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let kinds = ["turn", "proposal"];
        assert!(first.watched_record_epoch("p", &kinds).is_err());
        first.register_record_watch("p", &kinds).unwrap();
        let initial = second.watched_record_epoch("p", &kinds).unwrap();
        second
            .put_record("unwatched", "x", "p", &serde_json::json!(1))
            .unwrap();
        assert_eq!(first.watched_record_epoch("p", &kinds).unwrap(), initial);
        second
            .put_record("turn", "t", "p", &serde_json::json!(1))
            .unwrap();
        let inserted = first.watched_record_epoch("p", &kinds).unwrap();
        assert!(inserted > initial);
        let value = serde_json::json!("candidate");
        let update = [RecordUpdate {
            kind: "result",
            id: "r",
            scope: "p",
            expected: None,
            value: &value,
        }];
        let guards = || RecordCompareGuards {
            immutable_epoch: None,
            record_epoch: RecordMutationEpoch {
                scope: "p",
                kinds: &kinds,
                value: initial,
            },
        };
        assert!(!first
            .compare_put_records_with_guards(
                &update,
                &[],
                &[],
                &[],
                RecordCompareLimits {
                    max_bytes: 1024,
                    max_total_bytes: 4096,
                    max_records: 4,
                },
                guards()
            )
            .unwrap());
        let guards = RecordCompareGuards {
            immutable_epoch: None,
            record_epoch: RecordMutationEpoch {
                scope: "p",
                kinds: &kinds,
                value: inserted,
            },
        };
        assert!(first
            .compare_put_records_with_guards(
                &update,
                &[],
                &[],
                &[],
                RecordCompareLimits {
                    max_bytes: 1024,
                    max_total_bytes: 4096,
                    max_records: 4,
                },
                guards
            )
            .unwrap());
        second
            .put_record("turn", "t", "other", &serde_json::json!(2))
            .unwrap();
        let replaced = first.watched_record_epoch("p", &kinds).unwrap();
        assert!(replaced > inserted);
        second
            .put_record("turn", "t", "p", &serde_json::json!(1))
            .unwrap();
        assert!(first.watched_record_epoch("p", &kinds).unwrap() > replaced);
        let before_rollback = first.watched_record_epoch("p", &kinds).unwrap();
        {
            let mut db = second.db.lock().unwrap();
            let tx = db.transaction().unwrap();
            tx.execute("INSERT INTO records(kind,id,scope,updated_at,json) VALUES('proposal','rollback','p','now','{}')", []).unwrap();
            tx.rollback().unwrap();
        }
        assert_eq!(
            first.watched_record_epoch("p", &kinds).unwrap(),
            before_rollback
        );
        assert!(second.delete_record("turn", "t").unwrap());
        assert!(first.watched_record_epoch("p", &kinds).unwrap() > before_rollback);
    }

    #[test]
    fn watched_record_epoch_saturation_keeps_writes_available() {
        let (_dir, store) = store();
        let kinds = ["turn"];
        store.register_record_watch("p", &kinds).unwrap();
        {
            let db = store.db.lock().unwrap();
            db.execute_batch("DROP TRIGGER watched_epoch_monotonic;
                UPDATE watched_record_epochs SET value=9223372036854775806 WHERE scope='p';
                CREATE TRIGGER watched_epoch_monotonic BEFORE UPDATE ON watched_record_epochs
                WHEN NEW.scope != OLD.scope OR OLD.exhausted != 0 OR OLD.value >= 9223372036854775807
                  OR NEW.value != OLD.value+1 OR NEW.exhausted != CASE WHEN NEW.value=9223372036854775807 THEN 1 ELSE 0 END
                BEGIN SELECT RAISE(ABORT, 'invalid watched epoch'); END;").unwrap();
        }
        let before = store.watched_record_epoch("p", &kinds).unwrap();
        assert!(matches!(
            store.register_record_watch("p", &["proposal"]),
            Err(StoreError::RecordEpochExhausted)
        ));
        assert_eq!(store.watched_record_epoch("p", &kinds).unwrap(), before);
        assert!(store.watched_record_epoch("p", &["proposal"]).is_err());
        store
            .put_record("turn", "t", "p", &serde_json::json!("stop"))
            .unwrap();
        assert!(matches!(
            store.watched_record_epoch("p", &kinds),
            Err(StoreError::RecordEpochExhausted)
        ));
        store
            .put_record("turn", "t", "p", &serde_json::json!("stop again"))
            .unwrap();
        assert!(store.delete_record("turn", "t").unwrap());
        let value = serde_json::json!(1);
        let update = [RecordUpdate {
            kind: "result",
            id: "r",
            scope: "p",
            expected: None,
            value: &value,
        }];
        let result = store.compare_put_records_with_guards(
            &update,
            &[],
            &[],
            &[],
            RecordCompareLimits {
                max_bytes: 1024,
                max_total_bytes: 4096,
                max_records: 4,
            },
            RecordCompareGuards {
                immutable_epoch: None,
                record_epoch: RecordMutationEpoch {
                    scope: "p",
                    kinds: &kinds,
                    value: before,
                },
            },
        );
        assert!(matches!(result, Err(StoreError::RecordEpochExhausted)));
    }

    #[test]
    fn watched_record_epoch_guards_large_record_sets_without_per_record_checks() {
        let (_dir, store) = store();
        let kinds = ["turn", "proposal"];
        store.register_record_watch("p", &kinds).unwrap();
        let ids: Vec<String> = (0..20_000).map(|id| format!("record-{id}")).collect();
        let value = serde_json::json!({"state": "ready"});
        let records: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    if index % 2 == 0 { "turn" } else { "proposal" },
                    id.as_str(),
                    "p",
                    &value,
                )
            })
            .collect();
        store.put_records(&records).unwrap();
        let epoch = store.watched_record_epoch("p", &kinds).unwrap();
        let output = serde_json::json!("committed");
        let updates = [RecordUpdate {
            kind: "result",
            id: "result",
            scope: "p",
            expected: None,
            value: &output,
        }];
        assert!(store
            .compare_put_records_with_guards(
                &updates,
                &[],
                &[],
                &[],
                RecordCompareLimits {
                    max_bytes: 1024,
                    max_total_bytes: 4096,
                    max_records: 4,
                },
                RecordCompareGuards {
                    immutable_epoch: None,
                    record_epoch: RecordMutationEpoch {
                        scope: "p",
                        kinds: &kinds,
                        value: epoch,
                    }
                }
            )
            .unwrap());
        store
            .put_record("turn", "record-0", "p", &serde_json::json!("changed"))
            .unwrap();
        let second = [RecordUpdate {
            kind: "result",
            id: "other",
            scope: "p",
            expected: None,
            value: &output,
        }];
        assert!(!store
            .compare_put_records_with_guards(
                &second,
                &[],
                &[],
                &[],
                RecordCompareLimits {
                    max_bytes: 1024,
                    max_total_bytes: 4096,
                    max_records: 4,
                },
                RecordCompareGuards {
                    immutable_epoch: None,
                    record_epoch: RecordMutationEpoch {
                        scope: "p",
                        kinds: &kinds,
                        value: epoch,
                    }
                }
            )
            .unwrap());
    }

    #[test]
    #[ignore = "local watched-record overhead measurement"]
    fn watched_record_epoch_unwatched_write_overhead() {
        let (_baseline_dir, baseline) = store();
        let (_candidate_dir, candidate) = store();
        {
            let db = baseline.db.lock().unwrap();
            db.execute_batch(
                "DROP TRIGGER watched_record_before_insert;
                DROP TRIGGER watched_record_after_insert;
                DROP TRIGGER watched_record_before_update;
                DROP TRIGGER watched_record_after_update;
                DROP TRIGGER watched_record_after_delete;",
            )
            .unwrap();
        }
        let value = serde_json::json!({"state": "ready"});
        for round in 0..3 {
            for (name, store) in [("baseline", &baseline), ("watched-schema", &candidate)] {
                let start = std::time::Instant::now();
                for id in 0..1_000 {
                    store
                        .put_record("unwatched", &format!("{round}-{id}"), "project", &value)
                        .unwrap();
                }
                eprintln!("{name} round {round}: {} ms", start.elapsed().as_millis());
            }
        }
    }

    #[test]
    fn watched_record_epoch_tracks_scope_changes_and_protects_schema() {
        let dir = tempfile::tempdir().unwrap();
        let old_connection = Connection::open(dir.path().join("meta.db")).unwrap();
        let store = Store::open(dir.path()).unwrap();
        let kinds = ["turn"];
        store.register_record_watch("p", &kinds).unwrap();
        store.register_record_watch("q", &kinds).unwrap();
        let p0 = store.watched_record_epoch("p", &kinds).unwrap();
        let q0 = store.watched_record_epoch("q", &kinds).unwrap();
        old_connection.execute("INSERT INTO records(kind,id,scope,updated_at,json) VALUES('turn','t','p','now','{}')", []).unwrap();
        let p1 = store.watched_record_epoch("p", &kinds).unwrap();
        assert!(p1 > p0);
        assert_eq!(store.watched_record_epoch("q", &kinds).unwrap(), q0);
        old_connection
            .execute(
                "UPDATE records SET scope='q' WHERE kind='turn' AND id='t'",
                [],
            )
            .unwrap();
        let p2 = store.watched_record_epoch("p", &kinds).unwrap();
        let q1 = store.watched_record_epoch("q", &kinds).unwrap();
        assert!(p2 > p1 && q1 > q0);
        old_connection.execute("INSERT OR REPLACE INTO records(kind,id,scope,updated_at,json) VALUES('turn','t','p','now','{}')", []).unwrap();
        assert!(store.watched_record_epoch("p", &kinds).unwrap() > p2);
        assert!(store.watched_record_epoch("q", &kinds).unwrap() > q1);
        assert!(old_connection
            .execute("DELETE FROM watched_record_epochs WHERE scope='p'", [])
            .is_err());
        assert!(old_connection.execute("INSERT OR REPLACE INTO watched_record_epochs(scope,value,exhausted) VALUES('p',0,0)", []).is_err());
        assert!(old_connection
            .execute("DELETE FROM watched_record_kinds WHERE scope='p'", [])
            .is_err());
        assert!(old_connection
            .execute(
                "UPDATE watched_record_epochs SET value=0 WHERE scope='p'",
                []
            )
            .is_err());
        old_connection
            .execute_batch("DROP TRIGGER watched_record_after_delete")
            .unwrap();
        assert!(store.watched_record_epoch("p", &kinds).is_err());
        assert!(Store::open(dir.path()).is_err());
    }

    #[test]
    #[ignore = "child process for watched record epoch test"]
    fn watched_record_epoch_writer_child() {
        let Ok(root) = std::env::var("ARK_WATCHED_EPOCH_CHILD_ROOT") else {
            return;
        };
        let store = Store::open(root).unwrap();
        store
            .put_record("turn", "child-turn", "p", &serde_json::json!("stopped"))
            .unwrap();
    }

    #[test]
    fn watched_record_epoch_fences_another_process() {
        let (dir, store) = store();
        let kinds = ["turn", "proposal"];
        store.register_record_watch("p", &kinds).unwrap();
        let before = store.watched_record_epoch("p", &kinds).unwrap();
        drop(store);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::watched_record_epoch_writer_child",
                "--ignored",
            ])
            .env("ARK_WATCHED_EPOCH_CHILD_ROOT", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed")
        );
        let store = Store::open(dir.path()).unwrap();
        assert!(store.watched_record_epoch("p", &kinds).unwrap() > before);
        let value = serde_json::json!("commit");
        let updates = [RecordUpdate {
            kind: "result",
            id: "r",
            scope: "p",
            expected: None,
            value: &value,
        }];
        assert!(!store
            .compare_put_records_with_guards(
                &updates,
                &[],
                &[],
                &[],
                RecordCompareLimits {
                    max_bytes: 1024,
                    max_total_bytes: 4096,
                    max_records: 4,
                },
                RecordCompareGuards {
                    immutable_epoch: None,
                    record_epoch: RecordMutationEpoch {
                        scope: "p",
                        kinds: &kinds,
                        value: before,
                    }
                }
            )
            .unwrap());
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
    fn scoped_record_read_enforces_encoded_byte_limit() {
        let (_d, s) = store();
        let value = serde_json::json!({ "text": "世界" });
        let size = serde_json::to_string(&value).unwrap().len();
        s.put_record("design", "one", "project", &value).unwrap();
        assert_eq!(
            s.get_record_in_scope_bounded("design", "one", "project", size)
                .unwrap(),
            Some(value)
        );
        assert!(matches!(
            s.get_record_in_scope_bounded("design", "one", "project", size - 1),
            Err(StoreError::RecordTooLarge)
        ));
        assert!(s
            .get_record_in_scope_bounded("design", "one", "other", size)
            .unwrap()
            .is_none());
        assert!(s
            .get_record_in_scope_bounded("design", "missing", "project", size)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn store_location_detects_root_and_database_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store");
        drop(Store::open(&path).unwrap());
        let store = Store::open_guarded(&path).unwrap();
        assert!(store.verify_location().is_ok());
        std::fs::rename(path.join("meta.db"), path.join("old.db")).unwrap();
        std::fs::write(path.join("meta.db"), b"replacement").unwrap();
        assert!(StoreLocation::verify_connection(&store.db.lock().unwrap()).is_err());
        assert!(store.verify_location().is_err());
        let second = root.path().join("second");
        drop(Store::open(&second).unwrap());
        let store = Store::open_guarded(&second).unwrap();
        std::fs::rename(&second, root.path().join("old-second")).unwrap();
        std::fs::create_dir(&second).unwrap();
        assert!(store.verify_location().is_err());
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn sqlite_shared_lock_probe_child() {
        use std::os::fd::AsRawFd;
        let path = std::env::var_os("ARK_STORE_LOCK_PROBE_PATH").unwrap();
        let expected = std::env::var("ARK_STORE_LOCK_PROBE_EXPECT").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = 0x4000_0002;
        lock.l_len = 510;
        let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) };
        if expected == "available" {
            assert_eq!(status, 0);
        } else {
            assert_eq!(status, -1);
            assert!(matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EACCES) | Some(libc::EAGAIN)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn guarded_close_preserves_other_sqlite_connections_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .put_record("fixture", "one", "project", &serde_json::json!(1))
            .unwrap();
        let db = store.db.lock().unwrap();
        db.execute_batch("BEGIN; SELECT count(*) FROM records;")
            .unwrap();
        let probe = |expected: &str| {
            let module = module_path!().split_once("::").unwrap().1;
            let name = format!("{module}::sqlite_shared_lock_probe_child");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--ignored"])
                .env("ARK_STORE_LOCK_PROBE_PATH", dir.path().join("meta.db"))
                .env("ARK_STORE_LOCK_PROBE_EXPECT", expected)
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        probe("blocked");
        drop(Store::open_guarded(dir.path()).unwrap());
        probe("blocked");
        drop(std::fs::File::open(dir.path().join("meta.db")).unwrap());
        probe("available");
        db.execute_batch("ROLLBACK;").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn workspace_fence_refuses_project_mutation_from_another_connection() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let mut project = Project::new("Fence", "fixture");
        first.put_project(&project).unwrap();
        let fence = first.try_workspace_fence().unwrap();
        let mut unrelated = Project::new("Unrelated", "fixture");
        second.put_project(&unrelated).unwrap();
        unrelated.target = "updated".into();
        second.put_project(&unrelated).unwrap();
        project.workspace = Some("/changed".into());
        assert!(matches!(
            second.put_project(&project),
            Err(StoreError::ProjectBusy)
        ));
        assert!(second
            .get_project(&project.id)
            .unwrap()
            .unwrap()
            .workspace
            .is_none());
        drop(fence);
        second.put_project(&project).unwrap();
        assert_eq!(
            second
                .get_project(&project.id)
                .unwrap()
                .unwrap()
                .workspace
                .as_deref(),
            Some("/changed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_fence_detects_path_replacement() {
        let (dir, store) = store();
        let fence = store.try_workspace_fence().unwrap();
        std::fs::rename(
            dir.path().join("workspace.fence"),
            dir.path().join("old.fence"),
        )
        .unwrap();
        std::fs::write(dir.path().join("workspace.fence"), b"replacement").unwrap();
        assert!(store.verify_workspace_fence(&fence).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stale_project_edits_do_not_restore_old_workspace_or_parent() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let original = Project::new("Original", "fixture");
        first.put_project(&original).unwrap();
        let mut stale = original.clone();
        stale.name = "Renamed".into();
        let mut moved = original.clone();
        moved.workspace = Some("/changed".into());
        let result = first.put_project_after_read(&stale, || second.put_project(&moved).unwrap());
        assert!(matches!(result, Err(StoreError::ProjectLocationChanged)));
        assert_eq!(
            second
                .get_project(&original.id)
                .unwrap()
                .unwrap()
                .workspace
                .as_deref(),
            Some("/changed")
        );

        let original = Project::new("Another", "fixture");
        first.put_project(&original).unwrap();
        let mut stale = original.clone();
        stale.name = "Another renamed".into();
        let mut reparented = original.clone();
        reparented.parent = Some(ProjectId::new());
        let result =
            first.put_project_after_read(&stale, || second.put_project(&reparented).unwrap());
        assert!(matches!(result, Err(StoreError::ProjectLocationChanged)));
        assert_eq!(
            second.get_project(&original.id).unwrap().unwrap().parent,
            reparented.parent
        );
    }

    #[cfg(unix)]
    #[test]
    fn guarded_store_refuses_linked_database() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        let linked = root.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        symlink(&outside, linked.join("meta.db")).unwrap();
        assert!(Store::open_guarded(&linked).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        std::fs::remove_file(linked.join("meta.db")).unwrap();
        std::fs::hard_link(&outside, linked.join("meta.db")).unwrap();
        assert!(Store::open_guarded(&linked).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    }

    #[test]
    fn checked_compare_put_keeps_checked_records_unchanged() {
        let (_dir, store) = store();
        let original = serde_json::json!({"revision": 1});
        let next = serde_json::json!({"revision": 2});
        store.put_record("guard", "one", "p", &original).unwrap();
        let checks = [
            RecordCheck {
                kind: "guard",
                id: "one",
                scope: "p",
                expected: Some(&original),
            },
            RecordCheck {
                kind: "legacy",
                id: "absent",
                scope: "p",
                expected: None,
            },
        ];
        let updates = [RecordUpdate {
            kind: "index",
            id: "one",
            scope: "p",
            expected: None,
            value: &next,
        }];
        assert!(store
            .compare_put_records_checked(&updates, &checks, &[], 1024, 4096, None)
            .unwrap());
        assert_eq!(
            store.get_record("guard", "one").unwrap(),
            Some(original.clone())
        );
        assert_eq!(store.get_record("legacy", "absent").unwrap(), None);
        let foreign = [RecordCheck {
            kind: "guard",
            id: "one",
            scope: "foreign",
            expected: Some(&original),
        }];
        let next_update = [RecordUpdate {
            kind: "index",
            id: "one",
            scope: "p",
            expected: Some(&next),
            value: &original,
        }];
        assert!(!store
            .compare_put_records_checked(&next_update, &foreign, &[], 1024, 4096, None)
            .unwrap());
        assert_eq!(store.get_record("index", "one").unwrap(), Some(next));
    }

    #[test]
    fn checked_empty_scope_refuses_orphans_atomically() {
        let (_dir, store) = store();
        let value = serde_json::json!({"version": 1});
        store.put_record("journal", "old", "p", &value).unwrap();
        let updates = [RecordUpdate {
            kind: "format",
            id: "one",
            scope: "p",
            expected: None,
            value: &value,
        }];
        let scopes = [EmptyRecordScope {
            kind: "journal",
            scope: "p",
        }];
        let limits = || RecordCompareLimits {
            max_bytes: 1024,
            max_total_bytes: 4096,
            max_records: 1024,
        };
        assert!(!store
            .compare_put_records_checked_limit(&updates, &[], &scopes, &[], limits(), None)
            .unwrap());
        assert_eq!(store.get_record("format", "one").unwrap(), None);
        let other = [EmptyRecordScope {
            kind: "journal",
            scope: "other",
        }];
        assert!(store
            .compare_put_records_checked_limit(&updates, &[], &other, &[], limits(), None)
            .unwrap());
    }

    #[test]
    fn checked_empty_scope_serializes_orphan_and_format_claim() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_start = start.clone();
        let orphan = std::thread::spawn(move || {
            let value = serde_json::json!({"version": 1});
            let update = [RecordUpdate {
                kind: "journal",
                id: "old",
                scope: "p",
                expected: None,
                value: &value,
            }];
            let check = [RecordCheck {
                kind: "format",
                id: "one",
                scope: "p",
                expected: None,
            }];
            first_start.wait();
            first
                .compare_put_records_checked(&update, &check, &[], 1024, 4096, None)
                .unwrap()
        });
        let claim = std::thread::spawn(move || {
            let value = serde_json::json!({"version": 2});
            let update = [RecordUpdate {
                kind: "format",
                id: "one",
                scope: "p",
                expected: None,
                value: &value,
            }];
            let scopes = [EmptyRecordScope {
                kind: "journal",
                scope: "p",
            }];
            start.wait();
            second
                .compare_put_records_checked_limit(
                    &update,
                    &[],
                    &scopes,
                    &[],
                    RecordCompareLimits {
                        max_bytes: 1024,
                        max_total_bytes: 4096,
                        max_records: 1024,
                    },
                    None,
                )
                .unwrap()
        });
        assert_ne!(orphan.join().unwrap(), claim.join().unwrap());
    }

    #[test]
    fn checked_absence_serializes_independent_connections() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let run = |store: Store,
                   write_kind: &'static str,
                   check_kind: &'static str,
                   start: std::sync::Arc<std::sync::Barrier>| {
            std::thread::spawn(move || {
                let value = serde_json::json!({"version": 1});
                let updates = [RecordUpdate {
                    kind: write_kind,
                    id: "one",
                    scope: "p",
                    expected: None,
                    value: &value,
                }];
                let checks = [RecordCheck {
                    kind: check_kind,
                    id: "one",
                    scope: "p",
                    expected: None,
                }];
                start.wait();
                store
                    .compare_put_records_checked(&updates, &checks, &[], 1024, 4096, None)
                    .unwrap()
            })
        };
        let one = run(first, "legacy", "format", start.clone());
        let two = run(second, "format", "legacy", start);
        assert_ne!(one.join().unwrap(), two.join().unwrap());
    }

    #[test]
    fn checked_compare_put_refuses_conflicts_limits_and_epoch_changes() {
        let (_dir, store) = store();
        let old = serde_json::json!({"revision": 1});
        let next = serde_json::json!({"revision": 2});
        store.put_record("guard", "one", "p", &old).unwrap();
        store.put_record("foreign", "one", "other", &old).unwrap();
        let updates = [RecordUpdate {
            kind: "target",
            id: "one",
            scope: "p",
            expected: None,
            value: &next,
        }];
        let stale = [RecordCheck {
            kind: "guard",
            id: "one",
            scope: "p",
            expected: Some(&next),
        }];
        assert!(!store
            .compare_put_records_checked(&updates, &stale, &[], 1024, 4096, None)
            .unwrap());
        let foreign_absent = [RecordCheck {
            kind: "foreign",
            id: "one",
            scope: "p",
            expected: None,
        }];
        assert!(!store
            .compare_put_records_checked(&updates, &foreign_absent, &[], 1024, 4096, None)
            .unwrap());
        let duplicate = [RecordCheck {
            kind: "target",
            id: "one",
            scope: "p",
            expected: None,
        }];
        assert!(!store
            .compare_put_records_checked(&updates, &duplicate, &[], 1024, 4096, None)
            .unwrap());
        let derived = [DerivedRecord {
            kind: "guard",
            id: "one",
            scope: "p",
            value: &next,
        }];
        let check = [RecordCheck {
            kind: "guard",
            id: "one",
            scope: "p",
            expected: Some(&old),
        }];
        assert!(!store
            .compare_put_records_checked(&updates, &check, &derived, 1024, 4096, None)
            .unwrap());
        assert!(matches!(
            store.compare_put_records_checked(&updates, &check, &[], 1024, 8, None),
            Err(StoreError::RecordTooLarge)
        ));
        let too_many: Vec<_> = (0..1024)
            .map(|_| RecordCheck {
                kind: "guard",
                id: "one",
                scope: "p",
                expected: Some(&old),
            })
            .collect();
        assert!(matches!(
            store.compare_put_records_checked(&updates, &too_many, &[], 1024, 1_000_000, None),
            Err(StoreError::RecordTooLarge)
        ));
        assert!(!store
            .compare_put_records_checked(
                &updates,
                &check,
                &[],
                1024,
                4096,
                Some(ImmutableEpoch {
                    scope: "p",
                    value: 1
                })
            )
            .unwrap());
        assert!(store
            .compare_put_records_checked(
                &updates,
                &check,
                &[],
                1024,
                4096,
                Some(ImmutableEpoch {
                    scope: "other",
                    value: 0
                })
            )
            .is_err());
        assert_eq!(store.get_record("target", "one").unwrap(), None);
    }

    #[test]
    fn scoped_compare_put_is_atomic_and_refuses_stale_or_foreign_records() {
        let (_d, s) = store();
        let a = serde_json::json!({"revision": 1});
        let b = serde_json::json!({"revision": 2});
        let init = [
            RecordUpdate {
                kind: "design",
                id: "one",
                scope: "p",
                expected: None,
                value: &a,
            },
            RecordUpdate {
                kind: "design",
                id: "two",
                scope: "p",
                expected: None,
                value: &a,
            },
        ];
        assert!(s.compare_put_records(&init, 1024, 4096).unwrap());
        assert!(!s.compare_put_records(&init, 1024, 4096).unwrap());
        let stale = [
            RecordUpdate {
                kind: "design",
                id: "one",
                scope: "p",
                expected: Some(&a),
                value: &b,
            },
            RecordUpdate {
                kind: "design",
                id: "two",
                scope: "p",
                expected: Some(&b),
                value: &b,
            },
        ];
        assert!(!s.compare_put_records(&stale, 1024, 4096).unwrap());
        assert_eq!(s.get_record("design", "one").unwrap(), Some(a.clone()));
        let foreign = [RecordUpdate {
            kind: "design",
            id: "one",
            scope: "q",
            expected: Some(&a),
            value: &b,
        }];
        assert!(!s.compare_put_records(&foreign, 1024, 4096).unwrap());
        assert_eq!(s.get_record("design", "one").unwrap(), Some(a.clone()));
        assert!(matches!(
            s.compare_put_records(&foreign, 1, 4096),
            Err(StoreError::RecordTooLarge)
        ));
        assert!(matches!(
            s.compare_put_records(&foreign, 1024, 1),
            Err(StoreError::RecordTooLarge)
        ));
    }

    #[test]
    fn bounded_bulk_claims_are_atomic() {
        let (_dir, store) = store();
        let old = serde_json::json!(0);
        let next = serde_json::json!(1);
        store.put_record("guard", "one", "project", &old).unwrap();
        let ids: Vec<String> = (0..10_000).map(|id| format!("claim-{id}")).collect();
        store
            .put_record("claim", &ids[9_999], "foreign", &old)
            .unwrap();
        let mut updates = vec![RecordUpdate {
            kind: "guard",
            id: "one",
            scope: "project",
            expected: Some(&old),
            value: &next,
        }];
        updates.extend(ids.iter().map(|id| RecordUpdate {
            kind: "claim",
            id,
            scope: "project",
            expected: None,
            value: &next,
        }));
        assert!(matches!(
            store.compare_put_records(&updates, 1024, 1_000_000),
            Err(StoreError::RecordTooLarge)
        ));
        assert!(!store
            .compare_put_records_with_derived_limit(&updates, &[], 1024, 1_000_000, 10_001)
            .unwrap());
        assert_eq!(store.get_record("guard", "one").unwrap(), Some(old.clone()));
        assert_eq!(store.get_record("claim", &ids[0]).unwrap(), None);
        store.delete_record("claim", &ids[9_999]).unwrap();
        store.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_claim BEFORE INSERT ON records WHEN NEW.kind = 'claim' AND NEW.id = 'claim-5000' BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(store
            .compare_put_records_with_derived_limit(&updates, &[], 1024, 1_000_000, 10_001)
            .is_err());
        assert_eq!(store.get_record("guard", "one").unwrap(), Some(old.clone()));
        assert_eq!(store.get_record("claim", &ids[0]).unwrap(), None);
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_claim;")
            .unwrap();
        let duplicate = [DerivedRecord {
            kind: "guard",
            id: "one",
            scope: "project",
            value: &next,
        }];
        assert!(!store
            .compare_put_records_with_derived_limit(&updates, &duplicate, 1024, 1_000_000, 10_002)
            .unwrap());
        assert!(matches!(
            store.compare_put_records_with_derived_limit(&updates[..1], &[], 1024, 2, 1),
            Err(StoreError::RecordTooLarge)
        ));
        assert!(store
            .compare_put_records_with_derived_limit(&updates, &[], 1024, 1_000_000, 10_001)
            .unwrap());
        assert_eq!(
            store.get_record("guard", "one").unwrap(),
            Some(next.clone())
        );
        assert_eq!(store.get_record("claim", &ids[9_999]).unwrap(), Some(next));
        assert!(matches!(
            store.compare_put_records_with_derived_limit(&[], &[], 1024, 1024, 16_385),
            Err(StoreError::RecordTooLarge)
        ));
    }

    #[test]
    fn immutable_records_are_append_only_and_epoch_guards_commits() {
        let (_dir, store) = store();
        let first = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"first",
        };
        assert_eq!(store.immutable_epoch("project").unwrap(), 0);
        store.put_immutable_records(&[first], 512, 4096).unwrap();
        assert_eq!(store.immutable_epoch("project").unwrap(), 1);
        let retry = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"first",
        };
        store.put_immutable_records(&[retry], 512, 4096).unwrap();
        assert_eq!(store.immutable_epoch("project").unwrap(), 1);
        assert_eq!(
            store
                .get_immutable_record("nodes", "one", "project", 5)
                .unwrap(),
            Some(b"first".to_vec())
        );
        assert!(store
            .get_immutable_record("nodes", "one", "project", 4)
            .is_err());
        assert!(store
            .get_immutable_record("nodes", "one", "foreign", 100)
            .is_err());
        let conflict = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"other",
        };
        assert!(matches!(
            store.put_immutable_records(&[conflict], 512, 4096),
            Err(StoreError::ImmutableConflict)
        ));
        let db = store.db.lock().unwrap();
        assert!(db
            .execute(
                "UPDATE immutable_records SET bytes=X'00' WHERE kind='nodes'",
                []
            )
            .is_err());
        assert!(db
            .execute("DELETE FROM immutable_records WHERE kind='nodes'", [])
            .is_err());
        assert!(db.execute("INSERT OR REPLACE INTO immutable_records(kind,id,scope,bytes) VALUES('nodes','one','project',X'00')", []).is_err());
        assert!(db
            .execute(
                "UPDATE immutable_epochs SET epoch=0 WHERE scope='project'",
                []
            )
            .is_err());
        assert!(db
            .execute("DELETE FROM immutable_epochs WHERE scope='project'", [])
            .is_err());
        assert!(db
            .execute(
                "INSERT OR REPLACE INTO immutable_epochs(scope,epoch) VALUES('project',0)",
                []
            )
            .is_err());
        drop(db);
        let old = serde_json::json!(0);
        let next = serde_json::json!(1);
        store.put_record("guard", "one", "project", &old).unwrap();
        let update = [RecordUpdate {
            kind: "guard",
            id: "one",
            scope: "project",
            expected: Some(&old),
            value: &next,
        }];
        assert!(matches!(
            store.compare_put_records_at_immutable_epoch(
                &update,
                &[],
                1024,
                4096,
                ImmutableEpoch {
                    scope: "other",
                    value: 0
                }
            ),
            Err(StoreError::ImmutableConflict)
        ));
        assert!(matches!(
            store.compare_put_records_at_immutable_epoch(
                &update,
                &[],
                1024,
                1,
                ImmutableEpoch {
                    scope: "project",
                    value: 1
                }
            ),
            Err(StoreError::RecordTooLarge)
        ));
        let second = ImmutableRecord {
            kind: "nodes",
            id: "two",
            scope: "project",
            bytes: b"second",
        };
        store.put_immutable_records(&[second], 512, 4096).unwrap();
        assert!(!store
            .compare_put_records_at_immutable_epoch(
                &update,
                &[],
                1024,
                4096,
                ImmutableEpoch {
                    scope: "project",
                    value: 1
                }
            )
            .unwrap());
        assert_eq!(store.get_record("guard", "one").unwrap(), Some(old.clone()));
        assert!(store
            .compare_put_records_at_immutable_epoch(
                &update,
                &[],
                1024,
                4096,
                ImmutableEpoch {
                    scope: "project",
                    value: 2
                }
            )
            .unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn immutable_epoch_fences_a_second_guarded_connection() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();
        let second = Store::open_guarded(dir.path()).unwrap();
        let old = serde_json::json!(0);
        let next = serde_json::json!(1);
        first.put_record("guard", "one", "project", &old).unwrap();
        let observed = first.immutable_epoch("project").unwrap();
        let node = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"data",
        };
        second.put_immutable_records(&[node], 512, 4096).unwrap();
        let update = [RecordUpdate {
            kind: "guard",
            id: "one",
            scope: "project",
            expected: Some(&old),
            value: &next,
        }];
        assert!(!first
            .compare_put_records_at_immutable_epoch(
                &update,
                &[],
                1024,
                4096,
                ImmutableEpoch {
                    scope: "project",
                    value: observed
                }
            )
            .unwrap());
        assert_eq!(first.get_record("guard", "one").unwrap(), Some(old));
    }

    #[test]
    fn immutable_trigger_migration_replaces_a_previous_draft() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let db = Connection::open(dir.path().join("meta.db")).unwrap();
        db.execute_batch(
            "DELETE FROM immutable_schema; DROP TRIGGER immutable_records_epoch;
            CREATE TRIGGER immutable_records_epoch AFTER INSERT ON immutable_records BEGIN
                INSERT INTO immutable_epochs(scope,epoch) VALUES(NEW.scope,1)
                ON CONFLICT(scope) DO UPDATE SET epoch=epoch+1;
            END;",
        )
        .unwrap();
        drop(db);
        #[cfg(unix)]
        let guarded = Store::open_guarded(dir.path()).unwrap();
        #[cfg(unix)]
        assert!(guarded.immutable_epoch("project").is_err());
        let store = Store::open(dir.path()).unwrap();
        let first = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"one",
        };
        let second = ImmutableRecord {
            kind: "nodes",
            id: "two",
            scope: "project",
            bytes: b"two",
        };
        store.put_immutable_records(&[first], 512, 4096).unwrap();
        store.put_immutable_records(&[second], 512, 4096).unwrap();
        assert_eq!(store.immutable_epoch("project").unwrap(), 2);
        #[cfg(unix)]
        assert_eq!(guarded.immutable_epoch("project").unwrap(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn guarded_immutable_access_fails_closed_until_migration_commits() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let db = Connection::open(dir.path().join("meta.db")).unwrap();
        db.execute_batch("DROP TABLE immutable_records; DROP TABLE immutable_epochs; DROP TABLE immutable_schema;").unwrap();
        let guarded = Store::open_guarded(dir.path()).unwrap();
        assert!(guarded.immutable_epoch("project").is_err());
        db.execute_batch("BEGIN IMMEDIATE; CREATE TABLE immutable_epochs(scope TEXT PRIMARY KEY, epoch INTEGER NOT NULL);").unwrap();
        assert!(guarded.immutable_epoch("project").is_err());
        db.execute_batch("ROLLBACK;").unwrap();
        drop(db);
        drop(Store::open(dir.path()).unwrap());
        assert_eq!(guarded.immutable_epoch("project").unwrap(), 0);
        let node = ImmutableRecord {
            kind: "nodes",
            id: "one",
            scope: "project",
            bytes: b"data",
        };
        guarded.put_immutable_records(&[node], 512, 4096).unwrap();
        assert_eq!(guarded.immutable_epoch("project").unwrap(), 1);
    }

    #[test]
    fn derived_record_follows_guard_and_preserves_foreign_scope() {
        let (_d, s) = store();
        let old = serde_json::json!(1);
        let next = serde_json::json!(2);
        let foreign = serde_json::json!({"owner": "other"});
        s.put_record("guard", "a", "p", &old).unwrap();
        s.put_record("derived", "a", "p", &serde_json::json!("x".repeat(10000)))
            .unwrap();
        let update = [RecordUpdate {
            kind: "guard",
            id: "a",
            scope: "p",
            expected: Some(&old),
            value: &next,
        }];
        let derived = [DerivedRecord {
            kind: "derived",
            id: "a",
            scope: "p",
            value: &next,
        }];
        assert!(s
            .compare_put_records_with_derived(&update, &derived, 128, 512)
            .unwrap());
        assert_eq!(s.get_record("derived", "a").unwrap(), Some(next.clone()));
        assert!(!s
            .compare_put_records_with_derived(&update, &derived, 128, 512)
            .unwrap());
        s.put_record("derived", "a", "other", &foreign).unwrap();
        let update = [RecordUpdate {
            kind: "guard",
            id: "a",
            scope: "p",
            expected: Some(&next),
            value: &old,
        }];
        assert!(s
            .compare_put_records_with_derived(&update, &derived, 128, 512)
            .unwrap());
        assert_eq!(s.get_record("derived", "a").unwrap(), Some(foreign));
        assert!(matches!(
            s.compare_put_records_with_derived(&[], &derived, 128, 512),
            Err(StoreError::RecordTooLarge)
        ));
    }

    #[test]
    fn scoped_compare_put_serializes_independent_store_connections() {
        let (dir, first) = store();
        let second = Store::open(dir.path()).unwrap();
        let original = serde_json::json!({"revision": 0});
        first.put_record("design", "one", "p", &original).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let run = |store: Store, value: u64, start: std::sync::Arc<std::sync::Barrier>| {
            std::thread::spawn(move || {
                let expected = serde_json::json!({"revision": 0});
                let next = serde_json::json!({"revision": value});
                let update = [RecordUpdate {
                    kind: "design",
                    id: "one",
                    scope: "p",
                    expected: Some(&expected),
                    value: &next,
                }];
                start.wait();
                store.compare_put_records(&update, 1024, 4096).unwrap()
            })
        };
        let one = run(first, 1, start.clone());
        let two = run(second, 2, start);
        assert_ne!(one.join().unwrap(), two.join().unwrap());
    }

    #[test]
    fn scoped_compare_put_handles_large_existing_conflicts() {
        let (_dir, store) = store();
        store
            .put_record(
                "design",
                "large",
                "p",
                &serde_json::json!("x".repeat(1_000_000)),
            )
            .unwrap();
        let value = serde_json::Value::Null;
        let absent = [RecordUpdate {
            kind: "design",
            id: "large",
            scope: "p",
            expected: None,
            value: &value,
        }];
        assert!(!store
            .compare_put_records(&absent, 64 * 1024 * 1024, 4)
            .unwrap());
        let foreign = [RecordUpdate {
            kind: "design",
            id: "large",
            scope: "q",
            expected: Some(&value),
            value: &value,
        }];
        assert!(!store
            .compare_put_records(&foreign, 64 * 1024 * 1024, 8)
            .unwrap());
        let same_scope = [RecordUpdate {
            kind: "design",
            id: "large",
            scope: "p",
            expected: Some(&value),
            value: &value,
        }];
        assert!(!store
            .compare_put_records(&same_scope, 64 * 1024 * 1024, 8)
            .unwrap());
        assert!(matches!(
            store.compare_put_records(&same_scope, 8, 8),
            Err(StoreError::RecordTooLarge)
        ));
    }

    #[test]
    fn scoped_record_scan_is_ordered_bounded_and_excludes_other_scopes() {
        let (_dir, store) = store();
        let one = serde_json::json!("one");
        let two = serde_json::json!("two");
        store.put_record("scan", "b", "p", &two).unwrap();
        store.put_record("scan", "a", "p", &one).unwrap();
        store.put_record("scan", "c", "q", &one).unwrap();
        assert_eq!(
            store
                .next_record_in_scope_bounded("scan", "p", None, 1, 5)
                .unwrap(),
            Some(("a".into(), one.clone()))
        );
        assert_eq!(
            store
                .next_record_in_scope_bounded("scan", "p", Some("a"), 1, 5)
                .unwrap(),
            Some(("b".into(), two))
        );
        assert_eq!(
            store
                .next_record_in_scope_bounded("scan", "p", Some("b"), 1, 5)
                .unwrap(),
            None
        );
        assert!(matches!(
            store.next_record_in_scope_bounded("scan", "p", None, 1, 4),
            Err(StoreError::RecordTooLarge)
        ));
        store.put_record("scan", "aa", "p", &one).unwrap();
        assert!(matches!(
            store.next_record_in_scope_bounded("scan", "p", Some("a"), 1, 5),
            Err(StoreError::RecordTooLarge)
        ));
        store
            .db
            .lock()
            .unwrap()
            .execute("DROP INDEX records_by_scope_id", [])
            .unwrap();
        assert!(store
            .next_record_in_scope_bounded("scan", "p", None, 1, 5)
            .is_err());
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
    fn a_same_size_corrupt_blob_is_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let bytes = b"the whole chapter";
        let id = store.put_blob(bytes).unwrap();
        std::fs::write(store.object_path(&id), b"the wrong chapter").unwrap();
        assert!(store.read_blob(&id).is_err());
        assert_eq!(store.put_blob(bytes).unwrap(), id);
        assert_eq!(store.read_blob(&id).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn put_blob_refuses_a_symlink_at_the_content_address() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let bytes = b"the whole chapter";
        let id = ArtifactId::of_bytes(bytes);
        let outside = dir.path().join("outside");
        std::fs::write(&outside, bytes).unwrap();
        symlink(&outside, store.object_path(&id)).unwrap();
        assert!(store.read_blob(&id).is_err());
        assert!(store.put_blob(bytes).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_read_only_blob_remains_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let bytes = b"a read only blob";
        let id = store.put_blob(bytes).unwrap();
        std::fs::set_permissions(
            store.object_path(&id),
            std::fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        assert_eq!(store.put_blob(bytes).unwrap(), id);
    }

    #[test]
    fn blobs_are_deduplicated() {
        let (_d, s) = store();
        let a = s.put_blob(b"same bytes").unwrap();
        let b = s.put_blob(b"same bytes").unwrap();
        assert_eq!(a, b);
        assert!(s.has_blob(&a));
        assert_eq!(s.read_blob(&a).unwrap(), b"same bytes");
        assert!(s.read_blob_bounded(&a, 9).is_err());
        assert_eq!(s.read_blob_bounded(&a, 10).unwrap(), b"same bytes");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_a_blob_address_is_refused_without_waiting() {
        use std::os::unix::ffi::OsStrExt;
        if std::env::var_os("ARK_BLOB_FIFO_CHILD").is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::a_fifo_at_a_blob_address_is_refused_without_waiting",
                ])
                .env("ARK_BLOB_FIFO_CHILD", "1")
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    let output = child.wait_with_output().unwrap();
                    assert!(String::from_utf8(output.stdout)
                        .unwrap()
                        .contains("running 1 test"));
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("blob operation blocked on a FIFO");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let id = ArtifactId::of_bytes(b"fifo");
        let path = std::ffi::CString::new(store.object_path(&id).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(store.read_blob_bounded(&id, 1024).is_err());
        assert!(store.put_blob(b"fifo").is_err());
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
    fn interruption_does_not_overwrite_a_completed_invocation() {
        let (dir, store) = store();
        let other = Store::open(dir.path()).unwrap();
        let project = Project::new("recovery", "");
        store.put_project(&project).unwrap();
        let running = Invocation::new("fixture", ark_core::RiskTier::Read, serde_json::json!({}));
        store.put_invocation(&project.id, &running).unwrap();
        let observed = store
            .list_running_invocations_for_recovery()
            .unwrap()
            .pop()
            .unwrap();
        let mut completed = running.clone();
        completed.mark_ok(serde_json::json!({"result": "saved"}));
        other.put_invocation(&project.id, &completed).unwrap();
        assert!(!store
            .interrupt_invocation_if_unchanged(&observed, "interrupted")
            .unwrap());
        let current = store
            .get_invocation(&project.id, running.id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(current.status, ark_core::InvocationStatus::Ok);
        assert_eq!(current.output, serde_json::json!({"result": "saved"}));

        let second = Invocation::new("fixture", ark_core::RiskTier::Read, serde_json::json!({}));
        store.put_invocation(&project.id, &second).unwrap();
        let mut legacy = serde_json::to_value(&second).unwrap();
        legacy.as_object_mut().unwrap().remove("produced");
        legacy.as_object_mut().unwrap().remove("consumed");
        let raw = serde_json::to_string_pretty(&legacy).unwrap();
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE invocations SET json=?1 WHERE project_id=?2 AND id=?3",
                rusqlite::params![raw, project.id.as_str(), second.id.as_str()],
            )
            .unwrap();
        let observed = other
            .list_running_invocations_for_recovery()
            .unwrap()
            .pop()
            .unwrap();
        assert!(other
            .interrupt_invocation_if_unchanged(&observed, "interrupted")
            .unwrap());
        assert_eq!(
            store
                .get_invocation(&project.id, second.id.as_str())
                .unwrap()
                .unwrap()
                .status,
            ark_core::InvocationStatus::Failed
        );
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
    fn a_stored_turn_and_its_trace_hold_no_pasted_secret() {
        let (_d, s) = store();
        s.put_session_turn(
            "s",
            0,
            &serde_json::json!({
                "input": "DATABASE_URL=postgresql://u:p4ss@h/db",
                "answer": "I used the connection string"
            }),
        )
        .unwrap();
        let turn = &s.list_session_turns("s", None).unwrap()[0];
        assert!(!turn.to_string().contains("p4ss"), "{turn}");
        assert!(turn["answer"]
            .as_str()
            .unwrap()
            .contains("connection string"));

        s.put_turn_trace(
            "s",
            0,
            &serde_json::json!({
                "input": "run it",
                "events": [{"event": {"summary": "API_KEY=abc123"}}]
            }),
        )
        .unwrap();
        let trace = s.get_turn_trace("s", 0).unwrap().unwrap();
        assert!(!trace.to_string().contains("abc123"), "{trace}");

        s.put_session_turn(
            "s",
            1,
            &serde_json::json!({"input": "let password_len = 3;"}),
        )
        .unwrap();
        let turn = &s.list_session_turns("s", None).unwrap()[1];
        assert_eq!(turn["input"], "let password_len = 3;");
    }

    #[test]
    fn working_state_belongs_to_its_chat() {
        use ark_core::ResearchState;
        let (_d, s) = store();
        let p = Project::new("t", "");
        s.put_project(&p).unwrap();
        s.put_research_state(&p.id, "chat-1", &ResearchState::new("goal one"))
            .unwrap();
        assert!(s.get_research_state(&p.id, "chat-2").unwrap().is_none());
        s.put_research_state(&p.id, "chat-2", &ResearchState::new("goal two"))
            .unwrap();
        let one = s.get_research_state(&p.id, "chat-1").unwrap().unwrap();
        assert_eq!(one.goal, "goal one");
    }

    #[test]
    fn a_secret_in_a_question_or_answer_reaches_neither_state_nor_notes() {
        use ark_core::ResearchState;
        let (_d, s) = store();
        let p = Project::new("t", "");
        s.put_project(&p).unwrap();
        let secret = "DATABASE_URL=postgresql://u:p4ss@h/db";
        let mut st = ResearchState::new(format!("why does {secret} fail"));
        st.questions.push(secret.into());
        s.put_research_state(&p.id, "c", &st).unwrap();
        let note = Note::new(NoteKind::Finding, secret, ark_core::Author::Agent);
        s.put_note(&p.id, &note).unwrap();
        let back = s.get_research_state(&p.id, "c").unwrap().unwrap();
        let all = format!("{back:?} {:?}", s.list_notes(&p.id).unwrap());
        assert!(!all.contains("p4ss"), "{all}");
        assert!(all.contains("u:[redacted]@h"), "{all}");
    }

    #[test]
    fn a_database_with_project_keyed_working_state_migrates() {
        use ark_core::ResearchState;
        let dir = tempfile::tempdir().unwrap();
        {
            let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
            db.execute_batch(
                "CREATE TABLE research_state(
                     project_id TEXT PRIMARY KEY, updated_at TEXT NOT NULL, json TEXT NOT NULL);",
            )
            .unwrap();
            let json = serde_json::to_string(&ResearchState::new("old goal")).unwrap();
            db.execute(
                "INSERT INTO research_state VALUES('p', '2026-01-01T00:00:00Z', ?1)",
                [json],
            )
            .unwrap();
        }
        let s = Store::open(dir.path()).unwrap();
        let p = ProjectId("p".into());
        assert!(s.get_research_state(&p, "chat-1").unwrap().is_none());
        assert!(s.get_research_state(&p, "").unwrap().is_none());
        let old = s.get_research_state(&p, "#legacy").unwrap().unwrap();
        assert_eq!(old.goal, "old goal");
        s.put_research_state(&p, "chat-1", &ResearchState::new("new goal"))
            .unwrap();
        assert_eq!(
            s.get_research_state(&p, "#legacy").unwrap().unwrap().goal,
            "old goal"
        );
        drop(s);
        Store::open(dir.path()).unwrap();
    }

    #[test]
    fn research_state_and_cards_persist_and_dedup() {
        use ark_core::{Claim, ClaimType, EvidenceCard, InvocationId, ResearchState};
        let (_d, s) = store();
        let p = Project::new("t", "");
        s.put_project(&p).unwrap();

        // working state round-trips and replaces in place
        assert!(s.get_research_state(&p.id, "").unwrap().is_none());
        let mut st = ResearchState::new("find the root cause");
        st.findings
            .push(Claim::new("cache evicted early", ClaimType::Fact));
        s.put_research_state(&p.id, "", &st).unwrap();
        st.questions.push("which key first?".into());
        s.put_research_state(&p.id, "", &st).unwrap();
        let back = s.get_research_state(&p.id, "").unwrap().unwrap();
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
