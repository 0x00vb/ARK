//! Slash commands. A command runs against the [`Interaction`] and returns a [`Reply`]: a
//! [`Report`] to show and, rarely, an [`Effect`] the front-end carries out. Generic commands
//! (`/context`, `/sessions`, ...) and a consumer's domain commands use the same trait, so they
//! work unchanged in the terminal, the GUI and an API.

use serde::{Deserialize, Serialize};

use crate::interaction::Interaction;
use crate::report::Report;

/// Something only the front-end can do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "text", rename_all = "snake_case")]
pub enum Effect {
    /// Send this text to the agent as if the person typed it.
    Submit(String),
    /// Clear the visible conversation. The session and its context are untouched.
    ClearView,
    /// Leave the app. The session is saved.
    Exit,
    /// Open this chat, held on another computer (ADR 0052): a front-end that reaches its place
    /// opens it there.
    Open(String),
}

/// A command's result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub report: Report,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
}

impl Reply {
    pub fn report(report: Report) -> Self {
        Reply {
            report,
            effect: None,
        }
    }
    /// Nothing to show.
    pub fn none() -> Self {
        Reply::default()
    }
    pub fn submit(text: impl Into<String>) -> Self {
        Reply {
            report: Report::new(),
            effect: Some(Effect::Submit(text.into())),
        }
    }
    pub fn effect(mut self, e: Effect) -> Self {
        self.effect = Some(e);
        self
    }
}

impl From<Report> for Reply {
    fn from(r: Report) -> Self {
        Reply::report(r)
    }
}

/// A slash command. Implement this for domain commands (`/chapter`, `/deploy`, ...).
#[async_trait::async_trait]
pub trait Command: Send + Sync {
    /// The name without the slash.
    fn name(&self) -> &str;
    /// One line for `/help` and completion.
    fn summary(&self) -> &str;
    /// Argument hint, e.g. `<session>`.
    fn usage(&self) -> &str {
        ""
    }
    fn aliases(&self) -> &[&str] {
        &[]
    }
    /// The `/help` group it is listed under.
    fn group(&self) -> &str {
        "Commands"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply>;
}

/// A command's metadata, for help, completion and command palettes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    pub summary: String,
    pub usage: String,
    pub aliases: Vec<String>,
    pub group: String,
}

impl CommandInfo {
    pub fn of(c: &dyn Command) -> Self {
        CommandInfo {
            name: c.name().to_string(),
            summary: c.summary().to_string(),
            usage: c.usage().to_string(),
            aliases: c.aliases().iter().map(|a| a.to_string()).collect(),
            group: c.group().to_string(),
        }
    }
}

/// Split `/name rest of line` into `("name", "rest of line")`.
pub fn parse(line: &str) -> Option<(&str, &str)> {
    let body = line.trim_start().strip_prefix('/')?;
    if body.is_empty() || body.starts_with(' ') {
        return None;
    }
    Some(match body.split_once(char::is_whitespace) {
        Some((n, rest)) => (n, rest.trim()),
        None => (body.trim_end(), ""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_name_and_args() {
        assert_eq!(parse("/resume 8f3a"), Some(("resume", "8f3a")));
        assert_eq!(parse("/help"), Some(("help", "")));
        assert_eq!(
            parse("/model  anthropic  claude"),
            Some(("model", "anthropic  claude"))
        );
        assert_eq!(parse("/ not a command"), None);
        assert_eq!(parse("plain text"), None);
        assert_eq!(parse("/"), None);
    }

    #[test]
    fn effects_serialize_compactly() {
        let r = Reply::submit("read chapter 4");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["effect"]["type"], "submit");
        assert_eq!(v["effect"]["text"], "read chapter 4");
        let e: Effect = serde_json::from_value(serde_json::json!({ "type": "exit" })).unwrap();
        assert_eq!(e, Effect::Exit);
    }
}
