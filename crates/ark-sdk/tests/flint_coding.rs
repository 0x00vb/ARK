//! Explicit native Flint dogfood. Requires local weights, bridge and rustc paths.
use ark_sdk::prelude::*;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
const BROKEN: &str = include_str!("fixtures/flint-html/faulty.txt");
const ORIGINAL: &str = include_str!("fixtures/flint-html/original.txt");
const TESTS: &str = include_str!("fixtures/flint-html/tests.txt");
fn files() -> TurnTools {
    TurnTools {
        adapter: Arc::new(ark_files::Files),
        distiller: Some(ark_files::distill()),
    }
}
struct Tests {
    root: PathBuf,
    rustc: PathBuf,
}
#[async_trait::async_trait]
impl Adapter for Tests {
    fn name(&self) -> &str {
        "tests"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "tests.run",
            "Run the fixed Rust regression tests for src/lib.rs",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({"type":"object","properties":{},"additionalProperties":false}))]
    }
    async fn run(
        &self,
        action: &str,
        _input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action != "tests.run" || ctx.workspace.as_ref() != Some(&self.root) {
            return Err(AdapterError::Tool("Invalid fixture test request".into()));
        }
        let source = std::fs::read_to_string(self.root.join("src/lib.rs"))
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        if source != ORIGINAL {
            return Err(AdapterError::Tool(
                "The candidate differs from the reviewed ARK HTML fixture repair.".into(),
            ));
        }
        let binary = self.root.join("ark-html-tests");
        let built = std::process::Command::new(&self.rustc)
            .current_dir(&self.root)
            .args(["--test", "hidden.rs", "-o"])
            .arg(&binary)
            .output()
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        if !built.status.success() {
            return Err(AdapterError::Tool(
                String::from_utf8_lossy(&built.stderr).into_owned(),
            ));
        }
        let tested = std::process::Command::new(binary)
            .current_dir(&self.root)
            .output()
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        if !tested.status.success() {
            return Err(AdapterError::Tool(
                String::from_utf8_lossy(&tested.stdout).into_owned(),
            ));
        }
        Ok(ActionOutput::json(
            json!({"passed":tested.status.success(),"stdout":String::from_utf8_lossy(&tested.stdout),"stderr":String::from_utf8_lossy(&tested.stderr)}),
        ))
    }
}
#[derive(Default)]
struct ScopedApproval {
    decisions: Mutex<Vec<serde_json::Value>>,
}
#[async_trait::async_trait]
impl Approver for ScopedApproval {
    async fn decide(&self, r: &ApprovalRequest) -> ApprovalDecision {
        let allow = r.action == "tests.run"
            || (matches!(r.action.as_str(), "files.edit" | "files.write")
                && r.input["path"] == "src/lib.rs");
        self.decisions
            .lock()
            .unwrap()
            .push(json!({"action":r.action,"input":r.input,"allowed":allow}));
        if allow {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::Deny
        }
    }
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit native Flint bridge, model and rustc; runs real local inference"]
async fn native_flint_reads_repairs_and_tests_ark_html() {
    let evidence = PathBuf::from(
        std::env::var_os("ARK_FLINT_CODING_OUT").expect("explicit evidence directory"),
    );
    assert!(evidence.is_absolute(), "absolute evidence path required");
    std::fs::create_dir(&evidence).expect("fresh evidence directory");
    let root = evidence.join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), BROKEN).unwrap();
    std::fs::write(root.join("hidden.rs"), TESTS).unwrap();
    std::fs::write(root.join("untouched.txt"), "unchanged").unwrap();
    let rustc =
        std::fs::canonicalize(std::env::var_os("ARK_FLINT_RUSTC").expect("explicit rustc path"))
            .unwrap();
    let mut config = ark_model::InferenceConfig::new(
        std::env::var_os("ARK_FLINT_BRIDGE").expect("bridge path"),
        std::env::var_os("ARK_FLINT_MODEL").expect("checkpoint path"),
    );
    config.flint_tools = true;
    config.seed = 20261022;
    config.backend = "metal".into();
    config.context = 12288;
    config.output_tokens = 4096;
    config.array_bytes = 3 << 30;
    config.file_bytes = 32 << 30;
    config.threads = 8;
    config.prefill_timeout = Duration::from_secs(1200);
    config.call_timeout = Duration::from_secs(1800);
    let ark = Ark::builder()
        .store_root(evidence.join("store"))
        .provider(Arc::new(ark_model::Inference::new(config).unwrap()))
        .planner(false)
        .critic(false)
        .max_steps(10)
        .build()
        .unwrap();
    let project = ark
        .find_or_create_project("Flint ARK HTML regression", "")
        .unwrap();
    ark.engine().set_workspace(&project.id, &root).unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let approvals = Arc::new(ScopedApproval::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let trace_path = evidence.join("trace.jsonl");
    let started = std::time::Instant::now();
    let result=ark.ask_session(&mut session,
        "Repair the extracted ARK HTML decoder in src/lib.rs. Read it first using files.read. Decimal entities and both lowercase and uppercase hexadecimal prefixes must work. Restore uppercase #X acceptance alongside existing #x acceptance. Preserve all other behavior. Use files.edit for the minimal replacement, then tests.run. Do not edit any other file. Keep every other byte unchanged. Keep reasoning brief. Finish only after the tests pass.",
        TurnOptions{approvals:Some(Arc::new(ApprovalGate::new(approvals.clone()))),tools:vec![files(),TurnTools{adapter:Arc::new(Tests{root:root.clone(),rustc}),distiller:None}],..Default::default()},
        move |e| {
            use std::io::Write;
            let mut trace = std::fs::OpenOptions::new().create(true).append(true).open(&trace_path).unwrap();
            writeln!(trace, "{}", serde_json::to_string(&e).unwrap()).unwrap();
            match e {
            HarnessEvent::ToolCall{action,input,..}=>seen.lock().unwrap().push(json!({"kind":"call","action":action,"input":input})),
            HarnessEvent::ToolResult{action,ok,summary,error,..}=>seen.lock().unwrap().push(json!({"kind":"result","action":action,"ok":ok,"summary":summary,"error":format!("{error:?}")})),
            _=>{},
        }}).await;
    std::fs::write(
        evidence.join("elapsed-ms.txt"),
        started.elapsed().as_millis().to_string(),
    )
    .unwrap();
    let events = events.lock().unwrap().clone();
    std::fs::write(
        evidence.join("events.json"),
        serde_json::to_vec_pretty(&events).unwrap(),
    )
    .unwrap();
    std::fs::write(
        evidence.join("approvals.json"),
        serde_json::to_vec_pretty(&*approvals.decisions.lock().unwrap()).unwrap(),
    )
    .unwrap();
    let outcome = match &result {
        Ok(_) => "turn completed".to_string(),
        Err(error) => format!("turn failed: {error}"),
    };
    std::fs::write(evidence.join("outcome.txt"), outcome).unwrap();
    result.unwrap();
    assert!(
        events.iter().any(|event| event["kind"] == "result"
            && event["action"] == "files.read"
            && event["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("pub fn decode")
                    && summary.contains("e if e.starts_with(\"#x\")"))),
        "the model did not receive the source text"
    );
    for action in ["files.read", "files.edit", "tests.run"] {
        assert!(
            events
                .iter()
                .any(|e| e["kind"] == "result" && e["action"] == action && e["ok"] == true),
            "missing successful {action}: {events:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("untouched.txt")).unwrap(),
        "unchanged"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hidden.rs")).unwrap(),
        TESTS
    );
    let source = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert_eq!(source, ORIGINAL);
}

#[tokio::test]
async fn denied_fixture_write_does_not_change_outside_sentinel() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let sentinel = temp.path().join("outside.txt");
    std::fs::write(&sentinel, "unchanged").unwrap();
    let provider = ark_model::MockProvider::new(
        "denial fixture",
        vec![
            ark_model::Completion {
                content: String::new(),
                tool_calls: vec![ark_model::ToolCall {
                    id: "deny-1".into(),
                    name: "files.write".into(),
                    arguments: json!({"path":"../outside.txt","text":"changed"}),
                }],
                usage: Default::default(),
            },
            ark_model::Completion {
                content: "The write was denied.".into(),
                ..Default::default()
            },
        ],
    );
    let ark = Ark::builder()
        .store_root(temp.path().join("store"))
        .provider(Arc::new(provider))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.find_or_create_project("denial", "").unwrap();
    ark.engine().set_workspace(&project.id, &root).unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let approvals = Arc::new(ScopedApproval::default());
    ark.ask_session(
        &mut session,
        "Attempt the fixture write.",
        TurnOptions {
            approvals: Some(Arc::new(ApprovalGate::new(approvals.clone()))),
            tools: vec![files()],
            ..Default::default()
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
    assert!(approvals
        .decisions
        .lock()
        .unwrap()
        .iter()
        .any(|r| r["action"] == "files.write" && r["allowed"] == false));
}

#[tokio::test]
async fn fixture_read_delivers_source_to_the_next_model_call() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), BROKEN).unwrap();
    let provider = Arc::new(
        ark_model::MockProvider::new(
            "source delivery fixture",
            vec![
                ark_model::Completion {
                    tool_calls: vec![ark_model::ToolCall {
                        id: "read-1".into(),
                        name: "files.read".into(),
                        arguments: json!({"path":"src/lib.rs"}),
                    }],
                    ..Default::default()
                },
                ark_model::Completion {
                    content: "Read complete.".into(),
                    ..Default::default()
                },
            ],
        )
        .with_context_window(8192),
    );
    let ark = Ark::builder()
        .store_root(temp.path().join("store"))
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.find_or_create_project("source delivery", "").unwrap();
    ark.engine().set_workspace(&project.id, &root).unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    ark.ask_session(
        &mut session,
        "Read src/lib.rs.",
        TurnOptions {
            tools: vec![files()],
            ..Default::default()
        },
        |_| {},
    )
    .await
    .unwrap();
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[1]
            .messages
            .iter()
            .any(|message| message.role == ark_model::Role::Tool
                && message.content.contains("pub fn decode")
                && message.content.contains("e if e.starts_with(\"#x\")")),
        "source content must reach the next model request"
    );
}
