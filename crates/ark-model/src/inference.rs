//! Bounded local text generation through ARK Inference protocol version one.
//!
//! A dropped completion kills its child. Only a fully checked terminal frame permits reuse.
mod flint;
pub(crate) mod installed;

use crate::{Completion, Message, Provider, ProviderError, Result, Role, TextSink, ToolDef, Usage};
use async_trait::async_trait;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{timeout, timeout_at, Instant},
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
fn installed_config(
    executable: Option<PathBuf>,
    model: &str,
    resolve: impl FnOnce(&str, Option<&std::path::Path>) -> Result<InferenceConfig>,
) -> Result<InferenceConfig> {
    if std::path::Path::new(model).is_absolute() {
        return executable
            .map(|exe| InferenceConfig::new(exe, model))
            .ok_or_else(|| {
                refuse("Set an absolute ARK_INFERENCE_EXECUTABLE path for an explicit model path.")
            });
    }
    let mut config = resolve(model, executable.as_deref())?;
    if let Some(executable) = executable {
        config.executable = executable;
    }
    Ok(config)
}

/// Explicit local configuration. Array bytes are not a total-process memory limit.
#[derive(Clone, Debug)]
pub struct InferenceConfig {
    pub flint_tools: bool,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u32,
    pub seed: u64,
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
            flint_tools: false,
            temperature: 1.0,
            top_p: 0.95,
            top_k: 20,
            seed: 0,
            executable: executable.into(),
            model: model.into(),
            backend: "cpu".into(),
            context: 512,
            output_tokens: 32,
            array_bytes: 512 << 20,
            file_bytes: 16 << 30,
            input_bytes: FRAME,
            output_bytes: 8 << 20,
            batch: 256,
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
            || !(1..=32 << 30).contains(&self.file_bytes)
            || !self.temperature.is_finite()
            || self.temperature < 0.0
            || !self.top_p.is_finite()
            || self.top_p <= 0.0
            || self.top_p > 1.0
            || self.top_k > 500000
            || !(4..=FRAME).contains(&self.input_bytes)
            || !(1..=8 << 20).contains(&self.output_bytes)
            || !(1..=512).contains(&self.batch)
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
        if c.flint_tools {
            command.args([
                "--protocol",
                "2",
                "--temperature",
                &c.temperature.to_string(),
                "--top-p",
                &c.top_p.to_string(),
                "--top-k",
                &c.top_k.to_string(),
                "--seed",
                &c.seed.to_string(),
            ]);
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
        if tag != b'R' || data != (if c.flint_tools { 2u32 } else { 1u32 }).to_le_bytes() {
            return Err(broken());
        }
        Ok(worker)
    }
    async fn finish(self) -> Result<()> {
        let Self {
            mut child,
            input,
            output,
        } = self;
        drop(input);
        drop(output);
        match timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(_)) => Err(refuse(
                "Local inference worker did not close cleanly. Regeneration refused.",
            )),
            _ => {
                child.kill().await.map_err(|_| broken())?;
                Err(refuse(
                    "Local inference worker did not close cleanly. Regeneration refused.",
                ))
            }
        }
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
            b'D' => size == 32 || size == 37,
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
        let model = model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "flint".into());
        let mut config = installed_config(
            std::env::var_os("ARK_INFERENCE_EXECUTABLE").map(PathBuf::from),
            &model,
            installed::resolve,
        )?;
        if let Some(backend) = std::env::var_os("ARK_INFERENCE_BACKEND") {
            config.backend = backend
                .into_string()
                .map_err(|_| refuse("Invalid local inference backend."))?;
        }
        if let Some(format) = std::env::var_os("ARK_INFERENCE_FORMAT") {
            let format = format
                .into_string()
                .map_err(|_| refuse("Invalid local inference format."))?;
            config.flint_tools = match format.as_str() {
                "flint" => true,
                "text" => false,
                _ => return Err(refuse("Invalid local inference format.")),
            };
        }
        env_number("ARK_INFERENCE_TEMPERATURE", &mut config.temperature)?;
        env_number("ARK_INFERENCE_TOP_P", &mut config.top_p)?;
        env_number("ARK_INFERENCE_TOP_K", &mut config.top_k)?;
        env_number("ARK_INFERENCE_SEED", &mut config.seed)?;
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
        if self.config.flint_tools {
            let prompt = flint::render(system, messages, tools)?;
            if prompt.len() > self.config.input_bytes {
                return Err(refuse(
                    "Rendered local prompt exceeds the configured byte limit.",
                ));
            }
            return Ok(prompt.into_bytes());
        }
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
    async fn run(
        &self,
        payload: Vec<u8>,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        sink: TextSink<'_>,
    ) -> Result<Completion> {
        let mut slot = self
            .worker
            .try_lock()
            .map_err(|_| refuse("Local inference already has an active request."))?;
        let cached = slot
            .take()
            .and_then(|mut w| matches!(w.child.try_wait(), Ok(None)).then_some(w));
        let mut worker = match cached {
            Some(w) => w,
            None => Worker::start(&self.config).await?,
        };
        let mut config = self.config.clone();
        let mut prefill_left = config.prefill_timeout;
        let mut payload = payload;
        let mut usage = Usage::measured(0, 0);
        for index in 0..2 {
            let attempt = request(&mut worker, &config, payload, sink, &mut prefill_left).await?;
            usage.prompt_tokens = usage
                .prompt_tokens
                .checked_add(attempt.prompt)
                .ok_or_else(broken)?;
            usage.completion_tokens = usage
                .completion_tokens
                .checked_add(attempt.output)
                .ok_or_else(broken)?;
            tracing::debug!(
                attempt = index + 1,
                prompt_tokens = attempt.prompt,
                output_tokens = attempt.output,
                output_bytes = attempt.bytes,
                stop = attempt.stop,
                "Local inference physical attempt validated"
            );
            let decoded = match attempt.stop {
                Some(stop) => flint::decode(&attempt.content, stop, tools),
                None => Ok((attempt.content.clone(), Vec::new())),
            };
            match decoded {
                Ok((content, tool_calls)) => {
                    tracing::debug!(
                        attempt = index + 1,
                        outcome = "accepted",
                        "Local Flint parser outcome"
                    );
                    if index == 0 {
                        *slot = Some(worker);
                    } else {
                        worker.finish().await?;
                    }
                    if config.flint_tools && !content.is_empty() {
                        sink(&content);
                    }
                    return Ok(Completion {
                        content,
                        tool_calls,
                        usage,
                    });
                }
                Err(flint::DecodeFailure::FinalFormat { error, answer })
                    if index == 0
                        && !tools.is_empty()
                        && attempt.stop != Some(150018)
                        && answer.len() <= 8192 =>
                {
                    tracing::debug!(
                        attempt = index + 1,
                        outcome = "format_rejected",
                        "Local Flint parser outcome"
                    );
                    config.output_tokens = config
                        .output_tokens
                        .checked_sub(attempt.output)
                        .filter(|n| *n > 0)
                        .ok_or(error)?;
                    config.output_bytes = config
                        .output_bytes
                        .checked_sub(attempt.bytes)
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            refuse("No local output bytes remain for format regeneration.")
                        })?;
                    if prefill_left.is_zero() {
                        return Err(refuse("Local inference prefill deadline exceeded."));
                    }
                    let mut feedback = messages.to_vec();
                    feedback.push(Message::assistant(answer));
                    feedback.push(Message::user("The previous response was rejected before any action executed. Regenerate only complete XML tool_call blocks using the offered tool names and exact argument names. Do not add prose or fenced JSON. Do not repeat reasoning."));
                    payload = self.encode(system, &feedback, tools)?;
                    config.check()?;
                    tracing::debug!(
                        remaining_output_tokens = config.output_tokens,
                        remaining_output_bytes = config.output_bytes,
                        "Local Flint format regeneration"
                    );
                    worker.finish().await?;
                    worker = Worker::start(&config).await?;
                }
                Err(error) => {
                    let outcome = match &error {
                        flint::DecodeFailure::Reasoning(_) => "reasoning_refused",
                        flint::DecodeFailure::FinalFormat { .. } => "format_rejected",
                    };
                    tracing::debug!(attempt = index + 1, outcome, "Local Flint parser outcome");
                    return Err(error.into_error());
                }
            }
        }
        Err(broken())
    }
}
struct Attempt {
    content: String,
    prompt: u32,
    output: u32,
    bytes: usize,
    stop: Option<u32>,
}
async fn request(
    worker: &mut Worker,
    c: &InferenceConfig,
    payload: Vec<u8>,
    sink: TextSink<'_>,
    prefill_left: &mut Duration,
) -> Result<Attempt> {
    let prefill_started = Instant::now();
    let prefill_deadline = prefill_started + *prefill_left;
    let mut prefill_elapsed = None;
    let mut head = [b'G', 0, 0, 0, 0];
    head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    timeout_at(prefill_deadline, async {
        worker.input.write_all(&head).await.map_err(|_| broken())?;
        worker
            .input
            .write_all(&payload)
            .await
            .map_err(|_| broken())?;
        worker.input.flush().await.map_err(|_| broken())
    })
    .await
    .map_err(|_| refuse("Local inference prefill deadline exceeded."))??;
    drop(payload);
    let mut accepted = false;
    let mut chunks = 0u64;
    let mut bytes = 0usize;
    let mut content = String::new();
    let mut pending = Vec::new();
    loop {
        let (tag, data) = if chunks == 0 {
            timeout_at(prefill_deadline, worker.frame())
                .await
                .map_err(|_| refuse("Local inference prefill deadline exceeded."))??
        } else {
            timeout(c.progress_timeout, worker.frame())
                .await
                .map_err(|_| refuse("Local inference progress deadline exceeded."))??
        };
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
                prefill_elapsed.get_or_insert_with(|| prefill_started.elapsed());
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
                    if !c.flint_tools {
                        sink(text);
                    }
                    pending.drain(..valid);
                }
                if pending.len() > 3 {
                    return Err(broken());
                }
            }
            b'D' if accepted => {
                if data.len() != if c.flint_tools { 37 } else { 32 } {
                    return Err(broken());
                }
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
                *prefill_left = prefill_left
                    .checked_sub(prefill_elapsed.unwrap_or_else(|| prefill_started.elapsed()))
                    .ok_or_else(|| refuse("Local inference prefill deadline exceeded."))?;
                let stop = if c.flint_tools {
                    if data[32] != 1 {
                        return Err(refuse(
                            "Local Flint exhausted its output budget. No tool call was accepted.",
                        ));
                    }
                    let stop = u32::from_le_bytes(data[33..37].try_into().map_err(|_| broken())?);
                    if ![150001, 150003, 150005, 150018].contains(&stop) {
                        return Err(broken());
                    }
                    Some(stop)
                } else {
                    None
                };
                return Ok(Attempt {
                    content,
                    prompt: prompt as u32,
                    output: output as u32,
                    bytes,
                    stop,
                });
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
        timeout(
            self.config.call_timeout,
            self.run(payload, system, messages, tools, sink),
        )
        .await
        .map_err(|_| refuse("Local inference call deadline exceeded."))?
    }
    fn context_window(&self) -> Option<u32> {
        Some(self.config.context - self.config.output_tokens)
    }
    fn describe(&self) -> String {
        if self.config.flint_tools {
            "ARK Inference (local Flint)".into()
        } else {
            "ARK Inference (local text)".into()
        }
    }
    fn model_id(&self) -> String {
        self.config.model.to_string_lossy().into_owned()
    }
}
