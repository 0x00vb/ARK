//! Helper for driving external CLI tools. Captures the exact command, stdout,
//! stderr and exit code so every subprocess-backed action is observable and
//! reproducible.

use crate::AdapterError;
use tokio::process::Command;

/// A spawned tool in its own process group. Dropping the guard while the tool runs (a
/// cancelled turn drops the call's future) kills the whole group, so a shell's children die
/// with it: `kill_on_drop` alone stops `sh -c "sleep 30"` but leaves the `sleep` running.
struct Group(Option<u32>);

impl Group {
    fn of(child: &tokio::process::Child) -> Self {
        Group(child.id())
    }
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.take() {
            // SAFETY: killpg only sends a signal; the group id is the child's own pid.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    /// The tool finished on its own; leave anything it started deliberately alone.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

fn tool_command(resolved: &std::path::Path, args: &[String]) -> Command {
    let mut c = Command::new(resolved);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    c.process_group(0);
    c
}

/// The captured result of running an external command.
pub struct Run {
    pub command: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Whether a tool is on PATH.
pub fn tool_exists(tool: &str) -> bool {
    which(tool).is_some()
}

/// Resolve a tool to an absolute path via the same lookup a shell would use.
pub fn which(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    // Fall back to common Homebrew/Apple locations even if PATH is minimal (GUI apps).
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
    std::env::split_paths(&path)
        .chain(extra.iter().map(std::path::PathBuf::from))
        .map(|dir| dir.join(tool))
        .find(|p| p.is_file())
}

/// Run `tool` with `args`, capturing everything. Does not treat a non-zero exit
/// as an error — the caller decides, since some tools use exit codes as signal.
pub async fn run(tool: &str, args: &[String]) -> Result<Run, AdapterError> {
    let resolved = which(tool).ok_or_else(|| AdapterError::ToolUnavailable {
        tool: tool.to_string(),
        detail: "not found on PATH".into(),
    })?;

    let mut command = vec![tool.to_string()];
    command.extend(args.iter().cloned());

    let child =
        tool_command(&resolved, args)
            .spawn()
            .map_err(|e| AdapterError::ToolUnavailable {
                tool: tool.to_string(),
                detail: e.to_string(),
            })?;
    let mut group = Group::of(&child);
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| AdapterError::ToolUnavailable {
            tool: tool.to_string(),
            detail: e.to_string(),
        })?;
    group.disarm();

    Ok(Run {
        command,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    })
}

/// Run `tool` but kill it after `secs` seconds, returning whatever it produced so
/// far (marked with a synthetic exit code of 124, like `timeout(1)`). For driving
/// interactive tools (a debugger) that could otherwise block the engine forever.
pub async fn run_timeout(tool: &str, args: &[String], secs: u64) -> Result<Run, AdapterError> {
    let program = which(tool).ok_or_else(|| AdapterError::ToolUnavailable {
        tool: tool.to_string(),
        detail: "not found on PATH".into(),
    })?;
    let mut run = run_spec(&Spec {
        args: args.to_vec(),
        timeout: Some(std::time::Duration::from_secs(secs)),
        ..Spec::new(program)
    })
    .await?;
    run.command[0] = tool.to_string();
    Ok(run)
}

/// A process to run: what, where, with which environment, for how long, and how much of its
/// output to keep.
#[derive(Clone, Debug)]
pub struct Spec {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    /// The working folder (`None`: the caller's).
    pub cwd: Option<std::path::PathBuf>,
    /// Start from an empty environment with only these variables (`None`: inherit).
    pub env: Option<Vec<(String, String)>>,
    /// Kill the process group after this long; the run ends with code 124.
    pub timeout: Option<std::time::Duration>,
    /// Keep only the last this-many bytes of each stream (`None`: all of it).
    pub keep: Option<usize>,
}

impl Spec {
    pub fn new(program: impl Into<std::path::PathBuf>) -> Self {
        Spec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: None,
            timeout: None,
            keep: None,
        }
    }
}

/// Run a [`Spec`]. Both streams are read while the process runs, so a chatty process never
/// blocks on a full pipe. A cancelled call (the future dropped) or a timeout kills the whole
/// process group. A non-zero exit is not an error.
pub async fn run_spec(spec: &Spec) -> Result<Run, AdapterError> {
    let mut command = vec![spec.program.display().to_string()];
    command.extend(spec.args.iter().cloned());
    let mut c = tool_command(&spec.program, &spec.args);
    if let Some(dir) = &spec.cwd {
        c.current_dir(dir);
    }
    if let Some(env) = &spec.env {
        c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    let mut child = c.spawn().map_err(|e| AdapterError::ToolUnavailable {
        tool: command[0].clone(),
        detail: e.to_string(),
    })?;
    let mut group = Group::of(&child);
    let out = tokio::spawn(drain(child.stdout.take(), spec.keep));
    let err = tokio::spawn(drain(child.stderr.take(), spec.keep));

    let (status, timed_out) = match spec.timeout {
        Some(t) => match tokio::time::timeout(t, child.wait()).await {
            Ok(s) => (s.ok().and_then(|s| s.code()), false),
            Err(_) => {
                group.kill();
                let _ = child.kill().await;
                (None, true)
            }
        },
        None => (child.wait().await.ok().and_then(|s| s.code()), false),
    };
    if !timed_out {
        group.disarm();
    }
    // A process that left a child holding the pipes open must not hold the caller too.
    let grace = std::time::Duration::from_secs(2);
    let stdout = tokio::time::timeout(grace, out)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let mut stderr = tokio::time::timeout(grace, err)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    if timed_out {
        if let Some(t) = spec.timeout {
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str(&timeout_note(t));
        }
    }
    Ok(Run {
        command,
        stdout,
        stderr,
        code: status.unwrap_or(if timed_out { 124 } else { -1 }),
    })
}

/// The last line of a run's stderr when [`Spec::timeout`] stopped it.
pub fn timeout_note(t: std::time::Duration) -> String {
    format!("(killed after {}s timeout)", t.as_secs())
}

/// Read a stream to its end, keeping the last `keep` bytes; what was cut is said at the start.
async fn drain<R: tokio::io::AsyncRead + Unpin>(stream: Option<R>, keep: Option<usize>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut kept: Vec<u8> = Vec::new();
    let mut cut: u64 = 0;
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if let Some(k) = keep {
                    if kept.len() > k {
                        let over = kept.len() - k;
                        kept.drain(..over);
                        cut += over as u64;
                    }
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&kept).into_owned();
    if cut == 0 {
        text
    } else {
        format!("[{cut} earlier bytes not kept]\n{text}")
    }
}

/// Run and require exit 0, mapping failure into a `ToolFailed` error.
pub async fn run_ok(tool: &str, args: &[String]) -> Result<Run, AdapterError> {
    let r = run(tool, args).await?;
    if r.code != 0 {
        return Err(AdapterError::ToolFailed {
            tool: tool.to_string(),
            code: r.code,
            stderr: r.stderr.chars().take(2000).collect(),
        });
    }
    Ok(r)
}

#[cfg(test)]
mod rt_tests {
    use super::*;

    /// A cancelled call (its future dropped) takes the shell's children down with it.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_call_kills_the_shells_children() {
        let dir = std::env::temp_dir().join(format!("ark-pg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let args = ["-c".to_string(), script];
        let call = run_timeout("sh", &args, 60);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(800), call)
                .await
                .is_err()
        );
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // SAFETY: signal 0 only checks whether the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !alive,
            "the grandchild sleep {pid} outlived the cancelled call"
        );
    }
    #[tokio::test]
    async fn a_process_that_prints_more_than_a_pipe_holds_finishes() {
        // 200 KB on stdout: more than a pipe buffer. Reading only after exit would hang.
        let r = run_timeout(
            "sh",
            &["-c".into(), "head -c 200000 /dev/zero | tr '\\0' x".into()],
            10,
        )
        .await
        .unwrap();
        assert_eq!(r.code, 0);
        assert_eq!(r.stdout.len(), 200_000);
    }

    #[tokio::test]
    async fn a_spec_keeps_the_tail_runs_in_its_folder_and_env() {
        let dir = std::env::temp_dir();
        let mut spec = Spec::new(which("sh").unwrap());
        spec.args = vec![
            "-c".into(),
            "pwd; echo \"v=$ONLY\"; echo \"home=$HOME\"; head -c 5000 /dev/zero | tr '\\0' y"
                .into(),
        ];
        spec.cwd = Some(dir.clone());
        spec.env = Some(vec![("ONLY".into(), "1".into())]);
        spec.keep = Some(100);
        let r = run_spec(&spec).await.unwrap();
        assert!(r.stdout.starts_with("["), "{}", r.stdout);
        assert!(r.stdout.ends_with(&"y".repeat(50)));
        spec.keep = None;
        spec.args = vec!["-c".into(), "pwd; echo \"v=$ONLY home=$HOME\"".into()];
        let r = run_spec(&spec).await.unwrap();
        let real = dir.canonicalize().unwrap();
        assert!(r.stdout.contains(real.to_str().unwrap()), "{}", r.stdout);
        assert!(r.stdout.contains("v=1 home=\n"), "{}", r.stdout);
    }

    #[tokio::test]
    async fn run_timeout_captures_stdout() {
        let r = run_timeout(
            "sh",
            &["-c".into(), "echo hello123 && echo world456".into()],
            5,
        )
        .await
        .unwrap();
        assert!(r.stdout.contains("hello123"), "stdout was: {:?}", r.stdout);
        assert!(r.stdout.contains("world456"));
        assert_eq!(r.code, 0);
    }
}
