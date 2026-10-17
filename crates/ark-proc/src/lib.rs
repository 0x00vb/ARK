//! # ark-proc
//!
//! Project processes (ADR 0041). An agent starts a long-running command in the project folder
//! (a dev server, a watcher, a worker), with the person's own shell and tools, after the person
//! allows it. The person sees every process and its output and can stop it; the window opens
//! a process's page once it is ready.
//!
//! ```text
//! process.start {command, folder, port, open}
//!   -> the program in the person's PATH?   no: acquire_capability installs it (install.rs)
//!   -> $SHELL -c command, own process group, in the project folder
//!   -> output: kept (last 256 KB), sent to windows, scanned for a local address
//!   -> ready: that address (or `port`) answers  -> the window opens `open` in its Browser
//! process.stop  -> SIGTERM to the group, SIGKILL after 5 s
//! quitting      -> every group stopped; a crash leaves running.json, swept at the next start
//! ```
//!
//! Nothing is contained: a process runs as the person. That is what the approval says.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, Notify};

pub mod install;
pub mod ready;
mod serve;
pub mod shell;
mod tool;

pub use install::Installs;
pub use shell::Shell;
pub use tool::{distill, ProcessTool, LIST, START, STOP};

/// Output kept per process, for the window and the agent.
const KEEP: usize = 256 * 1024;
/// Processes running at once in one runtime.
const LIMIT: usize = 8;
/// Exited processes kept in the list.
const KEEP_EXITED: usize = 8;
/// From SIGTERM to SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The record of running processes, for the sweep after a crash.
const RUNNING: &str = "running.json";

/// Where a process is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Started; no address answers yet (or it serves none).
    Running,
    /// Its address answers.
    Ready,
    /// Ended on its own.
    Exited,
    /// Stopped by the agent, the person or ARK quitting.
    Stopped,
}

/// A process as the agent and the window see it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub command: String,
    /// The project folder.
    pub project: String,
    /// The folder inside it, `""` for the project folder itself.
    pub folder: String,
    pub state: State,
    /// The local address it serves, once seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The exit code, once it ended on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    pub pid: u32,
    /// RFC 3339.
    pub started: String,
}

/// What windows are told.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Started {
        process: Info,
    },
    Output {
        id: String,
        data: String,
    },
    /// Ready, exited or stopped. `open`: the address the window shows in its Browser.
    Changed {
        process: Info,
        #[serde(skip_serializing_if = "Option::is_none")]
        open: Option<String>,
    },
}

/// What a start asks for.
#[derive(Clone, Debug, Default)]
pub struct Start {
    pub command: String,
    /// Inside the project folder; `""` for the folder itself.
    pub folder: String,
    /// A port that answers when the process is ready, when its output names none.
    pub port: Option<u16>,
    /// A path (or local address) to show the person once ready.
    pub open: Option<String>,
}

/// Why a start did not start anything.
#[derive(Debug)]
pub enum Refused {
    /// The command's program is not in the person's PATH. `install`: what installs it.
    Missing {
        program: String,
        install: Option<String>,
        /// Why the last install of that tool failed (exit, end of its output), when one did.
        failed: Option<String>,
    },
    /// Too many run already.
    Limit(usize),
    /// The folder is not inside the project, or does not exist.
    Folder(String),
    Spawn(std::io::Error),
    Empty,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Missing { program, failed: Some(why), .. } => write!(
                f,
                "`{program}` is not installed on this computer. ARK tried to install it and it failed: {why} Do not ask to install it again. Tell the person what failed and what is missing."
            ),
            Refused::Missing { program, install: Some(id), .. } => write!(
                f,
                "`{program}` is not installed on this computer. Call acquire_capability with need \"{id}\": it asks the person to install it. Then start the process again."
            ),
            Refused::Missing { program, install: None, .. } => write!(
                f,
                "`{program}` is not installed on this computer, and ARK does not know how to install it. Tell the person what is missing."
            ),
            Refused::Limit(n) => write!(f, "{n} processes run already; stop one first (process.list)"),
            Refused::Folder(why) => write!(f, "{why}"),
            Refused::Spawn(e) => write!(f, "could not start: {e}"),
            Refused::Empty => write!(f, "`command` is the command line to run"),
        }
    }
}

/// One process.
struct Proc {
    id: String,
    command: String,
    root: PathBuf,
    folder: String,
    pid: u32,
    started: SystemTime,
    port: Option<u16>,
    open: Option<String>,
    st: Mutex<ProcState>,
    changed: Notify,
}

struct ProcState {
    state: State,
    url: Option<(String, u16)>,
    code: Option<i32>,
    log: VecDeque<u8>,
    /// Bytes of a character cut at the end of the last chunk.
    carry: Vec<u8>,
}

impl Proc {
    fn info(&self) -> Info {
        let st = self.st.lock().unwrap();
        Info {
            id: self.id.clone(),
            command: self.command.clone(),
            project: self.root.display().to_string(),
            folder: self.folder.clone(),
            state: st.state.clone(),
            url: st.url.as_ref().map(|(u, _)| u.clone()),
            code: st.code,
            pid: self.pid,
            started: rfc3339(self.started),
        }
    }

    fn running(&self) -> bool {
        matches!(self.st.lock().unwrap().state, State::Running | State::Ready)
    }

    /// The end of the output as text, without colour codes.
    fn tail(&self, chars: usize) -> String {
        let st = self.st.lock().unwrap();
        let bytes: Vec<u8> = st.log.iter().copied().collect();
        let text = ready::plain(&String::from_utf8_lossy(&bytes));
        let count = text.chars().count();
        if count <= chars {
            return text;
        }
        text.chars().skip(count - chars).collect()
    }
}

struct Inner {
    dir: PathBuf,
    shell: Shell,
    procs: Mutex<Vec<Arc<Proc>>>,
    events: broadcast::Sender<Event>,
    next: AtomicU64,
    installs: install::Catalog,
    /// The last failed install of each tool, by tool id, until one succeeds.
    install_failures: Mutex<std::collections::HashMap<String, String>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The runtime is going away: nothing it started outlives it.
        for p in self.procs.lock().unwrap().iter() {
            if p.running() {
                signal_group(p.pid, Signal::Kill);
            }
        }
        let _ = std::fs::remove_file(self.dir.join(RUNNING));
    }
}

/// [`Processes`] without keeping them alive ([`Processes::downgrade`]).
#[derive(Clone)]
pub struct WeakProcesses(std::sync::Weak<Inner>);

impl WeakProcesses {
    pub fn upgrade(&self) -> Option<Processes> {
        self.0.upgrade().map(|inner| Processes { inner })
    }
}

/// The processes of one runtime. Cheap to clone.
#[derive(Clone)]
pub struct Processes {
    inner: Arc<Inner>,
}

impl Processes {
    /// The service, its records in `dir` (`<store>/processes`), commands run through the
    /// person's shell. Ends what a crashed runtime left running. `None` where ARK cannot stop
    /// a process with everything it started (Windows, for now).
    pub fn open(dir: impl Into<PathBuf>) -> Option<Processes> {
        Processes::with_shell(dir, Shell::person())
    }

    /// [`open`](Processes::open) with a shell the app chose (tests, a server's own setup).
    pub fn with_shell(dir: impl Into<PathBuf>, shell: Shell) -> Option<Processes> {
        if !cfg!(unix) {
            return None;
        }
        let dir = dir.into();
        let _ = std::fs::create_dir_all(&dir);
        sweep(&dir);
        let (events, _) = broadcast::channel(4096);
        Some(Processes {
            inner: Arc::new(Inner {
                dir,
                shell,
                procs: Mutex::new(Vec::new()),
                events,
                next: AtomicU64::new(1),
                installs: install::Catalog::builtin(),
                install_failures: Mutex::new(Default::default()),
            }),
        })
    }

    /// Every event from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn shell(&self) -> &Shell {
        &self.inner.shell
    }

    /// Why the last install of the tool `id` failed, until an install of it succeeds.
    pub fn install_failure(&self, id: &str) -> Option<String> {
        self.inner.install_failures.lock().unwrap().get(id).cloned()
    }

    /// Keep why an install failed, so a start that needs the tool says it instead of asking again.
    pub fn record_install_failure(&self, id: &str, why: String) {
        self.inner
            .install_failures
            .lock()
            .unwrap()
            .insert(id.to_string(), why);
    }

    pub(crate) fn clear_install_failure(&self, id: &str) {
        self.inner.install_failures.lock().unwrap().remove(id);
    }

    pub(crate) fn installs(&self) -> &install::Catalog {
        &self.inner.installs
    }

    /// Every process, running first, then the ones that ended, newest first.
    pub fn list(&self) -> Vec<Info> {
        let procs = self.inner.procs.lock().unwrap();
        let mut v: Vec<Info> = procs.iter().rev().map(|p| p.info()).collect();
        v.sort_by_key(|i| !matches!(i.state, State::Running | State::Ready));
        v
    }

    /// Processes belonging to exactly one project root. Front-ends use this instead of
    /// filtering after exposing runtime-wide ids.
    pub fn list_for(&self, root: &Path) -> Vec<Info> {
        let procs = self.inner.procs.lock().unwrap();
        let mut v: Vec<Info> = procs
            .iter()
            .rev()
            .filter(|p| p.root == root)
            .map(|p| p.info())
            .collect();
        v.sort_by_key(|i| !matches!(i.state, State::Running | State::Ready));
        v
    }

    /// The ports on this computer that running processes of project `root` serve themselves:
    /// the port of their address, when the socket listening there belongs to the process (not
    /// another app that answers on it). What an agent's browser may open (ADR 0046). `root` is
    /// compared as given to [`start`](Processes::start). Asks the system, so call it off the
    /// async threads.
    pub fn serving(&self, root: &Path) -> Vec<u16> {
        let wanted: Vec<(u32, u16)> = self
            .inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.root == root && p.running())
            .filter_map(|p| Some((p.pid, p.st.lock().unwrap().url.as_ref()?.1)))
            .collect();
        let groups = wanted.iter().map(|(pid, _)| *pid).collect();
        let listening = serve::listeners(&groups);
        let mut ports: Vec<u16> = wanted
            .into_iter()
            .filter(|(pid, port)| listening.contains(&(*port, *pid)))
            .map(|(_, port)| port)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// A handle that does not keep the service alive: what runs is still stopped when the
    /// runtime goes away, and the handle then gives nothing.
    pub fn downgrade(&self) -> WeakProcesses {
        WeakProcesses(Arc::downgrade(&self.inner))
    }

    pub fn get(&self, id: &str) -> Option<Info> {
        self.find(id).map(|p| p.info())
    }

    /// The end of a process's output (up to `chars` characters), without colour codes.
    pub fn output(&self, id: &str, chars: usize) -> Option<String> {
        self.find(id).map(|p| p.tail(chars))
    }

    /// Everything kept of a process's output, as it was written (for a terminal view).
    pub fn raw_output(&self, id: &str) -> Option<String> {
        let p = self.find(id)?;
        let st = p.st.lock().unwrap();
        let bytes: Vec<u8> = st.log.iter().copied().collect();
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Kept output only when `id` belongs to `root`.
    pub fn raw_output_for(&self, root: &Path, id: &str) -> Option<String> {
        let p = self.find(id)?;
        if p.root != root {
            return None;
        }
        let st = p.st.lock().unwrap();
        let bytes: Vec<u8> = st.log.iter().copied().collect();
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn find(&self, id: &str) -> Option<Arc<Proc>> {
        self.inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// The running process with this command in this folder, if one is.
    pub fn running_like(&self, root: &Path, folder: &str, command: &str) -> Option<Info> {
        self.inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .find(|p| {
                p.root == root && p.folder == folder && p.command == command.trim() && p.running()
            })
            .map(|p| p.info())
    }

    /// Start a process in `root` (the project folder). Answers at once; [`Processes::settle`]
    /// waits for it to be ready.
    pub async fn start(&self, root: &Path, req: &Start) -> Result<Info, Refused> {
        let command = req.command.trim().to_string();
        if command.is_empty() {
            return Err(Refused::Empty);
        }
        let folder = clean_folder(&req.folder);
        let cwd = inside(root, &folder)?;
        if let Some(same) = self.running_like(root, &folder, &command) {
            return Ok(same);
        }
        let running = self
            .inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.running())
            .count();
        if running >= LIMIT {
            return Err(Refused::Limit(LIMIT));
        }
        let env = self.inner.shell.env().await;
        if let Some(program) = shell::program_of(&command) {
            if Shell::find(&program, &env, &cwd).is_none() {
                let install = self
                    .inner
                    .installs
                    .for_program(&program)
                    .map(|t| t.id.clone());
                let failed = install.as_ref().and_then(|id| self.install_failure(id));
                return Err(Refused::Missing {
                    program,
                    install,
                    failed,
                });
            }
        }
        self.spawn(root, folder, cwd, command, env, req).await
    }

    async fn spawn(
        &self,
        root: &Path,
        folder: String,
        cwd: PathBuf,
        command: String,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        req: &Start,
    ) -> Result<Info, Refused> {
        let mut cmd = tokio::process::Command::new(self.inner.shell.program());
        cmd.arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .env_clear()
            .envs(env)
            // A server opens no browser of its own: the window shows its page.
            .env("BROWSER", "none")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(Refused::Spawn)?;
        let pid = child.id().unwrap_or(0);
        let n = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let proc = Arc::new(Proc {
            id: format!("p{n}"),
            command,
            root: root.to_path_buf(),
            folder,
            pid,
            started: SystemTime::now(),
            port: req.port,
            open: req.open.clone().filter(|o| !o.trim().is_empty()),
            st: Mutex::new(ProcState {
                state: State::Running,
                url: None,
                code: None,
                log: VecDeque::new(),
                carry: Vec::new(),
            }),
            changed: Notify::new(),
        });
        {
            let mut procs = self.inner.procs.lock().unwrap();
            procs.push(proc.clone());
            // Forget the oldest that ended, past what is kept.
            let ended: Vec<String> = procs
                .iter()
                .filter(|p| !p.running())
                .map(|p| p.id.clone())
                .collect();
            if ended.len() > KEEP_EXITED {
                let drop: Vec<&String> = ended.iter().take(ended.len() - KEEP_EXITED).collect();
                procs.retain(|p| !drop.contains(&&p.id));
            }
        }
        self.record();
        let _ = self.inner.events.send(Event::Started {
            process: proc.info(),
        });

        let events = self.inner.events.clone();
        let readers = [
            child.stdout.take().map(|s| self.read(proc.clone(), s)),
            child.stderr.take().map(|s| self.read(proc.clone(), s)),
        ];
        // The tasks hold the service weakly: when the runtime drops it, `Drop` ends what runs.
        let weak = Arc::downgrade(&self.inner);
        let p = proc.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // The output a process wrote before it ended belongs to it.
            for r in readers.into_iter().flatten() {
                let _ = r.await;
            }
            let ended = {
                let mut st = p.st.lock().unwrap();
                if st.state != State::Stopped {
                    st.state = State::Exited;
                    st.code = status.ok().and_then(|s| s.code());
                }
                st.state.clone()
            };
            // The shell ended; anything it left behind in its group goes with it.
            signal_group(p.pid, Signal::Kill);
            tracing::debug!("process {} ({}) {:?}", p.id, p.command, ended);
            p.changed.notify_waiters();
            if let Some(inner) = weak.upgrade() {
                Processes { inner }.record();
            }
            let _ = events.send(Event::Changed {
                process: p.info(),
                open: None,
            });
        });
        tokio::spawn(watch_ready(self.inner.events.clone(), proc.clone()));
        Ok(proc.info())
    }

    /// Read one stream into the process's output until it closes.
    fn read<R>(&self, p: Arc<Proc>, mut stream: R) -> tokio::task::JoinHandle<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let events = self.inner.events.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let text = {
                    let mut st = p.st.lock().unwrap();
                    st.log.extend(&buf[..n]);
                    let over = st.log.len().saturating_sub(KEEP);
                    st.log.drain(..over);
                    let mut bytes = std::mem::take(&mut st.carry);
                    bytes.extend_from_slice(&buf[..n]);
                    let (text, carry) = utf8_prefix(bytes);
                    st.carry = carry;
                    if st.url.is_none() {
                        let seen: Vec<u8> = st.log.iter().rev().take(8192).rev().copied().collect();
                        st.url =
                            ready::local_address(&ready::plain(&String::from_utf8_lossy(&seen)));
                    }
                    text
                };
                p.changed.notify_waiters();
                if !text.is_empty() {
                    let _ = events.send(Event::Output {
                        id: p.id.clone(),
                        data: text,
                    });
                }
            }
        })
    }

    /// Wait up to `wait` for a process to be ready or to end; its state then.
    pub async fn settle(&self, id: &str, wait: Duration) -> Option<Info> {
        let p = self.find(id)?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = p.changed.notified();
            if !matches!(p.st.lock().unwrap().state, State::Running) {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                break;
            }
        }
        Some(p.info())
    }

    /// Wait up to `wait` for a process to end; its state then.
    pub async fn wait_end(&self, id: &str, wait: Duration) -> Option<Info> {
        let p = self.find(id)?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = p.changed.notified();
            if !p.running() {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                break;
            }
        }
        Some(p.info())
    }

    /// Stop a process and everything it started: SIGTERM, then SIGKILL after a grace.
    /// `None` when there is no such process.
    pub async fn stop(&self, id: &str) -> Option<Info> {
        let p = self.find(id)?;
        if !p.running() {
            return Some(p.info());
        }
        p.st.lock().unwrap().state = State::Stopped;
        signal_group(p.pid, Signal::Term);
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        while group_alive(p.pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        signal_group(p.pid, Signal::Kill);
        self.record();
        p.changed.notify_waiters();
        let _ = self.inner.events.send(Event::Changed {
            process: p.info(),
            open: None,
        });
        Some(p.info())
    }

    /// Stop `id` only when it belongs to `root`.
    pub async fn stop_for(&self, root: &Path, id: &str) -> Option<Info> {
        if self.find(id).is_none_or(|p| p.root != root) {
            return None;
        }
        self.stop(id).await
    }

    /// Stop every running process (the runtime is stopping).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self
            .inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.running())
            .map(|p| p.id.clone())
            .collect();
        let stops = ids.iter().map(|id| self.stop(id));
        for s in stops {
            s.await;
        }
    }

    /// Write the running processes' groups, for the sweep after a crash.
    fn record(&self) {
        let now = SystemTime::now();
        let running: Vec<Record> = self
            .inner
            .procs
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.running())
            .map(|p| Record {
                pid: p.pid,
                command: p.command.clone(),
                started: unix(p.started),
                recorded: unix(now),
            })
            .collect();
        let path = self.inner.dir.join(RUNNING);
        if running.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else if let Ok(json) = serde_json::to_vec_pretty(&running) {
            let _ = std::fs::write(&path, json);
        }
    }
}

/// Mark a process ready once its address (or the agent's port) answers.
async fn watch_ready(events: broadcast::Sender<Event>, p: Arc<Proc>) {
    loop {
        let (state, port) = {
            let st = p.st.lock().unwrap();
            (
                st.state.clone(),
                st.url.as_ref().map(|(_, port)| *port).or(p.port),
            )
        };
        if state != State::Running {
            return;
        }
        if let Some(port) = port {
            if ready::answers(port).await {
                let open = {
                    let mut st = p.st.lock().unwrap();
                    if st.state != State::Running {
                        return;
                    }
                    st.state = State::Ready;
                    if st.url.is_none() {
                        st.url = Some((format!("http://localhost:{port}/"), port));
                    }
                    let base = st.url.as_ref().map(|(u, _)| u.clone()).unwrap_or_default();
                    p.open.as_deref().map(|o| ready::with_path(&base, o))
                };
                p.changed.notify_waiters();
                let _ = events.send(Event::Changed {
                    process: p.info(),
                    open,
                });
                return;
            }
        }
        let _ = tokio::time::timeout(Duration::from_millis(250), p.changed.notified()).await;
    }
}

/// A running process's group, as recorded.
#[derive(Serialize, Deserialize)]
struct Record {
    /// The group leader (the shell), whose id is the group's.
    pid: u32,
    command: String,
    /// Unix seconds.
    started: u64,
    recorded: u64,
}

/// End the groups a runtime that did not stop cleanly left running: only a group whose leader
/// still runs and started when the record says (a reused id is someone else's).
fn sweep(dir: &Path) {
    let path = dir.join(RUNNING);
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let records: Vec<Record> = serde_json::from_slice(&bytes).unwrap_or_default();
    for r in records {
        match leader_started(r.pid) {
            Some(started) if started.abs_diff(r.started) <= 2 => {
                tracing::info!(
                    "ending `{}` (group {}), left running by an earlier run",
                    r.command,
                    r.pid
                );
                signal_group(r.pid, Signal::Kill);
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// When the process `pid` started (unix seconds), if it runs and leads its own group.
fn leader_started(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "pgid=,etime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut words = text.split_whitespace();
    let pgid: u32 = words.next()?.parse().ok()?;
    if pgid != pid {
        return None;
    }
    let elapsed = elapsed_secs(words.next()?)?;
    Some(unix(SystemTime::now()).saturating_sub(elapsed))
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`, in seconds.
fn elapsed_secs(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    Some(days * 86_400 + h * 3600 + m * 60 + sec)
}

enum Signal {
    Term,
    Kill,
}

fn signal_group(pid: u32, sig: Signal) {
    #[cfg(unix)]
    if pid > 1 {
        let sig = match sig {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: killpg only sends a signal; the group is one this service started.
        unsafe {
            libc::killpg(pid as libc::pid_t, sig);
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, sig);
}

fn group_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 checks only that the group exists.
        pid > 1 && unsafe { libc::killpg(pid as libc::pid_t, 0) } == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// `folder` with `/` separators and no leading `./` or `/`.
fn clean_folder(folder: &str) -> String {
    folder
        .trim()
        .trim_start_matches("./")
        .trim_matches('/')
        .to_string()
}

/// The absolute folder for `folder` inside `root`, when it exists and stays inside.
fn inside(root: &Path, folder: &str) -> Result<PathBuf, Refused> {
    let root = root
        .canonicalize()
        .map_err(|e| Refused::Folder(format!("the project folder: {e}")))?;
    let dir = root
        .join(folder)
        .canonicalize()
        .map_err(|_| Refused::Folder(format!("there is no folder `{folder}` in the project")))?;
    if !dir.starts_with(&root) || !dir.is_dir() {
        return Err(Refused::Folder(format!(
            "`{folder}` is not a folder inside the project"
        )));
    }
    Ok(dir)
}

/// The longest valid UTF-8 text at the start of `bytes`, and the bytes of a character cut at
/// the end (invalid bytes in the middle become U+FFFD).
fn utf8_prefix(bytes: Vec<u8>) -> (String, Vec<u8>) {
    match std::str::from_utf8(&bytes) {
        Ok(_) => (String::from_utf8(bytes).unwrap_or_default(), Vec::new()),
        Err(e) if e.error_len().is_none() => {
            let at = e.valid_up_to();
            let carry = bytes[at..].to_vec();
            (String::from_utf8_lossy(&bytes[..at]).into_owned(), carry)
        }
        Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), Vec::new()),
    }
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rfc3339(t: SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_time_reads_every_form() {
        assert_eq!(elapsed_secs("00:05"), Some(5));
        assert_eq!(elapsed_secs("01:02:03"), Some(3723));
        assert_eq!(elapsed_secs("2-00:00:01"), Some(172_801));
        assert_eq!(elapsed_secs("x"), None);
    }

    #[test]
    fn a_character_cut_between_chunks_waits_for_its_end() {
        let arrow = "➜".as_bytes();
        let mut first = b"Local ".to_vec();
        first.extend_from_slice(&arrow[..1]);
        let (text, carry) = utf8_prefix(first);
        assert_eq!(text, "Local ");
        let mut next = carry;
        next.extend_from_slice(&arrow[1..]);
        assert_eq!(utf8_prefix(next).0, "➜");
    }

    #[test]
    fn a_folder_stays_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("web")).unwrap();
        assert!(inside(dir.path(), "web").is_ok());
        assert!(inside(dir.path(), "").is_ok());
        assert!(matches!(inside(dir.path(), ".."), Err(Refused::Folder(_))));
        assert!(matches!(
            inside(dir.path(), "nope"),
            Err(Refused::Folder(_))
        ));
        assert_eq!(clean_folder("./web/"), "web");
    }
}
