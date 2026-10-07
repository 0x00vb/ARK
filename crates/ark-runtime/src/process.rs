//! Whether another process is still alive, to tell an interrupted record from one a live
//! process is still writing.

/// True when `pid` names a running process. This process counts as alive.
pub(crate) fn alive(pid: u32) -> bool {
    ark_os::process::alive(pid)
}

/// When this process started, as [`start_of`] reads it: stored beside a record's pid, so a
/// later reader tells this process from another that has the same pid after it.
pub fn own_start() -> Option<u64> {
    static OWN: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *OWN.get_or_init(|| start_of(std::process::id()))
}

/// True when the process that wrote a record is still running: `pid` names a live process and,
/// when the record kept its writer's start ([`own_start`]), that process started then. After a
/// crash and a reboot a pid is soon reused by another program (this one included); a different
/// start tells it apart. A record without a start falls back to the pid alone.
pub fn writer_alive(pid: u32, start: Option<u64>) -> bool {
    let Some(start) = start else {
        return alive(pid);
    };
    if pid == std::process::id() {
        return own_start() == Some(start);
    }
    alive(pid) && start_of(pid).is_none_or(|s| s == start)
}

/// An opaque, exact start of `pid`'s process: microseconds since the epoch on macOS, clock
/// ticks since boot on Linux. Only compared for equality, so a clock change does not move it.
///
/// macOS: `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID)`, which any user may read for any process
/// (`proc_pidinfo` refuses other users' processes, and after a reboot the pid of a dead run is
/// often a system daemon's). The start is the first field of `struct kinfo_proc`
/// (`kp_proc.p_un.__p_starttime`, a `timeval`); libc does not declare the struct.
#[cfg(target_os = "macos")]
pub fn start_of(pid: u32) -> Option<u64> {
    // `sizeof(struct kinfo_proc)` is 648 on 64-bit macOS; the buffer leaves room.
    let mut buf = [0u8; 1024];
    let mut len = buf.len();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as libc::c_int,
    ];
    // SAFETY: `buf` is writable for `len` bytes; sysctl writes at most that and sets `len`.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // No such process: success with nothing written.
    if r != 0 || len < 16 {
        return None;
    }
    let sec = i64::from_ne_bytes(buf[0..8].try_into().ok()?);
    let usec = i32::from_ne_bytes(buf[8..12].try_into().ok()?);
    (sec > 0).then(|| sec as u64 * 1_000_000 + usec as u64)
}

/// Field 22 of `/proc/<pid>/stat`: clock ticks from boot to the process's start.
#[cfg(target_os = "linux")]
pub fn start_of(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) is in parentheses and may hold spaces: count after it.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn start_of(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_reused_pid_is_not_the_writer() {
        let own = std::process::id();
        let start = own_start().expect("this process's start");
        assert!(writer_alive(own, Some(start)));
        // The same pid with another start: an earlier process that had it, now gone.
        assert!(!writer_alive(own, Some(start - 1)));
        // Another live process (the test runner's parent), with its start and with another.
        let parent = std::os::unix::process::parent_id();
        let theirs = start_of(parent).expect("the parent's start");
        assert!(writer_alive(parent, Some(theirs)));
        assert!(!writer_alive(parent, Some(theirs + 1)));
    }

    /// Another user's process (pid 1, launchd or init, runs as root) has a start too: a pid of a
    /// dead run reused by a system daemon is told apart.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn another_users_process_has_a_start() {
        let start = start_of(1).expect("pid 1's start");
        assert!(writer_alive(1, Some(start)));
        assert!(!writer_alive(1, Some(start + 1)));
    }

    /// The start read by sysctl is the one `proc_pidinfo` gives for a process it may read.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_start_agrees_with_proc_pidinfo() {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                std::process::id() as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        assert_eq!(n, size);
        assert_eq!(
            own_start(),
            Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
        );
    }

    #[test]
    fn a_record_without_a_start_goes_by_the_pid() {
        assert!(writer_alive(std::process::id(), None));
        assert!(!writer_alive(u32::MAX - 1, None));
        assert!(!writer_alive(u32::MAX - 1, Some(1)));
    }
}
