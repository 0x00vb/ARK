//! Starting programs and stopping them with everything they started.
//!
//! [`command`] is how ARK starts any program, so these hold everywhere:
//! - it starts in its own process group, so [`Tree`] can stop it and its children together
//!   (a Job Object on Windows, a process group on Unix);
//! - on Windows no console window opens for it, even from a windowed app;
//! - on Windows an npm `.cmd` launcher runs its target directly. Through `cmd.exe` a command
//!   line is cut at 8191 characters and re-quoted by `cmd`'s rules; a system prompt passed as
//!   an argument would break.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A command for `program` (a path, from [`crate::find`]) with the properties above.
pub fn command(program: &Path) -> tokio::process::Command {
    std_command(program).into()
}

/// [`command`] for code that runs a process without async.
pub fn std_command(program: &Path) -> std::process::Command {
    let (exe, first) = launcher(program);
    let mut c = std::process::Command::new(exe);
    c.args(first);
    sys::own_group(&mut c);
    c
}

/// What actually runs for `program`: itself, or for an npm launcher on Windows, its target
/// (`node <script>` or the `.exe` it points at). Anything else runs as given.
pub fn launcher(program: &Path) -> (PathBuf, Vec<OsString>) {
    if cfg!(windows) {
        if let Some(found) = npm_target(program) {
            return found;
        }
    }
    (program.to_path_buf(), Vec::new())
}

fn npm_target(program: &Path) -> Option<(PathBuf, Vec<OsString>)> {
    let ext = program.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let dir = program.parent()?;
    let text = std::fs::read_to_string(program).ok()?;
    let target = dir.join(shim_target(&text)?);
    if !target.is_file() {
        return None;
    }
    let kind = target
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match kind.as_deref() {
        Some("exe") => Some((target, Vec::new())),
        Some("js" | "mjs" | "cjs") => {
            let local = dir.join("node.exe");
            let node = if local.is_file() {
                local
            } else {
                crate::find::program("node")?
            };
            Some((node, vec![target.into_os_string()]))
        }
        _ => None,
    }
}

/// The file an npm `cmd-shim` launcher runs, relative to the launcher's folder: the last
/// quoted `"%dp0%\..."` path that is not the optional `node.exe` next to it.
fn shim_target(text: &str) -> Option<String> {
    text.split('"')
        .skip(1)
        .step_by(2)
        .filter_map(|q| {
            q.strip_prefix("%dp0%\\")
                .or_else(|| q.strip_prefix("%~dp0\\"))
        })
        .filter(|rel| !rel.eq_ignore_ascii_case("node.exe") && !rel.is_empty())
        .last()
        .map(|rel| rel.replace('\\', std::path::MAIN_SEPARATOR_STR))
}

/// Stops a started program together with everything it started. Dropping it while armed
/// stops them too: a cancelled call drops its future, and `kill_on_drop` alone would leave a
/// shell's children (or Node's) running.
pub struct Tree(sys::Tree);

impl Tree {
    /// For a child started with [`command`] (`child.id()`).
    pub fn of(pid: Option<u32>) -> Tree {
        Tree(sys::Tree::of(pid))
    }

    /// Stop the program and everything it started, now.
    pub fn kill(&mut self) {
        self.0.kill()
    }

    /// The program ended on its own: leave anything it started on purpose running.
    pub fn disarm(&mut self) {
        self.0.disarm()
    }

    /// Signal the attached group to stop without waiting. Contained work calls this before it
    /// reaps the direct child, so the group id is not released before the group is signalled.
    pub fn terminate(&self) {
        self.0.terminate();
    }

    /// Whether this guard attached to a process group or Job Object.
    pub fn attached(&self) -> bool {
        self.0.attached()
    }

    /// Terminate the attached group and wait for it to become empty. Used by contained,
    /// finite work only. Programs intentionally left running use the existing disarm path.
    pub async fn stop(&mut self) -> std::io::Result<()> {
        // An empty group is not signalled: its id may already be free for reuse.
        if !self.0.empty()? {
            self.0.terminate();
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if self.0.empty()? {
                self.0.disarm();
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the process group did not stop",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.0.kill()
    }
}

/// Give this process the `PATH` the person's terminal has. macOS and Linux start an app from
/// the Dock or a launcher with the system's short `PATH`, without the folders where `claude`,
/// `codex` or `node` usually live; the login shell's `PATH` is taken instead (bounded: a shell
/// that does not answer in 3 s leaves it as it is). Windows gives a windowed app the person's
/// own `PATH`, so there is nothing to do. Call it first, while this is the only thread.
pub fn login_path() {
    if cfg!(windows) || std::env::var_os("TERM").is_some() {
        return;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = std::process::Command::new(shell)
            .args(["-ilc", "printf '%s' \"$PATH\""])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    if let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(3)) {
        let path = String::from_utf8_lossy(&out.stdout);
        // Interactive shells may print a banner first; the PATH is the last line.
        if let Some(path) = path.lines().last().filter(|p| p.contains('/')) {
            std::env::set_var("PATH", path);
        }
    }
}

/// Let a windowed app started from a terminal print there (`--help`, `token create`). On
/// Windows such an app has no console of its own, so it attaches to the terminal's; elsewhere
/// it already prints there. Started from the Start menu or Explorer, nothing changes.
pub fn attach_console() {
    sys::attach_console()
}

/// Whether process `pid` is running. This process counts as alive.
pub fn alive(pid: u32) -> bool {
    pid == std::process::id() || sys::alive(pid)
}

#[cfg(unix)]
mod sys {
    use std::os::unix::process::CommandExt;

    pub fn own_group(c: &mut std::process::Command) {
        c.process_group(0);
    }

    pub struct Tree(Option<u32>);

    impl Tree {
        pub fn of(pid: Option<u32>) -> Tree {
            Tree(pid)
        }
        pub fn kill(&mut self) {
            if let Some(pid) = self.0.take() {
                // SAFETY: killpg only sends a signal; the group id is the child's own pid.
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        pub fn disarm(&mut self) {
            self.0 = None;
        }
        pub fn attached(&self) -> bool {
            self.0.is_some()
        }
        pub fn terminate(&self) {
            if let Some(pid) = self.0 {
                // SAFETY: the child was started in its own process group.
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        pub fn empty(&self) -> std::io::Result<bool> {
            let Some(pid) = self.0 else { return Ok(true) };
            // SAFETY: signal zero only checks group existence.
            if unsafe { libc::killpg(pid as libc::pid_t, 0) } == 0 {
                return Ok(false);
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => Ok(true),
                // macOS answers EPERM while the group's members are still exiting. The group
                // is ours, so this means "not empty yet"; the caller's deadline still applies.
                Some(libc::EPERM) => Ok(false),
                _ => Err(error),
            }
        }
    }

    pub fn attach_console() {}

    pub fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists; nothing is delivered.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        // EPERM: it exists but belongs to someone else.
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// `STILL_ACTIVE`: the exit code of a process that has not exited.
    const STILL_ACTIVE: u32 = 259;

    pub fn own_group(c: &mut std::process::Command) {
        c.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    /// A Job Object holding the child; children it starts join the job too. The handle is
    /// kept as an integer so the guard can cross threads.
    pub struct Tree(isize);

    fn limit(job: HANDLE, flags: u32) -> bool {
        // SAFETY: a zeroed limit struct is valid; only LimitFlags is set.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = flags;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
        }
    }

    impl Tree {
        pub fn of(pid: Option<u32>) -> Tree {
            let Some(pid) = pid else { return Tree(0) };
            // SAFETY: plain Win32 calls on handles this function owns and closes.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Tree(0);
                }
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                let ok = !process.is_null()
                    && limit(job, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
                    && AssignProcessToJobObject(job, process) != 0;
                if !process.is_null() {
                    CloseHandle(process);
                }
                if !ok {
                    CloseHandle(job);
                    return Tree(0);
                }
                Tree(job as isize)
            }
        }
        pub fn kill(&mut self) {
            if self.0 != 0 {
                // SAFETY: the job handle is ours; closing it ends the job's processes too.
                unsafe {
                    TerminateJobObject(self.0 as HANDLE, 1);
                    CloseHandle(self.0 as HANDLE);
                }
                self.0 = 0;
            }
        }
        pub fn disarm(&mut self) {
            if self.0 != 0 {
                // SAFETY: as above; without kill-on-close, closing the handle ends nothing.
                unsafe {
                    limit(self.0 as HANDLE, 0);
                    CloseHandle(self.0 as HANDLE);
                }
                self.0 = 0;
            }
        }
        pub fn attached(&self) -> bool {
            self.0 != 0
        }
        pub fn terminate(&self) {
            if self.0 != 0 {
                // SAFETY: the job handle belongs to this guard.
                unsafe {
                    TerminateJobObject(self.0 as HANDLE, 1);
                }
            }
        }
        pub fn empty(&self) -> std::io::Result<bool> {
            if self.0 == 0 {
                return Ok(true);
            }
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: info is sized for this information class; the job is owned.
            if unsafe {
                QueryInformationJobObject(
                    self.0 as HANDLE,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut std::ffi::c_void,
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(info.ActiveProcesses == 0)
        }
    }

    pub fn attach_console() {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        // SAFETY: fails harmlessly when there is no parent console or one is attached.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    pub fn alive(pid: u32) -> bool {
        // SAFETY: a query-only handle, closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                // It exists but belongs to someone else.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_an_npm_launcher_runs() {
        // npm 10's cmd-shim for a Node script.
        let js = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(
            shim_target(js).unwrap(),
            ["node_modules", "@openai", "codex", "bin", "codex.js"].join(sep)
        );
        // A package whose bin is a native executable.
        let exe = "@ECHO off\r\n\"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\"   %*\r\n";
        assert!(shim_target(exe).unwrap().ends_with("claude.exe"));
        assert_eq!(shim_target("@echo off\r\nnode x.js %*\r\n"), None);
    }

    #[test]
    fn a_plain_program_runs_as_given() {
        let p = Path::new("/usr/bin/env");
        assert_eq!(launcher(p), (p.to_path_buf(), Vec::new()));
    }

    #[test]
    fn this_process_is_alive_and_a_finished_one_is_not() {
        assert!(alive(std::process::id()));
        let exe = std::env::current_exe().unwrap();
        let mut child = std_command(&exe)
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    /// Stopping the tree stops a grandchild too (a shell's `sleep`, `cmd`'s `ping`).
    #[tokio::test]
    async fn killing_the_tree_stops_the_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut c = if cfg!(windows) {
            let mut c = command(Path::new("powershell.exe"));
            c.args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$p = Start-Process ping -ArgumentList '-n','60','127.0.0.1' -NoNewWindow -PassThru; Set-Content -Path '{}' -Value $p.Id; Wait-Process -Id $p.Id",
                    pidfile.display()
                ),
            ]);
            c
        } else {
            let mut c = command(Path::new("/bin/sh"));
            c.args([
                "-c",
                &format!("sleep 60 & echo $! > '{}'; wait", pidfile.display()),
            ]);
            c
        };
        let mut child = c.kill_on_drop(true).spawn().unwrap();
        let mut tree = Tree::of(child.id());
        let mut grandchild = None;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Ok(t) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = t.trim().parse::<u32>() {
                    grandchild = Some(p);
                    break;
                }
            }
        }
        let grandchild = grandchild.expect("the grandchild started");
        assert!(alive(grandchild));
        tree.kill();
        let _ = child.wait().await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !alive(grandchild),
            "grandchild {grandchild} outlived its tree"
        );
    }
}
