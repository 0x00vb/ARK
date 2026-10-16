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
import os,sys,struct,time
path=sys.argv[sys.argv.index('--model')+1]
mode=os.path.basename(path)
state=path+'.pids'
with open(state,'a') as f:f.write(str(os.getpid())+'\n')
first=len(open(state).readlines())==1
bad=mode if first else 'good'
def frame(tag,data):
 sys.stdout.buffer.write(tag+struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
def park():time.sleep(3600)
def stage(name):
 with open(path+'.stage','w') as f:f.write(name)
if bad=='startup':stage('startup');park()
if bad=='stderr':os.write(2,b'x'*2097152)
frame(b'R',struct.pack('<I',2 if bad=='version' else 1))
if bad=='write':stage('write');park()
while True:
 head=sys.stdin.buffer.read(5)
 if not head:break
 n=struct.unpack('<I',head[1:])[0]
 if len(sys.stdin.buffer.read(n))!=n:break
 if bad=='crash':sys.exit(1)
 if bad=='order':frame(b'T',b'x');park()
 if bad=='huge':sys.stdout.buffer.write(b'T'+struct.pack('<I',1048577));sys.stdout.buffer.flush();park()
 if bad=='error':frame(b'E',struct.pack('<I',99));park()
 frame(b'A',struct.pack('<QQ',100,1<<40 if bad=='capacity' else 100))
 if bad=='prefill':stage('prefill');park()
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
                result = timeout(Duration::from_secs(2), observed) => { result.expect("cancellation stage not observed"); },
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

#[test]
#[ignore = "isolated environment helper, invoked by factory_environment_is_validated"]
fn factory_environment_child() {
    let model = std::env::var("ARK_MODEL").unwrap();
    let result = ark_model::build("inference", Some(model));
    if std::env::var_os("ARK_INFERENCE_EXPECT_INVALID").is_some() {
        permanent(result);
    } else {
        let provider = result.unwrap();
        assert_eq!(provider.context_window(), Some(124));
        assert_eq!(provider.describe(), "ARK Inference (local text)");
        assert!(!provider.reads_pictures());
    }
}
#[test]
fn factory_environment_is_validated() {
    let fixture = Fixture::new("good");
    for (key, value) in [
        ("", ""),
        ("ARK_INFERENCE_ARRAY_BYTES", "0"),
        ("ARK_INFERENCE_CONTEXT", "not-a-number"),
        ("ARK_INFERENCE_BACKEND", "cuda"),
        ("ARK_INFERENCE_EXECUTABLE", "relative"),
        ("ARK_INFERENCE_THREADS", "65"),
        ("ARK_INFERENCE_OUTPUT_TOKENS", "128"),
        ("ARK_INFERENCE_PROGRESS_SECONDS", "0"),
        ("ARK_INFERENCE_PREFILL_SECONDS", "0"),
        ("ARK_INFERENCE_CALL_SECONDS", "soon"),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .args(["--ignored", "--exact", "factory_environment_child"])
            .env("ARK_INFERENCE_EXECUTABLE", &fixture.exe)
            .env("ARK_MODEL", &fixture.model)
            .env("ARK_INFERENCE_CONTEXT", "128")
            .env("ARK_INFERENCE_OUTPUT_TOKENS", "4");
        if !key.is_empty() {
            command
                .env(key, value)
                .env("ARK_INFERENCE_EXPECT_INVALID", "1");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "factory child failed: {}",
            String::from_utf8_lossy(&result.stdout)
        );
    }
    assert!(fixture.pids().is_empty());
}
