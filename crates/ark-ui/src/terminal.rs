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
use crate::resource::{ResourceKind, ResourceRef, ResourceSubscribers};

/// Scrollback kept per shell for a window that re-attaches (bytes).
const HISTORY_CAP: usize = 256 * 1024;

struct Shell {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    state: Arc<Mutex<ShellState>>,
    generation: String,
    project: String,
    /// Whether `by_project` points at this shell. Program setup terminals do not.
    mapped: bool,
    cwd: String,
}

#[derive(Default)]
struct ShellState {
    history: Vec<u8>,
    /// Absolute cursor of `history[0]` and one past its last byte.
    base: u64,
    next: u64,
    ended: bool,
}

/// The live shells by session id, and which one each project uses.
pub(crate) struct Terminals {
    shells: Mutex<HashMap<String, Shell>>,
    by_project: Mutex<HashMap<String, String>>,
    subscribers: Arc<ResourceSubscribers>,
}

#[derive(serde::Serialize)]
pub(crate) struct Opened {
    pub session: String,
    pub generation: String,
    pub cwd: String,
    /// Scrollback to replay when re-attaching; empty for a new shell.
    pub history: String,
    pub cursor: u64,
    pub ended: bool,
    pub reattached: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ReadBack {
    pub generation: String,
    pub data: String,
    pub base: u64,
    pub cursor: u64,
    pub gap: bool,
    pub ended: bool,
}

impl Terminals {
    pub fn new(subscribers: Arc<ResourceSubscribers>) -> Self {
        Self {
            shells: Mutex::new(HashMap::new()),
            by_project: Mutex::new(HashMap::new()),
            subscribers,
        }
    }

    pub fn subscribe(&self, project: &str, session: &str, r: ResourceRef) {
        self.subscribers
            .subscribe(ResourceKind::Terminal, project, session, r);
    }

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
                let state = s.state.lock().unwrap();
                return Ok(Opened {
                    session: id,
                    generation: s.generation.clone(),
                    cwd: s.cwd.clone(),
                    history: String::from_utf8_lossy(&state.history).into_owned(),
                    cursor: state.next,
                    ended: state.ended,
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
        let mut cmd = CommandBuilder::new(ark_os::shell::interactive());
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let id = format!("term-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let state = Arc::new(Mutex::new(ShellState::default()));

        let (feed, sid, st, gen, pid, subscribers) = (
            feed.clone(),
            id.clone(),
            state.clone(),
            generation.clone(),
            project.to_string(),
            self.subscribers.clone(),
        );
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let cursor = {
                            let mut state = st.lock().unwrap();
                            state.history.extend_from_slice(chunk);
                            state.next = state.next.saturating_add(n as u64);
                            if state.history.len() > HISTORY_CAP {
                                let cut = state.history.len() - HISTORY_CAP;
                                state.history.drain(0..cut);
                                state.base = state.base.saturating_add(cut as u64);
                            }
                            state.next
                        };
                        for reference in subscribers.aliases(ResourceKind::Terminal, &pid, &sid) {
                            feed.send(json!({ "kind": "term", "session": reference.location.session, "ref": reference, "generation": gen, "cursor": cursor, "data": String::from_utf8_lossy(chunk) }));
                        }
                    }
                }
            }
            st.lock().unwrap().ended = true;
            for reference in subscribers.aliases(ResourceKind::Terminal, &pid, &sid) {
                feed.send(json!({ "kind": "term", "session": reference.location.session, "ref": reference, "generation": gen, "exit": true }));
            }
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
                state,
                generation: generation.clone(),
                project: project.to_string(),
                mapped: true,
                cwd: cwd.clone(),
            },
        );
        self.by_project
            .lock()
            .unwrap()
            .insert(project.to_string(), id.clone());
        Ok(Opened {
            session: id,
            generation,
            cwd,
            history: String::new(),
            cursor: 0,
            ended: false,
            reattached: false,
        })
    }

    /// A program in a terminal of its own (no shell, no project): a program's install or
    /// sign-in the person watches and answers (ADR 0052). Its output is never kept beyond the
    /// scrollback a window re-attaches to.
    pub fn open_program(
        &self,
        feed: &Arc<Feed>,
        argv: &[String],
        cwd: PathBuf,
        cols: u16,
        rows: u16,
        mut reference: ResourceRef,
    ) -> anyhow::Result<Opened> {
        anyhow::ensure!(!argv.is_empty(), "nothing to run");
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let id = format!("term-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        reference.id = id.clone();
        self.subscribe(&reference.location.project, &id, reference.clone());
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let state = Arc::new(Mutex::new(ShellState::default()));
        let (feed, sid, st, gen, pid, subscribers) = (
            feed.clone(),
            id.clone(),
            state.clone(),
            generation.clone(),
            reference.location.project.clone(),
            self.subscribers.clone(),
        );
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let cursor = {
                            let mut state = st.lock().unwrap();
                            state.history.extend_from_slice(&buf[..n]);
                            state.next = state.next.saturating_add(n as u64);
                            let cut = state.history.len().saturating_sub(HISTORY_CAP);
                            state.history.drain(0..cut);
                            state.base = state.base.saturating_add(cut as u64);
                            state.next
                        };
                        for reference in subscribers.aliases(ResourceKind::Terminal, &pid, &sid) {
                            feed.send(json!({ "kind": "term", "session": reference.location.session, "ref": reference, "generation": gen, "cursor": cursor, "data": String::from_utf8_lossy(&buf[..n]) }));
                        }
                    }
                }
            }
            st.lock().unwrap().ended = true;
            for reference in subscribers.aliases(ResourceKind::Terminal, &pid, &sid) {
                feed.send(json!({ "kind": "term", "session": reference.location.session, "ref": reference, "generation": gen, "exit": true }));
            }
        });
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let cwd = cwd.display().to_string();
        self.shells.lock().unwrap().insert(
            id.clone(),
            Shell {
                master: pair.master,
                writer,
                state,
                generation: generation.clone(),
                // Not a project's shell: `close` leaves the projects' alone.
                project: format!("\u{0}{id}"),
                mapped: false,
                cwd: cwd.clone(),
            },
        );
        Ok(Opened {
            session: id,
            generation,
            cwd,
            history: String::new(),
            cursor: 0,
            ended: false,
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

    pub fn write_for(&self, project: &str, session: &str, data: &str) -> anyhow::Result<()> {
        let mut shells = self.shells.lock().unwrap();
        let s = shells
            .get_mut(session)
            .filter(|s| s.project == project)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}` in this project"))?;
        anyhow::ensure!(!s.state.lock().unwrap().ended, "that terminal ended");
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

    pub fn resize_for(
        &self,
        project: &str,
        session: &str,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let shells = self.shells.lock().unwrap();
        let s = shells
            .get(session)
            .filter(|s| s.project == project)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}` in this project"))?;
        s.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn read_for(
        &self,
        project: &str,
        session: &str,
        generation: &str,
        after: u64,
    ) -> anyhow::Result<ReadBack> {
        let shells = self.shells.lock().unwrap();
        let s = shells
            .get(session)
            .filter(|s| s.project == project)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}` in this project"))?;
        Self::read_shell(s, generation, after)
    }

    pub fn read_owned(
        &self,
        session: &str,
        generation: &str,
        after: u64,
    ) -> anyhow::Result<ReadBack> {
        let shells = self.shells.lock().unwrap();
        let s = shells
            .get(session)
            .ok_or_else(|| anyhow::anyhow!("no terminal `{session}`"))?;
        Self::read_shell(s, generation, after)
    }

    fn read_shell(s: &Shell, generation: &str, after: u64) -> anyhow::Result<ReadBack> {
        let state = s.state.lock().unwrap();
        Self::read_state(&s.generation, &state, generation, after)
    }

    fn read_state(
        current_generation: &str,
        state: &ShellState,
        generation: &str,
        after: u64,
    ) -> anyhow::Result<ReadBack> {
        anyhow::ensure!(
            current_generation == generation,
            "that terminal generation ended"
        );
        let requested = after.max(state.base).min(state.next);
        let mut start = (requested - state.base) as usize;
        // A bounded byte buffer may begin in the middle of a multibyte character. Skip only
        // continuation bytes; invalid terminal bytes later are rendered lossily as before.
        while start < state.history.len() && state.history[start] & 0b1100_0000 == 0b1000_0000 {
            start += 1;
        }
        let base = state.base + start as u64;
        Ok(ReadBack {
            generation: current_generation.to_string(),
            data: String::from_utf8_lossy(&state.history[start..]).into_owned(),
            base,
            cursor: state.next,
            gap: after < base,
            ended: state.ended,
        })
    }

    /// End a shell. Its process gets a hangup when the PTY closes.
    pub fn close(&self, session: &str) {
        if let Some(s) = self.shells.lock().unwrap().remove(session) {
            if s.mapped {
                self.by_project.lock().unwrap().remove(&s.project);
            }
        }
    }

    pub fn close_for(&self, project: &str, session: &str) -> anyhow::Result<()> {
        let belongs = self
            .shells
            .lock()
            .unwrap()
            .get(session)
            .map(|s| s.project == project);
        anyhow::ensure!(
            belongs != Some(false),
            "terminal `{session}` belongs to another project"
        );
        if belongs.is_none() {
            return Ok(());
        }
        self.close(session);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ShellState, Terminals};

    #[test]
    fn replay_reports_a_gap_and_starts_on_a_utf8_boundary() {
        let state = ShellState {
            // The cap discarded the first byte of `é`; replay must not emit its continuation.
            history: vec![0xa9, b'E', b'N', b'D'],
            base: 100,
            next: 104,
            ended: true,
        };
        let replay = Terminals::read_state("g", &state, "g", 0).unwrap();
        assert_eq!(replay.data, "END");
        assert_eq!((replay.base, replay.cursor), (101, 104));
        assert!(replay.gap && replay.ended);
    }

    #[test]
    fn replay_rejects_a_previous_shell_generation() {
        let state = ShellState::default();
        let error = Terminals::read_state("new", &state, "old", 0).unwrap_err();
        assert!(format!("{error}").contains("generation ended"));
    }
}
