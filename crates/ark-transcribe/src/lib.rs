//! Optional, local speech-to-text for ARK's injected dictation boundary (ADR 0070).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use anyhow::Context as _;
use ark_ui::transcription::{Segment, State, Status, TranscriptionProvider};
use async_trait::async_trait;
use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const MODEL_NAME: &str = "ggml-base.bin";
const MODEL_BYTES: u64 = 147_951_465;
const MODEL_SHA256: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin";

unsafe extern "C" fn discard_native_log(
    _level: whisper_rs_sys::ggml_log_level,
    _text: *const std::ffi::c_char,
    _user_data: *mut std::ffi::c_void,
) {
}

fn quiet_native_logs() {
    static LOGS: Once = Once::new();
    LOGS.call_once(|| {
        // SAFETY: whisper.cpp keeps this static C callback with null user data and also hands
        // it to GGML. It never dereferences a pointer, allocates or panics. Every constructor
        // completes this Once before its native inference can start.
        unsafe { whisper_rs::set_log_callback(Some(discard_native_log), std::ptr::null_mut()) };
    });
}

/// The multilingual whisper.cpp base model. Construction creates no files or downloads.
pub struct LocalWhisper {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
    preparing: tokio::sync::Mutex<()>,
    inference: Arc<tokio::sync::Semaphore>,
    /// The loaded model, kept while dictation is in use so a live preview does not read 148 MB
    /// from disk each time.
    loaded: Arc<Mutex<Option<Loaded>>>,
    releasing: Arc<std::sync::atomic::AtomicBool>,
}

struct Loaded {
    context: Arc<WhisperContext>,
    used: std::time::Instant,
}

const KEEP_LOADED: std::time::Duration = std::time::Duration::from_secs(45);

/// Whisper writes `[BLANK_AUDIO]` or `(music)` for stretches without speech.
fn is_marker(text: &str) -> bool {
    let text = text.trim();
    (text.starts_with('[') && text.ends_with(']')) || (text.starts_with('(') && text.ends_with(')'))
}

impl LocalWhisper {
    fn installed(path: &Path) -> bool {
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() == MODEL_BYTES)
    }

    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        // Native load and timing lines would otherwise go to the app's stderr. Errors still
        // return through Result. The callback is process-wide; another library can replace it.
        quiet_native_logs();
        let dir = model_dir.into();
        let status = if Self::installed(&dir.join(MODEL_NAME)) {
            Status::Ready
        } else {
            Status::NotInstalled
        };
        Self {
            dir,
            state: Arc::new(Mutex::new(State::new(status))),
            preparing: tokio::sync::Mutex::new(()),
            inference: Arc::new(tokio::sync::Semaphore::new(1)),
            loaded: Arc::new(Mutex::new(None)),
            releasing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn model_path(&self) -> PathBuf {
        self.dir.join(MODEL_NAME)
    }

    async fn install(&self) -> anyhow::Result<()> {
        let _one = self.preparing.lock().await;
        if Self::installed(&self.model_path()) {
            *self.state.lock().unwrap() = State::new(Status::Ready);
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self
            .dir
            .join(format!(".{MODEL_NAME}.{}.download", std::process::id()));
        let result = self.download(&tmp).await;
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            *self.state.lock().unwrap() = State::new(Status::Failed);
        }
        result
    }

    async fn download(&self, tmp: &Path) -> anyhow::Result<()> {
        *self.state.lock().unwrap() = State {
            status: Status::Downloading,
            downloaded: Some(0),
            total: Some(MODEL_BYTES),
        };
        let response = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(5 * 60))
            .build()?
            .get(MODEL_URL)
            .send()
            .await?
            .error_for_status()?;
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut stream = response.bytes_stream();
        let mut hash = Sha256::new();
        let mut downloaded = 0u64;
        while let Some(part) = stream.next().await {
            let part = part?;
            downloaded = downloaded.saturating_add(part.len() as u64);
            if downloaded > MODEL_BYTES {
                anyhow::bail!("speech model download is larger than expected");
            }
            hash.update(&part);
            file.write_all(&part).await?;
            let mut state = self.state.lock().unwrap();
            state.downloaded = Some(downloaded);
        }
        file.flush().await?;
        drop(file);
        let digest = format!("{:x}", hash.finalize());
        if downloaded != MODEL_BYTES || digest != MODEL_SHA256 {
            anyhow::bail!("speech model download did not match the pinned ARK release");
        }
        ark_os::fs::replace(tmp, &self.model_path())?;
        *self.state.lock().unwrap() = State::new(Status::Ready);
        Ok(())
    }
}

#[async_trait]
impl TranscriptionProvider for LocalWhisper {
    fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    async fn prepare(&self) -> anyhow::Result<()> {
        self.install().await
    }

    async fn transcribe(&self, pcm: Vec<i16>, language: Option<String>) -> anyhow::Result<String> {
        let mut segments = self.read(pcm, language, false).await?;
        segments.retain(|segment| !is_marker(&segment.text));
        Ok(segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>()
            .trim()
            .to_string())
    }

    async fn segments(
        &self,
        pcm: Vec<i16>,
        language: Option<String>,
    ) -> anyhow::Result<Vec<Segment>> {
        let mut segments = self.read(pcm, language, true).await?;
        segments.retain(|segment| !is_marker(&segment.text));
        Ok(segments)
    }
}

impl LocalWhisper {
    fn context(
        loaded: &Mutex<Option<Loaded>>,
        model: &Path,
    ) -> anyhow::Result<Arc<WhisperContext>> {
        let mut slot = loaded.lock().unwrap();
        if let Some(entry) = slot.as_mut() {
            entry.used = std::time::Instant::now();
            return Ok(entry.context.clone());
        }
        let context = Arc::new(
            WhisperContext::new_with_params(
                model.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .context("speech model could not be loaded")?,
        );
        *slot = Some(Loaded {
            context: context.clone(),
            used: std::time::Instant::now(),
        });
        Ok(context)
    }

    /// Timed stretches of speech. `timed` asks Whisper for timestamps, which a preview needs to
    /// know where speech can be settled.
    async fn read(
        &self,
        pcm: Vec<i16>,
        language: Option<String>,
        timed: bool,
    ) -> anyhow::Result<Vec<Segment>> {
        if pcm.is_empty() || pcm.len() > 8 * 60 * 16_000 {
            anyhow::bail!("recording is empty or longer than eight minutes");
        }
        let language = language.filter(|value| !value.trim().is_empty() && value != "auto");
        if let Some(value) = &language {
            anyhow::ensure!(
                !value.contains('\0') && whisper_rs::get_lang_id(value).is_some(),
                "unsupported dictation language"
            );
        }
        let permit = self.inference.clone().acquire_owned().await?;
        let model = self.model_path();
        let loaded = self.loaded.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Cancellation drops the async waiter, but native inference keeps its permit
            // until the blocking operation actually ends.
            let _permit = permit;
            let context = Self::context(&loaded, &model)?;
            let mut state = context
                .create_state()
                .context("speech engine could not start")?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_no_timestamps(!timed);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_special(false);
            // Whisper defaults to English. None must explicitly enable auto-detection.
            params.set_language(language.as_deref());
            let audio: Vec<f32> = pcm
                .into_iter()
                .map(|sample| f32::from(sample) / 32768.0)
                .collect();
            state
                .full(params, &audio)
                .context("recording could not be transcribed")?;
            let mut segments = Vec::new();
            for index in 0..state.full_n_segments() {
                let segment = state
                    .get_segment(index)
                    .context("transcript segment could not be read")?;
                segments.push(Segment {
                    // Whisper counts in hundredths of a second.
                    start_ms: u64::try_from(segment.start_timestamp()).unwrap_or(0) * 10,
                    end_ms: u64::try_from(segment.end_timestamp()).unwrap_or(0) * 10,
                    text: segment
                        .to_str_lossy()
                        .context("transcript segment could not be decoded")?
                        .into_owned(),
                });
            }
            Ok::<_, anyhow::Error>(segments)
        })
        .await?;
        self.release_later();
        result
    }

    /// Let go of the model once nothing has used it for a while. One watcher at a time.
    fn release_later(&self) {
        use std::sync::atomic::Ordering;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.releasing.swap(true, Ordering::SeqCst) {
            return;
        }
        let (loaded, releasing) = (self.loaded.clone(), self.releasing.clone());
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let mut slot = loaded.lock().unwrap();
                if slot
                    .as_ref()
                    .is_none_or(|entry| entry.used.elapsed() >= KEEP_LOADED)
                {
                    *slot = None;
                    releasing.store(false, Ordering::SeqCst);
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_never_downloads_or_creates_the_model_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("speech");
        let provider = LocalWhisper::new(&dir);
        assert_eq!(provider.state().status, Status::NotInstalled);
        assert!(!dir.exists());
    }

    #[test]
    fn an_installed_model_is_ready_without_loading_it() {
        let root = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(root.path().join(MODEL_NAME)).unwrap();
        file.set_len(MODEL_BYTES).unwrap();
        let provider = LocalWhisper::new(root.path());
        assert_eq!(provider.state().status, Status::Ready);
    }

    #[tokio::test]
    async fn malformed_language_is_refused_without_starting_the_decoder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("speech");
        let provider = LocalWhisper::new(&dir);
        let error = provider
            .transcribe(vec![0; 1600], Some("es\0".into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unsupported dictation language"));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn native_model_errors_still_return_without_creating_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("speech");
        let provider = LocalWhisper::new(&dir);
        let error = provider.transcribe(vec![0; 1600], None).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("speech model could not be loaded"));
        assert!(!dir.exists());
    }

    #[test]
    fn native_loading_writes_nothing_to_stderr() {
        if std::env::var_os("ARK_TRANSCRIBE_STDERR_CHILD").is_some() {
            let root = tempfile::tempdir().unwrap();
            let provider = LocalWhisper::new(root.path().join("speech"));
            let runtime = tokio::runtime::Runtime::new().unwrap();
            assert!(runtime
                .block_on(provider.transcribe(vec![0; 1600], None))
                .is_err());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::native_loading_writes_nothing_to_stderr"])
            .env("ARK_TRANSCRIBE_STDERR_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("whisper_") && !stderr.contains("ggml_"),
            "{stderr}"
        );
    }
}
