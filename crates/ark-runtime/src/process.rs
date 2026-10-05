//! Whether another process is still alive, to tell an interrupted record from one a live
//! process is still writing.

/// True when `pid` names a running process. This process counts as alive.
pub(crate) fn alive(pid: u32) -> bool {
    ark_os::process::alive(pid)
}
