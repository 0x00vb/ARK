use crate::{accounts::ModelAccount, codex_app::CodexApp, ModelOption};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = "0.166.0";
const NATIVE_DARWIN_ARM64: &str =
    "6782006c2be35409e75a4c7b827e5f3aca9b9051afb7ea4624ba011bffdf9452";
const CONTRACT: &str = "ark-codex-host-1";
const FEATURES: &str = include_str!("codex_host_features.txt");
const WAIT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug)]
pub(crate) enum QualificationError {
    Unsupported(String),
    Unavailable(String),
}
impl QualificationError {
    pub(crate) fn internal_reason(&self) -> &str {
        match self {
            Self::Unsupported(reason) | Self::Unavailable(reason) => reason,
        }
    }
    pub(crate) fn unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}
impl std::fmt::Display for QualificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(_) => {
                formatter.write_str("Update Codex in Settings > Models to use automations.")
            }
            Self::Unavailable(reason) if reason.starts_with("Sign in") => {
                formatter.write_str(reason)
            }
            Self::Unavailable(_) => {
                formatter.write_str("Refresh Models in Settings and try again.")
            }
        }
    }
}
impl From<String> for QualificationError {
    fn from(reason: String) -> Self {
        Self::Unavailable(reason)
    }
}
impl From<&str> for QualificationError {
    fn from(reason: &str) -> Self {
        Self::Unavailable(reason.into())
    }
}

#[derive(Clone)]
struct FileIdentity {
    path: PathBuf,
    digest: blake3::Hash,
    length: u64,
    modified: Option<std::time::SystemTime>,
}
impl FileIdentity {
    fn read(path: &Path) -> Result<Self, String> {
        let path = path
            .canonicalize()
            .map_err(|_| "Codex executable is unavailable")?;
        if !std::fs::metadata(&path)
            .map_err(|_| "Codex executable is unavailable")?
            .is_file()
        {
            return Err("Codex executable is not a regular file".into());
        }
        let mut file = File::open(&path).map_err(|_| "Codex executable is unreadable")?;
        if !file
            .metadata()
            .map_err(|_| "Codex executable is unreadable")?
            .is_file()
        {
            return Err("Codex executable is not a regular file".into());
        }
        let mut hash = blake3::Hasher::new();
        let mut buf = [0; 65536];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|_| "Codex executable is unreadable")?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        let metadata = file
            .metadata()
            .map_err(|_| "Codex executable is unreadable")?;
        Ok(Self {
            path,
            digest: hash.finalize(),
            length: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
    fn fast_verify(&self) -> Result<(), String> {
        let metadata = std::fs::metadata(&self.path)
            .map_err(|_| "Codex was updated. Select it again in Settings > Models.")?;
        if !metadata.is_file()
            || metadata.len() != self.length
            || metadata.modified().ok() != self.modified
        {
            return Err("Codex was updated. Select it again in Settings > Models.".into());
        }
        Ok(())
    }
    fn verify(&self) -> Result<(), String> {
        let current = Self::read(&self.path)?;
        if current.path != self.path || current.digest != self.digest {
            return Err("Codex was updated. Select it again in Settings > Models.".into());
        }
        Ok(())
    }
}

struct QualificationDirectory(Option<PathBuf>);
impl Drop for QualificationDirectory {
    fn drop(&mut self) {
        if let Some(directory) = &self.0 {
            if let Err(error) = std::fs::remove_dir_all(directory) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error,"could not remove incomplete Codex qualification files");
                }
            }
        }
    }
}

pub(crate) struct CodexHost {
    launcher_path: PathBuf,
    launcher: FileIdentity,
    native: FileIdentity,
    account: Option<ModelAccount>,
    home: PathBuf,
    pub(crate) model: String,
    directory: PathBuf,
    catalog_hash: blake3::Hash,
    account_fingerprint: blake3::Hash,
    stamp: Value,
}

impl Drop for CodexHost {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.directory) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "could not remove private Codex model configuration");
            }
        }
    }
}

pub(crate) fn startup_flags() -> Vec<String> {
    let mut flags = Vec::new();
    for line in FEATURES.lines().filter(|line| !line.contains(" removed")) {
        let name = line
            .split_whitespace()
            .next()
            .expect("audited feature name");
        flags.extend([
            "-c".into(),
            format!("features.{name}={}", name == "skip_host_skill_discovery"),
        ]);
    }
    for setting in [
        "tools.update_plan.enabled=false",
        "tools.experimental_request_user_input.enabled=false",
        "skills.include_instructions=false",
        "project_doc_max_bytes=0",
        "plugins._default.enabled=false",
        "web_search=\"disabled\"",
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
    ] {
        flags.extend(["-c".into(), setting.into()]);
    }
    flags
}

fn native_binary(launcher: &Path) -> Result<PathBuf, String> {
    if launcher.file_name().is_some_and(|name| name == "codex.js") {
        let package = launcher
            .parent()
            .and_then(Path::parent)
            .ok_or("Unsupported Codex launcher")?;
        let scope = package.parent().ok_or("Unsupported Codex launcher")?;
        let (platform, target) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => ("darwin-arm64", "aarch64-apple-darwin"),
            ("macos", "x86_64") => ("darwin-x64", "x86_64-apple-darwin"),
            ("linux", "aarch64") => ("linux-arm64", "aarch64-unknown-linux-musl"),
            ("linux", "x86_64") => ("linux-x64", "x86_64-unknown-linux-musl"),
            _ => return Err("This Codex platform has no verified tool isolation".into()),
        };
        let metadata: Value = serde_json::from_slice(
            &std::fs::read(package.join("package.json"))
                .map_err(|_| "Unsupported Codex package")?,
        )
        .map_err(|_| "Unsupported Codex package")?;
        if metadata["name"] != "@openai/codex" || metadata["version"] != VERSION {
            return Err("This Codex package has no verified tool isolation".into());
        }
        let nested = package.join(format!(
            "node_modules/@openai/codex-{platform}/vendor/{target}/bin/codex"
        ));
        let sibling = scope.join(format!("codex-{platform}/vendor/{target}/bin/codex"));
        let bundled = package.join(format!("vendor/{target}/bin/codex"));
        return [nested, sibling, bundled]
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| "Codex native executable is unavailable".into());
    }
    let mut magic = [0; 4];
    File::open(launcher)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|_| "Unsupported Codex executable")?;
    if !matches!(
        magic,
        [0x7f, b'E', b'L', b'F']
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
    ) {
        return Err("This Codex launcher has no verified native execution path".into());
    }
    Ok(launcher.to_path_buf())
}

pub(crate) fn restricted_command(
    bin: &Path,
    account: Option<&ModelAccount>,
) -> Result<tokio::process::Command, String> {
    let mut command = crate::cli::command(bin);
    command.env_clear();
    for name in [
        "HOME",
        "CODEX_HOME",
        "PATH",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "SYSTEMROOT",
        "WINDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(account) = account {
        account
            .command(&mut command)
            .map_err(|_| "The Codex account changed")?;
    }
    Ok(command)
}

async fn probe(
    bin: &Path,
    account: Option<&ModelAccount>,
    args: &[&str],
) -> Result<String, String> {
    let mut command = restricted_command(bin, account)?;
    command
        .args(args)
        .args(startup_flags())
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let child = command.spawn().map_err(|_| "Could not verify Codex")?;
    let mut tree = ark_os::process::Tree::of(child.id());
    let output = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .map_err(|_| "Codex verification timed out")?
        .map_err(|_| "Codex verification failed")?;
    tree.disarm();
    if !output.status.success() {
        return Err("Codex verification failed".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Codex verification returned invalid text".into())
}

fn metadata(home: &Path, model: &str, advertised: Option<&ModelOption>) -> Result<Value, String> {
    let cache_file = std::fs::metadata(home.join("models_cache.json"))
        .map_err(|_| "Codex has no current model metadata")?;
    if !cache_file.is_file() || cache_file.len() > 16 * 1024 * 1024 {
        return Err("Codex model metadata is not a bounded regular file".into());
    }
    let bytes = std::fs::read(home.join("models_cache.json"))
        .map_err(|_| "Codex has no current model metadata. Refresh Models and try again")?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Codex model metadata is too large".into());
    }
    let cache: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Codex model metadata is invalid")?;
    if cache["client_version"] != VERSION {
        return Err("Codex model metadata belongs to another program version".into());
    }
    let fetched = cache["fetched_at"]
        .as_str()
        .and_then(|stamp| {
            time::OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339).ok()
        })
        .ok_or("Codex model metadata has no valid freshness receipt")?;
    let age = time::OffsetDateTime::now_utc() - fetched;
    if age.is_negative() || age > time::Duration::minutes(5) {
        return Err("Codex model metadata is stale. Refresh Models and try again".into());
    }
    let selected = cache["models"]
        .as_array()
        .and_then(|models| models.iter().find(|entry| entry["slug"] == model))
        .ok_or("This Codex model has no verified metadata")?
        .clone();
    if let Some(advertised) = advertised {
        let levels = selected["supported_reasoning_levels"]
            .as_array()
            .ok_or("Codex model reasoning metadata is invalid")?;
        let efforts: Vec<_> = levels
            .iter()
            .filter_map(|level| level["effort"].as_str())
            .collect();
        if advertised
            .efforts
            .iter()
            .any(|effort| !efforts.contains(&effort.as_str()))
            || advertised.default_effort.as_deref() != selected["default_reasoning_level"].as_str()
        {
            return Err("Codex model discovery and metadata disagree".into());
        }
    }
    if !selected["context_window"].is_number()
        || !selected["input_modalities"].is_array()
        || !selected["model_messages"].is_object()
    {
        return Err("Codex model capabilities are incomplete".into());
    }
    Ok(selected)
}

fn private_directory(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|_| "Could not create private Codex state")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect Codex state")?;
    }
    Ok(())
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "Could not create private Codex state")?;
    file.write_all(bytes)
        .map_err(|_| "Could not save private Codex state".into())
}

impl CodexHost {
    pub(crate) async fn qualify(
        bin: PathBuf,
        account: Option<ModelAccount>,
        requested: String,
    ) -> Result<Arc<Self>, QualificationError> {
        let launcher = FileIdentity::read(&bin).map_err(QualificationError::Unsupported)?;
        let native_path = native_binary(&launcher.path).map_err(QualificationError::Unsupported)?;
        let native = FileIdentity::read(&native_path).map_err(QualificationError::Unsupported)?;
        if !cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || native.digest.to_hex().as_str() != NATIVE_DARWIN_ARM64
        {
            return Err(QualificationError::Unsupported(
                "This Codex native build has no verified tool isolation".into(),
            ));
        }
        let version = probe(&native.path, account.as_ref(), &["--version"]).await?;
        if version.trim() != format!("codex-cli {VERSION}") {
            return Err("This Codex version has no verified tool isolation".into());
        }
        let features = probe(&native.path, account.as_ref(), &["features", "list"]).await?;
        let inventory = features
            .lines()
            .map(|line| {
                let mut parts = line.split_whitespace().collect::<Vec<_>>();
                parts.pop();
                parts.join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        if inventory != FEATURES {
            return Err(
                "Codex features changed. Protected execution needs a new verification".into(),
            );
        }
        let home = if let Some(account) = &account {
            account
                .directory()
                .map_err(|_| "The Codex account changed")?
        } else {
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .ok_or("Codex home is unavailable")?
        }
        .canonicalize()
        .map_err(|_| "Codex home is unavailable")?;
        let directory =
            std::env::temp_dir().join(format!("ark-codex-host-{}", uuid::Uuid::new_v4()));
        private_directory(&directory)?;
        let mut directory_guard = QualificationDirectory(Some(directory.clone()));
        let result: Result<Arc<Self>, QualificationError> = async {
            let mut app = CodexApp::start_restricted_catalog(&native.path, env!("CARGO_PKG_VERSION"), account.as_ref(), &startup_flags(), &directory).await.map_err(|_| "Could not verify the Codex model catalog")?;
            let snapshot = app.qualification_snapshot().await;
            let account_identity = app.account_fingerprint().await;
            app.stop().await;
            let account_fingerprint = account_identity.map_err(|_| "Sign in to Codex in Settings > Models to use automations")?;
            let (models, default_model) = snapshot.map_err(|error| match error { crate::codex_app::AppError::Failed(reason) => reason, _ => "Could not verify Codex configuration and models".into() })?;
            let model = if requested == "default" { default_model.ok_or("Codex did not identify its default model")? } else { requested };
            let advertised = models.iter().find(|entry| entry.id == model).ok_or("Codex did not advertise this model")?;
            let selected = metadata(&home, &model, Some(advertised))?;
            let source_metadata = blake3::hash(&serde_json::to_vec(&selected).map_err(|_| "Invalid Codex metadata")?);
            let sanitized = tool_free_metadata(selected);
            let catalog = serde_json::to_vec(&json!({"models":[sanitized]})).map_err(|_| "Invalid Codex metadata")?;
            let catalog_hash = blake3::hash(&catalog);
            private_write(&directory.join("models.json"), &catalog)?;
            let stamp = json!({"contract":CONTRACT,"version":VERSION,"launcher":launcher.path,"launcherHash":launcher.digest.to_hex().to_string(),"native":native.path,"nativeHash":native.digest.to_hex().to_string(),"metadata":source_metadata.to_hex().to_string(),"account":account.as_ref().map(|account| &account.id),"revision":account.as_ref().map_or(0, |account| account.revision),"home":home,"model":model,"features":blake3::hash(FEATURES.as_bytes()).to_hex().to_string(),"accountFingerprint":account_fingerprint.to_hex().to_string()});
            Ok(Arc::new(Self { launcher_path: bin, launcher, native, account, home, model, directory: directory.clone(), catalog_hash, account_fingerprint, stamp }))
        }.await;
        if result.is_ok() {
            directory_guard.0 = None;
        }
        result
    }
    pub(crate) fn fast_verify(&self) -> Result<(), String> {
        if self
            .launcher_path
            .canonicalize()
            .map_err(|_| "Codex launcher changed")?
            != self.launcher.path
        {
            return Err("Codex was updated. Select it again in Settings > Models.".into());
        }
        self.launcher.fast_verify()?;
        self.native.fast_verify()?;
        if let Some(account) = &self.account {
            account
                .check()
                .map_err(|_| "The Codex account changed. Select it again in Settings > Models.")?;
        }
        Ok(())
    }
    pub(crate) fn verify(&self) -> Result<(), String> {
        if self
            .launcher_path
            .canonicalize()
            .map_err(|_| "Codex launcher changed")?
            != self.launcher.path
        {
            return Err("Codex launcher changed".into());
        }
        self.launcher.verify()?;
        if self.native.path != self.launcher.path {
            self.native.verify()?;
        }
        if let Some(account) = &self.account {
            account.check().map_err(|_| "The Codex account changed")?;
        }
        if blake3::hash(
            &std::fs::read(self.directory.join("models.json"))
                .map_err(|_| "Codex isolation metadata is unavailable")?,
        ) != self.catalog_hash
        {
            return Err("Codex changed. Refresh Models in Settings and try again.".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fixture(binary: &Path) -> Arc<Self> {
        let directory = tempfile::tempdir().unwrap().keep();
        let home = directory.join("state");
        private_directory(&home).unwrap();
        let catalog = b"{\"models\":[]}";
        private_write(&directory.join("models.json"), catalog).unwrap();
        Arc::new(Self {
            launcher_path: binary.to_path_buf(),
            launcher: FileIdentity::read(binary).unwrap(),
            native: FileIdentity::read(binary).unwrap(),
            account: None,
            home,
            model: "fixture-model".into(),
            directory,
            catalog_hash: blake3::hash(catalog),
            account_fingerprint: account_fingerprint(&json!({"account":{"type":"chatgpt","email":"fixture@example.test"},"workspaceRouting":{"chatgptAccountId":"fixture-account"}})).unwrap(),
            stamp: json!({"contract":"test-qualified"}),
        })
    }
    #[cfg(test)]
    pub(crate) fn refresh_fixture_identity(&mut self) {
        self.launcher = FileIdentity::read(&self.launcher_path).unwrap();
        self.native = self.launcher.clone();
    }
    #[cfg(test)]
    pub(crate) fn fixture_configuration(&self) -> Value {
        let mut features = serde_json::Map::new();
        for line in FEATURES.lines() {
            let name = line.split_whitespace().next().unwrap();
            features.insert(name.into(), json!(name == "skip_host_skill_discovery"));
        }
        json!({"config":{"features":features,"tools":{"update_plan":{"enabled":false},"experimental_request_user_input":{"enabled":false}},"web_search":"disabled","skills":{"include_instructions":false},"plugins":{"_default":{"enabled":false}},"project_doc_max_bytes":0,"model_catalog_json":self.directory.join("models.json")},"layers":[{"name":{"type":"sessionFlags"},"config":{"tools":{"update_plan":{"enabled":false},"experimental_request_user_input":{"enabled":false}}}}],"origins":{"tools.update_plan.enabled":{"name":{"type":"sessionFlags"}},"tools.experimental_request_user_input.enabled":{"name":{"type":"sessionFlags"}}}})
    }

    pub(crate) async fn verify_account(&self) -> Result<(), String> {
        self.verify()?;
        let mut app = CodexApp::start_restricted_catalog(
            self.native(),
            env!("CARGO_PKG_VERSION"),
            self.account.as_ref(),
            &self.flags(),
            self.directory(),
        )
        .await
        .map_err(|_| "Could not check the current Codex account")?;
        let fingerprint = app.account_fingerprint().await;
        app.stop().await;
        self.matches_account(fingerprint.map_err(|_| "Could not check the current Codex account")?)
    }
    pub(crate) fn account_revision(&self) -> u64 {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&self.account_fingerprint.as_bytes()[..8]);
        u64::from_le_bytes(bytes).max(1)
    }
    pub(crate) fn matches_account(&self, fingerprint: blake3::Hash) -> Result<(), String> {
        if self.account_fingerprint != fingerprint {
            return Err(
                "The Codex account changed. Review this automation again in Settings > Models"
                    .into(),
            );
        }
        Ok(())
    }
    pub(crate) fn native(&self) -> &Path {
        &self.native.path
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn flags(&self) -> Vec<String> {
        let mut flags = startup_flags();
        flags.extend([
            "-c".into(),
            format!(
                "model_catalog_json={}",
                json!(self.directory.join("models.json"))
            ),
        ]);
        flags
    }
    pub(crate) fn verify_configuration(&self, response: &Value) -> Result<(), String> {
        verify_startup_configuration(response)?;
        if response["config"]["model_catalog_json"] != json!(self.directory.join("models.json")) {
            return Err("Codex did not apply its isolated model metadata".into());
        }
        self.fast_verify()
    }
    fn thread_path(&self, id: &str) -> Result<PathBuf, String> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("Start a new chat to use this model for protected work.".into());
        }
        Ok(self
            .home
            .join("ark-host-threads")
            .join(format!("{id}.json")))
    }
    pub(crate) fn remember(&self, id: &str) -> Result<(), String> {
        let path = self.thread_path(id)?;
        private_directory(path.parent().ok_or("Invalid Codex conversation path")?)?;
        private_write(
            &path,
            &serde_json::to_vec(&self.stamp)
                .map_err(|_| "Start a new chat to use this model for protected work.")?,
        )
    }
    pub(crate) fn forget(&self, id: &str) -> Result<(), String> {
        self.owns(id)?;
        std::fs::remove_file(self.thread_path(id)?)
            .map_err(|_| "Could not remove the owned Codex conversation receipt".into())
    }
    pub(crate) fn owns(&self, id: &str) -> Result<(), String> {
        let path = self.thread_path(id)?;
        let stamp: Value = serde_json::from_slice(
            &std::fs::read(path)
                .map_err(|_| "Start a new chat to use this model for protected work.")?,
        )
        .map_err(|_| "Start a new chat to use this model for protected work.")?;
        if stamp != self.stamp {
            return Err("Start a new chat to use this model for protected work.".into());
        }
        Ok(())
    }
}

pub(crate) fn account_fingerprint(response: &Value) -> Result<blake3::Hash, String> {
    let account = &response["account"];
    if account["type"] != "chatgpt" {
        return Err("Sign in to Codex with ChatGPT in Settings > Models to use automations".into());
    }
    let email = account["email"]
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or("Codex did not identify its current account")?;
    let account_id = response["workspaceRouting"]["chatgptAccountId"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or("Codex did not identify its current account")?;
    let identity =
        serde_json::to_vec(&json!({"type":"chatgpt","email":email,"accountId":account_id}))
            .map_err(|_| "Codex did not identify its current account")?;
    Ok(blake3::hash(&identity))
}

fn tool_free_metadata(mut model: Value) -> Value {
    model["apply_patch_tool_type"] = Value::Null;
    model["experimental_supported_tools"] = json!([]);
    model["shell_type"] = json!("disabled");
    model["supports_search_tool"] = json!(false);
    model["tool_mode"] = json!("standard");
    model["multi_agent_version"] = Value::Null;
    model
}

pub(crate) fn verify_startup_configuration(response: &Value) -> Result<(), String> {
    let config = &response["config"];
    for line in FEATURES.lines().filter(|line| !line.contains(" removed")) {
        let feature = line.split_whitespace().next().expect("audited feature");
        if config["features"][feature].as_bool() != Some(feature == "skip_host_skill_discovery") {
            return Err(format!(
                "Codex did not apply tool isolation feature {feature}"
            ));
        }
    }
    let flags = response["layers"]
        .as_array()
        .and_then(|layers| {
            layers
                .iter()
                .find(|layer| layer["name"]["type"] == "sessionFlags")
        })
        .ok_or("Codex did not return its isolation configuration receipt")?;
    for key in ["update_plan", "experimental_request_user_input"] {
        if flags["config"]["tools"][key]["enabled"] != false
            || response["origins"][format!("tools.{key}.enabled")]["name"]["type"] != "sessionFlags"
        {
            return Err("Codex did not apply its tool isolation settings".into());
        }
    }
    if config["web_search"] != "disabled"
        || config["skills"]["include_instructions"] != false
        || config["plugins"]["_default"]["enabled"] != false
        || config["project_doc_max_bytes"] != 0
    {
        return Err("Codex did not apply its tool isolation settings".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_isolation_preserves_context_reasoning_modalities_and_model_messages() {
        let original = json!({"slug":"future-advertised-model","context_window":512000,"input_modalities":["text","image"],"default_reasoning_level":"medium","supported_reasoning_levels":[{"effort":"medium","description":"Measured"}],"model_messages":{"base_instructions":"provider prompt","context_management":{"preserve":true}},"unknown_non_tool_metadata":{"preserve":true},"apply_patch_tool_type":"freeform","experimental_supported_tools":["clock"],"shell_type":"shell_command","supports_search_tool":true,"tool_mode":"code_mode_only","multi_agent_version":"v2"});
        let sanitized = tool_free_metadata(original.clone());
        for key in [
            "slug",
            "context_window",
            "input_modalities",
            "default_reasoning_level",
            "supported_reasoning_levels",
            "model_messages",
            "unknown_non_tool_metadata",
        ] {
            assert_eq!(sanitized[key], original[key], "changed {key}");
        }
        assert!(sanitized["apply_patch_tool_type"].is_null());
        assert_eq!(sanitized["tool_mode"], "standard");
        assert!(sanitized["multi_agent_version"].is_null());
    }

    #[test]
    fn protected_identity_requires_a_chatgpt_account_and_workspace() {
        assert!(account_fingerprint(&json!({"account":{"type":"apiKey"}})).is_err());
        assert!(account_fingerprint(
            &json!({"account":{"type":"chatgpt","email":"fixture@example.test"}})
        )
        .is_err());
        let one = json!({"account":{"type":"chatgpt","email":"fixture@example.test"},"workspaceRouting":{"chatgptAccountId":"one"}});
        let mut two = one.clone();
        two["workspaceRouting"]["chatgptAccountId"] = json!("two");
        assert_ne!(
            account_fingerprint(&one).unwrap(),
            account_fingerprint(&two).unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restricted_processes_inherit_only_the_explicit_environment() {
        let output = restricted_command(Path::new("/usr/bin/env"), None)
            .unwrap()
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let allowed = [
            "HOME",
            "CODEX_HOME",
            "PATH",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "SYSTEMROOT",
            "WINDIR",
        ];
        for line in String::from_utf8(output.stdout).unwrap().lines() {
            let name = line.split_once('=').unwrap().0;
            assert!(
                allowed.contains(&name),
                "inherited unapproved variable {name}"
            );
        }
    }

    #[tokio::test]
    async fn cancelling_qualification_removes_the_private_directory() {
        let directory = tempfile::tempdir().unwrap().keep();
        let owned = directory.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let guard = QualificationDirectory(Some(owned));
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
            drop(guard);
        });
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!directory.exists());
    }

    #[test]
    fn unrecognized_launchers_never_gain_protected_tool_control() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("codex");
        std::fs::write(&script, "#!/bin/sh\necho codex-cli 0.166.0\n").unwrap();
        assert!(native_binary(&script).is_err());
    }

    #[test]
    fn executable_drift_invalidates_the_qualification_identity() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("codex");
        std::fs::write(&binary, "first native executable").unwrap();
        let identity = FileIdentity::read(&binary).unwrap();
        std::fs::write(&binary, "other native executable").unwrap();
        assert!(identity.verify().is_err());
    }

    #[test]
    fn qualification_rejects_missing_configuration_and_enabled_native_features() {
        assert!(verify_startup_configuration(&json!({})).is_err());
        let mut features = serde_json::Map::new();
        for line in FEATURES.lines() {
            let name = line.split_whitespace().next().unwrap();
            features.insert(name.into(), json!(name == "skip_host_skill_discovery"));
        }
        let mut response = json!({"config":{"features":features,"tools":{"update_plan":{"enabled":false},"experimental_request_user_input":{"enabled":false}},"web_search":"disabled","skills":{"include_instructions":false},"plugins":{"_default":{"enabled":false}},"project_doc_max_bytes":0},"layers":[{"name":{"type":"sessionFlags"},"config":{"tools":{"update_plan":{"enabled":false},"experimental_request_user_input":{"enabled":false}}}}],"origins":{"tools.update_plan.enabled":{"name":{"type":"sessionFlags"}},"tools.experimental_request_user_input.enabled":{"name":{"type":"sessionFlags"}}}});
        assert!(verify_startup_configuration(&response).is_ok());
        response["config"]["features"]["hooks"] = json!(true);
        assert!(verify_startup_configuration(&response).is_err());
    }

    #[test]
    #[ignore = "requires installed official Codex and current account metadata; makes no inference"]
    fn installed_codex_warm_capability_checks_are_cached() {
        let start = std::time::Instant::now();
        let provider = crate::build("codex", Some("gpt-6.1-sol@medium".into())).unwrap();
        assert!(provider.host_controls_tools());
        let cold = start.elapsed().as_millis();
        let mut warm = Vec::new();
        for _ in 0..4 {
            let start = std::time::Instant::now();
            assert!(provider.host_controls_tools());
            warm.push(start.elapsed().as_micros());
        }
        println!(
            "{}",
            json!({"cold_build_and_qualification_ms":cold,"warm_capability_us":warm,"inference_calls":0})
        );
    }

    #[tokio::test]
    #[ignore = "requires the installed official Codex and current account metadata; makes no inference"]
    async fn installed_codex_qualifies_without_a_model_turn() {
        let bin = crate::Cli::Codex.locate().expect("installed Codex");
        let host = CodexHost::qualify(bin, None, "gpt-6.1-sol".into())
            .await
            .unwrap_or_else(|error| panic!("qualification: {error}"));
        host.verify().unwrap();
        assert!(host.owns("00000000-0000-4000-8000-000000000000").is_err());
    }
}
