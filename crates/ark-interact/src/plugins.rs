//! MCP plugins as both front-ends change them: `/plugin` in a terminal, the Settings page in a
//! window. The plugins themselves are the engine's ([`ark_sdk::prelude::ark_runtime::Plugins`]);
//! this module only words the operations and keeps a slow server off the async runtime.
//!
//! None of these waits for a running turn. A change applies from the next one.

/// Provider-neutral marketplace views and state derivation.
pub mod catalog;

use std::collections::BTreeMap;
use std::sync::Arc;

use ark_sdk::prelude::ark_runtime::{MarketplacePlugin, PluginSpec, PluginStatus};
use ark_sdk::prelude::Engine;

use crate::report::{Cell, Report, Row, Table, Tone};

/// Every plugin and how it is.
pub fn list(engine: &Engine) -> Vec<PluginStatus> {
    engine.plugins().map(|p| p.list()).unwrap_or_default()
}

/// Whether this app can have plugins at all (it was opened on a store root).
pub fn supported(engine: &Engine) -> bool {
    engine.plugins().is_some()
}

/// Local records for marketplace entries the person has added. Add is intentionally inert until
/// the later account-wide provider connection completes.
pub fn marketplace(engine: &Engine) -> Vec<MarketplacePlugin> {
    engine
        .plugins()
        .map(|plugins| plugins.marketplace())
        .unwrap_or_default()
}

pub async fn add_marketplace(engine: Arc<Engine>, id: String) -> anyhow::Result<MarketplacePlugin> {
    let plugins = engine
        .plugins()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("this app has no plugins"))?;
    Ok(tokio::task::spawn_blocking(move || plugins.add_marketplace(&id)).await??)
}

pub async fn remove_marketplace(engine: Arc<Engine>, id: String) -> anyhow::Result<()> {
    let plugins = engine
        .plugins()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("this app has no plugins"))?;
    Ok(tokio::task::spawn_blocking(move || plugins.remove_marketplace(&id)).await??)
}

/// Start a server, list its tools and save it. Runs off the async runtime: a server may take
/// seconds to answer.
pub async fn add(engine: Arc<Engine>, spec: PluginSpec) -> anyhow::Result<PluginStatus> {
    Ok(tokio::task::spawn_blocking(move || engine.add_plugin(spec)).await??)
}

pub async fn remove(engine: Arc<Engine>, name: String) -> anyhow::Result<()> {
    let plugins = engine
        .plugins()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("this app has no plugins"))?;
    Ok(tokio::task::spawn_blocking(move || plugins.remove(&name)).await??)
}

pub async fn restart(engine: Arc<Engine>, name: String) -> anyhow::Result<PluginStatus> {
    let plugins = engine
        .plugins()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("this app has no plugins"))?;
    Ok(tokio::task::spawn_blocking(move || plugins.restart(&name)).await??)
}

/// A plugin from the words of `/plugin add`: a name, any `KEY=VALUE` settings, then the
/// command and its arguments. Double quotes keep spaces in one word.
///
/// `notes NOTES_DIR=/data python3 notes_server.py`
pub fn parse_add(line: &str) -> anyhow::Result<PluginSpec> {
    let words = split_words(line)?;
    let mut it = words.into_iter().peekable();
    let name = it.next().ok_or_else(|| {
        anyhow::anyhow!("usage: /plugin add <name> [KEY=VALUE…] <command> [args…]")
    })?;
    let mut env = BTreeMap::new();
    while let Some(w) = it.peek() {
        match w.split_once('=') {
            Some((k, v))
                if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                env.insert(k.to_string(), v.to_string());
                it.next();
            }
            _ => break,
        }
    }
    let command = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("a plugin needs a command to start its server"))?;
    let mut spec = PluginSpec::new(name, command).args(it);
    spec.env = env;
    Ok(spec)
}

/// Plugins from what a person pasted or typed after `/plugin add`: a JSON snippet in the
/// common `mcpServers` shape (`{"mcpServers": {"notes": {"command": …, "args": […],
/// "env": {…}}}}`), ARK's own `{"plugins": […]}`, one entry, or else the words
/// [`parse_add`] reads.
pub fn parse_many(text: &str) -> anyhow::Result<Vec<PluginSpec>> {
    let text = text.trim();
    if !text.starts_with('{') {
        return Ok(vec![parse_add(text)?]);
    }
    let doc: serde_json::Value =
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("not valid JSON: {e}"))?;
    let entry = |name: &str, v: &serde_json::Value| -> anyhow::Result<PluginSpec> {
        let mut v = v.clone();
        if v.get("url").is_some() && v.get("command").is_none() {
            anyhow::bail!("`{name}` is a remote server (url); ARK starts local servers only");
        }
        v["name"] = serde_json::Value::String(name.to_string());
        Ok(serde_json::from_value(v)?)
    };
    if let Some(servers) = doc.get("mcpServers").and_then(|m| m.as_object()) {
        return servers.iter().map(|(n, v)| entry(n, v)).collect();
    }
    if let Some(list) = doc.get("plugins").and_then(|p| p.as_array()) {
        return list
            .iter()
            .map(|v| Ok(serde_json::from_value(v.clone())?))
            .collect();
    }
    if doc.get("command").is_some() {
        let name = doc
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        return Ok(vec![entry(&name, &doc)?]);
    }
    anyhow::bail!("no servers in that JSON (expected `mcpServers`, `plugins` or one entry)")
}

/// [`add`] each plugin of [`parse_many`], in order. Stops at the first that fails; the ones
/// before it stay added.
pub async fn add_text(engine: Arc<Engine>, text: &str) -> anyhow::Result<Vec<PluginStatus>> {
    let mut out = Vec::new();
    for spec in parse_many(text)? {
        out.push(add(engine.clone(), spec).await?);
    }
    Ok(out)
}

fn split_words(line: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quoted {
        anyhow::bail!("a quote is not closed");
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// The plugins as a table: name, state, tools or the reason it is not running.
pub fn report(list: &[PluginStatus]) -> Report {
    if list.is_empty() {
        return Report::new().note(
            "No plugins. Add an MCP server with /plugin add <name> <command> [args…]; its tools become <name>.<tool>.",
        );
    }
    let mut t = Table::plain(3);
    for p in list {
        let (state, tone) = if p.running {
            ("running", Tone::Success)
        } else {
            ("stopped", Tone::Error)
        };
        let detail = if p.running {
            format!(
                "{} tool{}: {}",
                p.tools.len(),
                if p.tools.len() == 1 { "" } else { "s" },
                p.tools.join(", ")
            )
        } else {
            p.error.clone().unwrap_or_default()
        };
        t.push(Row {
            cells: vec![
                Cell::new(state, tone),
                Cell::new(p.name.clone(), Tone::Normal),
                Cell::new(detail, Tone::Muted),
            ],
            tone: Tone::Normal,
            command: None,
        });
    }
    Report::new()
        .table(t)
        .note("Changes apply from the next turn. /plugin add, /plugin remove <name>, /plugin restart <name>.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_reads_name_settings_command_and_args() {
        let s =
            parse_add(r#"notes NOTES_DIR=/data KEY="a b" python3 "my server.py" --fast"#).unwrap();
        assert_eq!(s.name, "notes");
        assert_eq!(s.command, "python3");
        assert_eq!(s.args, vec!["my server.py", "--fast"]);
        assert_eq!(s.env.get("NOTES_DIR").unwrap(), "/data");
        assert_eq!(s.env.get("KEY").unwrap(), "a b");
        // A word with `=` after the command is an argument, not a setting.
        let s = parse_add("x node srv.js --opt=1").unwrap();
        assert_eq!(s.args, vec!["srv.js", "--opt=1"]);
        assert!(parse_add("lonely").is_err());
        let many = parse_many(
            r#"{"mcpServers": {"notes": {"command": "npx", "args": ["-y", "notes-mcp"], "env": {"K": "v"}}, "web": {"url": "https://x"}}}"#,
        );
        assert!(many.unwrap_err().to_string().contains("remote"));
        let many = parse_many(
            r#"{"mcpServers": {"notes": {"command": "npx", "args": ["-y", "notes-mcp"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            (many[0].name.as_str(), many[0].command.as_str()),
            ("notes", "npx")
        );
        assert_eq!(
            parse_many("notes python3 s.py").unwrap()[0].args,
            vec!["s.py"]
        );
        assert!(parse_add(r#"x "open"#).is_err());
    }
}
