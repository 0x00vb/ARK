//! Mail and Calendar on a Google account: the Gmail API (send) and the Calendar API (list and
//! add events on the primary calendar). The scopes are in `catalog.json`; reading mail needs
//! a restricted scope and waits for Google's verification (`docs/GOOGLE.md`).

use ark_core::{ActionDescriptor, RiskTier};
use ark_tools::{ActionOutput, Adapter, AdapterError, Result, RunCtx};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::Ctx;

const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1";
const CALENDAR: &str = "https://www.googleapis.com/calendar/v3";

pub const SEND: &str = "mail.send";
pub const EVENTS: &str = "calendar.events";
pub const CREATE: &str = "calendar.create";

pub struct Mail(pub(super) Ctx);
pub struct Calendar(pub(super) Ctx);

#[async_trait]
impl Adapter for Mail {
    fn name(&self) -> &str {
        "mail"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            SEND,
            "send an email from the person's account",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({
            "type": "object",
            "required": ["to", "subject", "body"],
            "properties": {
                "to": { "type": "array", "items": { "type": "string" }, "description": "email addresses" },
                "cc": { "type": "array", "items": { "type": "string" } },
                "subject": { "type": "string" },
                "body": { "type": "string", "description": "plain text" }
            }
        }))]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        if action != SEND {
            return Err(AdapterError::UnknownAction(action.into()));
        }
        let to = addresses(SEND, &input["to"])?;
        if to.is_empty() {
            return Err(AdapterError::bad_input(SEND, "`to` needs an address"));
        }
        let cc = addresses(SEND, &input["cc"])?;
        let subject = one_line(SEND, "subject", input["subject"].as_str().unwrap_or(""))?;
        let body = input["body"].as_str().unwrap_or("");
        let raw = message(&to, &cc, &subject, body, &[]);
        let url = format!("{}/users/me/messages/send", self.0.base(GMAIL));
        let sent = self
            .0
            .send(|http| http.post(&url).json(&json!({ "raw": raw })))
            .await?;
        Ok(ActionOutput::json(json!({
            "sent": true,
            "id": sent["id"],
            "to": to,
            "subject": subject,
        })))
    }
}

#[async_trait]
impl Adapter for Calendar {
    fn name(&self) -> &str {
        "calendar"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                EVENTS,
                "list events on the person's calendar between two times (default: the next 7 days)",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "RFC 3339 time, e.g. 2026-10-05T00:00:00Z" },
                    "to": { "type": "string", "description": "RFC 3339 time" },
                    "query": { "type": "string", "description": "words to look for in events" }
                }
            })),
            ActionDescriptor::new(
                CREATE,
                "add an event to the person's calendar; invited people get an email",
                RiskTier::Mutate,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["title", "start", "end"],
                "properties": {
                    "title": { "type": "string" },
                    "start": { "type": "string", "description": "RFC 3339 time with offset (2026-10-05T15:00:00-03:00), or a date (2026-10-05) for all day" },
                    "end": { "type": "string", "description": "same form as start" },
                    "time_zone": { "type": "string", "description": "IANA zone (America/Sao_Paulo) when start and end have no offset" },
                    "description": { "type": "string" },
                    "location": { "type": "string" },
                    "attendees": { "type": "array", "items": { "type": "string" }, "description": "email addresses to invite" }
                }
            })),
        ]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            EVENTS => self.events(&input).await,
            CREATE => self.create(&input).await,
            other => Err(AdapterError::UnknownAction(other.into())),
        }
    }
}

impl Calendar {
    async fn events(&self, input: &Value) -> Result<ActionOutput> {
        let now = OffsetDateTime::now_utc();
        let from = match input["from"].as_str() {
            Some(t) => t.to_string(),
            None => rfc3339(now),
        };
        let to = match input["to"].as_str() {
            Some(t) => t.to_string(),
            None => rfc3339(now + time::Duration::days(7)),
        };
        let url = format!("{}/calendars/primary/events", self.0.base(CALENDAR));
        let mut query = vec![
            ("timeMin", from.clone()),
            ("timeMax", to.clone()),
            ("singleEvents", "true".into()),
            ("orderBy", "startTime".into()),
            ("maxResults", "50".into()),
        ];
        if let Some(q) = input["query"].as_str().filter(|q| !q.trim().is_empty()) {
            query.push(("q", q.to_string()));
        }
        let page = self.0.send(|http| http.get(&url).query(&query)).await?;
        let events: Vec<Value> = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                json!({
                    "id": e["id"],
                    "title": e["summary"].as_str().unwrap_or("(no title)"),
                    "start": when(&e["start"]),
                    "end": when(&e["end"]),
                    "location": e["location"].as_str().unwrap_or(""),
                    "attendees": e["attendees"].as_array().into_iter().flatten()
                        .filter_map(|a| a["email"].as_str()).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(ActionOutput::json(
            json!({ "from": from, "to": to, "events": events }),
        ))
    }

    async fn create(&self, input: &Value) -> Result<ActionOutput> {
        let title = one_line(CREATE, "title", input["title"].as_str().unwrap_or(""))?;
        if title.is_empty() {
            return Err(AdapterError::bad_input(CREATE, "an event needs a `title`"));
        }
        let zone = input["time_zone"].as_str();
        let start = time_field(input["start"].as_str().unwrap_or(""), zone)?;
        let end = time_field(input["end"].as_str().unwrap_or(""), zone)?;
        let attendees = addresses(CREATE, &input["attendees"])?;
        let mut event = json!({ "summary": title, "start": start, "end": end });
        for k in ["description", "location"] {
            if let Some(v) = input[k].as_str() {
                event[k] = json!(v);
            }
        }
        if !attendees.is_empty() {
            event["attendees"] = json!(attendees
                .iter()
                .map(|a| json!({ "email": a }))
                .collect::<Vec<_>>());
        }
        let url = format!("{}/calendars/primary/events", self.0.base(CALENDAR));
        let send_updates = if attendees.is_empty() { "none" } else { "all" };
        let made = self
            .0
            .send(|http| {
                http.post(&url)
                    .query(&[("sendUpdates", send_updates)])
                    .json(&event)
            })
            .await?;
        Ok(ActionOutput::json(json!({
            "created": true,
            "id": made["id"],
            "link": made["htmlLink"],
            "title": title,
            "start": when(&made["start"]),
            "end": when(&made["end"]),
            "attendees": attendees,
        })))
    }
}

/// The evidence card of a Mail or Calendar result. `None` for other actions.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| out[k].as_str().unwrap_or("");
    match action {
        SEND => Some((
            format!("sent \"{}\" to {}", s("subject"), list(&out["to"])),
            vec![],
        )),
        EVENTS => {
            let events = out["events"].as_array()?;
            let mut text = format!(
                "{} event(s) from {} to {}",
                events.len(),
                s("from"),
                s("to")
            );
            for e in events.iter().take(25) {
                let f = |k: &str| e[k].as_str().unwrap_or("");
                text.push_str(&format!(
                    "\n- {} to {}: {}",
                    f("start"),
                    f("end"),
                    f("title")
                ));
                if !f("location").is_empty() {
                    text.push_str(&format!(" ({})", f("location")));
                }
            }
            if events.len() > 25 {
                text.push_str("\n(more: get_evidence on this result, path \"events\")");
            }
            Some((text, vec![]))
        }
        CREATE => Some((
            format!(
                "added \"{}\", {} to {}{}",
                s("title"),
                s("start"),
                s("end"),
                match list(&out["attendees"]) {
                    a if a.is_empty() => String::new(),
                    a => format!(", invited {a}"),
                }
            ),
            vec![s("link").to_string()],
        )),
        _ => None,
    }
}

fn list(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Email addresses from a string or a list. Line breaks are refused: they would add headers.
pub(super) fn addresses(action: &str, v: &Value) -> Result<Vec<String>> {
    let raw: Vec<String> = match v {
        Value::Null => vec![],
        Value::String(s) => s.split(',').map(str::to_string).collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => return Err(AdapterError::bad_input(action, "addresses are text")),
    };
    let mut out = Vec::new();
    for a in raw.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        if a.contains(['\r', '\n']) || !a.contains('@') {
            return Err(AdapterError::bad_input(
                action,
                format!("`{a}` is not an email address"),
            ));
        }
        out.push(a.to_string());
    }
    Ok(out)
}

pub(super) fn one_line(action: &str, field: &str, s: &str) -> Result<String> {
    if s.contains(['\r', '\n']) {
        return Err(AdapterError::bad_input(
            action,
            format!("`{field}` must be one line"),
        ));
    }
    Ok(s.trim().to_string())
}

/// An RFC 2822 message, base64url as the Gmail API takes it. Non-ASCII subjects are encoded
/// (RFC 2047); the body travels as base64 UTF-8.
pub(super) fn message(
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let subject = if subject.is_ascii() {
        subject.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", b64.encode(subject))
    };
    let mut m = format!("To: {}\r\n", to.join(", "));
    if !cc.is_empty() {
        m.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
    }
    for (name, value) in headers {
        m.push_str(&format!("{name}: {value}\r\n"));
    }
    m.push_str(&format!(
        "Subject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
    ));
    let encoded = b64.encode(body.as_bytes());
    for chunk in encoded.as_bytes().chunks(76) {
        m.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        m.push_str("\r\n");
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(m.as_bytes())
}

/// A start or end as the Calendar API takes it: a date, or a time with an offset or a zone.
fn time_field(s: &str, zone: Option<&str>) -> Result<Value> {
    let s = s.trim();
    if s.len() == 10 && s.as_bytes()[4] == b'-' {
        return Ok(json!({ "date": s }));
    }
    if OffsetDateTime::parse(s, &Rfc3339).is_ok() {
        return Ok(json!({ "dateTime": s }));
    }
    match zone {
        Some(z) if s.contains('T') => Ok(json!({ "dateTime": s, "timeZone": z })),
        _ => Err(AdapterError::bad_input(
            CREATE,
            format!("`{s}` needs an offset (2026-10-05T15:00:00-03:00) or a `time_zone`"),
        )),
    }
}

fn when(v: &Value) -> String {
    v["dateTime"]
        .as_str()
        .or(v["date"].as_str())
        .unwrap_or("")
        .to_string()
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_dates_offsets_or_zoned() {
        assert_eq!(
            time_field("2026-10-05", None).ok().unwrap(),
            json!({ "date": "2026-10-05" })
        );
        assert_eq!(
            time_field("2026-10-05T15:00:00-03:00", None).ok().unwrap(),
            json!({ "dateTime": "2026-10-05T15:00:00-03:00" })
        );
        assert_eq!(
            time_field("2026-10-05T15:00:00", Some("America/Sao_Paulo"))
                .ok()
                .unwrap(),
            json!({ "dateTime": "2026-10-05T15:00:00", "timeZone": "America/Sao_Paulo" })
        );
        assert!(time_field("2026-10-05T15:00:00", None).is_err());
    }

    #[test]
    fn a_non_ascii_subject_is_encoded() {
        let raw = message(&["a@example.com".into()], &[], "Relatório", "Olá", &[]);
        let mime = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .unwrap();
        let mime = String::from_utf8(mime).unwrap();
        assert!(mime.contains("Subject: =?UTF-8?B?"), "{mime}");
        assert!(!mime.contains("Relatório"));
    }

    #[test]
    fn calendar_cards_list_events_and_say_who_was_invited() {
        let out = json!({ "from": "a", "to": "b", "events": [
            { "title": "Standup", "start": "2026-10-05T09:00:00Z", "end": "2026-10-05T09:15:00Z", "location": "Room 2" }
        ]});
        let (card, _) = distill(EVENTS, &out).unwrap();
        assert_eq!(card, "1 event(s) from a to b\n- 2026-10-05T09:00:00Z to 2026-10-05T09:15:00Z: Standup (Room 2)");
        let made = json!({ "title": "Review", "start": "s", "end": "e", "attendees": ["bob@example.com"], "link": "https://cal/x" });
        let (card, ids) = distill(CREATE, &made).unwrap();
        assert_eq!(card, "added \"Review\", s to e, invited bob@example.com");
        assert_eq!(ids, vec!["https://cal/x"]);
    }
}
