//! Bounded immutable copies for file viewers. Chunks never reopen the live project file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{inside, Error};

pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 1024 * 1024;
const COPY_BYTES: usize = 64 * 1024;

/// The temporary file is private, unnamed and removed even on early failure or cancellation.
pub struct Snapshot {
    file: File,
    pub version: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub phase: &'static str,
    pub bytes: u64,
    pub size: u64,
}

/// A bounded metadata probe for external changes, including files above the text-file cap.
pub fn probe(root: &Path, rel: &str) -> Result<(u64, String), Error> {
    let root = ark_os::dirs::canonicalize(root)?;
    let file = ark_os::fs::open_within(&root, &inside(&root, rel)?)?;
    let stamp = ark_os::fs::FileStamp::read(&file)?;
    Ok((
        stamp.size,
        blake3::hash(format!("{stamp:?}").as_bytes())
            .to_hex()
            .to_string(),
    ))
}

impl Snapshot {
    /// `limit` may lower, but never raise, the source cap. The cancel flag is checked at
    /// every copy block and again before returning a completed snapshot.
    pub fn open(root: &Path, rel: &str, limit: u64, cancel: &AtomicBool) -> Result<Self, Error> {
        Self::open_progress(root, rel, limit, cancel, |_| {})
    }

    pub fn open_progress(
        root: &Path,
        rel: &str,
        limit: u64,
        cancel: &AtomicBool,
        progress: impl FnMut(Progress),
    ) -> Result<Self, Error> {
        Self::copy_with_progress(root, rel, limit, cancel, |_, _| {}, progress)
    }

    #[cfg(test)]
    fn copy(
        root: &Path,
        rel: &str,
        limit: u64,
        cancel: &AtomicBool,
        mut copied: impl FnMut(usize, &Path),
    ) -> Result<Self, Error> {
        Self::copy_with_progress(root, rel, limit, cancel, &mut copied, |_| {})
    }

    fn copy_with_progress(
        root: &Path,
        rel: &str,
        limit: u64,
        cancel: &AtomicBool,
        mut copied: impl FnMut(usize, &Path),
        mut progress: impl FnMut(Progress),
    ) -> Result<Self, Error> {
        let root = ark_os::dirs::canonicalize(root)?;
        for attempt in 0..3 {
            if attempt > 0 {
                progress(Progress {
                    phase: "retrying",
                    bytes: 0,
                    size: 0,
                });
            }
            let path = inside(&root, rel)?;
            let mut source = ark_os::fs::open_within(&root, &path)?;
            let before = ark_os::fs::FileStamp::read(&source)?;
            if before.size > limit.min(MAX_BYTES) {
                return Err(Error::Format(format!(
                    "file exceeds the {} MiB view limit",
                    limit.min(MAX_BYTES) / 1024 / 1024
                )));
            }
            let mut file = tempfile::tempfile()?;
            let (first, count) = pass(
                &mut source,
                Some(&mut file),
                limit.min(MAX_BYTES),
                cancel,
                &mut |bytes| {
                    progress(Progress {
                        phase: "copying",
                        bytes,
                        size: before.size,
                    })
                },
            )?;
            copied(attempt, &path);
            let between = ark_os::fs::FileStamp::read(&source)?;
            source.rewind()?;
            let (second, checked) = pass(
                &mut source,
                None,
                limit.min(MAX_BYTES),
                cancel,
                &mut |bytes| {
                    progress(Progress {
                        phase: "verifying",
                        bytes,
                        size: before.size,
                    })
                },
            )?;
            let after = ark_os::fs::FileStamp::read(&source)?;
            if before != between
                || before != after
                || count != before.size
                || checked != count
                || first != second
            {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Input("Opening cancelled".into()));
            }
            file.rewind()?;
            return Ok(Self {
                file,
                version: first,
                size: count,
            });
        }
        Err(Error::Input(
            "The file is changing; try again when it is saved".into(),
        ))
    }

    /// Offsets must be chunk-aligned. Each read has a fixed bound independent of the input.
    pub fn chunk(&mut self, offset: u64) -> Result<Vec<u8>, Error> {
        if offset > self.size || !offset.is_multiple_of(CHUNK_BYTES as u64) {
            return Err(Error::Input("invalid file chunk offset".into()));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; (self.size - offset).min(CHUNK_BYTES as u64) as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

fn pass(
    source: &mut File,
    mut dest: Option<&mut File>,
    limit: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<(String, u64), Error> {
    let mut hash = blake3::Hasher::new();
    let mut bytes = [0; COPY_BYTES];
    let mut count = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Input("Opening cancelled".into()));
        }
        let n = source.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > limit {
            return Err(Error::Format("file exceeds the view limit".into()));
        }
        hash.update(&bytes[..n]);
        if let Some(dest) = dest.as_mut() {
            dest.write_all(&bytes[..n])?;
        }
        progress(count);
    }
    // Same version vocabulary as existing edits and media, without buffering the whole file.
    Ok((hash.finalize().to_hex()[..16].to_string(), count))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn cancellation_stops_copying_and_verifying() {
        let root = tempfile::tempdir().unwrap();
        File::create(root.path().join("model"))
            .unwrap()
            .set_len(CHUNK_BYTES as u64)
            .unwrap();
        for phase in ["copying", "verifying"] {
            let cancel = AtomicBool::new(false);
            let result =
                Snapshot::open_progress(root.path(), "model", MAX_BYTES, &cancel, |progress| {
                    if progress.phase == phase {
                        cancel.store(true, Ordering::Relaxed);
                    }
                });
            assert!(result.err().unwrap().to_string().contains("cancelled"));
        }
    }

    #[test]
    #[ignore = "writes a near-limit temporary file; run for the artifact performance gate"]
    fn measured_snapshot_limits() {
        let root = tempfile::tempdir().unwrap();
        for size in [1024 * 1024, MAX_BYTES] {
            File::create(root.path().join("model"))
                .unwrap()
                .set_len(size)
                .unwrap();
            let started = std::time::Instant::now();
            let mut snapshot =
                Snapshot::open(root.path(), "model", MAX_BYTES, &AtomicBool::new(false)).unwrap();
            let copied = started.elapsed();
            let mut count = 0;
            for offset in (0..size).step_by(CHUNK_BYTES) {
                count += snapshot.chunk(offset).unwrap().len() as u64;
            }
            assert_eq!(count, size);
            eprintln!(
                "snapshot bytes={size} open_ms={} total_ms={}",
                copied.as_millis(),
                started.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn chunks_keep_one_version_through_every_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let old = vec![17; CHUNK_BYTES * 3 + 7];
        let new = vec![31; old.len()];
        for boundary in 0..=4 {
            std::fs::write(root.path().join("a.stl"), &old).unwrap();
            let mut snap =
                Snapshot::open(root.path(), "a.stl", MAX_BYTES, &AtomicBool::new(false)).unwrap();
            assert_eq!(snap.version, crate::version_of(&old));
            let mut assembled = Vec::new();
            for chunk in 0..4 {
                if chunk == boundary {
                    std::fs::write(root.path().join("next"), &new).unwrap();
                    std::fs::rename(root.path().join("next"), root.path().join("a.stl")).unwrap();
                }
                assembled.extend(snap.chunk((chunk * CHUNK_BYTES) as u64).unwrap());
            }
            assert_eq!(assembled, old);
            assert!(snap.chunk(1).is_err());
            assert!(snap.chunk(u64::MAX).is_err());
        }
    }

    #[test]
    fn in_place_overwrite_and_truncation_never_publish_a_mixed_copy() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.stl");
        std::fs::write(&file, vec![1; CHUNK_BYTES]).unwrap();
        let cancel = AtomicBool::new(false);
        let mut snap = Snapshot::copy(root.path(), "a.stl", MAX_BYTES, &cancel, |attempt, path| {
            if attempt == 0 {
                std::fs::write(path, vec![2; 99]).unwrap();
            }
        })
        .unwrap();
        assert_eq!(snap.chunk(0).unwrap(), vec![2; 99]);
        let err = Snapshot::copy(root.path(), "a.stl", MAX_BYTES, &cancel, |attempt, path| {
            std::fs::write(path, vec![attempt as u8 + 3; 101]).unwrap();
        })
        .err()
        .unwrap();
        assert!(err.to_string().contains("file is changing"));
    }

    #[test]
    fn bounds_cancel_and_paths_fail_before_a_token_exists() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("large"), "123456789").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(Snapshot::open(root.path(), "large", 8, &cancel).is_err());
        assert!(Snapshot::open(root.path(), "../secret", 100, &cancel).is_err());
        assert!(Snapshot::open(root.path(), ".", 100, &cancel).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(Snapshot::open(root.path(), "large", 100, &cancel).is_err());
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("secret"), "private").unwrap();
        std::os::unix::fs::symlink(out.path(), root.path().join("link")).unwrap();
        assert!(Snapshot::open(root.path(), "link/secret", 100, &AtomicBool::new(false)).is_err());
    }
}
