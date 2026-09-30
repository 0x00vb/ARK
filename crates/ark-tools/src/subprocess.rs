//! Helper for driving external CLI tools. Captures the exact command, stdout,
//! stderr and exit code so every subprocess-backed action is observable and
//! reproducible.

use crate::AdapterError;
use tokio::process::Command;

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

    let out = Command::new(resolved)
        .args(args)
        .output()
        .await
        .map_err(|e| AdapterError::ToolUnavailable {
            tool: tool.to_string(),
            detail: e.to_string(),
        })?;

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
    let resolved = which(tool).ok_or_else(|| AdapterError::ToolUnavailable {
        tool: tool.to_string(),
        detail: "not found on PATH".into(),
    })?;
    let mut command = vec![tool.to_string()];
    command.extend(args.iter().cloned());

    use tokio::io::AsyncReadExt;
    let mut child = Command::new(resolved)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AdapterError::ToolUnavailable { tool: tool.to_string(), detail: e.to_string() })?;

    let dur = std::time::Duration::from_secs(secs);
    let (status, timed_out) = match tokio::time::timeout(dur, child.wait()).await {
        Ok(Ok(s)) => (s.code(), false),
        Ok(Err(_)) => (None, false),
        Err(_) => {
            let _ = child.kill().await;
            (None, true)
        }
    };

    // Drain whatever the tool wrote (small for batch tools; safe to read after exit).
    let mut stdout = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut stdout).await;
    }
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr).await;
    }
    if timed_out && stderr.is_empty() {
        stderr = format!("(killed after {secs}s timeout)");
    }
    Ok(Run { command, stdout, stderr, code: status.unwrap_or(if timed_out { 124 } else { -1 }) })
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
    #[tokio::test]
    async fn run_timeout_captures_stdout() {
        let r = run_timeout("sh", &["-c".into(), "echo hello123 && echo world456".into()], 5).await.unwrap();
        assert!(r.stdout.contains("hello123"), "stdout was: {:?}", r.stdout);
        assert!(r.stdout.contains("world456"));
        assert_eq!(r.code, 0);
    }
}
