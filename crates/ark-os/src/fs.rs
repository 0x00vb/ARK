//! File writes and names that behave the same on every system.

use std::io;
use std::path::Path;

/// Make one new private folder. Refuse an existing path rather than reusing it.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    std::fs::create_dir(path)
}

/// Open a regular project file for bounded capture. Unix uses descriptor-relative traversal.
/// Windows verifies the opened handle's resolved path before reading any bytes.
pub fn open_capture(root: &Path, path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        open_within(root, path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut name = vec![0u16; 32768];
        // SAFETY: file owns the handle, and name has the capacity passed to Windows.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                name.as_mut_ptr(),
                name.len() as u32,
                0,
            )
        } as usize;
        if n == 0 || n >= name.len() {
            return Err(io::Error::last_os_error());
        }
        let resolved = std::path::PathBuf::from(String::from_utf16_lossy(&name[..n]));
        let root = std::fs::canonicalize(root)?;
        if !file.metadata()?.is_file() || !resolved.starts_with(root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture is outside its folder or not a regular file",
            ));
        }
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (root, path);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file capture is unavailable",
        ))
    }
}

/// Read at most max bytes from an opened regular file. A growing file never causes an
/// unbounded allocation; one extra byte detects an overrun and the partial result is rejected.
pub fn read_bounded(mut file: std::fs::File, max: u64) -> io::Result<Vec<u8>> {
    let size = file.metadata()?.len();
    if size > max {
        return Err(io::Error::other("file exceeds the capture limit"));
    }
    read_limited(&mut file, size, max)
}

fn read_limited(source: &mut impl io::Read, size: u64, max: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(size.min(max).min(1024 * 1024) as usize);
    source.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::other("file grew past the capture limit"));
    }
    Ok(bytes)
}

/// Snapshot an already-open file, retaining native copy-on-write clones where available.
/// Clones are checked before admission to the byte budget. Fallback copying is byte-bounded.
/// A rejected partial copy or clone is removed.
pub fn copy_bounded(mut source: std::fs::File, dest: &Path, max: u64) -> io::Result<u64> {
    let before = source.metadata()?;
    if !before.is_file() || before.len() > max {
        return Err(io::Error::other("file exceeds the snapshot limit"));
    }
    copy_checked(&mut source, dest, max, &before)
}

fn copy_checked(
    source: &mut std::fs::File,
    dest: &Path,
    max: u64,
    before: &std::fs::Metadata,
) -> io::Result<u64> {
    use std::io::Write;
    let cloned = clone_capture(source, dest)?;
    let mut output = if cloned {
        None
    } else {
        Some(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)?,
        )
    };
    let result = (|| {
        if let Some(output) = &mut output {
            output.set_permissions(before.permissions())?;
            let copied = copy_limited(source, output, max)?;
            output.flush()?;
            if copied != before.len() {
                return Err(io::Error::other("file changed during snapshot"));
            }
        }
        let after = source.metadata()?;
        let size = std::fs::metadata(dest)?.len();
        if size > max
            || size != before.len()
            || after.len() != before.len()
            || after.modified()? != before.modified()?
        {
            return Err(io::Error::other("file changed during snapshot"));
        }
        Ok(size)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn copy_limited(
    source: &mut impl io::Read,
    dest: &mut impl io::Write,
    max: u64,
) -> io::Result<u64> {
    use std::io::Read;
    let copied = io::copy(&mut source.by_ref().take(max), dest)?;
    let mut extra = [0];
    if source.read(&mut extra)? != 0 {
        return Err(io::Error::other("file grew past the snapshot limit"));
    }
    Ok(copied)
}

#[cfg(target_os = "macos")]
fn clone_capture(source: &std::fs::File, dest: &Path) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let dest = std::ffi::CString::new(dest.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: source owns the fd and dest is a live nul-terminated path. No file is replaced.
    let result =
        unsafe { libc::fclonefileat(source.as_raw_fd(), libc::AT_FDCWD, dest.as_ptr(), 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EXDEV | libc::ENOSYS)
    ) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn clone_capture(source: &std::fs::File, dest: &Path) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    // SAFETY: both file descriptors are owned for this ioctl.
    let result = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    drop(output);
    std::fs::remove_file(dest)?;
    if matches!(
        error.raw_os_error(),
        Some(libc::EOPNOTSUPP | libc::EXDEV | libc::ENOTTY | libc::EINVAL)
    ) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_capture(_: &std::fs::File, _: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Open a regular file below a canonical root without following replacement symlinks.
/// Descriptor-relative traversal prevents a renamed ancestor from redirecting the read.
pub fn open_within(root: &Path, path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is outside the project",
            )
        })?;
        let mut dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        let parts: Vec<_> = relative.components().collect();
        for (i, part) in parts.iter().enumerate() {
            let std::path::Component::Normal(name) = part else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "invalid file path",
                ));
            };
            let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if i + 1 < parts.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                };
            // SAFETY: the directory fd and nul-terminated name remain alive for openat.
            let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openat returned a new owned descriptor.
            dir = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        if !dir.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(dir)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, path);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stable file snapshots are unavailable on this system",
        ))
    }
}

/// Identity and change metadata captured from an open file handle, never a later path lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    identity: (u64, u64),
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    pub fn read(file: &std::fs::File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = file.metadata()?;
            if !m.is_file() {
                return Err(io::Error::other("not a regular file"));
            }
            Ok(Self {
                size: m.len(),
                identity: (m.dev(), m.ino()),
                modified: (m.mtime(), m.mtime_nsec()),
                changed: (m.ctime(), m.ctime_nsec()),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stable file change metadata is unavailable on this system",
            ))
        }
    }
}

/// Put `tmp` in place of `dest` in one step, so a reader sees the old file or the new one,
/// never half of it. Windows refuses the swap for a moment while another program holds
/// `dest` (an editor saving, Excel, a virus scanner reading it), so it is retried for about
/// two seconds before the error is returned. On failure `tmp` is left for the caller.
pub fn replace(tmp: &Path, dest: &Path) -> io::Result<()> {
    let mut wait = std::time::Duration::from_millis(10);
    let mut waited = std::time::Duration::ZERO;
    loop {
        match std::fs::rename(tmp, dest) {
            Ok(()) => return Ok(()),
            Err(e) if held_open(&e) && waited < std::time::Duration::from_secs(2) => {
                std::thread::sleep(wait);
                waited += wait;
                wait = (wait * 2).min(std::time::Duration::from_millis(250));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Write `bytes` to `path` through a file beside it and [`replace`]: a reader never sees a
/// partial file. The temporary file is removed when the write fails.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    replace(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A sharing violation or a denied rename caused by another program holding the file.
fn held_open(e: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33).
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Why `name` (one path component) cannot be a file or folder name on some system, or
/// `None`. Checked the same on every system, so a project folder moves between them: no
/// `< > : " / \ | ? *` or control characters, no trailing dot or space, and none of
/// Windows' device names (`CON`, `NUL`, `COM1`, `LPT1`, ... with any extension).
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name == "." || name == ".." {
        return Some("a name is needed");
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Some("names cannot hold < > : \" / \\ | ? * or control characters");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("names cannot end with a dot or a space");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit()
        && stem.as_bytes()[3] != b'0');
    device.then_some("that name is reserved by Windows")
}

/// `name` made acceptable as a folder or file name everywhere (see [`name_problem`]):
/// forbidden characters become `-`, trailing dots and spaces go, a device name gets `_`.
pub fn clean_name(name: &str) -> String {
    let mut s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '-'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() || s == "." || s == ".." {
        return "untitled".into();
    }
    if name_problem(&s).is_some() {
        s.insert(0, '_');
    }
    s
}

/// Let only the person who owns `path` read it. Unix: mode 0600. Windows: nothing to do,
/// because a file under the person's profile folder already inherits an ACL that admits only
/// them, administrators and the system.
pub fn owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// A folder only its owner can change, for a socket or a key. Unix: a missing folder is made
/// with mode 0700 (its missing parents too). An existing one is refused unless this user owns
/// it and neither its group nor others can write to it, since whoever can write there can put
/// their own file in place of ours. Windows: the folder is made if missing.
pub fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        if !path.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        let meta = std::fs::symlink_metadata(path)?;
        let refused = |why: &str| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: {why}", path.display()),
            ))
        };
        if !meta.is_dir() {
            return refused("not a folder");
        }
        // SAFETY: geteuid has no arguments and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return refused("another user owns this folder");
        }
        if meta.mode() & 0o022 != 0 {
            return refused("other users can write to this folder");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_and_copy_fallback_stop_growth_after_the_metadata_check() {
        // The previously observed length is one. The stream has grown before its first read.
        let mut source = io::Cursor::new(vec![b'x'; 4096]);
        assert!(read_limited(&mut source, 1, 8).is_err());
        assert_eq!(source.position(), 9);
        let mut source = io::Cursor::new(vec![b'x'; 4096]);
        let mut output = Vec::new();
        assert!(copy_limited(&mut source, &mut output, 8).is_err());
        assert_eq!(output.len(), 8);
        assert_eq!(source.position(), 9);
    }

    #[test]
    fn a_snapshot_grown_after_metadata_is_rejected_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let dest = root.path().join("copy");
        std::fs::write(&path, b"a").unwrap();
        let mut source = std::fs::File::open(&path).unwrap();
        let before = source.metadata().unwrap();
        std::fs::write(&path, vec![b'x'; 4096]).unwrap();
        assert!(copy_checked(&mut source, &dest, 8, &before).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn capture_respects_exact_byte_limits_and_never_replaces_a_destination() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let source = root.join("source");
        let dest = root.join("snapshot");
        std::fs::write(&source, b"four").unwrap();
        assert!(read_bounded(open_capture(&root, &source).unwrap(), 3).is_err());
        assert_eq!(
            read_bounded(open_capture(&root, &source).unwrap(), 4).unwrap(),
            b"four"
        );
        assert!(copy_bounded(open_capture(&root, &source).unwrap(), &dest, 3).is_err());
        assert!(!dest.exists());
        assert_eq!(
            copy_bounded(open_capture(&root, &source).unwrap(), &dest, 4).unwrap(),
            4
        );
        std::fs::write(&source, b"next").unwrap();
        assert!(copy_bounded(open_capture(&root, &source).unwrap(), &dest, 4).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"four");
    }

    #[cfg(unix)]
    #[test]
    fn capture_refuses_replaced_files_and_ancestor_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        std::fs::write(outside.path().join("secret"), "outside").unwrap();
        symlink(outside.path(), root.join("dir")).unwrap();
        assert!(open_capture(&root, &root.join("dir/secret")).is_err());
        symlink(outside.path().join("secret"), root.join("leaf")).unwrap();
        assert!(open_capture(&root, &root.join("leaf")).is_err());
    }

    #[test]
    fn an_open_capture_is_bounded_even_if_the_file_grows_after_opening() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("growing");
        std::fs::write(&path, b"a").unwrap();
        let file = open_capture(&root, &path).unwrap();
        std::fs::write(&path, vec![1; 4096]).unwrap();
        assert!(read_bounded(file, 2).is_err());
        assert!(!root.join("copy").exists());
        assert!(copy_bounded(open_capture(&root, &path).unwrap(), &root.join("copy"), 2).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_scratch_folder_is_private_and_cannot_be_reused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("run");
        create_private_dir(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(create_private_dir(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_private_folder_is_made_0700_and_a_shared_one_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let made = dir.path().join("a/b");
        private_dir(&made).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&made), 0o700);
        assert_eq!(mode(&dir.path().join("a")), 0o700);
        private_dir(&made).unwrap();
        std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o777)).unwrap();
        let e = private_dir(&made).unwrap_err();
        assert!(e.to_string().contains("other users can write"), "{e}");
        let file = dir.path().join("f");
        std::fs::write(&file, "").unwrap();
        assert!(private_dir(&file).is_err());
    }

    #[test]
    fn names_every_system_accepts() {
        for ok in [
            "notes.md",
            "Q3 report.xlsx",
            "console.txt",
            "COM0",
            "LPT10",
            ".hidden",
            "José",
        ] {
            assert_eq!(name_problem(ok), None, "{ok}");
        }
        for bad in [
            "a:b",
            "a.txt:stream",
            "CON",
            "nul.md",
            "nul.tar.gz",
            "Com1.txt",
            "LPT9",
            "x?",
            "end.",
            "end ",
            "",
            "..",
        ] {
            assert!(name_problem(bad).is_some(), "{bad}");
        }
        assert_eq!(clean_name("Q3: plan?"), "Q3- plan-");
        assert_eq!(clean_name("CON"), "_CON");
        assert_eq!(clean_name("draft..."), "draft");
        assert_eq!(clean_name(" .. "), "untitled");
        for n in ["Q3: plan?", "CON", "a/b\\c", "x. "] {
            assert_eq!(name_problem(&clean_name(n)), None, "{n}");
        }
    }

    #[test]
    fn writes_replace_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp file left"
        );
        owner_only(&p).unwrap();
    }

    /// Another program holding the file only delays the swap.
    #[cfg(windows)]
    #[test]
    fn a_file_held_open_for_a_moment_is_still_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("held.txt");
        std::fs::write(&p, "old").unwrap();
        let held = std::fs::File::open(&p).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        write_atomic(&p, b"new").unwrap();
        t.join().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
    }
}
