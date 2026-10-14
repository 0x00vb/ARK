//! Signed updates for the native window's JavaScript, CSS and image assets (ADR 0067).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tauri::Manager as _;

use crate::assets::{Assets, ReleasedAssets};

const MAX_MANIFEST: usize = 512 * 1024;
const MAX_ARCHIVE: usize = 32 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 64 * 1024 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 512;

#[derive(Clone)]
pub struct Config {
    pub endpoint: String,
    /// Base64 of the Minisign public-key text, kept separate from the native updater key.
    pub public_key: String,
    pub cache: PathBuf,
    pub ark_version: String,
    pub app_version: String,
    pub asset_contract: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Latest {
    manifest: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Manifest {
    schema: u32,
    kind: String,
    sequence: u64,
    revision: String,
    ark_native: String,
    ark_assets: String,
    app_native: String,
    app_source: String,
    asset_contract: u32,
    created_at: String,
    bundle_url: String,
    bundle_sha256: String,
    bundle_size: u64,
    file_count: usize,
    files: Vec<AssetFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AssetFile {
    path: String,
    sha256: String,
    size: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Status {
    Idle { current: String },
    Checking { current: String },
    Current { current: String },
    Downloading { current: String, version: String },
    Ready { current: String, version: String },
    Applying { current: String, version: String },
    Applied { current: String },
    RolledBack { current: String, failed: String },
    Failed { current: String, kind: Failure },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    Check,
    Download,
    Verification,
}

struct State {
    status: Status,
    listener: Option<tauri::ipc::Channel<serde_json::Value>>,
    checking: bool,
    awaiting: Option<String>,
}

pub(crate) struct PageUpdates {
    config: Config,
    assets: Arc<Assets>,
    state: Mutex<State>,
}

static INSTANCE: std::sync::OnceLock<Arc<PageUpdates>> = std::sync::OnceLock::new();

fn revision(active: Option<&ReleasedAssets>) -> String {
    active.map_or_else(|| "embedded".into(), |release| release.revision.clone())
}

pub(crate) fn active(config: &Config) -> Option<ReleasedAssets> {
    match active_checked(config) {
        Ok(release) => release,
        Err(error) => {
            eprintln!("saved page update rejected: {error:#}");
            None
        }
    }
}

fn active_checked(config: &Config) -> anyhow::Result<Option<ReleasedAssets>> {
    let mut root = config.cache.join("active");
    if config.cache.join("pending").exists() {
        let pending = read_pending(&config.cache)?;
        let current = read_saved(config, &root).ok();
        let must_restore = pending_requires_restore(
            root.exists(),
            current.as_ref().map(|value| value.revision.as_str()),
            &pending,
        );
        let previous = config.cache.join("previous");
        if must_restore {
            let rejected = config.cache.join("rejected-startup");
            if rejected.exists() {
                fs::remove_dir_all(&rejected)?;
            }
            if root.exists() {
                fs::rename(&root, &rejected)?;
            }
            if previous.exists() {
                read_saved(config, &previous)?;
                fs::rename(&previous, &root)?;
            }
        }
        clear_pending(&config.cache)?;
    }
    let mut manifest = read_saved(config, &root).ok();
    if manifest.is_none() {
        let previous = config.cache.join("previous");
        if let Ok(value) = read_saved(config, &previous) {
            let rejected = config.cache.join("rejected-startup");
            if rejected.exists() {
                let _ = fs::remove_dir_all(&rejected);
            }
            if root.exists() {
                let _ = fs::rename(&root, &rejected);
            }
            if fs::rename(&previous, &root).is_ok() {
                manifest = Some(value);
            } else {
                root = previous;
                manifest = Some(value);
            }
        }
    }
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let high = read_high_water(&config.cache)?;
    if manifest.sequence > high {
        write_high_water(&config.cache, manifest.sequence)?;
    }
    Ok(Some(ReleasedAssets {
        root,
        revision: manifest.revision,
    }))
}

fn pending_requires_restore(
    active_exists: bool,
    active_revision: Option<&str>,
    pending: &str,
) -> bool {
    !active_exists || active_revision.is_none() || active_revision == Some(pending)
}

pub(crate) fn start(app: tauri::AppHandle, assets: Arc<Assets>, config: Config) {
    let current = assets.release_revision();
    let updater = Arc::new(PageUpdates {
        config,
        assets,
        state: Mutex::new(State {
            status: Status::Idle { current },
            listener: None,
            checking: false,
            awaiting: None,
        }),
    });
    if INSTANCE.set(updater.clone()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        updater.check().await;
        let _ = app;
    });
}

pub(crate) fn status() -> Option<Status> {
    Some(INSTANCE.get()?.state.lock().unwrap().status.clone())
}

pub(crate) fn listen(channel: tauri::ipc::Channel<serde_json::Value>) {
    let Some(updater) = INSTANCE.get() else {
        return;
    };
    let mut state = updater.state.lock().unwrap();
    if let Ok(value) = serde_json::to_value(&state.status) {
        let _ = channel.send(value);
    }
    state.listener = Some(channel);
}

fn set(updater: &PageUpdates, status: Status) {
    let mut state = updater.state.lock().unwrap();
    state.status = status.clone();
    if let (Some(listener), Ok(value)) = (&state.listener, serde_json::to_value(status)) {
        let _ = listener.send(value);
    }
}

pub(crate) async fn check_now() -> Option<Status> {
    let updater = INSTANCE.get()?.clone();
    updater.check().await;
    status()
}

impl PageUpdates {
    async fn check(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap();
            if state.checking {
                return;
            }
            state.checking = true;
        }
        let current = self.assets.release_revision();
        set(
            self,
            Status::Checking {
                current: current.clone(),
            },
        );
        let result = self.fetch_and_stage().await;
        self.state.lock().unwrap().checking = false;
        match result {
            Ok(Some(version)) => set(self, Status::Ready { current, version }),
            Ok(None) => set(self, Status::Current { current }),
            Err(error) => {
                let words = error.to_string().to_lowercase();
                let kind = if words.contains("signature")
                    || words.contains("digest")
                    || words.contains("manifest")
                    || words.contains("archive")
                    || words.contains("path")
                {
                    Failure::Verification
                } else if words.contains("download") || words.contains("http") {
                    Failure::Download
                } else {
                    Failure::Check
                };
                eprintln!("page update failed: {error:#}");
                set(self, Status::Failed { current, kind });
            }
        }
    }

    async fn fetch_and_stage(&self) -> anyhow::Result<Option<String>> {
        https(&self.config.endpoint)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if https(attempt.url().as_str()).is_ok() {
                    attempt.follow()
                } else {
                    attempt.error("page update redirect requires a plain HTTPS address")
                }
            }))
            .build()?;
        let latest: Latest = serde_json::from_slice(
            &bounded_download(&client, &self.config.endpoint, MAX_MANIFEST).await?,
        )?;
        https(&latest.manifest)?;
        let manifest_bytes = bounded_download(&client, &latest.manifest, MAX_MANIFEST).await?;
        verify_signature(&self.config.public_key, &manifest_bytes, &latest.signature)?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        validate_manifest(&self.config, &manifest)?;
        let high = read_high_water(&self.config.cache)?;
        if manifest.sequence <= high {
            return Ok(None);
        }
        set(
            self,
            Status::Downloading {
                current: self.assets.release_revision(),
                version: manifest.revision.clone(),
            },
        );
        let archive = bounded_download(&client, &manifest.bundle_url, MAX_ARCHIVE).await?;
        if archive.len() as u64 != manifest.bundle_size
            || hex(&Sha256::digest(&archive)) != manifest.bundle_sha256
        {
            anyhow::bail!("the page archive digest or size does not match its signed manifest");
        }
        stage(
            &self.config,
            &manifest,
            &manifest_bytes,
            &latest.signature,
            &archive,
        )?;
        Ok(Some(manifest.revision))
    }
}

async fn bounded_download(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        anyhow::bail!("download is larger than {limit} bytes");
    }
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if out.len().saturating_add(chunk.len()) > limit {
            anyhow::bail!("download is larger than {limit} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn https(value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
    {
        anyhow::bail!("page updates require a plain HTTPS address");
    }
    Ok(())
}

fn verify_signature(key: &str, bytes: &[u8], encoded: &str) -> anyhow::Result<()> {
    let key = decode_text(key)?;
    let key = PublicKey::decode(&key)?;
    let signature = Signature::decode(&decode_text(encoded.trim())?)?;
    // Tauri's signer emits Minisign's direct Ed25519 form. The manifest is bounded to 512 KiB
    // before this call, so supporting that form does not expose an unbounded-message path.
    key.verify(bytes, &signature, true)?;
    Ok(())
}

fn decode_text(value: &str) -> anyhow::Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(value)?;
    Ok(String::from_utf8(bytes)?)
}

fn validate_manifest(config: &Config, manifest: &Manifest) -> anyhow::Result<()> {
    if manifest.schema != 1 || manifest.kind != "ark-page-assets" {
        anyhow::bail!("unsupported page manifest");
    }
    if manifest.ark_native != config.ark_version
        || manifest.app_native != config.app_version
        || manifest.asset_contract != config.asset_contract
    {
        anyhow::bail!("the page manifest is not compatible with this app");
    }
    if manifest.sequence == 0 || !safe_segment(&manifest.revision) {
        anyhow::bail!("invalid page release revision");
    }
    if manifest.bundle_size == 0 || manifest.bundle_size > MAX_ARCHIVE as u64 {
        anyhow::bail!("invalid page archive size");
    }
    https(&manifest.bundle_url)?;
    if manifest.bundle_sha256.len() != 64
        || !manifest
            .bundle_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        anyhow::bail!("invalid page archive digest");
    }
    if manifest.file_count != manifest.files.len()
        || manifest.file_count == 0
        || manifest.file_count > MAX_FILES
    {
        anyhow::bail!("invalid page file count");
    }
    let required = [
        "web/index.html",
        "web/js/app.js",
        "web/js/transport.js",
        "consumer/desktop.js",
    ];
    if required
        .iter()
        .any(|path| !manifest.files.iter().any(|file| file.path == *path))
    {
        anyhow::bail!("page manifest is missing a required startup asset");
    }
    let mut paths = HashSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        validate_path(&file.path)?;
        if !paths.insert(&file.path) || file.size > MAX_FILE {
            anyhow::bail!("duplicate or oversized page asset");
        }
        total = total
            .checked_add(file.size)
            .ok_or_else(|| anyhow::anyhow!("page asset size overflow"))?;
        if total > MAX_EXTRACTED
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            anyhow::bail!("invalid page asset digest or total size");
        }
    }
    Ok(())
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn validate_path(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 240 || !value.is_ascii() || value.contains('\\') {
        anyhow::bail!("invalid page asset path");
    }
    let path = Path::new(value);
    let normalized = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if normalized != value
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || !matches!(path.components().next(), Some(Component::Normal(root)) if root == "web" || root == "consumer")
    {
        anyhow::bail!("page asset path leaves its allowed roots");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_'))
    {
        anyhow::bail!("page asset path has unsupported characters");
    }
    Ok(())
}

fn stage(
    config: &Config,
    manifest: &Manifest,
    manifest_bytes: &[u8],
    signature: &str,
    archive: &[u8],
) -> anyhow::Result<()> {
    fs::create_dir_all(&config.cache)?;
    private_dir(&config.cache)?;
    let root = config.cache.join(format!(
        "stage-{}-{}",
        std::process::id(),
        manifest.sequence
    ));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir(&root)?;
    private_dir(&root)?;
    let _cleanup = StageCleanup(root.clone());
    let expected: HashMap<&str, &AssetFile> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut seen = HashSet::new();
    let mut tar = tar::Archive::new(Cursor::new(archive));
    for item in tar.entries()? {
        let mut item = item?;
        if !item.header().entry_type().is_file() {
            anyhow::bail!("page archive contains a non-file entry");
        }
        let path = item
            .path()?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("page archive path is not UTF-8"))?
            .to_string();
        validate_path(&path)?;
        let file = expected
            .get(path.as_str())
            .ok_or_else(|| anyhow::anyhow!("page archive contains an undeclared path"))?;
        if !seen.insert(path.clone()) || item.size() != file.size || item.size() > MAX_FILE {
            anyhow::bail!("page archive contains a duplicate or wrong-sized file");
        }
        let mut bytes = Vec::with_capacity(item.size() as usize);
        item.read_to_end(&mut bytes)?;
        if hex(&Sha256::digest(&bytes)) != file.sha256 {
            anyhow::bail!("page asset digest mismatch: {path}");
        }
        let target = root.join(&path);
        fs::create_dir_all(target.parent().unwrap())?;
        File::create(&target)?.write_all(&bytes)?;
    }
    if seen.len() != expected.len() {
        anyhow::bail!("page archive is missing declared files");
    }
    fs::write(root.join("manifest.json"), manifest_bytes)?;
    fs::write(root.join("manifest.sig"), signature.trim())?;
    File::open(&root)?.sync_all()?;
    let staged = config.cache.join("staged");
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    fs::rename(root, staged)?;
    File::open(&config.cache)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn private_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn read_saved(config: &Config, root: &Path) -> anyhow::Result<Manifest> {
    let bytes = fs::read(root.join("manifest.json"))?;
    if bytes.len() > MAX_MANIFEST {
        anyhow::bail!("saved page manifest is too large");
    }
    let signature = fs::read_to_string(root.join("manifest.sig"))?;
    verify_signature(&config.public_key, &bytes, signature.trim())?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    validate_manifest(config, &manifest)?;
    for file in &manifest.files {
        let bytes = fs::read(root.join(&file.path))?;
        if bytes.len() as u64 != file.size || hex(&Sha256::digest(&bytes)) != file.sha256 {
            anyhow::bail!("saved page asset failed verification");
        }
    }
    verify_file_set(root, &manifest)?;
    Ok(manifest)
}

struct StageCleanup(PathBuf);

impl Drop for StageCleanup {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn verify_file_set(root: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let expected: HashSet<PathBuf> = manifest
        .files
        .iter()
        .map(|file| PathBuf::from(&file.path))
        .chain([
            PathBuf::from("manifest.json"),
            PathBuf::from("manifest.sig"),
        ])
        .collect();
    let mut found = HashSet::new();
    collect_files(root, root, &mut found)?;
    if found != expected {
        anyhow::bail!("saved page release contains missing or undeclared files");
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            anyhow::bail!("saved page release contains a symbolic link");
        }
        if kind.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if kind.is_file() {
            files.insert(entry.path().strip_prefix(root)?.to_path_buf());
        } else {
            anyhow::bail!("saved page release contains an unsupported file");
        }
    }
    Ok(())
}

fn read_high_water(root: &Path) -> anyhow::Result<u64> {
    match fs::read_to_string(root.join("high-water")) {
        Ok(value) => value
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid page update high-water mark")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn apply(app: tauri::AppHandle) -> anyhow::Result<Option<Status>> {
    let Some(updater) = INSTANCE.get().cloned() else {
        return Ok(None);
    };
    let result = apply_staged(app, updater.clone());
    if result.is_err() {
        set(
            &updater,
            Status::Failed {
                current: updater.assets.release_revision(),
                kind: Failure::Verification,
            },
        );
    }
    result
}

fn apply_staged(
    app: tauri::AppHandle,
    updater: Arc<PageUpdates>,
) -> anyhow::Result<Option<Status>> {
    let staged = updater.config.cache.join("staged");
    let manifest = read_saved(&updater.config, &staged)?;
    let current = updater.assets.release_revision();
    set(
        &updater,
        Status::Applying {
            current,
            version: manifest.revision.clone(),
        },
    );
    write_pending(&updater.config.cache, &manifest.revision)?;
    let active_root = updater.config.cache.join("active");
    let previous = updater.config.cache.join("previous");
    if previous.exists() {
        if let Err(error) = fs::remove_dir_all(&previous) {
            let _ = clear_pending(&updater.config.cache);
            return Err(error.into());
        }
    }
    if active_root.exists() {
        if let Err(error) = fs::rename(&active_root, &previous) {
            let _ = clear_pending(&updater.config.cache);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(&staged, &active_root) {
        if previous.exists() {
            let _ = fs::rename(&previous, &active_root);
        }
        let _ = clear_pending(&updater.config.cache);
        return Err(error.into());
    }
    if let Err(error) = write_high_water(&updater.config.cache, manifest.sequence) {
        if let Err(restore) =
            restore_failed_swap(&updater.config.cache, &active_root, &staged, &previous)
        {
            return Err(anyhow::anyhow!(
                "high-water write failed: {error}; page swap remains pending: {restore}"
            ));
        }
        clear_pending(&updater.config.cache)?;
        return Err(error);
    }
    updater.assets.use_release(Some(ReleasedAssets {
        root: active_root,
        revision: manifest.revision.clone(),
    }));
    updater.state.lock().unwrap().awaiting = Some(manifest.revision.clone());
    if reload(&app, &updater.assets).is_err() {
        rollback(&app, &updater, &manifest.revision)?;
        return Ok(status());
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(7)).await;
        let awaiting = updater.state.lock().unwrap().awaiting.clone();
        if awaiting.as_deref() == Some(&manifest.revision) {
            let _ = rollback(&app, &updater, &manifest.revision);
        }
    });
    Ok(status())
}

fn restore_failed_swap(
    cache: &Path,
    active: &Path,
    staged: &Path,
    previous: &Path,
) -> anyhow::Result<()> {
    fs::rename(active, staged)?;
    if previous.exists() {
        fs::rename(previous, active)?;
    }
    File::open(cache)?.sync_all()?;
    Ok(())
}

fn write_high_water(root: &Path, sequence: u64) -> anyhow::Result<()> {
    let temporary = root.join("high-water.new");
    fs::write(&temporary, format!("{sequence}\n"))?;
    File::open(&temporary)?.sync_all()?;
    fs::rename(temporary, root.join("high-water"))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn write_pending(root: &Path, revision: &str) -> anyhow::Result<()> {
    if !safe_segment(revision) {
        anyhow::bail!("invalid pending page revision");
    }
    let temporary = root.join("pending.new");
    fs::write(&temporary, format!("{revision}\n"))?;
    File::open(&temporary)?.sync_all()?;
    fs::rename(temporary, root.join("pending"))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn read_pending(root: &Path) -> anyhow::Result<String> {
    let revision = fs::read_to_string(root.join("pending"))?;
    let revision = revision.trim();
    if !safe_segment(revision) {
        anyhow::bail!("invalid pending page revision");
    }
    Ok(revision.to_string())
}

fn clear_pending(root: &Path) -> anyhow::Result<()> {
    let pending = root.join("pending");
    match fs::remove_file(pending) {
        Ok(()) => {
            File::open(root)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn ready(revision: &str) -> anyhow::Result<Option<Status>> {
    let Some(updater) = INSTANCE.get() else {
        return Ok(None);
    };
    let mut state = updater.state.lock().unwrap();
    if state.awaiting.as_deref() == Some(revision) {
        clear_pending(&updater.config.cache)?;
        state.awaiting = None;
        drop(state);
        set(
            updater,
            Status::Applied {
                current: revision.to_string(),
            },
        );
    }
    Ok(status())
}

fn rollback(app: &tauri::AppHandle, updater: &PageUpdates, failed: &str) -> anyhow::Result<()> {
    let active_root = updater.config.cache.join("active");
    let previous = updater.config.cache.join("previous");
    let rejected = updater.config.cache.join(format!("rejected-{failed}"));
    if rejected.exists() {
        fs::remove_dir_all(&rejected)?;
    }
    if active_root.exists() {
        fs::rename(&active_root, &rejected)?;
    }
    clear_pending(&updater.config.cache)?;
    let release = if previous.exists() {
        fs::rename(&previous, &active_root)?;
        active(&updater.config)
    } else {
        None
    };
    updater.assets.use_release(release.clone());
    updater.state.lock().unwrap().awaiting = None;
    set(
        updater,
        Status::RolledBack {
            current: revision(release.as_ref()),
            failed: failed.to_string(),
        },
    );
    let _ = reload(app, &updater.assets);
    Ok(())
}

fn reload(app: &tauri::AppHandle, assets: &Assets) -> anyhow::Result<()> {
    let webview = app
        .get_webview("main")
        .ok_or_else(|| anyhow::anyhow!("the main window is unavailable"))?;
    let url = tauri::Url::parse(&format!(
        "tauri://localhost/{}",
        assets.versioned("index.html")
    ))?;
    webview.navigate(url)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &Path) -> Config {
        Config {
            endpoint: "https://download.example/latest.json".into(),
            public_key: "unused".into(),
            cache: root.into(),
            ark_version: "0.23.1".into(),
            app_version: "0.8.2".into(),
            asset_contract: 1,
        }
    }

    fn manifest(path: &str) -> Manifest {
        let mut files: Vec<AssetFile> = [
            "web/index.html",
            "web/js/app.js",
            "web/js/transport.js",
            "consumer/desktop.js",
        ]
        .into_iter()
        .map(|path| AssetFile {
            path: path.into(),
            sha256: "b".repeat(64),
            size: 1,
        })
        .collect();
        if !files.iter().any(|file| file.path == path) {
            files.push(AssetFile {
                path: path.into(),
                sha256: "b".repeat(64),
                size: 1,
            });
        }
        Manifest {
            schema: 1,
            kind: "ark-page-assets".into(),
            sequence: 2,
            revision: "ui.2.abc".into(),
            ark_native: "0.23.1".into(),
            ark_assets: "v0.23.2".into(),
            app_native: "0.8.2".into(),
            app_source: "abc".into(),
            asset_contract: 1,
            created_at: "2026-10-14T12:00:00Z".into(),
            bundle_url: "https://download.example/ui.2.abc.tar".into(),
            bundle_sha256: "a".repeat(64),
            bundle_size: 1,
            file_count: files.len(),
            files,
        }
    }

    #[test]
    fn manifest_requires_exact_native_contract() {
        let root = tempfile::tempdir().unwrap();
        let c = config(root.path());
        assert!(validate_manifest(&c, &manifest("web/app.css")).is_ok());
        let mut wrong = manifest("web/app.css");
        wrong.app_native = "0.8.1".into();
        assert!(validate_manifest(&c, &wrong).is_err());
    }

    #[test]
    fn archive_paths_are_ascii_relative_and_confined() {
        for good in ["web/app.css", "web/js/app.js", "consumer/fonts/inter.woff2"] {
            assert!(validate_path(good).is_ok(), "{good}");
        }
        for bad in [
            "/web/app.css",
            "web/../secret",
            "web//app.js",
            "web/./app.js",
            "other/app.js",
            "web/a b.js",
            "web/á.js",
            "web\\app.js",
        ] {
            assert!(validate_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_refuses_duplicates_and_oversized_files() {
        let root = tempfile::tempdir().unwrap();
        let c = config(root.path());
        let mut duplicate = manifest("web/app.css");
        duplicate.files.push(duplicate.files[0].clone());
        duplicate.file_count = duplicate.files.len();
        assert!(validate_manifest(&c, &duplicate).is_err());
        let mut large = manifest("web/app.css");
        large.files[0].size = MAX_FILE + 1;
        assert!(validate_manifest(&c, &large).is_err());
    }

    #[test]
    fn saved_release_refuses_undeclared_files() {
        let root = tempfile::tempdir().unwrap();
        let value = manifest("web/app.css");
        for file in &value.files {
            let path = root.path().join(&file.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        fs::write(root.path().join("manifest.json"), b"{}").unwrap();
        fs::write(root.path().join("manifest.sig"), b"signature").unwrap();
        assert!(verify_file_set(root.path(), &value).is_ok());
        fs::write(root.path().join("web/extra.js"), b"x").unwrap();
        assert!(verify_file_set(root.path(), &value).is_err());
    }

    #[test]
    fn rollback_markers_are_durable_and_malformed_high_water_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        write_pending(root.path(), "ui.2.abc").unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("pending")).unwrap(),
            "ui.2.abc\n"
        );
        clear_pending(root.path()).unwrap();
        assert!(!root.path().join("pending").exists());
        fs::write(root.path().join("high-water"), "not-a-number\n").unwrap();
        assert!(read_high_water(root.path()).is_err());
    }

    #[test]
    fn every_pending_swap_phase_recovers_the_safe_bundle() {
        assert!(!pending_requires_restore(
            true,
            Some("ui.1.old"),
            "ui.2.new"
        ));
        assert!(pending_requires_restore(false, None, "ui.2.new"));
        assert!(pending_requires_restore(true, None, "ui.2.new"));
        assert!(pending_requires_restore(true, Some("ui.2.new"), "ui.2.new"));
    }

    #[test]
    fn failed_swap_restore_keeps_the_pending_marker() {
        let root = tempfile::tempdir().unwrap();
        let active = root.path().join("active");
        let staged = root.path().join("staged");
        let previous = root.path().join("previous");
        fs::create_dir(&active).unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("occupied"), b"x").unwrap();
        write_pending(root.path(), "ui.2.new").unwrap();
        assert!(restore_failed_swap(root.path(), &active, &staged, &previous).is_err());
        assert!(root.path().join("pending").exists());
        assert!(active.exists());
    }
}
