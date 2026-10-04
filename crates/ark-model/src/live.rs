//! A vendor CLI kept running between calls: requests go in as JSON lines on stdin, events come
//! back as JSON lines on stdout. Starting the CLI is most of a short call's time (Claude
//! Code: 2.7 s a call started fresh, 0.7 s on a running process, measured 2026-10-03), so a
//! managed vendor session keeps its process.
//!
//! The process is the owner's to take for a call and give back after a complete reply. A
//! call that is cancelled drops it, and dropping kills it, so a process whose reply was cut
//! off is never written to again.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How much of the CLI's stderr is kept, for error messages.
const STDERR_TAIL: usize = 4096;

pub(crate) struct LiveProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
}

/// Why no line came.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// The process closed its output (it exited).
    Closed,
    TimedOut,
    Io(std::io::Error),
}

impl LiveProcess {
    /// Start `cmd` with piped stdio. It is killed when this value is dropped.
    pub(crate) fn spawn(mut cmd: Command) -> std::io::Result<Self> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let stderr = Arc::new(Mutex::new(String::new()));
        let tail = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut t = tail.lock().unwrap();
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                if t.len() > STDERR_TAIL {
                    let cut = t.len() - STDERR_TAIL;
                    let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                    t.drain(..cut);
                }
            }
        });
        Ok(LiveProcess {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr,
        })
    }

    /// Write one line (a newline is added).
    pub(crate) async fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await
    }

    /// The next line of output, waiting at most `wait`.
    pub(crate) async fn next_line(&mut self, wait: Duration) -> Result<String, ReadError> {
        match tokio::time::timeout(wait, self.lines.next_line()).await {
            Err(_) => Err(ReadError::TimedOut),
            Ok(Err(e)) => Err(ReadError::Io(e)),
            Ok(Ok(None)) => Err(ReadError::Closed),
            Ok(Ok(Some(l))) => Ok(l),
        }
    }

    /// Whether the process is still running.
    pub(crate) fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The last line the CLI wrote to stderr, if any.
    pub(crate) fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .unwrap()
            .trim()
            .lines()
            .last()
            .unwrap_or("")
            .to_string()
    }

    /// Stop the process and wait for it to exit.
    pub(crate) async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_go_in_and_come_back() {
        let mut c = Command::new("/bin/sh");
        c.args([
            "-c",
            "while IFS= read -r l; do echo \"got $l\"; echo oops >&2; done",
        ]);
        let mut p = LiveProcess::spawn(c).unwrap();
        p.send("one").await.unwrap();
        assert_eq!(
            p.next_line(Duration::from_secs(5)).await.unwrap(),
            "got one"
        );
        p.send("two").await.unwrap();
        assert_eq!(
            p.next_line(Duration::from_secs(5)).await.unwrap(),
            "got two"
        );
        assert!(p.running());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(p.stderr_tail(), "oops");
        assert!(matches!(
            p.next_line(Duration::from_millis(50)).await,
            Err(ReadError::TimedOut)
        ));
        p.stop().await;
    }

    #[tokio::test]
    async fn an_exit_closes_the_output() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "read l; echo bye; exit 3"]);
        let mut p = LiveProcess::spawn(c).unwrap();
        p.send("x").await.unwrap();
        assert_eq!(p.next_line(Duration::from_secs(5)).await.unwrap(), "bye");
        assert!(matches!(
            p.next_line(Duration::from_secs(5)).await,
            Err(ReadError::Closed)
        ));
    }
}
