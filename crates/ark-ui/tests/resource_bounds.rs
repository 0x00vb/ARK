//! Leave the workspace running: many turns through the host (each runs a real subprocess),
//! then many browser connections opened and dropped. Memory, file descriptors and child
//! processes must level off, and the store must grow by a bounded amount per turn.
//!
//! `ARK_STRESS_TURNS` (default 300) and `ARK_STRESS_CONNECTS` (default 300) set the size.
//! Run with `--nocapture` to print the table recorded in docs/OPERATIONS.md.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_sdk::prelude::*;
use ark_ui::ArkUi;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// `sh.echo` runs `/bin/sh -c 'echo …'` as a child process.
struct Shell;

#[async_trait::async_trait]
impl Adapter for Shell {
    fn name(&self) -> &str {
        "sh"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("sh.echo", "echo through a shell", RiskTier::Read).volatile()]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let r = ark_tools::subprocess::run("sh", &["-c".into(), "echo the locket".into()]).await?;
        Ok(ActionOutput::json(json!({ "stdout": r.stdout })))
    }
}

/// Calls the tool, then answers. Keeps nothing between calls (a recording mock would grow).
struct Steady;

#[async_trait::async_trait]
impl Provider for Steady {
    async fn complete(
        &self,
        _s: &str,
        messages: &[Message],
        _t: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        Ok(if messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "It says the locket. ".repeat(20),
                tool_calls: vec![],
                usage: Usage::measured(1000, 100),
            }
        } else {
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: "sh.echo".into(),
                    arguments: json!({}),
                }],
                usage: Usage::measured(1000, 10),
            }
        })
    }
    fn describe(&self) -> String {
        "steady".into()
    }
}

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    rss_mb: f64,
    fds: usize,
    children: usize,
    store_kb: u64,
}

fn sample(store: &std::path::Path) -> Sample {
    let pid = std::process::id().to_string();
    let rss_kb: f64 = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0.0);
    let fd_dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let fds = std::fs::read_dir(fd_dir).map(|d| d.count()).unwrap_or(0);
    // Children of this process, other than the `pgrep` doing the counting.
    let children = std::process::Command::new("pgrep")
        .args(["-P", &pid])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .count()
                .saturating_sub(1)
        })
        .unwrap_or(0);
    let store_kb = walk(store) / 1024;
    Sample {
        rss_mb: rss_kb / 1024.0,
        fds,
        children,
        store_kb,
    }
}

fn walk(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .map(|d| {
            d.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => walk(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_running_workspace_stays_bounded() {
    let turns = env_usize("ARK_STRESS_TURNS", 300);
    let connects = env_usize("ARK_STRESS_CONNECTS", 300);
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(Steady);
    let provider: Arc<dyn ark_model::Provider> = model;
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Shell))
        .profile(AgentProfile::builder().core_actions(["sh.echo"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (host, served) = ArkUi::new(App::new("Stress").ark(ark))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();

    let mut rx = host.subscribe();
    let mut rows: Vec<(usize, Sample, u64)> = Vec::new();
    let mut warm = None;
    let started = Instant::now();
    for i in 1..=turns {
        let t0 = Instant::now();
        host.call(
            "send",
            json!({ "text": format!("turn {i}: what does the shell say?") }),
        )
        .await
        .unwrap();
        loop {
            let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("the turn finished")
                .unwrap();
            if v["kind"] == "turn_finished" {
                assert_eq!(v["ok"], true, "turn {i}: {v}");
                break;
            }
        }
        let ms = t0.elapsed().as_millis() as u64;
        if i == 50.min(turns) || i % 100 == 0 || i == turns {
            let s = sample(dir.path());
            if i == 50.min(turns) {
                warm = Some(s);
            }
            assert_eq!(
                s.children, 0,
                "no child process outlives its call (turn {i})"
            );
            rows.push((i, s, ms));
        }
    }
    let after_turns = sample(dir.path());

    // Browser windows that connect and go away.
    let port = served.port;
    let token = served.token.clone();
    for _ in 0..connects {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req = format!(
            "GET /events?token={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 256];
        let n = sock.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
        drop(sock);
    }
    // A dropped stream is noticed at its next write; one event reaches every stream.
    host.call("send", json!({ "text": "one more" }))
        .await
        .unwrap();
    loop {
        let v = rx.recv().await.unwrap();
        if v["kind"] == "turn_finished" {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_connects = sample(dir.path());

    println!("\n| turn | RSS (MB) | open fds | child processes | store (KB) | turn (ms) |");
    println!("|---:|---:|---:|---:|---:|---:|");
    for (i, s, ms) in &rows {
        println!(
            "| {i} | {:.1} | {} | {} | {} | {ms} |",
            s.rss_mb, s.fds, s.children, s.store_kb
        );
    }
    println!(
        "after {connects} connects: RSS {:.1} MB, {} fds, {} children; {} turns in {:.1}s",
        after_connects.rss_mb,
        after_connects.fds,
        after_connects.children,
        turns,
        started.elapsed().as_secs_f64()
    );

    let warm = warm.unwrap();
    assert!(
        after_turns.fds <= warm.fds + 8,
        "file descriptors level off: {} at warm-up, {} at the end",
        warm.fds,
        after_turns.fds
    );
    assert!(
        after_connects.fds <= after_turns.fds + 8,
        "dropped connections release their sockets: {} -> {}",
        after_turns.fds,
        after_connects.fds
    );
    assert_eq!(after_connects.children, 0);
    let per_turn_kb = (after_turns.store_kb.saturating_sub(warm.store_kb)) as f64
        / (turns.saturating_sub(50).max(1)) as f64;
    assert!(
        per_turn_kb < 64.0,
        "store grows {per_turn_kb:.1} KB per turn"
    );
    assert!(
        after_turns.rss_mb < warm.rss_mb + 64.0,
        "memory levels off: {:.1} MB at warm-up, {:.1} MB at the end",
        warm.rss_mb,
        after_turns.rss_mb
    );
}
