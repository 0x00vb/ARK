//! Helper for driving external CLI tools. Captures the exact command, stdout,
//! stderr and exit code so every subprocess-backed action is observable and
//! reproducible.

use crate::AdapterError;
use ark_os::process::Tree;
use tokio::process::Command;

/// A tool started the way ARK starts every program (`ark_os::process::command`): in its own
/// group, so a cancelled call (its future dropped) or a timeout stops the tool together with
/// what it started (`ark_os::process::Tree`). `kill_on_drop` alone stops `sh -c "sleep 30"` but
/// leaves the `sleep` running.
fn tool_command(resolved: &std::path::Path, args: &[String]) -> Command {
    let mut c = ark_os::process::command(resolved);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    c
}

/// The captured result of running an external command.
pub struct Run {
    pub command: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    /// The limit ([`Spec::limits`]) it went past and was stopped for.
    pub stopped: Option<ark_os::process::Stop>,
}

/// Whether a tool is on PATH.
pub fn tool_exists(tool: &str) -> bool {
    which(tool).is_some()
}

/// Resolve a tool to an absolute path the way a shell would (`PATHEXT` on Windows), also
/// where installers put programs a GUI app's short `PATH` misses (`ark_os::find::program`).
pub fn which(tool: &str) -> Option<std::path::PathBuf> {
    ark_os::find::program(tool)
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
    let mut group = Tree::of(child.id());
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
        stopped: None,
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
    /// Stop the program and what it started after this long; the run ends with code 124.
    pub timeout: Option<std::time::Duration>,
    /// Keep only the last this-many bytes of each stream (`None`: all of it).
    pub keep: Option<usize>,
    /// The most memory and processor time the program and what it starts may use; past
    /// either it is stopped, and [`Run::stopped`] says which (`run_contained` only).
    pub limits: ark_os::process::Limits,
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
            limits: Default::default(),
        }
    }
}

/// Run a [`Spec`]. Both streams are read while the process runs, so a chatty process never
/// blocks on a full pipe. A cancelled call (the future dropped) or a timeout stops the program
/// and everything it started. A non-zero exit is not an error.
pub async fn run_spec(spec: &Spec) -> Result<Run, AdapterError> {
    run_managed(spec, std::future::pending(), false)
        .await
        .map_err(|e| e.error)
}

/// Distinguishes a launch error from an incomplete teardown. Only the latter prevents safe
/// replacement work from using the same resource slot.
#[derive(Debug)]
pub struct ContainedError {
    pub error: AdapterError,
    pub cleanup_incomplete: bool,
}

impl From<AdapterError> for ContainedError {
    fn from(error: AdapterError) -> Self {
        Self {
            error,
            cleanup_incomplete: false,
        }
    }
}

/// Run finite contained work. Cancellation is observed without dropping the owner: the
/// child is reaped, its group is stopped even on success, and output readers are joined.
/// No CPU priority or rate limit is installed.
pub async fn run_contained(
    spec: &Spec,
    cancel: impl std::future::Future<Output = ()>,
) -> Result<Run, ContainedError> {
    run_managed(spec, cancel, true).await
}

async fn run_managed(
    spec: &Spec,
    cancel: impl std::future::Future<Output = ()>,
    contained: bool,
) -> Result<Run, ContainedError> {
    let mut command = vec![spec.program.display().to_string()];
    command.extend(spec.args.iter().cloned());
    let mut c = tool_command(&spec.program, &spec.args);
    if let Some(dir) = &spec.cwd {
        c.current_dir(dir);
    }
    if let Some(env) = &spec.env {
        c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    let limits = if contained {
        spec.limits
    } else {
        Default::default()
    };
    #[cfg(unix)]
    if let Some(seconds) = limits.cpu_seconds {
        ark_os::process::limit_cpu(&mut c, seconds);
    }
    let mut child = c.spawn().map_err(|e| AdapterError::ToolUnavailable {
        tool: command[0].clone(),
        detail: e.to_string(),
    })?;
    let mut group = Tree::of(child.id());
    if contained && !group.attached() {
        let _ = child.kill().await;
        return Err(ContainedError {
            error: AdapterError::Tool("could not attach contained process cleanup".into()),
            cleanup_incomplete: true,
        });
    }
    let mut out = Reader(tokio::spawn(drain(child.stdout.take(), spec.keep)));
    let mut err = Reader(tokio::spawn(drain(child.stderr.take(), spec.keep)));

    let wait = async {
        match spec.timeout {
            Some(t) => tokio::time::timeout(t, child.wait()).await.map_err(|_| ()),
            None => Ok(child.wait().await),
        }
    };
    // Memory (and on Windows processor time) is measured while it runs; Unix limits each
    // process's processor time itself.
    let watch = async {
        if limits == Default::default() {
            return std::future::pending().await;
        }
        let mut every = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            every.tick().await;
            if let Some(stop) = group.usage().over(&limits) {
                return stop;
            }
        }
    };
    let mut stopped = None;
    let (status, timed_out, cancelled) = tokio::select! {
        biased;
        _ = cancel => (None, false, true),
        stop = watch => {
            stopped = Some(stop);
            (None, false, false)
        }
        result = wait => match result {
            Ok(status) => {
                let status = status.ok();
                if let (Some(s), Some(max)) = (&status, limits.cpu_seconds) {
                    if ark_os::process::stopped_by_cpu_limit(s) {
                        stopped = Some(ark_os::process::Stop::Cpu(max));
                    }
                }
                (status.and_then(|s| s.code()), false, false)
            }
            Err(()) => (None, true, false),
        }
    };
    if stopped.is_some() {
        group.terminate();
        let _ = child.kill().await;
    }
    if timed_out || cancelled {
        if contained {
            group.terminate();
        } else {
            group.kill();
        }
        let _ = child.kill().await;
    }
    if contained {
        group.stop().await.map_err(|e| ContainedError {
            error: AdapterError::Tool(format!("contained process cleanup failed: {e}")),
            cleanup_incomplete: true,
        })?;
    } else if !timed_out {
        group.disarm();
    }
    // A process that left a child holding the pipes open must not hold the caller too.
    let grace = std::time::Duration::from_secs(2);
    let stdout = out.finish(grace).await;
    let mut stderr = err.finish(grace).await;
    if timed_out {
        if let Some(t) = spec.timeout {
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str(&timeout_note(t));
        }
    }
    if let Some(stop) = stopped {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&stop_note(stop));
    }
    Ok(Run {
        command,
        stdout,
        stderr,
        code: status.unwrap_or(if timed_out {
            124
        } else if cancelled {
            130
        } else {
            -1
        }),
        stopped,
    })
}

struct Reader(tokio::task::JoinHandle<String>);
impl Reader {
    async fn finish(&mut self, grace: std::time::Duration) -> String {
        match tokio::time::timeout(grace, &mut self.0).await {
            Ok(result) => result.unwrap_or_default(),
            Err(_) => {
                self.0.abort();
                let _ = (&mut self.0).await;
                String::new()
            }
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The last line of a run's stderr when a limit stopped it: what the limit was, in plain words.
pub fn stop_note(stop: ark_os::process::Stop) -> String {
    match stop {
        ark_os::process::Stop::Memory(max) => {
            format!("(stopped: it used more than {} of memory)", bytes(max))
        }
        ark_os::process::Stop::Cpu(max) => {
            format!("(stopped: it used more than {max} s of processor time)")
        }
    }
}

/// `512 MB`, `2 GB`: a byte count as a person reads it.
pub fn bytes(n: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if n >= GB && n.is_multiple_of(GB) {
        format!("{} GB", n / GB)
    } else if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else {
        format!("{} MB", n.div_ceil(MB))
    }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn contained_success_stops_background_children_but_ordinary_runs_keep_them() {
        let dir = tempfile::tempdir().unwrap();
        for contained in [false, true] {
            let path = dir
                .path()
                .join(if contained { "contained" } else { "ordinary" });
            let mut spec = Spec::new("/bin/sh");
            spec.args = vec![
                "-c".into(),
                "sleep 30 </dev/null >/dev/null 2>&1 & echo $! > \"$1\"".into(),
                "sh".into(),
                path.display().to_string(),
            ];
            spec.keep = Some(100);
            let result = if contained {
                run_contained(&spec, std::future::pending())
                    .await
                    .map_err(|e| e.error)
            } else {
                run_spec(&spec).await
            };
            let pid: u32 = std::fs::read_to_string(path)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let was_alive = ark_os::process::alive(pid);
            // Always clean this test's ordinary background child, including on assertion failure.
            if was_alive {
                let mut kill = Spec::new("/bin/kill");
                kill.args = vec!["-KILL".into(), pid.to_string()];
                let _ = run_spec(&kill).await;
            }
            assert!(
                result.is_ok(),
                "{}",
                result.err().map(|e| e.to_string()).unwrap_or_default()
            );
            assert_eq!(was_alive, !contained);
        }
    }

    #[tokio::test]
    async fn a_missing_program_is_not_an_incomplete_teardown() {
        let result = run_contained(
            &Spec::new("/does-not-exist/ark-run"),
            std::future::pending(),
        )
        .await;
        let error = result.err().expect("a launch error");
        assert!(!error.cleanup_incomplete);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contained_cancellation_joins_child_and_readers() {
        let (cancel, receiver) = tokio::sync::oneshot::channel::<()>();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let mut spec = Spec::new("/bin/sh");
        spec.args = vec![
            "-c".into(),
            "echo $$ > \"$1\"; exec sleep 30".into(),
            "sh".into(),
            path.display().to_string(),
        ];
        spec.keep = Some(100);
        let job = tokio::spawn(async move {
            run_contained(&spec, async {
                let _ = receiver.await;
            })
            .await
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let pid = std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        cancel.send(()).unwrap();
        let result = job.await.unwrap().unwrap();
        assert_eq!(result.code, 130);
        assert!(!ark_os::process::alive(pid));
    }

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
        let alive = ark_os::process::alive(pid as u32);
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
