//! The owner's lost task (ADR 0050), end to end. On Codex, a syntax-highlighting task read the
//! editor's code, sent an edit whose arguments did not parse, and stopped; "continue" started
//! over, the task fell out of the context, and the agent worked on something else.
//!
//! Here a child process plays the first turns on a scripted model: several large reads, an
//! unusable large edit three times (the turn fails), then "continue", which reads again and is
//! killed (SIGKILL) while the model writes. The parent reopens the store as a new process would
//! and sends "cointinue". The model it talks to must get the original request, what the stopped
//! turns read, its last note and why they stopped; it repairs the edit, runs the project's test
//! with `process.start`, and finishes. The file has the change once.
//!
//! The test binary runs itself as the child (`ARK_LOST_CHILD`), as `crash_recovery.rs` does.

use std::collections::VecDeque;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_sdk::prelude::ark_model::{
    Completion, Message, Provider, ProviderError, TextSink, ToolCall, ToolDef, Usage,
};
use ark_sdk::prelude::*;
use serde_json::json;

const CHILD: &str = "ARK_LOST_CHILD";
const DIR: &str = "ARK_LOST_DIR";

const QUESTION: &str = "On the editor file, we shuold support sintax highlighting (according to the ark user theme).\nAnaliize it, and make a small plan and impleemnt";
const PLAN: &str = "Plan: (1) keep the native textarea for editing, (2) add a highlight layer behind it, (3) colour tokens from the theme";
const EDITOR: &str = "web/js/file.js";

enum Step {
    Ok(Completion),
    /// Stream the text, then fail as Codex did: arguments that are not JSON.
    Unusable(&'static str),
    /// Stream the text, say so to the parent, and never finish (the process is killed).
    Hang(&'static str),
}

/// A scripted model that keeps every request.
struct Script {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<Vec<Message>>>,
}

impl Script {
    fn new(steps: Vec<Step>) -> Arc<Script> {
        Arc::new(Script {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn request(&self, i: usize) -> String {
        self.requests.lock().unwrap()[i]
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        s: &str,
        m: &[Message],
        t: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.complete_streaming(s, m, t, &|_: &str| {}).await
    }
    async fn complete_streaming(
        &self,
        _s: &str,
        m: &[Message],
        _t: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ark_model::Result<Completion> {
        self.requests.lock().unwrap().push(m.to_vec());
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            Some(Step::Ok(c)) => {
                if !c.content.is_empty() {
                    on_text(&c.content);
                }
                Ok(c)
            }
            Some(Step::Unusable(text)) => {
                on_text(text);
                Err(ProviderError::Malformed {
                    provider: "codex".into(),
                    detail: "arguments for files.edit are not JSON: expected `,` or `}` at line 1 column 1573".into(),
                })
            }
            Some(Step::Hang(text)) => {
                on_text(text);
                ready();
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(ProviderError::Api {
                    provider: "codex".into(),
                    detail: "not killed".into(),
                })
            }
            None => Ok(say("(script ended)")),
        }
    }
    fn describe(&self) -> String {
        "script".into()
    }
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::default(),
    }
}

fn call(note: &str, id: &str, name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: note.into(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::default(),
    }
}

fn read(note: &str, id: &str, path: &str) -> Step {
    Step::Ok(call(note, id, "files.read", json!({ "path": path })))
}

fn ready() {
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    std::fs::write(dir.join("ready"), b"").unwrap();
}

fn ark(dir: &std::path::Path, model: Arc<Script>) -> Ark {
    Ark::builder()
        .store_root(dir)
        .allow_mutate(true)
        .files(true)
        .processes(true)
        .provider(model as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap()
}

/// The project and its folder, with the editor's code: large files, as the real ones are.
fn project(ark: &Ark) -> (ark_core::ProjectId, std::path::PathBuf) {
    let p = ark.find_or_create_project("ark", "").unwrap();
    let dir = ark.engine().workspace_dir(&p.id).unwrap();
    if !dir.join(EDITOR).exists() {
        std::fs::create_dir_all(dir.join("web/js")).unwrap();
        std::fs::write(
            dir.join(EDITOR),
            format!(
                "export function editor(file) {{\n  const area = document.createElement('textarea');\n{}  return area;\n}}\n",
                "  // render the file in a plain textarea\n".repeat(400)
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("web/app.css"),
            ".wb-editor { font: 13px var(--mono); }\n".repeat(600),
        )
        .unwrap();
        std::fs::write(
            dir.join("web/theme.css"),
            ":root { --accent: #3b82f6; --text: #1f2937; }\n".repeat(300),
        )
        .unwrap();
    }
    (p.id, dir)
}

/// The child's part: the turn that fails, then "continue", killed while the model writes.
async fn child() {
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    let model = Script::new(vec![
        read("I'll inspect the editor first.", "r1", EDITOR),
        read(PLAN, "r2", "web/app.css"),
        read("Now the theme tokens.", "r3", "web/theme.css"),
        Step::Unusable("Implementing the highlight layer now."),
        Step::Unusable("Implementing the highlight layer now."),
        Step::Unusable("Implementing the highlight layer now."),
        // "continue"
        read("Re-reading the editor before the edit.", "r4", EDITOR),
        Step::Hang("Writing the edit"),
    ]);
    let ark = ark(&dir, model);
    let (p, _) = project(&ark);
    let mut session = ark.start_session(&p).unwrap();
    let failed = ark
        .ask_session(&mut session, QUESTION, TurnOptions::default(), |_| {})
        .await;
    assert!(failed.is_err(), "the first turn fails on its unusable edit");
    std::fs::write(dir.join("failed"), b"").unwrap();
    let _ = ark
        .ask_session(&mut session, "continue", TurnOptions::default(), |_| {})
        .await;
    std::fs::write(dir.join("finished"), b"").unwrap();
}

fn crash(test: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env(DIR, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !dir.path().join("ready").exists() {
        assert!(
            !dir.path().join("finished").exists(),
            "the child finished before the kill"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the child never got ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        dir.path().join("failed").exists(),
        "the first turn failed first"
    );
    c.kill().unwrap(); // SIGKILL: no cleanup runs
    c.wait().unwrap();
    dir
}

#[tokio::test]
async fn a_failed_and_killed_task_continues_with_its_goal_and_finishes() {
    if std::env::var(CHILD).is_ok() {
        return child().await;
    }
    let dir = crash("a_failed_and_killed_task_continues_with_its_goal_and_finishes");

    // A new process opens the store. The model repairs the edit, runs the test, answers.
    let model = Script::new(vec![
        Step::Ok(call(
            "Adding the highlight layer behind the textarea.",
            "e1",
            "files.edit",
            json!({ "path": EDITOR, "edits": [{
                "find": "  const area = document.createElement('textarea');\n",
                "replace": "  const area = document.createElement('textarea');\n  const highlightLayer = paint(file, theme()); // \"quoted\" \\ backslash\n",
            }]}),
        )),
        Step::Ok(call(
            "Running the editor's test.",
            "t1",
            "process.start",
            json!({ "command": "grep -c highlightLayer web/js/file.js && echo tests passed", "wait": 30 }),
        )),
        Step::Ok(say("Syntax highlighting is in place and its test passes.")),
        Step::Ok(say("Syntax highlighting is in place and its test passes.")),
    ]);
    let ark = ark(dir.path(), model.clone());
    let (p, folder) = project(&ark);
    let mut session = ark.latest_session(&p).unwrap().unwrap();
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(
        turns.len(),
        2,
        "the failed turn and the killed one are kept"
    );
    assert!(turns[0]
        .error
        .as_deref()
        .unwrap()
        .starts_with(ark_sdk::prelude::ark_runtime::UNUSABLE_REPLY));

    let out = ark
        .ask_session(&mut session, "cointinue", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        out.answer,
        "Syntax highlighting is in place and its test passes."
    );

    // What the model got first: the task, not "cointinue".
    let first = model.request(0);
    let head = model.requests.lock().unwrap()[0][0].content.clone();
    assert!(
        head.contains("sintax highlighting"),
        "the goal leads: {head}"
    );
    assert!(
        head.contains("continue") && head.contains("cointinue"),
        "with what the person said since: {head}"
    );
    for path in [EDITOR, "web/app.css", "web/theme.css"] {
        assert!(
            first.contains(&format!("files.read path={path}")),
            "it knows it read {path}: {first}"
        );
    }
    assert!(
        first.contains(
            "Your last note before it stopped: \"Re-reading the editor before the edit.\""
        ),
        "its own last note: {first}"
    );
    assert!(
        first.contains("It stopped because: interrupted"),
        "why it stopped: {first}"
    );
    // Its plan from the failed turn is in the earlier turn's calls.
    assert!(
        ark.engine()
            .store
            .list_invocations(&p)
            .unwrap()
            .iter()
            .any(|i| i.narration.as_deref() == Some(PLAN)),
        "the plan was kept with the call it came with"
    );

    // The test ran in the person's shell and its result reached the model.
    let after_test = model.request(2);
    assert!(after_test.contains("tests passed"), "{after_test}");

    let text = std::fs::read_to_string(folder.join(EDITOR)).unwrap();
    assert_eq!(
        text.matches("highlightLayer").count(),
        1,
        "the change is made once"
    );
    assert!(
        text.contains(r#"// "quoted" \ backslash"#),
        "code with quotes arrives whole"
    );

    let state = ark
        .engine()
        .store
        .get_research_state(&p, session.id().as_str())
        .unwrap()
        .unwrap();
    assert_eq!(
        state.goal, QUESTION,
        "the goal is the request, not \"cointinue\""
    );
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 3);
    assert!(!turns[2].incomplete);
}

/// The turn fails on unusable replies, the process ends normally, a new one continues.
#[tokio::test]
async fn a_task_that_failed_on_unusable_replies_continues_in_a_new_process() {
    let dir = tempfile::tempdir().unwrap();
    {
        let model = Script::new(vec![
            read(PLAN, "r1", EDITOR),
            Step::Unusable("Writing it."),
            Step::Unusable("Writing it."),
            Step::Unusable("Writing it."),
        ]);
        let ark = ark(dir.path(), model.clone());
        let (p, _) = project(&ark);
        let mut session = ark.start_session(&p).unwrap();
        let Err(e) = ark
            .ask_session(&mut session, QUESTION, TurnOptions::default(), |_| {})
            .await
        else {
            panic!("the turn fails on unusable replies");
        };
        assert!(e.to_string().contains("column 1573"));
        // Each unusable reply was answered with what was wrong.
        assert!(model
            .request(2)
            .contains("Your last reply could not be used"));
    }
    let model = Script::new(vec![Step::Ok(say("Done."))]);
    let ark = ark(dir.path(), model.clone());
    let (p, _) = project(&ark);
    let mut session = ark.latest_session(&p).unwrap().unwrap();
    ark.ask_session(&mut session, "retry", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    let first = model.request(0);
    assert!(first.contains("sintax highlighting"), "{first}");
    assert!(
        first.contains("It stopped because: the model's reply could not be used"),
        "{first}"
    );
    assert!(first.contains(&format!("Your last note before it stopped: \"{PLAN}\"")));
}
