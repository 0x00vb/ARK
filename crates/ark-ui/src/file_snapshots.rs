//! Host-owned, route-bound file copies. The registry never holds its lock across file I/O.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_sdk::prelude::ark_files::{
    self,
    snapshot::{Progress, Snapshot, CHUNK_BYTES},
};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::resource::ResourceRef;

const MAX_OPEN: usize = 8;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(120);
const LIFETIME: Duration = Duration::from_secs(600);
const MODEL_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) struct Snapshots {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    total: Arc<AtomicU64>,
    jobs: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    pause: Mutex<Option<(Arc<std::sync::Barrier>, Arc<AtomicU64>)>>,
}

struct Entry {
    owner: ResourceRef,
    created: Instant,
    touched: Mutex<Instant>,
    cancel: AtomicBool,
    state: Mutex<State>,
    reads: tokio::sync::Semaphore,
}

enum State {
    Opening(Progress),
    Ready {
        snapshot: Arc<Mutex<Snapshot>>,
        change: String,
        _reservation: Reservation,
    },
    Refused(String),
}

struct Reservation {
    total: Arc<AtomicU64>,
    size: u64,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.total.fetch_sub(self.size, Ordering::SeqCst);
    }
}

impl Drop for Snapshots {
    fn drop(&mut self) {
        for entry in self.entries.get_mut().unwrap().values() {
            entry.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Snapshots {
    pub fn new() -> Arc<Self> {
        let registry = Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            total: Arc::default(),
            jobs: Arc::new(tokio::sync::Semaphore::new(MAX_OPEN)),
            #[cfg(test)]
            pause: Mutex::new(None),
        });
        let weak = Arc::downgrade(&registry);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(registry) = weak.upgrade() else {
                    break;
                };
                registry.sweep();
            }
        });
        registry
    }

    fn sweep(&self) {
        self.entries.lock().unwrap().retain(|_, entry| {
            let keep = entry.created.elapsed() < LIFETIME
                && entry.touched.lock().unwrap().elapsed() < IDLE;
            if !keep {
                entry.cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
    }

    fn entry(&self, owner: &ResourceRef, id: &str) -> anyhow::Result<Arc<Entry>> {
        self.sweep();
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("This file view expired; open it again"))?;
        anyhow::ensure!(
            entry.owner == *owner,
            "that file view belongs to another resource"
        );
        *entry.touched.lock().unwrap() = Instant::now();
        Ok(entry.clone())
    }

    pub fn begin(
        self: &Arc<Self>,
        root: std::path::PathBuf,
        owner: ResourceRef,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(cfg!(unix), "File viewing is unavailable on this computer");
        anyhow::ensure!(!owner.id.is_empty(), "a file path is required");
        self.sweep();
        let job = self.jobs.clone().try_acquire_owned().map_err(|_| {
            anyhow::anyhow!("Wait for a closing file view before opening another (8 open limit)")
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let entry = Arc::new(Entry {
            owner,
            created: Instant::now(),
            touched: Mutex::new(Instant::now()),
            cancel: AtomicBool::new(false),
            reads: tokio::sync::Semaphore::new(2),
            state: Mutex::new(State::Opening(Progress {
                phase: "opening",
                bytes: 0,
                size: 0,
            })),
        });
        {
            let mut entries = self.entries.lock().unwrap();
            anyhow::ensure!(
                entries.len() < MAX_OPEN,
                "Close another file view before opening this file (8 view limit)"
            );
            entries.insert(id.clone(), entry.clone());
        }
        let total = self.total.clone();
        #[cfg(test)]
        let mut pause = self.pause.lock().unwrap().clone();
        tokio::task::spawn_blocking(move || {
            let _job = job;
            let result = (|| -> anyhow::Result<State> {
                let path = &entry.owner.id;
                anyhow::ensure!(!entry.cancel.load(Ordering::Relaxed), "Opening cancelled");
                let (size, change) = ark_files::snapshot::probe(&root, path)?;
                let limit = if ark_files::kind_of(path) == ark_files::Kind::Map {
                    ark_files::MAX_BYTES
                } else {
                    ark_files::view_only(path).map_or(MODEL_BYTES, |view| view.max_bytes())
                };
                anyhow::ensure!(
                    size <= limit,
                    "File exceeds the {} MiB view limit",
                    limit / 1024 / 1024
                );
                total
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                        used.checked_add(size).filter(|n| *n <= MAX_TOTAL)
                    })
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "Close another file view to free space (512 MiB view limit)"
                        )
                    })?;
                let reservation = Reservation { total, size };
                let mut snapshot =
                    Snapshot::open_progress(&root, path, size, &entry.cancel, |progress| {
                        *entry.state.lock().unwrap() = State::Opening(progress);
                        #[cfg(test)]
                        if progress.phase == "copying" {
                            if let Some((gate, arrived)) = pause.take() {
                                arrived.fetch_add(1, Ordering::SeqCst);
                                gate.wait();
                            }
                        }
                    })?;
                if let Some(view) = ark_files::view_only(path) {
                    view.preflight(&snapshot.chunk(0)?, snapshot.size)?;
                }
                anyhow::ensure!(!entry.cancel.load(Ordering::Relaxed), "Opening cancelled");
                Ok(State::Ready {
                    snapshot: Arc::new(Mutex::new(snapshot)),
                    change,
                    _reservation: reservation,
                })
            })();
            if !entry.cancel.load(Ordering::Relaxed) {
                *entry.state.lock().unwrap() =
                    result.unwrap_or_else(|e| State::Refused(e.to_string()));
            }
        });
        Ok(json!({ "request": id, "state": "opening" }))
    }

    pub fn status(&self, owner: &ResourceRef, id: &str) -> anyhow::Result<Value> {
        let entry = self.entry(owner, id)?;
        let state = entry.state.lock().unwrap();
        Ok(match &*state {
            State::Opening(p) => {
                json!({ "request": id, "state": p.phase, "bytes": p.bytes, "size": p.size })
            }
            State::Refused(why) => json!({ "request": id, "state": "refused", "why": why }),
            State::Ready {
                snapshot, change, ..
            } => {
                let snapshot = snapshot.lock().unwrap();
                let path = &owner.id;
                let view = ark_files::view_only(path);
                json!({ "request": id, "state": "ready", "snapshot": id, "kind": ark_files::kind_of(path).as_str(),
                    "format": view.map(|v| v.extension).unwrap_or("geojson"),
                    "media_type": view.map(|v| v.media_type).unwrap_or("application/geo+json"),
                    "version": snapshot.version, "change": change, "size": snapshot.size,
                    "chunk_size": CHUNK_BYTES, "expires_ms": IDLE.as_millis() })
            }
        })
    }

    pub fn chunk(&self, owner: &ResourceRef, id: &str, offset: u64) -> anyhow::Result<Value> {
        let entry = self.entry(owner, id)?;
        let _read = entry
            .reads
            .try_acquire()
            .map_err(|_| anyhow::anyhow!("Only two file chunks may be read at once"))?;
        let snapshot = match &*entry.state.lock().unwrap() {
            State::Ready { snapshot, .. } => snapshot.clone(),
            _ => anyhow::bail!("This file view is not ready"),
        };
        let mut snapshot = snapshot.lock().unwrap();
        let bytes = snapshot.chunk(offset)?;
        Ok(
            json!({ "snapshot": id, "offset": offset, "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
            "eof": offset + bytes.len() as u64 == snapshot.size }),
        )
    }

    pub fn close(&self, owner: &ResourceRef, id: &str) -> anyhow::Result<Value> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(id) {
            anyhow::ensure!(
                entry.owner == *owner,
                "that file view belongs to another resource"
            );
            entry.cancel.store(true, Ordering::Relaxed);
        }
        entries.remove(id);
        Ok(json!({}))
    }

    pub fn close_resource(&self, owner: &ResourceRef) {
        self.entries.lock().unwrap().retain(|_, entry| {
            if entry.owner == *owner {
                entry.cancel.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::resource::{ResourceAuthority, ResourceKind, ResourceLocation};

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_jobs_keep_their_permits_until_the_copy_stops() {
        let root = tempfile::tempdir().unwrap();
        std::fs::File::create(root.path().join("a.bin"))
            .unwrap()
            .set_len(MODEL_BYTES)
            .unwrap();
        let snapshots = Snapshots::new();
        let gate = Arc::new(std::sync::Barrier::new(MAX_OPEN + 1));
        let arrived = Arc::new(AtomicU64::new(0));
        *snapshots.pause.lock().unwrap() = Some((gate.clone(), arrived.clone()));
        let owner = owner();
        for _ in 0..MAX_OPEN {
            snapshots.begin(root.path().into(), owner.clone()).unwrap();
        }
        for _ in 0..500 {
            if arrived.load(Ordering::SeqCst) == MAX_OPEN as u64 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(arrived.load(Ordering::SeqCst), MAX_OPEN as u64);
        snapshots.close_resource(&owner);
        assert!(snapshots.entries.lock().unwrap().is_empty());
        assert_eq!(snapshots.total.load(Ordering::SeqCst), MAX_TOTAL);
        for _ in 0..100 {
            assert!(snapshots.begin(root.path().into(), owner.clone()).is_err());
        }
        gate.wait();
        for _ in 0..500 {
            if snapshots.jobs.available_permits() == MAX_OPEN {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(snapshots.jobs.available_permits(), MAX_OPEN);
        assert_eq!(snapshots.total.load(Ordering::SeqCst), 0);
    }

    fn owner() -> ResourceRef {
        ResourceRef {
            location: ResourceLocation {
                runtime: "runtime".into(),
                project: "project".into(),
                session: "chat".into(),
            },
            kind: ResourceKind::File,
            id: "a.bin".into(),
            authority: ResourceAuthority::Chat,
        }
    }

    #[tokio::test]
    async fn snapshots_are_route_bound_and_close_releases_the_reservation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.bin"), [1, 2, 3]).unwrap();
        let snapshots = Snapshots::new();
        let owner = owner();
        let start = snapshots.begin(root.path().into(), owner.clone()).unwrap();
        let id = start["request"].as_str().unwrap();
        for _ in 0..100 {
            if snapshots.status(&owner, id).unwrap()["state"] == "ready" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(snapshots.chunk(&owner, id, 0).unwrap()["data"], "AQID");
        for field in ["runtime", "project", "session", "path", "authority"] {
            let mut forged = owner.clone();
            match field {
                "runtime" => forged.location.runtime = "other".into(),
                "project" => forged.location.project = "other".into(),
                "session" => forged.location.session = "other".into(),
                "path" => forged.id = "other".into(),
                _ => {
                    forged.authority = ResourceAuthority::Project {
                        route: "other".into(),
                    }
                }
            }
            assert!(snapshots.chunk(&forged, id, 0).is_err());
            assert!(snapshots.close(&forged, id).is_err());
            assert!(snapshots.status(&forged, id).is_err());
        }
        snapshots.close(&owner, id).unwrap();
        assert_eq!(snapshots.total.load(Ordering::SeqCst), 0);
        assert!(snapshots.chunk(&owner, id, 0).is_err());
    }

    #[tokio::test]
    async fn oversized_ifc_is_refused_before_copy_or_reservation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::File::create(root.path().join("large.ifc"))
            .unwrap()
            .set_len(2 * 1024 * 1024 + 1)
            .unwrap();
        let snapshots = Snapshots::new();
        let mut owner = owner();
        owner.id = "large.ifc".into();
        let start = snapshots.begin(root.path().into(), owner.clone()).unwrap();
        let id = start["request"].as_str().unwrap();
        let mut status = Value::Null;
        for _ in 0..100 {
            status = snapshots.status(&owner, id).unwrap();
            if status["state"] == "refused" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(status["why"], "File exceeds the 2 MiB view limit");
        assert!(snapshots.chunk(&owner, id, 0).is_err());
        assert_eq!(snapshots.total.load(Ordering::SeqCst), 0);
        snapshots.close(&owner, id).unwrap();
    }

    #[tokio::test]
    async fn oversized_maps_are_refused_before_copy_or_reservation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::File::create(root.path().join("large.geojson"))
            .unwrap()
            .set_len(ark_files::MAX_BYTES + 1)
            .unwrap();
        let snapshots = Snapshots::new();
        let mut owner = owner();
        owner.id = "large.geojson".into();
        let start = snapshots.begin(root.path().into(), owner.clone()).unwrap();
        let id = start["request"].as_str().unwrap();
        let mut status = Value::Null;
        for _ in 0..100 {
            status = snapshots.status(&owner, id).unwrap();
            if status["state"] == "refused" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(status["state"], "refused");
        assert_eq!(status["why"], "File exceeds the 25 MiB view limit");
        assert!(status.get("snapshot").is_none());
        assert!(snapshots.chunk(&owner, id, 0).is_err());
        assert_eq!(snapshots.total.load(Ordering::SeqCst), 0);
        snapshots.close(&owner, id).unwrap();
    }

    #[tokio::test]
    async fn simultaneous_pending_opens_cancel_without_leaking_a_reservation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::File::create(root.path().join("a.bin"))
            .unwrap()
            .set_len(MODEL_BYTES)
            .unwrap();
        let snapshots = Snapshots::new();
        let owner = owner();
        let mut ids = Vec::new();
        for _ in 0..MAX_OPEN {
            ids.push(
                snapshots.begin(root.path().into(), owner.clone()).unwrap()["request"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        assert!(snapshots.begin(root.path().into(), owner.clone()).is_err());
        for id in ids {
            snapshots.close(&owner, &id).unwrap();
        }
        for _ in 0..200 {
            if snapshots.total.load(Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(snapshots.total.load(Ordering::SeqCst), 0);
        assert!(snapshots.entries.lock().unwrap().is_empty());
    }
}
