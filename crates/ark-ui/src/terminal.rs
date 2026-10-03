//! The window's terminal: one shell per project, in the project's folder, so the agent's edits
//! and the person's commands share a tree. Output goes to every window as `{kind: "term",
//! session, data}` and into a scrollback buffer, so a closed and reopened terminal re-attaches
//! to the same shell with its history.
//!
//! It never goes through the approval gate: an interactive shell is the person's own hands. The
//! agent cannot type into it (a `present` directive only fills the prompt; the page leaves
//! Enter to the person).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde_json::json;

use crate::host::Feed;

/// Scrollback kept per shell for a window that re-attaches (bytes).
const HISTORY_CAP: usize = 256 * 1024;

struct Shell {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    history: Arc<Mutex<Vec<u8>>>,
    project: String,
    cwd: String,
}

/// The live shells by session id, and which one each project uses.
#[derive(Default)]
pub(crate) struct Terminals {
    shells: Mutex<HashMap<String, Shell>>,
    by_project: Mutex<HashMap<String, String>>,
}

#[derive(serde::Serialize)]
pub(crate) struct Opened {
    pub session: String,
    pub cwd: String,
    /// Scrollback to replay when re-attaching; empty for a new shell.
    pub history: String,
    pub reattached: bool,
}

impl Terminals {
    /// The project's shell: the live one with its scrollback, or a new one in `cwd`.
    pub fn open(
        &self,
        feed: &Arc<Feed>,
        project: &str,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Opened> {
        if let Some(id) = self.by_project.lock().unwrap().get(project).cloned() {
            if let Some(s) = self.shells.lock().unwrap().get(&id) {
                return Ok(Opened {
                    session: id,
                    cwd: s.cwd.clone(),
                    history: String::from_utf8_lossy(&s.history.lock().unwrap()).into_owned(),
                    reattached: true,
                });
            }
        }
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let id = format!("term-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let history = Arc::new(Mutex::new(Vec::<u8>::new()));

        let (feed, sid, hist) = (feed.clone(), id.clone(), history.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        {
                            let mut h = hist.lock().unwrap();
                            h.extend_from_slice(chunk);
                            if h.len() > HISTORY_CAP {
                                let cut = h.len() - HISTORY_CAP;
                                h.drain(0..cut);
                            }
                        }
                        feed.send(json!({ "kind": "term", "session": sid, "data": String::from_utf8_lossy(chunk) }));
                    }
                }
            }
            feed.send(json!({ "kind": "term", "session": sid, "exit": true }));
        });
        // Reap the shell when it exits.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        let cwd = cwd.display().to_string();
        self.shells.lock().unwrap().insert(
            id.clone(),
            Shell {
                master: pair.master,
                writer,
                history,
                project: project.to_string(),
                cwd: cwd.clone(),
            },
        );
        self.by_project
            .lock()
            .unwrap()
            .insert(project.to_string(), id.clone());
        Ok(Opened {
            session: id,
            cwd,
            history: String::new(),
            reattached: false,
        })
    }

    pub fn write(&self, session: &str, data: &str) -> anyhow::Result<()> {
        let mut shells = self.shells.lock().unwrap();
        let s = shells
            .get_mut(session)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}`"))?;
        s.writer.write_all(data.as_bytes())?;
        s.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, session: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        let shells = self.shells.lock().unwrap();
        let s = shells
            .get(session)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}`"))?;
        s.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// End a shell. Its process gets a hangup when the PTY closes.
    pub fn close(&self, session: &str) {
        if let Some(s) = self.shells.lock().unwrap().remove(session) {
            self.by_project.lock().unwrap().remove(&s.project);
        }
    }
}
