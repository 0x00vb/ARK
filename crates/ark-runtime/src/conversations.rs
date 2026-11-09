//! Recall of a scope's other conversations, on the model's request. A deterministic word match
//! over their titles, gists and stored turns finds them; a bounded excerpt reads one. Nothing is
//! carried into a turn unless the model calls it, and no other scope is ever read.
use std::{collections::BTreeSet, sync::Arc};

use ark_core::{ActionDescriptor, ProjectId, RiskTier, SessionId};
use ark_store::Store;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde_json::{json, Value};

use crate::{distill::Distiller, memory::words, AgentError, SessionRecord};

pub const SEARCH: &str = "conversations.search";
pub const READ: &str = "conversations.read";

/// Newest conversations a search scans, and the newest turns of each.
const SCAN_SESSIONS: usize = 200;
const SCAN_TURNS: usize = 40;
const HITS: usize = 5;
const SNIPPET_CHARS: usize = 400;
/// The whole search result, serialized.
const SEARCH_CHARS: usize = 2_000;
/// The whole read result, serialized.
const READ_CHARS: usize = 4_000;
/// A snippet shorter than this is not worth a hit.
const MIN_SNIPPET: usize = 80;
/// What a read without a turn keeps of the gists, and of each turn.
const GIST_CHARS: usize = 1_200;
const TURN_CHARS: usize = 1_500;
const READ_TURNS: usize = 8;

/// A person's own conversation: not a work item's, not run by an execution coordinator, not
/// bound to an execution scope.
pub fn is_conversation(record: &SessionRecord) -> bool {
    record.work_item.is_none() && !record.execution_managed && record.execution_scope.is_none()
}

/// The conversations of `scope` other than `current`, newest first. A record that is not a
/// session header is no conversation; it is logged and left out, so it never stops a turn.
fn others(
    store: &Store,
    scope: &ProjectId,
    current: &SessionId,
) -> Result<Vec<SessionRecord>, AgentError> {
    let mut list = Vec::new();
    for value in store.list_sessions(Some(scope))? {
        let record: SessionRecord = match serde_json::from_value(value) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(%error, "a stored session record is unreadable; recall skips it");
                continue;
            }
        };
        if &record.project == scope && &record.id != current && is_conversation(&record) {
            list.push(record);
        }
    }
    Ok(list)
}

/// Whether `scope` holds a conversation other than `exclude` with a stored turn: the condition
/// for offering [`ConversationsTool`].
pub fn has_other_conversations(
    store: &Store,
    scope: &ProjectId,
    exclude: &SessionId,
) -> Result<bool, AgentError> {
    Ok(others(store, scope, exclude)?
        .iter()
        .any(|r| r.turn_count > 0))
}

/// `conversations.search` and `conversations.read` over one scope's other conversations.
pub struct ConversationsTool {
    store: Arc<Store>,
    scope: ProjectId,
    current: SessionId,
}

impl ConversationsTool {
    /// `scope`: the project whose conversations are searched (an agent's scope, or the top
    /// project of a general chat). `current`: the conversation the turn runs in, never returned.
    pub fn new(store: Arc<Store>, scope: ProjectId, current: SessionId) -> Self {
        Self {
            store,
            scope,
            current,
        }
    }

    fn search(&self, query: &str) -> Result<Value, AgentError> {
        let query = words(query);
        if query.is_empty() {
            return Err(AgentError::NotAllowed(
                "the query has no words to match".into(),
            ));
        }
        let mut ranked = Vec::new();
        for record in others(&self.store, &self.scope, &self.current)?
            .into_iter()
            .take(SCAN_SESSIONS)
        {
            let title = overlap(&record.title, &query);
            let mut best: Option<(usize, String, Option<u32>)> = None;
            let mut keep = |score: usize, text: String, turn: Option<u32>| {
                if score > 0 && best.as_ref().is_none_or(|(s, _, _)| score >= *s) {
                    best = Some((score, text, turn));
                }
            };
            for gist in &record.gists {
                keep(overlap(gist, &query), gist.clone(), None);
            }
            for turn in self
                .store
                .list_session_turns(record.id.as_str(), Some(SCAN_TURNS))?
            {
                let text = turn_text(&turn);
                keep(overlap(&text, &query), text, turn_index(&turn));
            }
            let score = title * 2 + best.as_ref().map_or(0, |(s, _, _)| *s);
            if score == 0 {
                continue;
            }
            let (snippet, turn) = match best {
                Some((_, text, turn)) => (excerpt(&text, &query, SNIPPET_CHARS), turn),
                None => (window(&flat(&record.title), 0, SNIPPET_CHARS), None),
            };
            ranked.push((score, record, snippet, turn));
        }
        // Stable: equal scores stay newest first.
        ranked.sort_by_key(|hit| std::cmp::Reverse(hit.0));
        let mut hits: Vec<Value> = Vec::new();
        'hits: for (_, record, mut snippet, turn) in ranked.into_iter().take(HITS) {
            loop {
                let mut hit = json!({
                    "conversation": record.id.as_str(),
                    "title": window(&flat(&record.title), 0, 100),
                    "date": record.updated_at.date().to_string(),
                    "snippet": snippet,
                });
                if let Some(turn) = turn {
                    hit["turn"] = json!(turn);
                }
                hits.push(hit);
                let size = chars(&json!({ "hits": hits }));
                if size <= SEARCH_CHARS {
                    break;
                }
                hits.pop();
                let len = snippet.chars().count();
                let over = size - SEARCH_CHARS;
                if len < over + MIN_SNIPPET {
                    break 'hits;
                }
                snippet = window(&snippet, 0, len - over);
            }
        }
        Ok(json!({ "hits": hits }))
    }

    fn read(&self, id: &str, turn: Option<u32>) -> Result<Value, AgentError> {
        let record = self
            .store
            .resolve_session(id)?
            .map(serde_json::from_value::<SessionRecord>)
            .transpose()?;
        let Some(record) = record else {
            return Err(not_here(id));
        };
        if record.id == self.current {
            return Err(AgentError::NotAllowed(
                "that is this conversation: its recent turns are already here".into(),
            ));
        }
        if record.project != self.scope || !is_conversation(&record) {
            return Err(not_here(id));
        }
        let text = match turn {
            Some(index) => {
                let found = self
                    .store
                    .list_session_turns_before(record.id.as_str(), Some(index + 1), 1)?
                    .into_iter()
                    .find(|t| turn_index(t) == Some(index));
                let Some(found) = found else {
                    return Err(AgentError::NotAllowed(format!(
                        "conversation `{}` has no turn {index}; it has {} turns, numbered from 0",
                        record.id, record.turn_count
                    )));
                };
                turn_text(&found)
            }
            None => self.overview(&record)?,
        };
        let wrap = |text: &str| {
            let mut out = json!({
                "conversation": record.id.as_str(),
                "title": window(&flat(&record.title), 0, 100),
                "date": record.updated_at.date().to_string(),
                "turns": record.turn_count,
                "text": text,
            });
            if let Some(turn) = turn {
                out["turn"] = json!(turn);
            }
            out
        };
        Ok(fitted(text, READ_CHARS, wrap))
    }

    /// The gists (newest kept) and the last turns (newest kept), oldest first.
    fn overview(&self, record: &SessionRecord) -> Result<String, AgentError> {
        let mut text = String::new();
        if !record.gists.is_empty() {
            let mut kept: Vec<String> = Vec::new();
            let mut used = 0;
            for gist in record.gists.iter().rev() {
                let line = format!("- {}\n", flat(gist));
                used += line.chars().count();
                if used > GIST_CHARS {
                    break;
                }
                kept.push(line);
            }
            kept.reverse();
            text.push_str("Earlier, in short:\n");
            text.extend(kept);
            text.push('\n');
        }
        let mut turns: Vec<String> = Vec::new();
        let mut used = text.chars().count();
        for turn in self
            .store
            .list_session_turns(record.id.as_str(), Some(READ_TURNS))?
            .iter()
            .rev()
        {
            let block = format!(
                "[turn {}] {}\n\n",
                turn_index(turn).map_or_else(|| "?".into(), |i| i.to_string()),
                window(&turn_text(turn), 0, TURN_CHARS)
            );
            used += block.chars().count();
            if used > READ_CHARS && !turns.is_empty() {
                break;
            }
            turns.push(block);
        }
        turns.reverse();
        text.extend(turns);
        Ok(text.trim_end().to_string())
    }
}

#[async_trait::async_trait]
impl Adapter for ConversationsTool {
    fn name(&self) -> &str {
        "conversations"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                SEARCH,
                "search your past chats (not this one): up to 5 excerpts",
                RiskTier::Observe,
            )
            .with_input_schema(json!({"type":"object","additionalProperties":false,"required":["query"],"properties":{"query":{"type":"string","maxLength":200}}})),
            ActionDescriptor::new(
                READ,
                "read a past chat found by search: one turn, or its summary",
                RiskTier::Observe,
            )
            .with_input_schema(json!({"type":"object","additionalProperties":false,"required":["conversation"],"properties":{"conversation":{"type":"string"},"turn":{"type":"integer","minimum":0}}})),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let fields: &[&str] = match action {
            SEARCH => &["query"],
            READ => &["conversation", "turn"],
            _ => return Err(AdapterError::UnknownAction(action.into())),
        };
        if input
            .as_object()
            .is_none_or(|o| o.keys().any(|k| !fields.contains(&k.as_str())))
        {
            return Err(AdapterError::bad_input(action, "unknown field"));
        }
        let result = if action == SEARCH {
            let Some(query) = input["query"].as_str() else {
                return Err(AdapterError::bad_input(action, "query must be text"));
            };
            self.search(query)
        } else {
            let Some(id) = input["conversation"].as_str().filter(|s| !s.is_empty()) else {
                return Err(AdapterError::bad_input(
                    action,
                    "conversation must be an id from conversations.search",
                ));
            };
            let turn = match &input["turn"] {
                Value::Null => None,
                v => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
                    Some(n) => Some(n),
                    None => {
                        return Err(AdapterError::bad_input(
                            action,
                            "turn must be a number from 0",
                        ))
                    }
                },
            };
            self.read(id, turn)
        };
        match result {
            Ok(value) => Ok(ActionOutput::json(value)),
            Err(AgentError::NotAllowed(why)) => Err(AdapterError::bad_input(action, why)),
            Err(e) => Err(AdapterError::Tool(e.to_string())),
        }
    }
}

fn not_here(id: &str) -> AgentError {
    AgentError::NotAllowed(format!(
        "no other conversation `{id}` here; use an id from conversations.search"
    ))
}

fn overlap(text: &str, query: &BTreeSet<String>) -> usize {
    words(text).intersection(query).count()
}

fn turn_index(turn: &Value) -> Option<u32> {
    turn["index"].as_u64().and_then(|n| u32::try_from(n).ok())
}

fn turn_text(turn: &Value) -> String {
    format!(
        "Person: {}\nReply: {}",
        turn["input"].as_str().unwrap_or(""),
        turn["answer"].as_str().unwrap_or("")
    )
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn chars(value: &Value) -> usize {
    value.to_string().chars().count()
}

/// At most `max` characters of `text` on one line, starting a little before its first word in
/// `query`.
fn excerpt(text: &str, query: &BTreeSet<String>, max: usize) -> String {
    let flat = flat(text);
    let mut offset = 0;
    let mut at = 0;
    for token in flat.split(' ') {
        if !words(token).is_disjoint(query) {
            at = offset;
            break;
        }
        offset += token.chars().count() + 1;
    }
    window(&flat, at.saturating_sub(max / 4), max)
}

/// At most `max` characters of `text` from character `start`, moved back so the window is full;
/// `…` marks text left out at either end.
fn window(text: &str, start: usize, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let start = start.min(chars.len() - max);
    let (mut lo, mut hi) = (start, start + max);
    let head = lo > 0;
    let tail = hi < chars.len();
    if head {
        lo += 1;
    }
    if tail {
        hi = hi.saturating_sub(1).max(lo);
    }
    let mut out = String::new();
    if head {
        out.push('…');
    }
    out.extend(&chars[lo..hi]);
    if tail {
        out.push('…');
    }
    out
}

/// `wrap(text)` with `text` cut until the serialized value has at most `cap` characters.
fn fitted(mut text: String, cap: usize, wrap: impl Fn(&str) -> Value) -> Value {
    loop {
        let value = wrap(&text);
        let size = chars(&value);
        let len = text.chars().count();
        if size <= cap || len == 0 {
            return value;
        }
        text = window(&text, 0, len.saturating_sub(size - cap));
    }
}

/// What the model sees of its other chats: the excerpts and the chat it read, whole. Both are
/// bounded where they are made (2,000 and 4,000 characters), so nothing more is cut here.
pub fn distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match action {
            SEARCH => {
                let hits = out
                    .get("hits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if hits.is_empty() {
                    return Some(("No earlier chat matches.".to_string(), Vec::new()));
                }
                let mut text =
                    String::from("Earlier chats that match (conversations.read opens one):\n");
                let mut ids = Vec::new();
                for hit in &hits {
                    let turn = hit
                        .get("turn")
                        .and_then(Value::as_u64)
                        .map(|t| format!(", turn {t}"))
                        .unwrap_or_default();
                    text.push_str(&format!(
                        "- {} \"{}\" ({}{}): {}\n",
                        s(hit, "conversation"),
                        s(hit, "title"),
                        s(hit, "date"),
                        turn,
                        s(hit, "snippet")
                    ));
                    ids.push(s(hit, "conversation"));
                }
                Some((text, ids))
            }
            READ => Some((
                format!(
                    "Chat {} \"{}\" ({}):\n{}",
                    s(out, "conversation"),
                    s(out, "title"),
                    s(out, "date"),
                    s(out, "text")
                ),
                vec![s(out, "conversation")],
            )),
            _ => None,
        }
    })
}
