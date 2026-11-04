//! The inbox of a Google account (ADR 0062, `docs/AUTOMATIONS.md` › Mail): [`Feed`] polls new
//! inbound mail through `users.history.list` for event automations, and [`Inbox`] reads a
//! message and replies in its thread. Reading needs Gmail's restricted read scope, so it is its
//! own capability and never rides on Mail's send-only consent.
//!
//! | Rule | Why |
//! |---|---|
//! | The first poll saves the mailbox's current history id and returns nothing | old mail never runs |
//! | History is paged to the end of whole pages before the cursor moves | a page is never half read |
//! | Only mail in INBOX that this account did not send becomes an event | its own replies never start a run |
//! | An expired history id is a gap, never a silent jump | the person decides how to recover |
//! | The sender is the literal address in `From`, lowercased | the UI says the exact address, not a verified person |
//! | A reply goes only to an address that wrote in the thread, threaded by its headers | an approved reply cannot be pointed elsewhere |
//! | A send with no answer is an unconfirmed effect, never retried | Gmail send has no idempotency key |

use std::collections::BTreeMap;
use std::time::Duration;

use ark_core::{ActionDescriptor, ExternalEffect, ExternalEffectState, RiskTier};
use ark_tools::events::{Batch, Direction, EventSource, ExternalEvent, Limits, SourceError};
use ark_tools::{ActionOutput, Adapter, AdapterError, Result, RunCtx};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};

use super::google::{addresses, message, one_line};
use super::{CallError, Ctx};

const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1";

pub const READ: &str = "inbox.read";
pub const REPLY: &str = "inbox.reply";
/// The event source name of Gmail feeds.
pub const SOURCE: &str = "gmail";

/// Text of one message given to a run or a read, at most.
const TEXT_MAX: usize = 64 << 10;
/// History and message pages read in one poll or reconcile, at most.
const PAGES: usize = 50;
/// How much earlier than a gap's start a reconcile looks.
const RECONCILE_MARGIN_MS: u64 = 10 * 60 * 1000;
/// How long a send may take before its outcome counts as unknown.
const SEND_WAIT: Duration = Duration::from_secs(60);

pub struct Inbox(pub(super) Ctx);

#[async_trait]
impl Adapter for Inbox {
    fn name(&self) -> &str {
        "inbox"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(READ, "read one message of the person's inbox", RiskTier::Observe)
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": { "message": { "type": "string", "description": "the message id" } }
                })),
            ActionDescriptor::new(
                REPLY,
                "reply in a mail thread to someone who wrote in it",
                RiskTier::Mutate,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["to", "thread", "body"],
                "properties": {
                    "to": { "type": "string", "description": "the address that wrote in the thread" },
                    "thread": { "type": "string", "description": "the thread id" },
                    "body": { "type": "string", "description": "plain text" }
                }
            })),
        ]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            READ => self.read(&input).await,
            REPLY => self.reply(&input).await,
            _ => Err(AdapterError::UnknownAction(action.into())),
        }
    }
}

impl Inbox {
    async fn read(&self, input: &Value) -> Result<ActionOutput> {
        let id = id_input(READ, "message", input)?;
        let url = format!("{}/users/me/messages/{id}?format=full", self.0.base(GMAIL));
        let m = self.0.send(|http| http.get(&url)).await?;
        let h = Headers::of(&m);
        let mut text = body_text(&m);
        cut(&mut text, TEXT_MAX);
        Ok(ActionOutput::json(json!({
            "id": m["id"],
            "thread": m["threadId"],
            "from": h.get("from"),
            "to": h.get("to"),
            "subject": h.get("subject"),
            "date": h.get("date"),
            "text": text,
        })))
    }

    async fn reply(&self, input: &Value) -> Result<ActionOutput> {
        let to = addresses(REPLY, &input["to"])?;
        let [to] = to.as_slice() else {
            return Err(AdapterError::bad_input(REPLY, "`to` is one address"));
        };
        let to = canonical(to).ok_or_else(|| {
            AdapterError::bad_input(REPLY, format!("`{to}` is not an email address"))
        })?;
        let thread = id_input(REPLY, "thread", input)?;
        let body = input["body"].as_str().unwrap_or("");
        if body.trim().is_empty() {
            return Err(AdapterError::bad_input(REPLY, "`body` is empty"));
        }
        let base = self.0.base(GMAIL).to_string();
        let url = format!(
            "{base}/users/me/threads/{thread}?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Message-ID&metadataHeaders=References"
        );
        let t = self.0.send(|http| http.get(&url)).await?;
        let last = t["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .filter(|m| {
                !m["labelIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|l| matches!(l.as_str(), Some("SPAM" | "DRAFT" | "SENT")))
            })
            .map(Headers::of)
            .find(|h| h.get("from").and_then(|f| canonical(&f)).as_deref() == Some(&to))
            .ok_or_else(|| {
                AdapterError::bad_input(
                    REPLY,
                    format!(
                        "{to} did not write in this thread; a reply goes only to someone who did"
                    ),
                )
            })?;
        let subject = last.get("subject").unwrap_or_default();
        let subject = if subject.to_ascii_lowercase().starts_with("re:") {
            subject
        } else {
            format!("Re: {subject}")
        };
        let subject = one_line(REPLY, "subject", &subject)?;
        let parent = last.get("message-id").unwrap_or_default();
        let references = match last.get("references") {
            Some(r) if !parent.is_empty() => format!("{r} {parent}"),
            Some(r) => r,
            None => parent.clone(),
        };
        let mut headers = Vec::new();
        if !parent.is_empty() {
            headers.push(("In-Reply-To", parent.as_str()));
            headers.push(("References", references.as_str()));
        }
        let raw = message(std::slice::from_ref(&to), &[], &subject, body, &headers);
        let url = format!("{base}/users/me/messages/send");
        let sent = self
            .0
            .call(|http| {
                http.post(&url)
                    .timeout(SEND_WAIT)
                    .json(&json!({ "raw": raw, "threadId": thread }))
            })
            .await;
        match sent {
            Ok(v) => Ok(ActionOutput::json(json!({
                "sent": true, "id": v["id"], "to": to, "thread": thread, "subject": subject,
            }))),
            Err(CallError::Network { sent: true, .. })
            | Err(CallError::Status {
                status: 408 | 500..=599,
                ..
            }) => {
                let effect = format!("{REPLY}:{thread}:{to}");
                let says = format!(
                    "The reply to {to} may or may not have been sent. Check the account's sent mail before sending it again."
                );
                Ok(ActionOutput::json(json!({
                    "status": "submitted_unconfirmed",
                    "effect": { "id": effect, "state": "unconfirmed", "retry_allowed": false, "user_message": says },
                    "to": to, "thread": thread, "subject": subject,
                }))
                .with_effect(ExternalEffect {
                    id: effect,
                    state: ExternalEffectState::Unconfirmed,
                    retry_allowed: false,
                    user_message: Some(says),
                }))
            }
            Err(e) => Err(AdapterError::Tool(e.to_string())),
        }
    }
}

/// The evidence card of an inbox result.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| out[k].as_str().unwrap_or("");
    match action {
        READ => Some((
            format!(
                "mail from {}: \"{}\"\n{}",
                s("from"),
                s("subject"),
                s("text")
            ),
            vec![],
        )),
        REPLY if out["sent"] == true => Some((
            format!("replied \"{}\" to {}", s("subject"), s("to")),
            vec![],
        )),
        REPLY => Some((
            format!("the reply to {} may not have been sent", s("to")),
            vec![],
        )),
        _ => None,
    }
}

/// New inbound mail of one account, as events.
pub struct Feed {
    ctx: Ctx,
    /// The account's address: events name it.
    address: String,
    limits: Limits,
}

impl Feed {
    pub(super) fn new(ctx: Ctx, address: String) -> Self {
        Feed {
            ctx,
            address,
            limits: Limits::default(),
        }
    }

    async fn get(&self, path: &str) -> std::result::Result<Value, CallError> {
        let url = format!("{}/users/me/{path}", self.ctx.base(GMAIL));
        self.ctx.call(|http| http.get(&url)).await
    }

    /// The mailbox's present history id.
    async fn present(&self) -> std::result::Result<String, SourceError> {
        let p = self.get("profile").await.map_err(source_error)?;
        history_id(&p["historyId"])
            .ok_or_else(|| SourceError::Transient("the profile has no history id".into()))
    }

    /// The messages as events: inbound mail in INBOX only. A message gone meanwhile is skipped.
    async fn events(
        &self,
        ids: Vec<String>,
    ) -> std::result::Result<Vec<ExternalEvent>, SourceError> {
        let mut out = Vec::new();
        for id in ids {
            match self.get(&format!("messages/{id}?format=full")).await {
                Ok(m) => {
                    if let Some(e) = event(&self.address, &m, &self.limits) {
                        out.push(e);
                    }
                }
                Err(CallError::Status { status: 404, .. }) => {}
                Err(e) => return Err(source_error(e)),
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl EventSource for Feed {
    fn source(&self) -> &str {
        SOURCE
    }

    fn account(&self) -> &str {
        &self.address
    }

    async fn poll(
        &self,
        cursor: Option<&str>,
        max: usize,
    ) -> std::result::Result<Batch, SourceError> {
        let Some(start) = cursor else {
            return Ok(Batch {
                events: Vec::new(),
                cursor: self.present().await?,
                more: false,
            });
        };
        let mut ids: Vec<String> = Vec::new();
        let mut page: Option<String> = None;
        let mut next_cursor = start.to_string();
        let mut more = false;
        for _ in 0..PAGES {
            let mut path = format!(
                "history?startHistoryId={start}&historyTypes=messageAdded&labelId=INBOX&maxResults=100"
            );
            if let Some(p) = &page {
                path.push_str(&format!("&pageToken={p}"));
            }
            let h = match self.get(&path).await {
                Ok(h) => h,
                Err(CallError::Status {
                    status: 404, why, ..
                }) => {
                    return Err(SourceError::Gap {
                        since_ms: None,
                        detail: format!(
                            "Gmail can no longer list changes since the last check ({why})"
                        ),
                    })
                }
                Err(e) => return Err(source_error(e)),
            };
            let mut last = None;
            for record in h["history"].as_array().into_iter().flatten() {
                last = history_id(&record["id"]).or(last);
                for added in record["messagesAdded"].as_array().into_iter().flatten() {
                    let m = &added["message"];
                    let Some(id) = m["id"].as_str() else { continue };
                    if inbound(m) && !ids.iter().any(|x| x == id) {
                        ids.push(id.to_string());
                    }
                }
            }
            page = h["nextPageToken"].as_str().map(str::to_string);
            match &page {
                None => {
                    next_cursor = history_id(&h["historyId"]).or(last).unwrap_or(next_cursor);
                    more = false;
                    break;
                }
                Some(_) if ids.len() >= max => {
                    next_cursor = last.unwrap_or(next_cursor);
                    more = true;
                    break;
                }
                Some(_) => {
                    if let Some(l) = last {
                        next_cursor = l;
                    }
                    more = true;
                }
            }
        }
        Ok(Batch {
            events: self.events(ids).await?,
            cursor: next_cursor,
            more,
        })
    }

    async fn reconcile(
        &self,
        since_ms: u64,
        _max: usize,
    ) -> std::result::Result<Batch, SourceError> {
        // The cursor first: what arrives while the old mail is listed comes with the next poll.
        let cursor = self.present().await?;
        // Mail can arrive late with an older date, and clocks differ: look a little earlier.
        // A message seen twice keeps its key, so the log keeps it once.
        let since_ms = since_ms.saturating_sub(RECONCILE_MARGIN_MS);
        let mut ids: Vec<String> = Vec::new();
        let mut page: Option<String> = None;
        for round in 0.. {
            if round == PAGES {
                return Err(SourceError::Transient(format!(
                    "more than {} messages arrived since then; a check cannot list them all, so start from now",
                    PAGES * 100
                )));
            }
            let mut path = format!(
                "messages?labelIds=INBOX&maxResults=100&q=after%3A{}",
                since_ms / 1000
            );
            if let Some(p) = &page {
                path.push_str(&format!("&pageToken={p}"));
            }
            let l = self.get(&path).await.map_err(source_error)?;
            for m in l["messages"].as_array().into_iter().flatten() {
                if let Some(id) = m["id"].as_str() {
                    if !ids.iter().any(|x| x == id) {
                        ids.push(id.to_string());
                    }
                }
            }
            page = l["nextPageToken"].as_str().map(str::to_string);
            if page.is_none() {
                break;
            }
        }
        let events = self
            .events(ids)
            .await?
            .into_iter()
            .filter(|e| e.at_ms >= since_ms)
            .collect();
        Ok(Batch {
            events,
            cursor,
            more: false,
        })
    }
}

fn source_error(e: CallError) -> SourceError {
    match e {
        CallError::SignIn(why) => SourceError::Disconnected(why),
        CallError::Status {
            status: 429,
            retry_after,
            ..
        } => SourceError::RateLimited {
            retry_at_ms: now_ms().saturating_add(retry_after.unwrap_or(60).saturating_mul(1000)),
        },
        CallError::Status {
            status: 403,
            why,
            retry_after,
        } if why.to_ascii_lowercase().contains("rate") => SourceError::RateLimited {
            retry_at_ms: now_ms().saturating_add(retry_after.unwrap_or(60).saturating_mul(1000)),
        },
        CallError::Status {
            status: 401 | 403,
            why,
            ..
        } => SourceError::Disconnected(why),
        e => SourceError::Transient(e.to_string()),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Gmail sends history ids as strings; accept numbers too.
fn history_id(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .filter(|s| !s.is_empty())
}

/// In INBOX and not sent, drafted or junked by this account.
fn inbound(m: &Value) -> bool {
    let labels: Vec<&str> = m["labelIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    labels.contains(&"INBOX")
        && !labels
            .iter()
            .any(|l| matches!(*l, "SENT" | "DRAFT" | "SPAM"))
}

/// One message (format `full`) as an event of `account`. `None`: not inbound mail.
fn event(account: &str, m: &Value, limits: &Limits) -> Option<ExternalEvent> {
    if !inbound(m) {
        return None;
    }
    let id = m["id"].as_str().filter(|s| !s.is_empty())?;
    let thread = m["threadId"].as_str().unwrap_or(id);
    let h = Headers::of(m);
    let from = h.get("from").and_then(|f| canonical(&f));
    let line = |s: String| -> String {
        let mut s: String = s
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        cut(&mut s, limits.field / 4);
        s
    };
    let mut fields = BTreeMap::new();
    fields.insert("thread".to_string(), line(thread.to_string()));
    if let Some(s) = h.get("subject") {
        fields.insert("subject".to_string(), line(s));
    }
    if let Some(s) = h.get("message-id") {
        fields.insert("message_id".to_string(), line(s));
    }
    if let Some(r) = h.get("reply-to").and_then(|r| canonical(&r)) {
        if Some(&r) != from.as_ref() {
            fields.insert("reply_to".to_string(), line(r));
        }
    }
    let mut text = body_text(m);
    cut(&mut text, limits.payload.min(TEXT_MAX));
    Some(ExternalEvent {
        source: SOURCE.into(),
        account: account.into(),
        key: line(id.to_string()),
        kind: "message".into(),
        direction: Direction::Inbound,
        label: match &from {
            Some(f) => line(format!("Mail from {f}")),
            None => "Mail from an address that could not be read".into(),
        },
        actor: from.map(line),
        subject: Some(line(thread.to_string())),
        at_ms: m["internalDate"]
            .as_str()
            .and_then(|d| d.parse().ok())
            .or_else(|| m["internalDate"].as_u64())
            .unwrap_or_else(now_ms),
        fields,
        payload: (!text.is_empty()).then_some(text),
    })
}

/// The address of a `From`-like header, lowercased: `Lara <Lara@Example.com>` is
/// `lara@example.com`. `None` when it is not one plain address.
fn canonical(header: &str) -> Option<String> {
    let s = header.trim();
    let addr = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    let addr = addr.trim().to_ascii_lowercase();
    let (user, domain) = addr.split_once('@')?;
    let plain = |p: &str| {
        !p.is_empty()
            && !p.contains('@')
            && !p
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | '"'))
    };
    (plain(user) && plain(domain) && domain.contains('.')).then_some(addr)
}

/// A message's headers, by lowercase name; the first of each.
struct Headers(BTreeMap<String, String>);

impl Headers {
    fn of(m: &Value) -> Headers {
        let mut out = BTreeMap::new();
        for h in m["payload"]["headers"].as_array().into_iter().flatten() {
            if let (Some(n), Some(v)) = (h["name"].as_str(), h["value"].as_str()) {
                out.entry(n.to_ascii_lowercase())
                    .or_insert_with(|| v.to_string());
            }
        }
        Headers(out)
    }
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// The plain text of a message: its first `text/plain` part, else its HTML without tags, else
/// Gmail's snippet.
fn body_text(m: &Value) -> String {
    fn find<'a>(p: &'a Value, mime: &str) -> Option<&'a Value> {
        if p["mimeType"].as_str() == Some(mime) && p["body"]["data"].is_string() {
            return Some(p);
        }
        p["parts"].as_array()?.iter().find_map(|c| find(c, mime))
    }
    let decode = |p: &Value| -> Option<String> {
        let data = p["body"]["data"].as_str()?.trim_end_matches('=');
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(data)
            .ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    let payload = &m["payload"];
    if let Some(t) = find(payload, "text/plain").and_then(decode) {
        return t;
    }
    if let Some(h) = find(payload, "text/html").and_then(decode) {
        let mut out = String::new();
        let mut tag = false;
        for c in h.chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        return out.trim().to_string();
    }
    m["snippet"].as_str().unwrap_or("").to_string()
}

/// Cut `s` to at most `max` bytes at a character boundary, saying so.
fn cut(s: &mut String, max: usize) {
    const MORE: &str = "\n[cut: the message is longer]";
    if s.len() <= max {
        return;
    }
    let mut end = max.saturating_sub(MORE.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str(MORE);
}

fn id_input(action: &str, field: &str, input: &Value) -> Result<String> {
    let v = input[field].as_str().unwrap_or("").trim();
    if v.is_empty()
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AdapterError::bad_input(
            action,
            format!("`{field}` is an id"),
        ));
    }
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_deadlines_saturate_for_429_and_rate_403() {
        for status in [429, 403] {
            let error = source_error(CallError::Status {
                status,
                why: "rate limit".into(),
                retry_after: Some(u64::MAX),
            });
            assert!(matches!(
                error,
                SourceError::RateLimited {
                    retry_at_ms: u64::MAX
                }
            ));
            let error = source_error(CallError::Status {
                status,
                why: "rate limit".into(),
                retry_after: Some(u64::MAX / 1000),
            });
            assert!(matches!(
                error,
                SourceError::RateLimited {
                    retry_at_ms: u64::MAX
                }
            ));
        }
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE.encode(s)
    }

    fn mail(labels: &[&str], from: &str, body: &str) -> Value {
        json!({
            "id": "m1", "threadId": "t1", "labelIds": labels, "internalDate": "1700000000000",
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [
                    { "name": "From", "value": from },
                    { "name": "Subject", "value": "Q4\r\nBcc: x@evil.example" },
                    { "name": "Message-ID", "value": "<abc@mail.example>" },
                    { "name": "Reply-To", "value": "other@example.com" }
                ],
                "parts": [
                    { "mimeType": "text/html", "body": { "data": b64("<p>html</p>") } },
                    { "mimeType": "text/plain", "body": { "data": b64(body) } }
                ]
            }
        })
    }

    #[test]
    fn senders_are_one_plain_lowercased_address() {
        assert_eq!(
            canonical("\"Lara B\" <Lara@Example.com>").as_deref(),
            Some("lara@example.com")
        );
        assert_eq!(
            canonical("lara@example.com").as_deref(),
            Some("lara@example.com")
        );
        for bad in [
            "Lara",
            "a@b",
            "a@@b.com",
            "a b@c.com",
            "<>",
            "x@y.com, z@w.com",
        ] {
            assert_eq!(canonical(bad), None, "{bad}");
        }
    }

    #[test]
    fn inbound_mail_becomes_an_event_with_one_line_fields() {
        let limits = Limits::default();
        let e = event(
            "office@example.com",
            &mail(
                &["INBOX", "UNREAD"],
                "Lara <LARA@example.com>",
                "the totals?",
            ),
            &limits,
        )
        .unwrap();
        assert_eq!(e.actor.as_deref(), Some("lara@example.com"));
        assert_eq!(e.subject.as_deref(), Some("t1"));
        assert_eq!(e.key, "m1");
        assert_eq!(e.at_ms, 1_700_000_000_000);
        assert_eq!(e.payload.as_deref(), Some("the totals?"));
        assert_eq!(e.fields["subject"], "Q4  Bcc: x@evil.example");
        assert_eq!(e.fields["reply_to"], "other@example.com");
        assert!(e.check(&limits).is_ok());
        assert!(event(
            "o",
            &mail(&["INBOX", "SENT"], "me@example.com", "x"),
            &limits
        )
        .is_none());
        assert!(event(
            "o",
            &mail(&["SPAM", "INBOX"], "lara@example.com", "x"),
            &limits
        )
        .is_none());
    }

    #[test]
    fn a_malformed_or_oversized_message_is_still_a_valid_event() {
        let limits = Limits {
            payload: 200,
            ..Limits::default()
        };
        let e = event(
            "o",
            &mail(&["INBOX"], "not an address", &"é".repeat(500)),
            &limits,
        )
        .unwrap();
        assert_eq!(e.actor, None);
        assert!(e.label.contains("could not be read"));
        let p = e.payload.as_deref().unwrap();
        assert!(
            p.len() <= 200 && p.ends_with("[cut: the message is longer]"),
            "{}",
            p.len()
        );
        assert!(e.check(&limits).is_ok());
        let mut bare = mail(&["INBOX"], "lara@example.com", "");
        bare["payload"]["parts"] = json!([]);
        bare["snippet"] = json!("just the snippet");
        let e = event("o", &bare, &limits).unwrap();
        assert_eq!(e.payload.as_deref(), Some("just the snippet"));
    }
}
