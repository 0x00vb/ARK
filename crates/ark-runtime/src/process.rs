//! Whether another process is still alive, to tell an interrupted record from one a live
//! process is still writing.

/// True when `pid` names a running process. This process counts as alive.
#[cfg(unix)]
pub(crate) fn alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is delivered.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    // EPERM: it exists but belongs to someone else.
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn alive(pid: u32) -> bool {
    pid == std::process::id()
}
