//! Python, the first runtime: one environment ARK keeps with `uv`, the same on every machine.
//! The first run makes it (`uv` fetches Python and the packages, outside the sandbox: ARK's own
//! step, not the agent's code); later runs only start it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ark_tools::subprocess::{self, Spec};
use async_trait::async_trait;

use crate::{Launch, Runtime};

/// The Python version, and the packages with exact versions, so a program that ran once runs
/// the same later and elsewhere. Changing either makes the environment again.
const VERSION: &str = "3.12";
const PACKAGES: &[&str] = &[
    "pandas==3.0.6",
    "numpy==2.5.3",
    "matplotlib==3.11.2",
    "openpyxl==3.1.5",
];

/// Longest a first-run install may take (downloads included).
const INSTALL: Duration = Duration::from_secs(600);

/// Python through `uv`, kept in `home`.
pub struct Python {
    home: PathBuf,
    uv: Option<PathBuf>,
    /// One install at a time: two agents asking at once wait for the same environment.
    lock: tokio::sync::Mutex<()>,
}

impl Python {
    /// The environment lives in `home` (made when needed). `uv` is looked up on PATH and in
    /// its usual install folders.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let uv = subprocess::which("uv").or_else(|| {
            let user = std::env::var_os("HOME").map(PathBuf::from)?;
            [".local/bin/uv", ".cargo/bin/uv"]
                .iter()
                .map(|p| user.join(p))
                .find(|p| p.is_file())
        });
        Python {
            home: home.into(),
            uv,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Use this `uv` (an app that ships its own).
    pub fn with_uv(mut self, uv: impl Into<PathBuf>) -> Self {
        self.uv = Some(uv.into());
        self
    }

    fn venv(&self) -> PathBuf {
        self.home.join("venv")
    }

    fn stamp(&self) -> PathBuf {
        self.home.join("ready")
    }

    fn wanted() -> String {
        format!("python {VERSION}\n{}\n", PACKAGES.join("\n"))
    }

    fn is_ready(&self) -> bool {
        self.venv().join("bin/python").exists()
            && std::fs::read_to_string(self.stamp()).ok().as_deref() == Some(&Self::wanted())
    }

    async fn install(&self, uv: &Path) -> Result<(), String> {
        std::fs::create_dir_all(&self.home).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(self.stamp());
        let venv = self.venv().display().to_string();
        let step = |args: Vec<String>| {
            let mut spec = Spec::new(uv);
            spec.args = args;
            spec.timeout = Some(INSTALL);
            spec.keep = Some(4000);
            spec
        };
        let steps = [
            step(vec![
                "venv".into(),
                "--clear".into(),
                "--managed-python".into(),
                "--python".into(),
                VERSION.into(),
                venv.clone(),
            ]),
            step(
                ["pip", "install", "--python", &format!("{venv}/bin/python")]
                    .iter()
                    .map(|s| s.to_string())
                    .chain(PACKAGES.iter().map(|s| s.to_string()))
                    .collect(),
            ),
        ];
        for spec in &steps {
            let r = subprocess::run_spec(spec)
                .await
                .map_err(|e| e.to_string())?;
            if r.code != 0 {
                return Err(format!("setting up Python failed: {}", r.stderr.trim()));
            }
        }
        // Build matplotlib's font cache now (about 20 s); each run starts with a copy of it.
        let mut warm = Spec::new(self.venv().join("bin/python"));
        warm.args = vec!["-c".into(), "import matplotlib.pyplot".into()];
        let mut env = self.env();
        env.push(("MPLCONFIGDIR".into(), self.cache().display().to_string()));
        warm.env = Some(env);
        warm.timeout = Some(Duration::from_secs(120));
        let _ = subprocess::run_spec(&warm).await;
        std::fs::write(self.stamp(), Self::wanted()).map_err(|e| e.to_string())
    }

    /// matplotlib's font cache, built at install.
    fn cache(&self) -> PathBuf {
        self.home.join("matplotlib")
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("MPLBACKEND".into(), "Agg".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ("PYTHONUNBUFFERED".into(), "1".into()),
            ("VIRTUAL_ENV".into(), self.venv().display().to_string()),
        ]
    }
}

#[async_trait]
impl Runtime for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    fn says(&self) -> String {
        let names: Vec<&str> = PACKAGES
            .iter()
            .map(|p| p.split("==").next().unwrap_or(p))
            .collect();
        format!("Python {VERSION} with {}", names.join(", "))
    }

    fn available(&self) -> Result<(), String> {
        match &self.uv {
            Some(_) => Ok(()),
            None if self.is_ready() => Ok(()),
            None => Err("needs uv (https://docs.astral.sh/uv/) to set up Python".into()),
        }
    }

    async fn ready(&self) -> Result<Launch, String> {
        let _one = self.lock.lock().await;
        if !self.is_ready() {
            self.available()?;
            let uv = self.uv.clone().ok_or("no uv")?;
            tracing::info!("setting up Python {VERSION} in {}", self.home.display());
            self.install(&uv).await?;
        }
        let python = self.venv().join("bin/python");
        // The environment's python links to the interpreter uv installed; the run reads both.
        let interpreter = python
            .canonicalize()
            .ok()
            .and_then(|p| p.parent()?.parent().map(Path::to_path_buf));
        Ok(Launch {
            program: python,
            args: vec!["-B".into(), "-s".into()],
            file: "main.py",
            env: self.env(),
            reads: std::iter::once(self.home.clone())
                .chain(interpreter)
                .collect(),
            // matplotlib looks for its cache in `~/.matplotlib`; a run's home is writable.
            home: vec![(self.cache(), ".matplotlib")],
        })
    }
}
