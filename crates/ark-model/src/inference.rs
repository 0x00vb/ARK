//! Bounded local text generation through ARK Inference protocol version one.
//!
//! A dropped completion kills its child. Only a fully checked terminal frame permits reuse.
use crate::{Completion, Message, Provider, ProviderError, Result, Role, TextSink, ToolDef, Usage};
use async_trait::async_trait;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
const FRAME: usize = 1 << 20;
fn refuse(detail: &str) -> ProviderError {
    ProviderError::Api {
        provider: "inference".into(),
        detail: detail.into(),
    }
}
fn broken() -> ProviderError {
    ProviderError::Api {
        provider: "inference".into(),
        detail: "Local inference transport failed. The next call starts a new process.".into(),
    }
}
/// Explicit local configuration. Array bytes are not a total-process memory limit.
#[derive(Clone, Debug)]
pub struct InferenceConfig {
    pub executable: PathBuf,
    pub model: PathBuf,
    pub backend: String,
    pub context: u32,
    pub output_tokens: u32,
    pub array_bytes: u64,
    pub file_bytes: u64,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub batch: u32,
    pub threads: u32,
    pub startup_timeout: Duration,
    /// From sending the request to the first token frame. It covers the whole prefill.
    pub prefill_timeout: Duration,
    /// Each later frame.
    pub progress_timeout: Duration,
    pub call_timeout: Duration,
}
impl InferenceConfig {
    pub fn new(executable: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            model: model.into(),
            backend: "cpu".into(),
            context: 512,
            output_tokens: 32,
            array_bytes: 512 << 20,
            file_bytes: 16 << 30,
            input_bytes: FRAME,
            output_bytes: 8 << 20,
            batch: 32,
            threads: 2,
            startup_timeout: Duration::from_secs(120),
            // Prefill is one wait before the first token. On CPU it can take minutes.
            prefill_timeout: Duration::from_secs(600),
            progress_timeout: Duration::from_secs(120),
            call_timeout: Duration::from_secs(600),
        }
    }
    fn check(&self) -> Result<()> {
        if !self.executable.is_absolute()
            || !self.model.is_absolute()
            || !matches!(self.backend.as_str(), "cpu" | "metal")
            || !(1..=16384).contains(&self.context)
            || !(1..self.context).contains(&self.output_tokens)
            || !(1..=16 << 30).contains(&self.array_bytes)
            || !(1..=16 << 30).contains(&self.file_bytes)
            || !(4..=FRAME).contains(&self.input_bytes)
            || !(1..=8 << 20).contains(&self.output_bytes)
            || !(1..=32).contains(&self.batch)
            || !(1..=64).contains(&self.threads)
            || self.startup_timeout.is_zero()
            || self.prefill_timeout.is_zero()
            || self.progress_timeout.is_zero()
            || self.call_timeout.is_zero()
        {
            return Err(refuse("Invalid local inference paths or limits."));
        }
        Ok(())
    }
}
struct Worker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}
impl Worker {
    async fn start(c: &InferenceConfig) -> Result<Self> {
        let mut command = Command::new(&c.executable);
        command
            .env_clear()
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Windows needs its system directory to load system libraries.
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .arg("--model")
            .arg(&c.model)
            .arg("--backend")
            .arg(&c.backend);
        for (name, value) in [
            ("--context", u64::from(c.context)),
            ("--tokens", u64::from(c.output_tokens)),
            ("--array-bytes", c.array_bytes),
            ("--file-bytes", c.file_bytes),
            ("--input-bytes", c.input_bytes as u64),
            ("--output-bytes", c.output_bytes as u64),
            ("--batch", u64::from(c.batch)),
            ("--threads", u64::from(c.threads)),
        ] {
            command.arg(name).arg(value.to_string());
        }
        let mut child = command
            .spawn()
            .map_err(|_| refuse("Cannot start local inference. Check the executable path."))?;
        let input = child.stdin.take().ok_or_else(broken)?;
        let output = child.stdout.take().ok_or_else(broken)?;
        let mut worker = Self {
            child,
            input,
            output,
        };
        let (tag, data) = timeout(c.startup_timeout, worker.frame())
            .await
            .map_err(|_| refuse("Local inference startup deadline exceeded."))??;
        if tag == b'E' && data == 3u32.to_le_bytes() {
            return Err(refuse("Local model loading failed. Check the file-byte limit, model format and selected backend."));
        }
        if tag != b'R' || data != 1u32.to_le_bytes() {
            return Err(broken());
        }
        Ok(worker)
    }
    async fn frame(&mut self) -> Result<(u8, Vec<u8>)> {
        let mut head = [0; 5];
        self.output
            .read_exact(&mut head)
            .await
            .map_err(|_| broken())?;
        let size = u32::from_le_bytes(head[1..].try_into().map_err(|_| broken())?) as usize;
        // Each tag has a strict bound before allocation.
        let valid = match head[0] {
            b'R' | b'E' => size == 4,
            b'A' => size == 16,
            b'D' => size == 32,
            b'T' => (1..=FRAME).contains(&size),
            _ => false,
        };
        if !valid {
            return Err(broken());
        }
        let mut data = vec![0; size];
        self.output
            .read_exact(&mut data)
            .await
            .map_err(|_| broken())?;
        Ok((head[0], data))
    }
}
/// Text-only provider. One active call and one loaded child per instance.
/// Nonempty tools, pictures and tool-call history fail before process startup.
pub struct Inference {
    config: InferenceConfig,
    worker: Mutex<Option<Worker>>,
}
impl Inference {
    pub fn new(config: InferenceConfig) -> Result<Self> {
        config.check()?;
        Ok(Self {
            config,
            worker: Mutex::new(None),
        })
    }
    pub(crate) fn from_env(model: Option<String>) -> Result<Self> {
        let exe = std::env::var_os("ARK_INFERENCE_EXECUTABLE")
            .ok_or_else(|| refuse("Set an absolute ARK_INFERENCE_EXECUTABLE path."))?;
        let model = model
            .filter(|m| !m.is_empty())
            .ok_or_else(|| refuse("Select an absolute local GGUF model path."))?;
        let mut config = InferenceConfig::new(PathBuf::from(exe), model);
        if let Some(backend) = std::env::var_os("ARK_INFERENCE_BACKEND") {
            config.backend = backend
                .into_string()
                .map_err(|_| refuse("Invalid local inference backend."))?;
        }
        env_number("ARK_INFERENCE_CONTEXT", &mut config.context)?;
        env_number("ARK_INFERENCE_OUTPUT_TOKENS", &mut config.output_tokens)?;
        env_number("ARK_INFERENCE_ARRAY_BYTES", &mut config.array_bytes)?;
        env_number("ARK_INFERENCE_FILE_BYTES", &mut config.file_bytes)?;
        env_number("ARK_INFERENCE_INPUT_BYTES", &mut config.input_bytes)?;
        env_number("ARK_INFERENCE_OUTPUT_BYTES", &mut config.output_bytes)?;
        env_number("ARK_INFERENCE_BATCH", &mut config.batch)?;
        env_number("ARK_INFERENCE_THREADS", &mut config.threads)?;
        env_seconds("ARK_INFERENCE_STARTUP_SECONDS", &mut config.startup_timeout)?;
        env_seconds("ARK_INFERENCE_PREFILL_SECONDS", &mut config.prefill_timeout)?;
        env_seconds(
            "ARK_INFERENCE_PROGRESS_SECONDS",
            &mut config.progress_timeout,
        )?;
        env_seconds("ARK_INFERENCE_CALL_SECONDS", &mut config.call_timeout)?;
        Self::new(config)
    }
    fn encode(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Vec<u8>> {
        if !tools.is_empty()
            || messages.iter().any(|m| {
                !m.pictures.is_empty()
                    || !m.tool_calls.is_empty()
                    || m.tool_call_id.is_some()
                    || m.tool_name.is_some()
                    || m.role == Role::Tool
            })
        {
            return Err(refuse(
                "Local inference currently supports text conversations without tools or images.",
            ));
        }
        let count = messages
            .len()
            .checked_add(usize::from(!system.is_empty()))
            .ok_or_else(broken)?;
        if !(1..=1024).contains(&count) {
            return Err(refuse(
                "Conversation message count exceeds the local limit.",
            ));
        }
        // Preflight all serialized bytes before allocating the transport payload.
        let mut bytes = 4usize;
        for text in std::iter::once(system)
            .filter(|s| !s.is_empty())
            .chain(messages.iter().map(|m| m.content.as_str()))
        {
            bytes = bytes
                .checked_add(5)
                .and_then(|n| n.checked_add(text.len()))
                .ok_or_else(broken)?;
        }
        if bytes > self.config.input_bytes {
            return Err(refuse("Conversation exceeds the local input byte limit."));
        }
        let mut data = Vec::with_capacity(bytes);
        data.extend_from_slice(&(count as u32).to_le_bytes());
        let mut add = |role: u8, text: &str| {
            data.push(role);
            data.extend_from_slice(&(text.len() as u32).to_le_bytes());
            data.extend_from_slice(text.as_bytes());
        };
        if !system.is_empty() {
            add(1, system);
        }
        for m in messages {
            add(
                match m.role {
                    Role::System => 1,
                    Role::User => 2,
                    Role::Assistant => 3,
                    Role::Tool => unreachable!(),
                },
                &m.content,
            );
        }
        Ok(data)
    }
    async fn run(&self, payload: Vec<u8>, sink: TextSink<'_>) -> Result<Completion> {
        let c = &self.config;
        let mut slot = self
            .worker
            .try_lock()
            .map_err(|_| refuse("Local inference already has an active request."))?;
        // The slot stays empty during every await. Dropping this future kills an owned worker.
        // A cached child that exited while idle is replaced, not written to.
        let cached = slot
            .take()
            .and_then(|mut w| matches!(w.child.try_wait(), Ok(None)).then_some(w));
        let mut worker = match cached {
            Some(w) => w,
            None => Worker::start(c).await?,
        };
        let mut head = [b'G', 0, 0, 0, 0];
        head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        worker.input.write_all(&head).await.map_err(|_| broken())?;
        worker
            .input
            .write_all(&payload)
            .await
            .map_err(|_| broken())?;
        worker.input.flush().await.map_err(|_| broken())?;
        drop(payload);
        let mut accepted = false;
        let mut chunks = 0u64;
        let mut bytes = 0usize;
        let mut content = String::new();
        let mut pending = Vec::new();
        loop {
            let (deadline, stage) = if chunks == 0 {
                (c.prefill_timeout, "prefill")
            } else {
                (c.progress_timeout, "progress")
            };
            let (tag, data) = timeout(deadline, worker.frame())
                .await
                .map_err(|_| refuse(&format!("Local inference {stage} deadline exceeded.")))??;
            match tag {
                b'A' if !accepted => {
                    let logical = integer(&data[..8]);
                    let capacity = integer(&data[8..16]);
                    if logical == 0 || capacity < logical || capacity > c.array_bytes {
                        return Err(broken());
                    }
                    accepted = true;
                }
                b'T' if accepted && chunks < u64::from(c.output_tokens) => {
                    bytes = bytes.checked_add(data.len()).ok_or_else(broken)?;
                    if bytes > c.output_bytes {
                        return Err(refuse("Local inference exceeded its output byte limit."));
                    }
                    chunks += 1;
                    pending.extend_from_slice(&data);
                    let valid = match std::str::from_utf8(&pending) {
                        Ok(_) => pending.len(),
                        Err(e) if e.error_len().is_none() => e.valid_up_to(),
                        Err(_) => return Err(broken()),
                    };
                    if valid != 0 {
                        let text = std::str::from_utf8(&pending[..valid]).map_err(|_| broken())?;
                        content.push_str(text);
                        sink(text);
                        pending.drain(..valid);
                    }
                    if pending.len() > 3 {
                        return Err(broken());
                    }
                }
                b'D' if accepted => {
                    let prompt = integer(&data[..8]);
                    let output = integer(&data[8..16]);
                    if integer(&data[16..24]) != 0
                        || integer(&data[24..32]) != 0
                        || !pending.is_empty()
                        || prompt == 0
                        || prompt > u64::from(c.context - c.output_tokens)
                        || output != chunks
                        || output > u64::from(c.output_tokens)
                    {
                        return Err(broken());
                    }
                    let completion = Completion {
                        content,
                        usage: Usage::measured(prompt as u32, output as u32),
                        ..Completion::default()
                    };
                    *slot = Some(worker);
                    return Ok(completion);
                }
                b'E' => {
                    let code = u32::from_le_bytes(data.try_into().map_err(|_| broken())?);
                    return Err(match code {
                        2 => refuse("Local inference refused the request. Check context, array and output limits."),
                        3 => refuse("Local inference computation failed. No completion was accepted."),
                        _ => broken(),
                    });
                }
                _ => return Err(broken()),
            }
        }
    }
}
fn env_number<T: std::str::FromStr>(name: &str, output: &mut T) -> Result<()> {
    if let Some(value) = std::env::var_os(name) {
        *output = value
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| refuse("Invalid local inference environment limit."))?;
    }
    Ok(())
}
fn env_seconds(name: &str, output: &mut Duration) -> Result<()> {
    let mut seconds = output.as_secs();
    env_number(name, &mut seconds)?;
    *output = Duration::from_secs(seconds);
    Ok(())
}
fn integer(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("checked frame width"))
}
#[async_trait]
impl Provider for Inference {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        self.complete_streaming(system, messages, tools, &|_| {})
            .await
    }
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        sink: TextSink<'_>,
    ) -> Result<Completion> {
        let payload = self.encode(system, messages, tools)?;
        timeout(self.config.call_timeout, self.run(payload, sink))
            .await
            .map_err(|_| refuse("Local inference call deadline exceeded."))?
    }
    fn context_window(&self) -> Option<u32> {
        Some(self.config.context - self.config.output_tokens)
    }
    fn describe(&self) -> String {
        "ARK Inference (local text)".into()
    }
    fn model_id(&self) -> String {
        self.config.model.to_string_lossy().into_owned()
    }
}
