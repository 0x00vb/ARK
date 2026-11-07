//! A served app stopped for real, then started again on the same store (ADR 0037, ADR 0038).
//!
//! The person's chat hands a task to an agent and waits. The agent saves a chapter, then its
//! next model call hangs. The process is stopped: SIGTERM, as a deploy does, or SIGKILL, as a
//! crash does. The next process continues the task without saving twice, and the result
//! reaches the chat that asked, once.
//!
//! The model is a fake OpenAI-compatible server in this test, so it runs without a live model.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const WRITER: &str = env!("CARGO_BIN_EXE_writer");

/// The fake model. The chat hands the task to `scribe`; the agent saves `notes.md`, then
/// answers, unless `hang` holds its answer back.
struct Model {
    hang: AtomicBool,
    /// One line per request: who asked and the last tool result it carried.
    log: Mutex<Vec<String>>,
}

impl Model {
    fn reply(&self, body: &Value) -> Option<Value> {
        let tools: Vec<&str> = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let results: Vec<String> = messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or("").to_string())
            .collect();
        let text: String = messages
            .iter()
            .map(|m| m["content"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let last = results.last().cloned().unwrap_or_default();
        if tools.contains(&"agents__delegate") {
            self.log.lock().unwrap().push(format!("chat | {last}"));
            return Some(if results.is_empty() {
                call(
                    "agents__delegate",
                    json!({ "to": "scribe", "objective": "Save the notes to notes.md with manuscript.save" }),
                )
            } else {
                say("The scribe saved the notes. [inv:1]")
            });
        }
        if tools.contains(&"manuscript__save") && text.contains("== OBJECTIVE ==") {
            self.log.lock().unwrap().push(format!("agent | {last}"));
            if results.is_empty() {
                return Some(call(
                    "manuscript__save",
                    json!({ "file": "notes.md", "text": "The rain stopped at noon." }),
                ));
            }
            if self.hang.load(Ordering::SeqCst) {
                return None;
            }
            return Some(say("Saved notes.md. [inv:1]"));
        }
        Some(say("OK"))
    }
}

fn say(text: &str) -> Value {
    json!({ "content": text })
}

fn call(name: &str, args: Value) -> Value {
    json!({ "tool_calls": [{ "index": 0, "id": "call_1", "type": "function",
        "function": { "name": name, "arguments": args.to_string() } }] })
}

/// Serve the fake model; returns its base URL.
async fn serve(model: Arc<Model>) -> String {
    serve_with(Arc::new(move |b: &Value| model.reply(b))).await
}

/// One `writer --serve` process over the store at `root`.
struct Served {
    child: Child,
    url: String,
    token: String,
}

impl Served {
    fn start(root: &Path, model: &str, token: &str) -> Served {
        let mut child = Command::new(WRITER)
            .args([
                "--serve",
                "127.0.0.1:0",
                "--tokens",
                "--model",
                "openai:fake",
            ])
            .env("WRITER_ROOT", root)
            .env("OPENAI_BASE_URL", model)
            .env("OPENAI_API_KEY", "test")
            .env("ARK_STOP_GRACE_SECS", "10")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url = line.trim().trim_end_matches('/').to_string();
        assert!(url.starts_with("http://"), "no address: {line:?}");
        Served {
            child,
            url,
            token: token.to_string(),
        }
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        if matches!(
            method,
            "spawn_agent" | "graph" | "orch_events" | "schedule" | "schedules" | "task"
        ) {
            let location = self.raw_rpc("summary", json!({}))["location"].clone();
            return self.raw_rpc(
                "resource",
                json!({
                    "ref": { "location": location, "kind": "project", "id": "" },
                    "operation": { "kind": method, "input": params },
                }),
            );
        }
        self.raw_rpc(method, params)
    }

    fn raw_rpc(&self, method: &str, params: Value) -> Value {
        let out = Command::new("curl")
            .args(["-s", "-m", "20", "-X", "POST"])
            .arg(format!("{}/rpc", self.url))
            .args(["-H", &format!("authorization: Bearer {}", self.token)])
            .args(["-H", "content-type: application/json"])
            .args([
                "-d",
                &json!({ "method": method, "params": params }).to_string(),
            ])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{method}: {}", String::from_utf8_lossy(&out.stdout)));
        assert!(v.get("error").is_none_or(Value::is_null), "{method}: {v}");
        v.get("result").cloned().unwrap_or(v)
    }

    /// Send a signal; returns how long the process took to exit and whether it exited cleanly.
    fn stop(mut self, signal: &str) -> (Duration, bool) {
        let at = Instant::now();
        Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()
            .unwrap();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return (at.elapsed(), status.success());
            }
            assert!(at.elapsed() < Duration::from_secs(30), "still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        }
    }
    None
}

fn until<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let at = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            at.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The scenario, stopped with `signal`. Returns the stop's duration and whether it was clean.
fn stopped_while_an_agent_works(signal: &str) -> (Duration, bool) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let model = Arc::new(Model {
        hang: AtomicBool::new(true),
        log: Mutex::new(Vec::new()),
    });
    let base = rt.block_on(serve(model.clone()));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let token = Command::new(WRITER)
        .args(["--new-token", "test"])
        .env("WRITER_ROOT", &root)
        .output()
        .unwrap();
    let token = String::from_utf8_lossy(&token.stdout).trim().to_string();

    // First process: the chat hands the task off and waits; the agent saves, then hangs.
    let first = Served::start(&root, &base, &token);
    first.rpc(
        "spawn_agent",
        json!({ "spec": { "name": "scribe", "role": "saves notes", "tools": ["manuscript.*"], "allow_mutate": true } }),
    );
    let chat = first.rpc("summary", json!({}))["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    first.rpc("send", json!({ "text": "have the scribe save the notes" }));
    let saved = until("the agent to save, then think", 60, || {
        let asked = model
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("agent | ") && l.contains("notes.md"));
        asked.then(|| find(&root, "notes.md")).flatten()
    });
    let written = std::fs::metadata(&saved).unwrap().modified().unwrap();
    let task = first.rpc("graph", json!({}))["tasks"][0]["task"]
        .as_str()
        .unwrap()
        .to_string();

    let stop = first.stop(signal);

    // Second process: the task continues and ends; its result reaches the chat once.
    model.hang.store(false, Ordering::SeqCst);
    let second = Served::start(&root, &base, &token);
    let done = until("the task to end", 60, || {
        let t = second.rpc("task", json!({ "task": task }))["task"].clone();
        matches!(t["state"].as_str(), Some("done" | "failed" | "cancelled")).then_some(t)
    });
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["reported"], true);
    assert_eq!(done["report_to"].as_str(), Some(chat.as_str()));

    // Not failed on the way: it was queued to continue, and continued.
    let events = second.rpc("orch_events", json!({ "limit": 500 }));
    let text = events.to_string();
    assert!(!text.contains("\"state\":\"failed\""), "{text}");
    // Saved once: the continuation's save was answered from the first one, not run.
    assert_eq!(
        std::fs::metadata(&saved).unwrap().modified().unwrap(),
        written
    );
    let log = model.log.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|l| l.starts_with("agent | ") && l.contains("not run again")),
        "{log:#?}"
    );

    // The chat that asked has the result once, after its turn that did not finish.
    let turns = second.rpc("transcript", json!({ "session": chat }));
    let posts: Vec<&Value> = turns
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["posts"].as_array().into_iter().flatten())
        .collect();
    // Either stop leaves the chat's turn interrupted, not stopped by the person, with a way to
    // continue it (ADR 0048).
    assert_eq!(turns[0]["cancelled"], false, "{turns}");
    assert_eq!(turns[0]["interrupted"], true, "{turns}");
    assert_eq!(turns[0]["resume"], true, "{turns}");
    assert_eq!(posts.len(), 1, "{turns}");
    assert_eq!(posts[0]["text"], "Saved notes.md. [inv:1]");
    drop(second);
    stop
}

#[test]
fn sigterm_while_an_agent_works_continues_it_after_the_restart_and_reports_once() {
    let (took, clean) = stopped_while_an_agent_works("-TERM");
    assert!(clean, "a SIGTERM exits cleanly");
    assert!(took < Duration::from_secs(8), "stopped in {took:?}");
}

#[test]
fn sigkill_while_an_agent_works_continues_it_after_the_restart_and_reports_once() {
    let (_, clean) = stopped_while_an_agent_works("-KILL");
    assert!(!clean);
}

/// The fake model for a chat's own turn (ADR 0048): asked to save chapter one, it saves, then
/// hangs (the process is killed then). Told to continue, it saves the same chapter again, then
/// answers.
struct ChatModel {
    log: Mutex<Vec<String>>,
}

impl ChatModel {
    fn reply(&self, body: &Value) -> Option<Value> {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let text: String = messages
            .iter()
            .map(|m| m["content"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let results = messages.iter().filter(|m| m["role"] == "tool").count();
        self.log.lock().unwrap().push(text.clone());
        let save = || {
            call(
                "manuscript__save",
                json!({ "file": "chapter-one.md", "text": "It rained all night." }),
            )
        };
        // A turn that continues a stopped one gets its goal, then what the person said since.
        if text.contains("Then they said") && text.contains("  - Continue") {
            return Some(if results == 0 {
                save()
            } else {
                say("Chapter one is saved.")
            });
        }
        if text.contains("save chapter one") {
            return (results == 0).then(save);
        }
        Some(say("OK"))
    }
}

/// A fake model's answer to a request body (a streamed delta), or `None` to hang.
type Reply = Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

/// Serve `reply` as an OpenAI-compatible model; `None` hangs. Returns the base URL.
async fn serve_with(reply: Reply) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let reply = reply.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 16384];
                let (head, body) = loop {
                    let n = match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    let len: usize = head
                        .to_lowercase()
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(str::to_string))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + 4 + len {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    break (head, buf[end + 4..end + 4 + len].to_vec());
                };
                if head.starts_with("GET") {
                    let list = r#"{"object":"list","data":[{"id":"fake"}]}"#;
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{list}",
                                list.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    return;
                }
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let Some(delta) = reply(&body) else {
                    let _ = sock.read(&mut tmp).await;
                    return;
                };
                let mut out = String::from(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                );
                let chunk = json!({ "choices": [{ "index": 0, "delta": delta }] });
                out.push_str(&format!("data: {chunk}\n\n"));
                let usage = json!({ "choices": [], "usage": { "prompt_tokens": 50, "completion_tokens": 10 } });
                out.push_str(&format!("data: {usage}\n\ndata: [DONE]\n\n"));
                let _ = sock.write_all(out.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/v1")
}

/// Answer the window's open question "allow once", when one comes within `secs`.
fn allow_once(served: &Served, secs: u64) {
    let id = until("a question", secs, || {
        served.rpc("hello", json!({}))["approvals"]
            .as_array()
            .and_then(|a| a.first().cloned())
            .map(|a| a["id"].clone())
    });
    served.rpc("approve", json!({ "id": id, "decision": "allow_once" }));
}

/// ADR 0048, end to end in a real process: the chat's own turn saves a chapter, the process is
/// killed (SIGKILL, as a crash) in its next model call, and starts again. The chat shows the
/// turn interrupted, with the save done and a way to continue; "Continue" finishes the work and
/// the chapter is not saved a second time.
#[test]
fn a_chat_turn_killed_after_a_save_continues_without_saving_again() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let model = Arc::new(ChatModel {
        log: Mutex::new(Vec::new()),
    });
    let m = model.clone();
    let base = rt.block_on(serve_with(Arc::new(move |b: &Value| m.reply(b))));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let token = Command::new(WRITER)
        .args(["--new-token", "test"])
        .env("WRITER_ROOT", &root)
        .output()
        .unwrap();
    let token = String::from_utf8_lossy(&token.stdout).trim().to_string();

    let first = Served::start(&root, &base, &token);
    let chat = first.rpc("summary", json!({}))["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // This test is about a question: the person asked for it (a chat starts in Auto, ADR 0107).
    first.rpc("execution_mode", json!({ "mode": "manual", "revision": 0 }));
    first.rpc("send", json!({ "text": "save chapter one" }));
    allow_once(&first, 30);
    let saved = until("the save, then the next model call", 30, || {
        let after = model.log.lock().unwrap().len() >= 2;
        after.then(|| find(&root, "chapter-one.md")).flatten()
    });
    let written = std::fs::metadata(&saved).unwrap().modified().unwrap();
    let (_, _) = first.stop("-KILL");

    let second = Served::start(&root, &base, &token);
    let turns = second.rpc("transcript", json!({ "session": chat }));
    let t = &turns[0];
    assert_eq!(t["interrupted"], true, "{turns}");
    assert_eq!(t["resume"], true, "{turns}");
    assert_eq!(t["unfinished"], "Interrupted before it finished.");
    let results: Vec<&Value> = t["activity"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "result")
        .collect();
    assert_eq!(results.len(), 1, "{turns}");
    assert_ne!(
        results[0]["label"], "cut short",
        "the save is shown done: {turns}"
    );

    // What the Continue button sends.
    second.rpc("open_session", json!({ "id": chat }));
    second.rpc("send", json!({ "text": "Continue" }));
    let turns = until("the continuation to finish", 30, || {
        let turns = second.rpc("transcript", json!({ "session": chat }));
        (turns.as_array().map_or(0, Vec::len) == 2).then_some(turns)
    });
    assert_eq!(turns[1]["answer"], "Chapter one is saved.", "{turns}");
    assert!(turns[1].get("resume").is_none_or(|r| r == false));
    assert_eq!(
        std::fs::metadata(&saved).unwrap().modified().unwrap(),
        written,
        "saved once"
    );
    let log = model.log.lock().unwrap().clone();
    let continued = log.iter().find(|l| l.contains("  - Continue")).unwrap();
    assert!(continued.contains("done: manuscript.save"), "{continued}");
    assert!(
        continued.contains("The person asked for this (it is not finished yet):\nsave chapter one"),
        "the goal is the request, not \"Continue\": {continued}"
    );
    assert!(log.iter().any(|l| l.contains("not run again")), "{log:#?}");
}

/// The fake model for a schedule (ADR 0053): the chat schedules a stock count at `at` (UTC);
/// each run saves `stock.md`, then answers.
struct ScheduleModel {
    at: String,
    log: Mutex<Vec<String>>,
}

impl ScheduleModel {
    fn reply(&self, body: &Value) -> Option<Value> {
        let tools: Vec<&str> = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let text: String = messages
            .iter()
            .map(|m| m["content"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let results = messages.iter().filter(|m| m["role"] == "tool").count();
        if tools.contains(&"agents__schedule") {
            self.log.lock().unwrap().push(format!("chat | {text}"));
            return Some(if results == 0 {
                call(
                    "agents__schedule",
                    json!({
                        "objective": "Count the stock and save the count to stock.md with manuscript.save",
                        "at": self.at, "tz": "UTC"
                    }),
                )
            } else {
                say("Every day at that time. [inv:1]")
            });
        }
        if text.contains("== OBJECTIVE ==") && text.contains("This is the run scheduled for") {
            self.log.lock().unwrap().push(format!("run | {text}"));
            return Some(if results == 0 {
                call(
                    "manuscript__save",
                    json!({ "file": "stock.md", "text": "12 boxes" }),
                )
            } else {
                say("Counted 12 boxes and saved stock.md.")
            });
        }
        Some(say("OK"))
    }
}

/// ADR 0053, end to end in real processes: the chat is asked for a daily stock count, the person
/// approves the schedule, the process is killed, and the next process runs it at its time: the
/// run asks before it saves (scheduling bypasses no approval), saves once, and its result is
/// posted in the chat that asked.
#[test]
fn a_schedule_made_in_the_chat_survives_a_kill_and_runs_at_its_time() {
    // One or two minutes ahead, in UTC: the time the run is for.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ahead = if now % 60 > 40 { 120 } else { 60 };
    let slot = (now / 60) * 60 + ahead;
    let at = format!("{:02}:{:02}", slot % 86_400 / 3600, slot % 3600 / 60);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let model = Arc::new(ScheduleModel {
        at,
        log: Mutex::new(Vec::new()),
    });
    let m = model.clone();
    let base = rt.block_on(serve_with(Arc::new(move |b: &Value| m.reply(b))));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let token = Command::new(WRITER)
        .args(["--new-token", "test"])
        .env("WRITER_ROOT", &root)
        .output()
        .unwrap();
    let token = String::from_utf8_lossy(&token.stdout).trim().to_string();

    let first = Served::start(&root, &base, &token);
    first.rpc("hello", json!({ "tz": "Europe/Madrid" }));
    let chat = first.rpc("summary", json!({}))["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    first.rpc(
        "send",
        json!({ "text": "every day at that time, count the stock and save it" }),
    );
    // Making standing work is asked.
    allow_once(&first, 30);
    let list = until("the schedule", 30, || {
        let l = first.rpc("schedules", json!({ "session": chat }));
        (l.as_array().map_or(0, Vec::len) == 1).then_some(l)
    });
    let s = &list[0];
    assert_eq!(s["tz"], "UTC", "{s}");
    assert_eq!(s["state"], "scheduled", "{s}");
    assert_eq!(s["next_ms"].as_u64(), Some(slot * 1000), "{s}");
    let id = s["id"].as_str().unwrap().to_string();
    // The model was told the date and the person's zone.
    let log = model.log.lock().unwrap().clone();
    assert!(log[0].contains("(Europe/Madrid)"), "{}", log[0]);
    let (_, _) = first.stop("-KILL");
    assert!(find(&root, "stock.md").is_none());

    // The next process runs it at its time; the save is asked like any agent's.
    let second = Served::start(&root, &base, &token);
    allow_once(&second, ahead + 60);
    let saved = until("stock.md", 60, || find(&root, "stock.md"));
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "12 boxes");
    let one = until("the run to end", 60, || {
        let v = second.rpc("schedule", json!({ "schedule": id }));
        (v["schedule"]["state"] == "succeeded").then_some(v)
    });
    let runs = one["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{one}");
    assert_eq!(runs[0]["result"], "Counted 12 boxes and saved stock.md.");
    let started = runs[0]["at_ms"].as_u64().unwrap();
    assert!(
        started >= slot * 1000 && started < slot * 1000 + 30_000,
        "ran at its time: {started} for {}",
        slot * 1000
    );
    // The line shows the next time; the last run is said apart.
    assert!(one["schedule"]["status"]
        .as_str()
        .unwrap()
        .starts_with("next "));
    assert!(one["schedule"]["last"]
        .as_str()
        .unwrap()
        .starts_with("ran "));
    assert_eq!(one["schedule"]["runs"], 1);
    // The run's brief said which time it was for, and nothing of the chat.
    let log = model.log.lock().unwrap().clone();
    let run = log.iter().find(|l| l.starts_with("run | ")).unwrap();
    assert!(!run.contains("every day at that time"), "{run}");
    // Its result is posted in the chat that asked.
    let turns = second.rpc("transcript", json!({ "session": chat }));
    let posts: Vec<&Value> = turns
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["posts"].as_array().into_iter().flatten())
        .collect();
    assert_eq!(posts.len(), 1, "{turns}");
    assert_eq!(posts[0]["text"], "Counted 12 boxes and saved stock.md.");
}
