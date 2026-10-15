//! Signed updates for a packaged native app.
//!
//! The consumer enables `ark-ui/desktop-updater` and supplies Tauri's updater endpoint and
//! public key. On macOS and Linux, ARK checks, downloads and installs in the background. It
//! never restarts the app: work keeps running, and the installed version opens after the person
//! next quits ARK. Windows stays disabled because Tauri must exit to launch its installer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
use std::fs::OpenOptions;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

/// What a consumer can show. Errors leave the running version untouched.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Idle {
        current: String,
    },
    Checking {
        current: String,
    },
    Current {
        current: String,
    },
    Downloading {
        current: String,
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    Installing {
        current: String,
        version: String,
    },
    Ready {
        current: String,
        version: String,
    },
    Failed {
        current: String,
        kind: Failure,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Check,
    Download,
    Verification,
    Install,
}

impl Default for Status {
    fn default() -> Self {
        Self::Idle {
            current: String::new(),
        }
    }
}

static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| Mutex::new(Status::default()));
static LISTENER: LazyLock<Mutex<Option<tauri::ipc::Channel<serde_json::Value>>>> =
    LazyLock::new(|| Mutex::new(None));
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The last known update state. It is process-local and contains no credentials.
pub(crate) fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

/// A native restart is available only after a verified update replaced the installed bundle.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub(crate) fn restart_ready() -> bool {
    matches!(status(), Status::Ready { .. })
}

fn set(status: Status) {
    *STATUS.lock().unwrap() = status.clone();
    if let Ok(value) = serde_json::to_value(status) {
        if let Some(channel) = LISTENER.lock().unwrap().as_ref() {
            let _ = channel.send(value);
        }
    }
}

/// Replace the page-load listener and immediately send the current state.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub(crate) fn listen(channel: tauri::ipc::Channel<serde_json::Value>) {
    let value = serde_json::to_value(status()).unwrap_or_default();
    let _ = channel.send(value);
    *LISTENER.lock().unwrap() = Some(channel);
}

fn current(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

#[cfg_attr(debug_assertions, allow(dead_code))]
fn busy() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

struct Operation;

impl Operation {
    fn claim() -> Option<Self> {
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

/// Start the quiet launch check. A short delay keeps it off the first-paint path.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub(crate) fn start(app: AppHandle) {
    set(Status::Idle {
        current: current(&app),
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        if matches!(status(), Status::Idle { .. }) {
            check_and_install(app).await;
        }
    });
}

/// Check now, for a consumer's Settings action. A launch check already in progress is reused.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub(crate) async fn check_now() -> anyhow::Result<Status> {
    if busy() {
        return Ok(status());
    }
    let app = crate::desktop::APP
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("the native window is not running"))?;
    check_and_install(app).await;
    Ok(status())
}

async fn check_and_install(app: AppHandle) {
    let Some(_operation) = Operation::claim() else {
        return;
    };
    let installed = current(&app);
    // A disk image looks like a normal app bundle, but macOS mounts it read-only. Tauri's
    // replacement happens after the download, so reject that location before touching an update.
    // The app keeps running and the person can copy it to Applications first.
    #[cfg(target_os = "macos")]
    if let Err(error) = replacement_location() {
        set(Status::Failed {
            current: installed,
            kind: Failure::Install,
        });
        tracing::warn!(%error, "native update needs a writable app location");
        return;
    }
    set(Status::Checking {
        current: installed.clone(),
    });
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            set(Status::Failed {
                current: installed,
                kind: Failure::Check,
            });
            tracing::warn!(%error, "native update check failed");
            return;
        }
    };
    let update = match updater.check().await {
        Ok(update) => update,
        Err(error) => {
            set(Status::Failed {
                current: installed,
                kind: Failure::Check,
            });
            tracing::warn!(%error, "native update check failed");
            return;
        }
    };
    let Some(update) = update else {
        set(Status::Current { current: installed });
        return;
    };
    // Tauri's Windows installer restarts by default. ARK never interrupts work. On every
    // platform the installed version is used only after the person next quits and opens it.
    let update = update.restart_after_install(false);

    let version = update.version.clone();
    set(Status::Downloading {
        current: installed.clone(),
        version: version.clone(),
        downloaded: 0,
        total: None,
    });
    let progress_current = installed.clone();
    let progress_version = version.clone();
    let install_current = installed.clone();
    let install_version = version.clone();
    let mut downloaded = 0u64;
    if let Err(error) = update
        .download_and_install(
            move |chunk, total| {
                downloaded = downloaded.saturating_add(chunk as u64);
                set(Status::Downloading {
                    current: progress_current.clone(),
                    version: progress_version.clone(),
                    downloaded,
                    total,
                });
            },
            move || {
                set(Status::Installing {
                    current: install_current,
                    version: install_version,
                });
            },
        )
        .await
    {
        let words = error.to_string().to_lowercase();
        let kind = if words.contains("signature") || words.contains("verif") {
            Failure::Verification
        } else if matches!(status(), Status::Installing { .. }) {
            Failure::Install
        } else {
            Failure::Download
        };
        set(Status::Failed {
            current: installed,
            kind,
        });
        tracing::error!(%error, "native update was not installed");
        return;
    }

    set(Status::Ready {
        current: installed,
        version: version.clone(),
    });
    tracing::info!(%version, "native update is ready for restart");
    let _ = app
        .notification()
        .builder()
        .title(format!("ARK {version} is ready"))
        .body("It will be used after you quit and open ARK.")
        .show();
}

/// Confirm that the installed `.app` bundle can be replaced before the updater downloads a
/// package. A metadata-only check is not enough: a mounted disk image reports ordinary paths
/// while refusing writes.
#[cfg(target_os = "macos")]
fn replacement_location() -> std::io::Result<()> {
    replacement_location_for(&std::env::current_exe()?)
}

#[cfg(target_os = "macos")]
fn replacement_location_for(executable: &std::path::Path) -> std::io::Result<()> {
    let bundle = executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or_else(|| std::io::Error::other("the app bundle was not found"))?;
    let folder = bundle
        .parent()
        .ok_or_else(|| std::io::Error::other("the app bundle has no enclosing folder"))?;
    let probe = folder.join(format!(".ark-update-{}", std::process::id()));
    let _file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    drop(_file);
    std::fs::remove_file(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_busy_but_a_ready_update_is_not() {
        set(Status::Current {
            current: "1.0.0".into(),
        });
        assert!(!restart_ready());
        set(Status::Downloading {
            current: "1.0.0".into(),
            version: "1.1.0".into(),
            downloaded: 1,
            total: Some(2),
        });
        ACTIVE.store(true, Ordering::Release);
        assert!(busy());
        ACTIVE.store(false, Ordering::Release);
        set(Status::Ready {
            current: "1.0.0".into(),
            version: "1.1.0".into(),
        });
        assert!(!busy());
        assert!(restart_ready());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_replacement_probe_needs_an_app_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("ark-desktop");
        let error = replacement_location_for(&executable).unwrap_err();
        assert!(error.to_string().contains("app bundle"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_replacement_probe_accepts_a_writable_bundle_folder() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("ARK.app/Contents/MacOS/ark-desktop");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        replacement_location_for(&executable).unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
