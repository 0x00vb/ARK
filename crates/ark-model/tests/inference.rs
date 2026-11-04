#![cfg(unix)]
use ark_model::{Inference, InferenceConfig, Message, Provider, ProviderError, Role, ToolDef};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{sleep, timeout};
struct Fixture {
    dir: PathBuf,
    exe: PathBuf,
    model: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ark-inference-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let exe = dir.join("worker");
        let model = dir.join(mode);
        std::fs::write(&exe, r#"#!/usr/bin/python3
import json,os,sys,struct,time
path=sys.argv[sys.argv.index('--model')+1]
mode=os.path.basename(path)
state=path+'.pids'
with open(path+'.argv','w') as f:json.dump(sys.argv[1:],f)
with open(path+'.argvs','a') as f:f.write(json.dumps(sys.argv[1:])+'\n')
with open(state,'a') as f:f.write(str(os.getpid())+'\n')
first=len(open(state).readlines())==1
bad=mode if first else 'good'
def frame(tag,data):
 sys.stdout.buffer.write(tag+struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
def park():time.sleep(3600)
def stage(name):
 with open(path+'.stage','w') as f:f.write(name)
if bad=='startup':stage('startup');park()
if mode=='flint-recover-startup' and not first:stage('reload');park()
if bad=='stderr':os.write(2,b'x'*2097152)
frame(b'R',struct.pack('<I',2 if bad=='version' or mode.startswith('flint-') else 1))
if bad=='write':stage('write');park()
while True:
 head=sys.stdin.buffer.read(5)
 if not head:
  if mode=='flint-recover-eof':stage('eof');park()
  with open(path+'.closed','a') as f:f.write(str(os.getpid())+'\n')
  break
 n=struct.unpack('<I',head[1:])[0]
 payload=sys.stdin.buffer.read(n)
 if len(payload)!=n:break
 with open(path+'.payloads','a') as f:f.write(json.dumps(payload.decode('utf-8','replace'))+'\n')
 if bad=='crash':sys.exit(1)
 if bad=='order':frame(b'T',b'x');park()
 if bad=='huge':sys.stdout.buffer.write(b'T'+struct.pack('<I',1048577));sys.stdout.buffer.flush();park()
 if bad=='error':frame(b'E',struct.pack('<I',99));park()
 if mode=='flint-recover-prefill' or (mode=='flint-recover-zero' and not first):time.sleep(.09)
 frame(b'A',struct.pack('<QQ',100,1<<40 if bad=='capacity' else 100))
 if mode=='flint-accept-late':time.sleep(.12)
 if bad=='prefill':stage('prefill');park()
 if mode.startswith('flint-'):
  if mode=='flint-block':stage('prefill');park()
  answer=b'<think>Check.</think><tool_call>file.read<arg_key>path</arg_key><arg_value>src/lib.rs</arg_value></tool_call>'
  stop=150005
  if mode.startswith('flint-recover') and (first or mode=='flint-recover-twice'):
   answer=b'<think>PRIVATE reasoning</think>```json {"name":"file.read"} ```'
   stop=150005
  if mode=='flint-recover-zero' and first:answer=b''
  if mode=='flint-recover-bytes' and first:answer=b'x'*4096
  if mode=='flint-recover-counter' and first:answer=b'bad'
  if mode=='flint-recover-context' and not first:
   frame(b'E',struct.pack('<I',2));continue
  if mode=='flint-next-user-final':answer=b'<think>Check.</think>Flint is running locally.';stop=150003
  if mode=='flint-reasoning':answer=b'<think>unfinished '+answer
  if mode=='flint-partial':answer=answer[:-3]
  if mode=='flint-accept-late':time.sleep(.12)
  if mode=='flint-recover-zero' and first:time.sleep(.09)
  if answer:frame(b'T',answer)
  frame(b'D',struct.pack('<QQQQBI',12,1 if answer else 0,1 if mode=='flint-recover-counter' else 0,0,0 if mode=='flint-limit' else 1,0xffffffff if mode=='flint-limit' else stop))
  continue
 if bad=='partial':sys.stdout.buffer.write(b'T\x01');sys.stdout.buffer.flush();park()
 if bad=='empty':frame(b'T',b'');park()
 if bad=='invalid':frame(b'T',b'\xff');park()
 if bad=='incomplete':frame(b'T',b'\xe2');frame(b'D',struct.pack('<QQQQ',12,1,0,0));park()
 if bad=='excess':frame(b'T',b'x'*33);park()
 if bad=='chunks':
  for _ in range(5):frame(b'T',b'x')
  park()
 frame(b'T',b'\xe2');frame(b'T',b'\x82\xac')
 if bad in ['stream','terminal']:stage('terminal');park()
 frame(b'D',struct.pack('<QQQQ',125 if bad=='prompt' else 12,3 if bad=='count' else 2,1 if bad=='staging' else 0,1 if bad=='cleanup' else 0))
 if bad=='exit':stage('exit');sys.exit(0)
"#).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir, exe, model }
    }
    fn config(&self) -> InferenceConfig {
        let mut c = InferenceConfig::new(&self.exe, &self.model);
        c.context = 128;
        c.output_tokens = 4;
        c.output_bytes = 32;
        // Generous spawn deadlines: Python starts slowly on a loaded machine.
        c.startup_timeout = Duration::from_secs(10);
        c.prefill_timeout = Duration::from_millis(150);
        c.progress_timeout = Duration::from_millis(150);
        c.call_timeout = Duration::from_secs(15);
        c
    }
    fn pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.model.with_extension("pids"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.parse().unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}
/// The fake worker needs a working `/usr/bin/python3`. Without one, spawning tests skip.
fn no_python() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *OK.get_or_init(|| {
        std::process::Command::new("/usr/bin/python3")
            .args(["-c", ""])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        eprintln!("skipped: /usr/bin/python3 does not run");
    }
    !ok
}
fn assert_batch_arguments(model: &std::path::Path, batch: u32) {
    let args: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(model.with_extension("argv")).unwrap())
            .unwrap();
    for (name, value) in [("--batch", batch), ("--threads", 2)] {
        assert_eq!(args.iter().filter(|arg| arg.as_str() == name).count(), 1);
        let index = args.iter().position(|arg| arg == name).unwrap();
        assert_eq!(args[index + 1], value.to_string());
    }
}

#[tokio::test]
async fn batch_capacity_is_forwarded_and_reuses_one_worker() {
    if no_python() {
        return;
    }
    for batch in [1, 32, 256, 512] {
        let fixture = Fixture::new("good");
        let mut config = fixture.config();
        assert_eq!(config.batch, 256);
        config.batch = batch;
        let provider = Inference::new(config).unwrap();
        for _ in 0..2 {
            let completion = provider
                .complete("", &[Message::user("hello")], &[])
                .await
                .unwrap();
            assert_eq!(completion.content, "€");
        }
        assert_batch_arguments(&fixture.model, batch);
        assert_eq!(fixture.pids().len(), 1);
    }
}

#[test]
fn invalid_batch_capacity_is_refused_before_spawn() {
    let fixture = Fixture::new("good");
    for batch in [0, 513] {
        let mut config = fixture.config();
        config.batch = batch;
        permanent(Inference::new(config));
    }
    assert!(fixture.pids().is_empty());
    assert!(!fixture.model.with_extension("argv").exists());
}

/// Waits until the process is gone or a zombie. `kill -0` succeeds on a zombie, and this child
/// stays one until the provider reaps it.
async fn exited(pid: u32) {
    timeout(Duration::from_secs(3), async {
        loop {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout);
            if stat.trim().is_empty() || stat.trim_start().starts_with('Z') {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle child did not exit");
}
async fn dead(pid: u32) {
    timeout(Duration::from_secs(3), async {
        loop {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned child remains alive");
}
#[tokio::test]
async fn utf8_streams_and_warm_model_reuses_then_drops() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("good");
    let provider = Inference::new(fixture.config()).unwrap();
    let pieces = Mutex::new(String::new());
    for _ in 0..3 {
        let answer = provider
            .complete_streaming("system", &[Message::user("hello")], &[], &|text| {
                pieces.lock().unwrap().push_str(text)
            })
            .await
            .unwrap();
        assert_eq!(answer.content, "€");
        assert_eq!(answer.usage.prompt_tokens, 12);
        assert_eq!(answer.usage.completion_tokens, 2);
        assert!(answer.usage.measured);
    }
    assert_eq!(*pieces.lock().unwrap(), "€€€");
    assert_eq!(fixture.pids().len(), 1);
    drop(provider);
    dead(fixture.pids()[0]).await;
}
#[tokio::test]
async fn a_worker_that_exits_while_idle_is_replaced() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("exit");
    let provider = Inference::new(fixture.config()).unwrap();
    let messages = [Message::user("hello")];
    assert_eq!(
        provider.complete("", &messages, &[]).await.unwrap().content,
        "€"
    );
    timeout(Duration::from_secs(2), async {
        while std::fs::read_to_string(fixture.model.with_extension("stage"))
            .ok()
            .as_deref()
            != Some("exit")
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker did not exit");
    exited(fixture.pids()[0]).await;
    assert_eq!(
        provider.complete("", &messages, &[]).await.unwrap().content,
        "€"
    );
    assert_eq!(fixture.pids().len(), 2);
    drop(provider);
    dead(fixture.pids()[1]).await;
}
#[tokio::test]
async fn prefill_and_later_frames_have_separate_deadlines() {
    if no_python() {
        return;
    }
    // A long prefill outlives the per-frame deadline.
    let fixture = Fixture::new("prefill");
    let mut c = fixture.config();
    c.prefill_timeout = Duration::from_millis(800);
    c.progress_timeout = Duration::from_millis(50);
    let provider = Inference::new(c).unwrap();
    let start = std::time::Instant::now();
    let error = provider
        .complete("", &[Message::user("hello")], &[])
        .await
        .unwrap_err();
    assert!(start.elapsed() >= Duration::from_millis(800), "{error}");
    assert!(error.to_string().contains("prefill deadline"), "{error}");
    drop(provider);
    dead(fixture.pids()[0]).await;
    // After the first token, a stalled child hits the short per-frame deadline.
    let fixture = Fixture::new("terminal");
    let mut c = fixture.config();
    c.prefill_timeout = Duration::from_secs(10);
    c.progress_timeout = Duration::from_millis(150);
    let provider = Inference::new(c).unwrap();
    let start = std::time::Instant::now();
    let error = provider
        .complete("", &[Message::user("hello")], &[])
        .await
        .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(10), "{error}");
    assert!(error.to_string().contains("progress deadline"), "{error}");
    drop(provider);
    dead(fixture.pids()[0]).await;
}
#[tokio::test]
async fn malformed_crashed_or_late_workers_fail_and_restart() {
    if no_python() {
        return;
    }
    for mode in [
        "version",
        "order",
        "huge",
        "error",
        "crash",
        "partial",
        "invalid",
        "incomplete",
        "excess",
        "count",
        "prefill",
        "empty",
        "cleanup",
        "staging",
        "prompt",
        "capacity",
        "chunks",
    ] {
        let fixture = Fixture::new(mode);
        let provider = Inference::new(fixture.config()).unwrap();
        assert!(
            provider
                .complete("", &[Message::user("hello")], &[])
                .await
                .is_err(),
            "{mode}"
        );
        let pid = fixture.pids()[0];
        dead(pid).await;
        assert_eq!(
            provider
                .complete("", &[Message::user("hello")], &[])
                .await
                .unwrap()
                .content,
            "€",
            "{mode}"
        );
        assert_eq!(fixture.pids().len(), 2);
        drop(provider);
        dead(fixture.pids()[1]).await;
    }
}
#[tokio::test]
async fn cancellation_during_startup_write_prefill_and_terminal_restarts() {
    if no_python() {
        return;
    }
    for mode in ["startup", "write", "prefill", "terminal"] {
        let fixture = Fixture::new(mode);
        let mut c = fixture.config();
        c.prefill_timeout = Duration::from_secs(2);
        c.progress_timeout = Duration::from_secs(2);
        let stage_timeout = c.startup_timeout;
        let provider = Inference::new(c).unwrap();
        let text = if mode == "write" {
            "x".repeat(1000000)
        } else {
            "hello".into()
        };
        let messages = [Message::user(text)];
        {
            let call = provider.complete("", &messages, &[]);
            tokio::pin!(call);
            let observed = async {
                loop {
                    if std::fs::read_to_string(fixture.model.with_extension("stage"))
                        .ok()
                        .as_deref()
                        == Some(mode)
                    {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            };
            tokio::select! {
                result = timeout(stage_timeout, observed) => { result.unwrap_or_else(|error| panic!("cancellation stage {mode} not observed: {error}")); },
                result = &mut call => panic!("worker completed before cancellation stage {mode}: {result:?}"),
            }
            assert!(
                timeout(Duration::from_millis(80), &mut call).await.is_err(),
                "{mode}"
            );
        }
        assert_eq!(fixture.pids().len(), 1);
        dead(fixture.pids()[0]).await;
        assert!(
            provider
                .complete("", &[Message::user("hello")], &[])
                .await
                .is_ok(),
            "{mode}"
        );
        drop(provider);
        dead(fixture.pids()[1]).await;
    }
}
#[tokio::test]
async fn streaming_is_provisional_and_concurrency_refuses() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("stream");
    let mut c = fixture.config();
    c.prefill_timeout = Duration::from_secs(2);
    c.progress_timeout = Duration::from_secs(2);
    let provider = Inference::new(c).unwrap();
    let notify = Arc::new(tokio::sync::Notify::new());
    let sink = |_: &str| notify.notify_one();
    let messages = [Message::user("hello")];
    {
        let call = provider.complete_streaming("", &messages, &[], &sink);
        tokio::pin!(call);
        tokio::select! { _ = notify.notified() => {}, result = &mut call => panic!("terminal arrived before streaming: {result:?}") }
        assert!(matches!(
            provider.complete("", &messages, &[]).await,
            Err(ProviderError::Api { .. })
        ));
        // Dropping a still-pending future after visible text cancels its process.
    }
    dead(fixture.pids()[0]).await;
    assert!(provider.complete("", &messages, &[]).await.is_ok());
    drop(provider);
    dead(fixture.pids()[1]).await;
}
#[tokio::test]
async fn stderr_cannot_block_and_call_deadlines_evict() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("stderr");
    let provider = Inference::new(fixture.config()).unwrap();
    assert!(provider
        .complete("", &[Message::user("hello")], &[])
        .await
        .is_ok());
    drop(provider);
    dead(fixture.pids()[0]).await;
    let fixture = Fixture::new("startup");
    let mut c = fixture.config();
    c.call_timeout = Duration::from_millis(100);
    let provider = Inference::new(c).unwrap();
    assert!(provider
        .complete("", &[Message::user("hello")], &[])
        .await
        .is_err());
    for pid in fixture.pids() {
        dead(pid).await;
    }
}
fn permanent<T>(result: ark_model::Result<T>) {
    match result {
        Err(error) => {
            assert!(matches!(&error, ProviderError::Api { .. }));
            assert!(!error.is_transient());
        }
        Ok(_) => panic!("unsupported request succeeded"),
    }
}
#[tokio::test]
async fn unsupported_or_oversized_input_never_starts_a_child() {
    let fixture = Fixture::new("good");
    let provider = Inference::new(fixture.config()).unwrap();
    let tool = ToolDef {
        name: "test".into(),
        description: "test".into(),
        parameters: serde_json::json!({}),
    };
    permanent(provider.complete("", &[Message::user("hi")], &[tool]).await);
    let mut picture = Message::user("hi");
    picture.pictures.push(ark_model::Picture {
        media_type: "image/png".into(),
        bytes: Arc::new(vec![0]),
    });
    permanent(provider.complete("", &[picture], &[]).await);
    let history = Message::assistant_tool_calls(vec![ark_model::ToolCall {
        id: "id".into(),
        name: "test".into(),
        arguments: serde_json::json!({}),
    }]);
    permanent(provider.complete("", &[history], &[]).await);
    let mut message = Message::user("hi");
    message.role = Role::Tool;
    permanent(provider.complete("", &[message], &[]).await);
    permanent(
        provider
            .complete("", &vec![Message::user("x"); 1025], &[])
            .await,
    );
    permanent(
        provider
            .complete("", &[Message::user("x".repeat(1048576))], &[])
            .await,
    );
    permanent(provider.complete("", &[], &[]).await);
    assert!(fixture.pids().is_empty());
    let mut c = fixture.config();
    c.executable = "relative".into();
    permanent(Inference::new(c));
    let mut c = fixture.config();
    c.array_bytes = 0;
    permanent(Inference::new(c));
    // Reserving the whole context for output leaves no input window.
    let mut c = fixture.config();
    c.output_tokens = c.context;
    permanent(Inference::new(c));
}

#[tokio::test]
#[ignore = "isolated environment helper, invoked by factory_environment_is_validated"]
async fn factory_environment_child() {
    let model = std::env::var("ARK_MODEL").unwrap();
    let result = ark_model::build("inference", Some(model.clone()));
    if std::env::var_os("ARK_INFERENCE_EXPECT_INVALID").is_some() {
        permanent(result);
    } else {
        let provider = result.unwrap();
        assert_eq!(provider.context_window(), Some(124));
        assert_eq!(provider.describe(), "ARK Inference (local text)");
        assert!(!provider.reads_pictures());
        let completion = provider
            .complete("", &[Message::user("hello")], &[])
            .await
            .unwrap();
        assert_eq!(completion.content, "€");
        let batch = std::env::var("ARK_INFERENCE_EXPECT_BATCH")
            .unwrap()
            .parse()
            .unwrap();
        assert_batch_arguments(std::path::Path::new(&model), batch);
    }
}
#[test]
fn factory_environment_is_validated() {
    let python_missing = no_python();
    for (key, value, expected_batch) in [
        ("", "", Some(256)),
        ("ARK_INFERENCE_BATCH", "1", Some(1)),
        ("ARK_INFERENCE_BATCH", "32", Some(32)),
        ("ARK_INFERENCE_BATCH", "256", Some(256)),
        ("ARK_INFERENCE_BATCH", "512", Some(512)),
        ("ARK_INFERENCE_BATCH", "0", None),
        ("ARK_INFERENCE_BATCH", "513", None),
        ("ARK_INFERENCE_BATCH", "not-a-number", None),
        ("ARK_INFERENCE_ARRAY_BYTES", "0", None),
        ("ARK_INFERENCE_CONTEXT", "not-a-number", None),
        ("ARK_INFERENCE_BACKEND", "cuda", None),
        ("ARK_INFERENCE_EXECUTABLE", "relative", None),
        ("ARK_INFERENCE_THREADS", "65", None),
        ("ARK_INFERENCE_OUTPUT_TOKENS", "128", None),
        ("ARK_INFERENCE_PROGRESS_SECONDS", "0", None),
        ("ARK_INFERENCE_PREFILL_SECONDS", "0", None),
        ("ARK_INFERENCE_CALL_SECONDS", "soon", None),
    ] {
        if python_missing && expected_batch.is_some() {
            continue;
        }
        let fixture = Fixture::new("good");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .args(["--ignored", "--exact", "factory_environment_child"])
            .env("ARK_INFERENCE_EXECUTABLE", &fixture.exe)
            .env("ARK_MODEL", &fixture.model)
            .env("ARK_INFERENCE_CONTEXT", "128")
            .env("ARK_INFERENCE_OUTPUT_TOKENS", "4");
        if !key.is_empty() {
            command.env(key, value);
        }
        if let Some(batch) = expected_batch {
            command.env("ARK_INFERENCE_EXPECT_BATCH", batch.to_string());
        } else {
            command.env("ARK_INFERENCE_EXPECT_INVALID", "1");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "factory child failed: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        if expected_batch.is_some() {
            assert_eq!(fixture.pids().len(), 1);
        } else {
            assert!(fixture.pids().is_empty());
            assert!(!fixture.model.with_extension("argv").exists());
        }
    }
}

#[tokio::test]
async fn flint_next_user_final_streams_without_format_regeneration() {
    if no_python() {
        return;
    }
    for tools in [Vec::new(), recovery_tools().to_vec()] {
        let fixture = Fixture::new("flint-next-user-final");
        let provider = Inference::new(recovery_config(&fixture)).unwrap();
        let streamed = Mutex::new(String::new());
        let completion = provider
            .complete_streaming("", &[Message::user("Reply")], &tools, &|text| {
                streamed.lock().unwrap().push_str(text)
            })
            .await
            .unwrap();
        assert_eq!(completion.content, "Flint is running locally.");
        assert_eq!(*streamed.lock().unwrap(), completion.content);
        assert!(completion.tool_calls.is_empty());
        assert_eq!(fixture.pids().len(), 1);
        drop(provider);
        dead(fixture.pids()[0]).await;
    }
}

#[tokio::test]
async fn flint_tools_handoff_reuses_and_rejects_partial_output() {
    if no_python() {
        return;
    }
    let tools = [ToolDef {
        name: "file.read".into(),
        description: "Read".into(),
        parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
    }];
    for mode in [
        "flint-good",
        "flint-limit",
        "flint-reasoning",
        "flint-partial",
    ] {
        let fixture = Fixture::new(mode);
        let mut config = fixture.config();
        config.flint_tools = true;
        config.file_bytes = 32 << 30;
        config.output_bytes = 4096;
        let provider = Inference::new(config).unwrap();
        let streamed = Mutex::new(String::new());
        let result = provider
            .complete_streaming("", &[Message::user("Read the file")], &tools, &|s| {
                streamed.lock().unwrap().push_str(s)
            })
            .await;
        assert!(streamed.lock().unwrap().is_empty());
        if mode == "flint-good" {
            let completion = result.unwrap();
            assert_eq!(completion.tool_calls.len(), 1);
            assert_eq!(completion.tool_calls[0].arguments["path"], "src/lib.rs");
            provider
                .complete("", &[Message::user("Again")], &tools)
                .await
                .unwrap();
            assert_eq!(fixture.pids().len(), 1);
        } else {
            assert!(result.is_err(), "{mode}");
        }
        drop(provider);
        for pid in fixture.pids() {
            dead(pid).await;
        }
    }
}

fn recovery_tools() -> [ToolDef; 1] {
    [ToolDef {
        name: "file.read".into(),
        description: "Read".into(),
        parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
    }]
}
fn recovery_config(fixture: &Fixture) -> InferenceConfig {
    let mut c = fixture.config();
    c.flint_tools = true;
    c.output_bytes = 4096;
    c.prefill_timeout = Duration::from_secs(2);
    c
}
#[tokio::test]
async fn flint_regenerates_once_with_remaining_budgets_and_no_rejected_stream() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("flint-recover");
    let provider = Inference::new(recovery_config(&fixture)).unwrap();
    let streamed = Mutex::new(String::new());
    let completion = provider
        .complete_streaming("", &[Message::user("Read")], &recovery_tools(), &|s| {
            streamed.lock().unwrap().push_str(s)
        })
        .await
        .unwrap();
    assert_eq!(completion.tool_calls[0].arguments["path"], "src/lib.rs");
    assert_eq!(completion.usage.prompt_tokens, 24);
    assert_eq!(completion.usage.completion_tokens, 2);
    assert!(streamed.lock().unwrap().is_empty());
    let args: Vec<Vec<String>> = std::fs::read_to_string(fixture.model.with_extension("argvs"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(args.len(), 2);
    let at = args[1].iter().position(|s| s == "--tokens").unwrap();
    assert_eq!(args[1][at + 1], "3");
    let at = args[1].iter().position(|s| s == "--output-bytes").unwrap();
    assert_eq!(
        args[1][at + 1].parse::<usize>().unwrap(),
        4096 - b"<think>PRIVATE reasoning</think>```json {\"name\":\"file.read\"} ```".len()
    );
    let payloads: Vec<String> = std::fs::read_to_string(fixture.model.with_extension("payloads"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(!payloads[1].contains("PRIVATE reasoning"));
    assert!(payloads[1].contains("Regenerate only complete XML"));
    for pid in fixture.pids() {
        dead(pid).await;
    }
    let closed = std::fs::read_to_string(fixture.model.with_extension("closed")).unwrap();
    assert_eq!(closed.lines().count(), 2);
    provider
        .complete("", &[Message::user("Read again")], &recovery_tools())
        .await
        .unwrap();
    assert_eq!(
        fixture.pids().len(),
        3,
        "recovery worker must never enter the warm pool"
    );
    drop(provider);
    for pid in fixture.pids() {
        dead(pid).await;
    }
}
#[tokio::test]
async fn flint_recovery_refusals_never_publish_or_spawn_a_third_attempt() {
    if no_python() {
        return;
    }
    for (mode, workers) in [
        ("flint-recover-twice", 2),
        ("flint-recover-bytes", 1),
        ("flint-recover-counter", 1),
        ("flint-reasoning", 1),
        ("flint-limit", 1),
        ("flint-recover-context", 2),
    ] {
        let fixture = Fixture::new(mode);
        let provider = Inference::new(recovery_config(&fixture)).unwrap();
        let streamed = Mutex::new(String::new());
        assert!(
            provider
                .complete_streaming("", &[Message::user("Read")], &recovery_tools(), &|s| {
                    streamed.lock().unwrap().push_str(s)
                })
                .await
                .is_err(),
            "{mode}"
        );
        assert!(streamed.lock().unwrap().is_empty(), "{mode}");
        assert_eq!(fixture.pids().len(), workers, "{mode}");
        drop(provider);
        for pid in fixture.pids() {
            dead(pid).await;
        }
    }
}
#[tokio::test]
async fn flint_acceptance_frame_does_not_restart_prefill_deadline() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("flint-accept-late");
    let mut c = recovery_config(&fixture);
    c.prefill_timeout = Duration::from_millis(200);
    let provider = Inference::new(c).unwrap();
    let error = provider
        .complete("", &[Message::user("Read")], &recovery_tools())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("prefill deadline"), "{error}");
    assert_eq!(fixture.pids().len(), 1);
    drop(provider);
    dead(fixture.pids()[0]).await;
}
#[tokio::test]
async fn flint_recovery_prefills_share_the_original_deadline() {
    if no_python() {
        return;
    }
    for mode in ["flint-recover-prefill", "flint-recover-zero"] {
        let fixture = Fixture::new(mode);
        let mut c = recovery_config(&fixture);
        c.prefill_timeout = Duration::from_millis(150);
        let provider = Inference::new(c).unwrap();
        let result = provider
            .complete("", &[Message::user("Read")], &recovery_tools())
            .await;
        assert!(result.unwrap_err().to_string().contains("prefill deadline"));
        drop(provider);
        for pid in fixture.pids() {
            dead(pid).await;
        }
    }
}
#[tokio::test]
async fn flint_recovery_eof_and_reload_obey_the_original_call_deadline() {
    if no_python() {
        return;
    }
    for mode in ["flint-recover-eof", "flint-recover-startup"] {
        let fixture = Fixture::new(mode);
        let mut c = recovery_config(&fixture);
        c.call_timeout = Duration::from_secs(3);
        let provider = Inference::new(c).unwrap();
        let messages = [Message::user("Read")];
        let tools = recovery_tools();
        let error = timeout(
            Duration::from_secs(5),
            provider.complete("", &messages, &tools),
        )
        .await
        .expect("outer call deadline did not bound startup and recovery")
        .unwrap_err();
        assert!(error.to_string().contains("call deadline"), "{error}");
        assert!(
            fixture.pids().len() <= 2,
            "a logical call spawned more than two attempts"
        );
        drop(provider);
        for pid in fixture.pids() {
            dead(pid).await;
        }
    }
}

#[tokio::test]
async fn flint_feedback_expansion_is_checked_before_replacement() {
    if no_python() {
        return;
    }
    let probe = Fixture::new("flint-good");
    let provider = Inference::new(recovery_config(&probe)).unwrap();
    provider
        .complete("", &[Message::user("Read")], &recovery_tools())
        .await
        .unwrap();
    drop(provider);
    dead(probe.pids()[0]).await;
    let raw = std::fs::read_to_string(probe.model.with_extension("payloads")).unwrap();
    let payload: String = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    let fixture = Fixture::new("flint-recover");
    let mut config = recovery_config(&fixture);
    config.input_bytes = payload.len();
    let provider = Inference::new(config).unwrap();
    let error = provider
        .complete("", &[Message::user("Read")], &recovery_tools())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("byte limit"), "{error}");
    assert_eq!(fixture.pids().len(), 1);
    drop(provider);
    dead(fixture.pids()[0]).await;
}
#[tokio::test]
async fn cancelling_format_regeneration_evicts_and_reaps_the_active_worker() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("flint-recover-startup");
    let provider = Inference::new(recovery_config(&fixture)).unwrap();
    let tools = recovery_tools();
    let messages = [Message::user("Read")];
    {
        let call = provider.complete("", &messages, &tools);
        tokio::pin!(call);
        let observed = async {
            loop {
                if std::fs::read_to_string(fixture.model.with_extension("stage"))
                    .ok()
                    .as_deref()
                    == Some("reload")
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result=timeout(Duration::from_secs(10), observed)=>result.unwrap(),
            result=&mut call=>panic!("completed before cancellation: {result:?}"),
        }
    }
    assert_eq!(fixture.pids().len(), 2);
    for pid in fixture.pids() {
        dead(pid).await;
    }
}

#[tokio::test]
async fn flint_normal_stop_with_no_tokens_left_and_expanded_message_count_refuse() {
    if no_python() {
        return;
    }
    for message_limit in [false, true] {
        let fixture = Fixture::new("flint-recover");
        let mut config = recovery_config(&fixture);
        if !message_limit {
            config.output_tokens = 1;
        }
        let messages = vec![Message::user("Read"); if message_limit { 1023 } else { 1 }];
        let provider = Inference::new(config).unwrap();
        let error = provider
            .complete("", &messages, &recovery_tools())
            .await
            .unwrap_err();
        if message_limit {
            assert!(error.to_string().contains("message count"), "{error}");
        }
        assert_eq!(fixture.pids().len(), 1);
        drop(provider);
        dead(fixture.pids()[0]).await;
    }
}
#[tokio::test]
async fn cancelling_first_worker_eof_closure_allows_a_fresh_later_call() {
    if no_python() {
        return;
    }
    let fixture = Fixture::new("flint-recover-eof");
    let provider = Inference::new(recovery_config(&fixture)).unwrap();
    let tools = recovery_tools();
    let messages = [Message::user("Read")];
    {
        let call = provider.complete("", &messages, &tools);
        tokio::pin!(call);
        let observed = async {
            loop {
                if std::fs::read_to_string(fixture.model.with_extension("stage"))
                    .ok()
                    .as_deref()
                    == Some("eof")
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result=timeout(Duration::from_secs(10), observed)=>result.unwrap(),
            result=&mut call=>panic!("completed before cancellation: {result:?}"),
        }
    }
    assert_eq!(fixture.pids().len(), 1);
    dead(fixture.pids()[0]).await;
    let completion = provider.complete("", &messages, &tools).await.unwrap();
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(fixture.pids().len(), 2);
    drop(provider);
    dead(fixture.pids()[1]).await;
}
