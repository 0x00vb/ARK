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
/// pandas, numpy and matplotlib for analysis and charts; openpyxl and xlsxwriter to write
/// workbooks; python-calamine to read `.xlsx`, `.xls`, `.xlsb` and `.ods` fast
/// (`pd.read_excel(engine="calamine")`); duckdb for SQL over files and data frames without
/// loading them whole; pyarrow for columnar memory and Parquet.
const PACKAGES: &[&str] = &[
    "pandas==3.0.6",
    "numpy==2.5.3",
    "matplotlib==3.11.2",
    "openpyxl==3.1.5",
    "xlsxwriter==3.2.9",
    "python-calamine==0.8.2",
    "duckdb==1.5.6",
    "pyarrow==25.0.1",
];
/// DuckDB extensions installed with the environment, since a run has no network to fetch
/// them: `excel` reads `.xlsx` in SQL (`read_xlsx`).
const DUCKDB_EXTENSIONS: &[&str] = &["excel"];

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
    /// its usual install folders (`~/.local/bin`, `~/.cargo/bin`).
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let exe = format!("uv{}", std::env::consts::EXE_SUFFIX);
        let uv = subprocess::which(&exe).or_else(|| {
            let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)?;
            [".local/bin", ".cargo/bin"]
                .iter()
                .map(|p| user.join(p).join(&exe))
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

    /// The environment's interpreter.
    fn python(&self) -> PathBuf {
        if cfg!(windows) {
            self.venv().join("Scripts").join("python.exe")
        } else {
            self.venv().join("bin").join("python")
        }
    }

    /// Where the interpreter the environment was made from lives (`home` in `pyvenv.cfg`): a
    /// run reads it too.
    fn base(&self) -> Option<PathBuf> {
        let cfg = std::fs::read_to_string(self.venv().join("pyvenv.cfg")).ok()?;
        let home = cfg.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "home").then(|| PathBuf::from(v.trim()))
        })?;
        // On Unix `home` is the install's `bin` folder; its libraries are beside it.
        Some(match home.file_name() {
            Some(n) if n == "bin" => home.parent().map(Path::to_path_buf).unwrap_or(home),
            _ => home,
        })
    }

    fn stamp(&self) -> PathBuf {
        self.home.join("ready")
    }

    /// DuckDB's extensions, installed with the environment and copied into each run's home.
    fn extensions(&self) -> PathBuf {
        self.home.join("duckdb-extensions")
    }

    fn wanted() -> String {
        format!(
            "python {VERSION}\n{}\nduckdb extensions {}\n",
            PACKAGES.join("\n"),
            DUCKDB_EXTENSIONS.join(" ")
        )
    }

    fn is_ready(&self) -> bool {
        self.python().exists()
            && std::fs::read_to_string(self.stamp()).ok().as_deref() == Some(&Self::wanted())
    }

    async fn install(&self, uv: &Path) -> Result<(), String> {
        std::fs::create_dir_all(&self.home).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(self.stamp());
        let venv = self.venv().display().to_string();
        let python = self.python().display().to_string();
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
                ["pip", "install", "--python", &python]
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
        // Install DuckDB's extensions (a download). Neither stops the setup when it fails: a
        // run then builds the cache itself, or cannot read `.xlsx` in SQL.
        let slash = |p: PathBuf| p.display().to_string().replace('\\', "/");
        let (extensions, home) = (slash(self.extensions()), slash(self.home.clone()));
        let install: Vec<String> = DUCKDB_EXTENSIONS
            .iter()
            .map(|e| format!("INSTALL {e};"))
            .collect();
        let warmups = [
            "import matplotlib.pyplot".to_string(),
            format!(
                "import duckdb; c = duckdb.connect(config={{'extension_directory': '{extensions}', 'home_directory': '{home}'}}); c.sql(\"{}\")",
                install.join(" ")
            ),
        ];
        for code in warmups {
            let mut warm = Spec::new(self.python());
            warm.args = vec!["-c".into(), code];
            let mut env = self.env();
            env.push(("MPLCONFIGDIR".into(), self.cache().display().to_string()));
            warm.env = Some(env);
            warm.timeout = Some(Duration::from_secs(120));
            warm.keep = Some(2000);
            match subprocess::run_spec(&warm).await {
                Ok(r) if r.code == 0 => {}
                Ok(r) => tracing::warn!("Python setup step failed: {}", r.stderr.trim()),
                Err(e) => tracing::warn!("Python setup step failed: {e}"),
            }
        }
        std::fs::write(self.stamp(), Self::wanted()).map_err(|e| e.to_string())
    }

    /// matplotlib's font cache, built at install.
    fn cache(&self) -> PathBuf {
        self.home.join("matplotlib")
    }

    fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("MPLBACKEND".into(), "Agg".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ("PYTHONUNBUFFERED".into(), "1".into()),
            ("VIRTUAL_ENV".into(), self.venv().display().to_string()),
        ];
        // The setup steps run outside the sandbox and need the system's own variables there.
        if cfg!(windows) {
            for k in [
                "SystemRoot",
                "windir",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "LOCALAPPDATA",
            ] {
                if let Ok(v) = std::env::var(k) {
                    env.push((k.into(), v));
                }
            }
        }
        env
    }
}

#[async_trait]
impl Runtime for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    fn skills(&self) -> Vec<(&'static str, &'static str)> {
        vec![(
            "data-analysis",
            include_str!("../skills/data-analysis/SKILL.md"),
        )]
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
        // The environment's python starts the interpreter uv installed; the run reads both.
        Ok(Launch {
            program: self.python(),
            args: vec!["-B".into(), "-s".into()],
            file: "main.py",
            env: self.env(),
            reads: std::iter::once(self.home.clone())
                .chain(self.base())
                .collect(),
            // matplotlib looks for its cache in `~/.matplotlib` and DuckDB for its extensions
            // in `~/.duckdb/extensions`; a run's home is writable.
            home: vec![
                (self.cache(), ".matplotlib"),
                (self.extensions(), ".duckdb/extensions"),
            ],
        })
    }
}
