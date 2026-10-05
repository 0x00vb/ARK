//! Opening a link or a file in the person's default app.
//!
//! Never through a shell: the target goes to the system whole. `cmd /C start` would cut an
//! OAuth link at its first `&` and run whatever follows as a command.

use std::ffi::OsStr;

/// Open `target` (a URL or a file path) with the app the system opens it with. Returns once
/// the system has taken it, not when the app closes.
pub fn open(target: impl AsRef<OsStr>) -> std::io::Result<()> {
    sys::open(target.as_ref())
}

#[cfg(unix)]
mod sys {
    use std::ffi::OsStr;

    /// macOS's `open`; the desktop's opener elsewhere.
    const OPENER: &str = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "xdg-open"
    };

    pub fn open(target: &OsStr) -> std::io::Result<()> {
        let s = std::process::Command::new(OPENER)
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if s.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("{OPENER} exited with {s}")))
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn open(target: &OsStr) -> std::io::Result<()> {
        let verb = wide(OsStr::new("open"));
        let file = wide(target);
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success.
        if r as isize > 32 {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "the system could not open it (ShellExecute {})",
                r as isize
            )))
        }
    }
}
