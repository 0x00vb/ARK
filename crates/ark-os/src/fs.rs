//! File writes and names that behave the same on every system.

use std::io;
use std::path::Path;

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
