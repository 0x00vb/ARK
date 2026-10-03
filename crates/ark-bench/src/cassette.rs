//! Record and replay. A live run can keep every model reply (`--record`); a later run serves
//! those replies instead of calling the model (`--replay`). Replay reproduces a live run
//! offline and for free: the same answers, the same provider-reported usage, so the same
//! report, as long as the harness asks the same questions.
//!
//! Each reply is filed under its run (workload and repetition) and a fingerprint of the
//! request. Ids that differ between runs (invocation and task ids, UUIDs) are normalized out
//! of the fingerprint. Replay serves the unused reply with the same fingerprint, else the next
//! unused reply in recorded order, and counts that run as diverged: the harness asked
//! something the recording never saw, so the rest of the run is no longer a replay.
//!
//! A reply may quote an id from its run (`get_evidence` on `inv:…`). Two requests with the
//! same fingerprint have the same ids in the same places, so a replayed reply has the
//! recorded request's ids translated to the current request's before it is served.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ark_sdk::prelude::ark_model::{
    Completion, Message, Provider, ProviderError, Result as ModelResult, TextSink, ToolCall,
    ToolDef, Usage,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::record::render;

/// One recorded reply.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub workload: String,
    pub rep: u32,
    /// Its position among the run's model calls.
    pub seq: u32,
    /// The request's fingerprint (see the module docs).
    pub key: String,
    pub model: String,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    /// The ids in the request, in order of first appearance.
    #[serde(default)]
    pub ids: Vec<String>,
}

enum Mode {
    Record(Mutex<Vec<Entry>>),
    Replay(HashMap<(String, u32), Vec<Entry>>),
}

/// The replies of a whole benchmark run, recorded or loaded for replay.
pub struct Cassette {
    mode: Mode,
    diverged: Mutex<HashMap<(String, u32), u32>>,
}

impl std::fmt::Debug for Cassette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.is_replay() {
            "Cassette(replay)"
        } else {
            "Cassette(record)"
        })
    }
}

impl Cassette {
    /// An empty cassette that records.
    pub fn record() -> Arc<Cassette> {
        Arc::new(Cassette {
            mode: Mode::Record(Mutex::new(Vec::new())),
            diverged: Mutex::new(HashMap::new()),
        })
    }

    /// A recorded cassette (JSON lines) to replay.
    pub fn load(path: &Path) -> anyhow::Result<Arc<Cassette>> {
        let text = std::fs::read_to_string(path)?;
        let mut runs: HashMap<(String, u32), Vec<Entry>> = HashMap::new();
        for (n, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let e: Entry = serde_json::from_str(line)
                .map_err(|err| anyhow::anyhow!("{}:{}: {err}", path.display(), n + 1))?;
            runs.entry((e.workload.clone(), e.rep)).or_default().push(e);
        }
        for v in runs.values_mut() {
            v.sort_by_key(|e| e.seq);
        }
        Ok(Arc::new(Cassette {
            mode: Mode::Replay(runs),
            diverged: Mutex::new(HashMap::new()),
        }))
    }

    pub fn is_replay(&self) -> bool {
        matches!(self.mode, Mode::Replay(_))
    }

    /// Write what was recorded so far (JSON lines). Nothing for a replay.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Mode::Record(entries) = &self.mode {
            let mut out = String::new();
            for e in entries.lock().unwrap().iter() {
                out.push_str(&serde_json::to_string(e)?);
                out.push('\n');
            }
            std::fs::write(path, out)?;
        }
        Ok(())
    }

    /// Requests in this run that the recording did not have.
    pub fn diverged(&self, workload: &str, rep: u32) -> u32 {
        self.diverged
            .lock()
            .unwrap()
            .get(&(workload.to_string(), rep))
            .copied()
            .unwrap_or(0)
    }

    /// The provider for one run: it records `inner`'s replies, or replays (and never calls
    /// `inner`, which may be `None`).
    pub fn provider(
        self: &Arc<Self>,
        workload: &str,
        rep: u32,
        inner: Option<Arc<dyn Provider>>,
    ) -> Arc<dyn Provider> {
        let pending = match &self.mode {
            Mode::Replay(runs) => runs
                .get(&(workload.to_string(), rep))
                .cloned()
                .unwrap_or_default(),
            Mode::Record(_) => Vec::new(),
        };
        let model = pending
            .first()
            .map(|e| e.model.clone())
            .or_else(|| inner.as_ref().map(|p| p.model_id()))
            .unwrap_or_else(|| "replay".into());
        Arc::new(Tape {
            cassette: self.clone(),
            workload: workload.to_string(),
            rep,
            inner,
            model,
            seq: AtomicU32::new(0),
            pending: Mutex::new(pending.into_iter().map(Some).collect()),
        })
    }

    fn add(&self, e: Entry) {
        if let Mode::Record(entries) = &self.mode {
            entries.lock().unwrap().push(e);
        }
    }

    fn mark_diverged(&self, workload: &str, rep: u32) {
        *self
            .diverged
            .lock()
            .unwrap()
            .entry((workload.to_string(), rep))
            .or_default() += 1;
    }
}

struct Tape {
    cassette: Arc<Cassette>,
    workload: String,
    rep: u32,
    inner: Option<Arc<dyn Provider>>,
    model: String,
    seq: AtomicU32,
    /// Replay: the run's replies not served yet, in recorded order.
    pending: Mutex<Vec<Option<Entry>>>,
}

impl Tape {
    fn replay(&self, key: &str, ids: &[String]) -> ModelResult<Completion> {
        let mut pending = self.pending.lock().unwrap();
        let exact = pending
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.key == key));
        let at = match exact {
            Some(i) => i,
            None => {
                self.cassette.mark_diverged(&self.workload, self.rep);
                pending
                    .iter()
                    .position(Option::is_some)
                    .ok_or_else(|| ProviderError::Api {
                        provider: "replay".into(),
                        detail: format!(
                            "the recording of {} rep {} has no more replies",
                            self.workload, self.rep
                        ),
                    })?
            }
        };
        let mut e = pending[at].take().expect("found above");
        if exact.is_some() {
            let map: Vec<(&String, &String)> =
                e.ids.iter().zip(ids).filter(|(a, b)| a != b).collect();
            if !map.is_empty() {
                let swap = |t: &str| {
                    map.iter()
                        .fold(t.to_string(), |t, (a, b)| t.replace(a.as_str(), b))
                };
                e.content = swap(&e.content);
                for c in &mut e.tool_calls {
                    if let Ok(v) = serde_json::from_str(&swap(&c.arguments.to_string())) {
                        c.arguments = v;
                    }
                }
            }
        }
        Ok(Completion {
            content: e.content,
            tool_calls: e.tool_calls,
            usage: e.usage,
        })
    }

    fn keep(&self, key: String, ids: Vec<String>, c: &Completion) {
        self.cassette.add(Entry {
            workload: self.workload.clone(),
            rep: self.rep,
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            key,
            model: self.model.clone(),
            content: c.content.clone(),
            tool_calls: c.tool_calls.clone(),
            usage: c.usage,
            ids,
        });
    }
}

#[async_trait]
impl Provider for Tape {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ModelResult<Completion> {
        let (key, ids) = fingerprint_ids(system, messages, tools);
        match &self.inner {
            Some(inner) if !self.cassette.is_replay() => {
                let c = inner.complete(system, messages, tools).await?;
                self.keep(key, ids, &c);
                Ok(c)
            }
            _ => self.replay(&key, &ids),
        }
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ModelResult<Completion> {
        let (key, ids) = fingerprint_ids(system, messages, tools);
        match &self.inner {
            Some(inner) if !self.cassette.is_replay() => {
                let c = inner
                    .complete_streaming(system, messages, tools, on_text)
                    .await?;
                self.keep(key, ids, &c);
                Ok(c)
            }
            _ => {
                let c = self.replay(&key, &ids)?;
                if !c.content.is_empty() {
                    on_text(&c.content);
                }
                Ok(c)
            }
        }
    }

    fn describe(&self) -> String {
        match &self.inner {
            Some(p) if !self.cassette.is_replay() => p.describe(),
            _ => format!("replay of {}", self.model),
        }
    }
    fn model_id(&self) -> String {
        self.model.clone()
    }
}

/// The request with run-specific ids replaced, hashed (FNV-1a, 64 bits, stable across builds).
pub fn fingerprint(system: &str, messages: &[Message], tools: &[ToolDef]) -> String {
    fingerprint_ids(system, messages, tools).0
}

/// [`fingerprint`], and the ids it replaced, in order of first appearance.
fn fingerprint_ids(system: &str, messages: &[Message], tools: &[ToolDef]) -> (String, Vec<String>) {
    let (text, ids) = normalize(&render(system, messages, tools));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (format!("{h:016x}"), ids)
}

/// Replace every run of 8 or more hex digits (with the dashes of a UUID) by `#`. Also
/// returns the replaced runs, each once, in order of first appearance.
fn normalize(s: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut ids: Vec<String> = Vec::new();
    let mut run = String::new();
    let mut flush = |run: &mut String, out: &mut String| {
        let hex = run.chars().filter(|c| c.is_ascii_hexdigit()).count();
        if hex >= 8 && run.chars().any(|c| c.is_ascii_digit()) {
            out.push('#');
            let id = run.trim_end_matches('-').to_string();
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in s.chars() {
        if c.is_ascii_hexdigit() || (c == '-' && !run.is_empty()) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    (out, ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_do_not_change_the_fingerprint() {
        let a = [Message::user(
            "read [inv:3f2a9c1e-77aa-4b1d-9a0e-1c2d3e4f5a6b] for t-1a2b3c4d",
        )];
        let b = [Message::user(
            "read [inv:9e8d7c6b-1111-4222-8333-944455556666] for t-9f8e7d6c",
        )];
        let c = [Message::user("read something else")];
        assert_eq!(fingerprint("s", &a, &[]), fingerprint("s", &b, &[]));
        assert_ne!(fingerprint("s", &a, &[]), fingerprint("s", &c, &[]));
        // Words made of hex letters stay as they are.
        assert_eq!(normalize("decade faced").0, "decade faced");
    }

    #[test]
    fn a_recording_replays_and_counts_divergence() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let live: Arc<dyn Provider> = Arc::new(
            ark_sdk::prelude::ark_model::MockProvider::new("m", vec![]).with_fallback(|r| {
                Completion {
                    content: format!("seen {}", r.messages.len()),
                    ..Default::default()
                }
            }),
        );
        let rec = Cassette::record();
        let p = rec.provider("w", 0, Some(live));
        let one = [Message::user("one")];
        let two = [Message::user("one"), Message::user("two")];
        rt.block_on(p.complete("s", &one, &[])).unwrap();
        rt.block_on(p.complete("s", &two, &[])).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        rec.save(&path).unwrap();

        let play = Cassette::load(&path).unwrap();
        let p = play.provider("w", 0, None);
        // Out of order but known: served by fingerprint, no divergence.
        assert_eq!(
            rt.block_on(p.complete("s", &two, &[])).unwrap().content,
            "seen 2"
        );
        assert_eq!(
            rt.block_on(p.complete("s", &one, &[])).unwrap().content,
            "seen 1"
        );
        assert_eq!(play.diverged("w", 0), 0);
        assert!(rt.block_on(p.complete("s", &one, &[])).is_err());
        assert_eq!(play.diverged("w", 0), 1);
    }
}
