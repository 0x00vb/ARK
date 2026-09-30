//! MCP plugin adapter — the out-of-process plugin boundary (brief #6, ADR-0003/0014).
//!
//! A third party ships a plugin as an **MCP server** (stdio, line-delimited JSON-RPC
//! 2.0) written in any language. ARK spawns it, performs the `initialize`
//! handshake, lists its tools, and registers each as a ARK action named
//! `<plugin>.<tool>` — behind the *same* approval gate, invocation log and evidence
//! model as a built-in adapter. Adding a Python/Go/Rust security integration needs no
//! ARK rebuild: drop a line in `plugins.json` and it appears as capabilities.
//!
//! Two bridges make a generic MCP tool feel native:
//! - **Risk from annotations.** MCP tool annotations (`readOnlyHint`,
//!   `destructiveHint`, `openWorldHint`) map to the [`RiskTier`] the gate reads; a
//!   plugin can also state it outright with `x-ark-risk`.
//! - **Artifacts → paths.** When a ARK call carries an `artifact` id and the
//!   tool's schema wants a `path`, the adapter resolves the artifact to a real file
//!   path, so file-oriented MCP tools operate on ARK artifacts unchanged.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use ark_core::{ActionDescriptor, RiskTier};

use crate::{ActionOutput, Adapter, AdapterError, AdapterHealth, RunCtx};

/// One tool advertised by an MCP server.
#[derive(Clone, Debug)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub risk: RiskTier,
}

/// A live connection to one MCP server over stdio. All access is serialized through a
/// mutex, so a blocking request/response cycle is safe.
struct McpConn {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

pub struct McpClient {
    conn: Mutex<McpConn>,
    /// The server's self-reported name/version, for health.
    server_info: Mutex<String>,
}

impl McpClient {
    /// Spawn the server and complete the MCP `initialize` handshake.
    pub fn connect(command: &str, args: &[String], env: &HashMap<String, String>) -> crate::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args).envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| AdapterError::Tool(format!("spawn MCP server `{command}`: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| AdapterError::Tool("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| AdapterError::Tool("no stdout".into()))?;
        let conn = McpConn { child, stdin, reader: BufReader::new(stdout), next_id: 0 };
        let client = McpClient { conn: Mutex::new(conn), server_info: Mutex::new(String::new()) };

        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "ark", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        if let Some(info) = init.get("serverInfo") {
            let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("mcp");
            let ver = info.get("version").and_then(|v| v.as_str()).unwrap_or("");
            *client.server_info.lock().unwrap() = format!("{name} {ver}").trim().to_string();
        }
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    /// One blocking JSON-RPC request/response. Skips notifications/unrelated ids.
    fn request(&self, method: &str, params: Value) -> crate::Result<Value> {
        let mut c = self.conn.lock().unwrap();
        c.next_id += 1;
        let id = c.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(c.stdin, "{msg}").map_err(|e| AdapterError::Tool(format!("write: {e}")))?;
        c.stdin.flush().ok();

        // Read lines until we see the response for our id (or an error).
        loop {
            let mut line = String::new();
            let n = c.reader.read_line(&mut line).map_err(|e| AdapterError::Tool(format!("read: {e}")))?;
            if n == 0 {
                return Err(AdapterError::Tool(format!("MCP server closed the stream during `{method}`")));
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // ignore non-JSON diagnostics
            };
            if v.get("id").and_then(|i| i.as_i64()) != Some(id) {
                continue; // a notification or another id
            }
            if let Some(err) = v.get("error") {
                return Err(AdapterError::Tool(format!("MCP `{method}` error: {err}")));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&self, method: &str, params: Value) -> crate::Result<()> {
        let mut c = self.conn.lock().unwrap();
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(c.stdin, "{msg}").map_err(|e| AdapterError::Tool(format!("write: {e}")))?;
        c.stdin.flush().ok();
        Ok(())
    }

    pub fn list_tools(&self) -> crate::Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({}))?;
        let tools = result.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        Ok(tools.iter().map(parse_tool).collect())
    }

    pub fn call_tool(&self, name: &str, args: Value) -> crate::Result<Value> {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
    }

    fn server_info(&self) -> String {
        self.server_info.lock().unwrap().clone()
    }

    fn alive(&self) -> bool {
        // try_wait: Ok(None) => still running.
        matches!(self.conn.lock().unwrap().child.try_wait(), Ok(None))
    }
}

/// Map one MCP tool descriptor to our shape, deriving the risk tier.
fn parse_tool(v: &Value) -> McpTool {
    let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("tool").to_string();
    let description = v.get("description").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let input_schema = v
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let ann = v.get("annotations").cloned().unwrap_or(Value::Null);
    McpTool { name, description, input_schema, risk: risk_from_annotations(&ann) }
}

/// Derive a [`RiskTier`] from MCP annotations. An explicit `x-ark-risk` wins;
/// otherwise: destructive → Mutate; read-only + touches the outside world → Observe;
/// read-only local → Read; unknown defaults to the safe-but-gated Mutate so a plugin
/// that forgets to declare intent can't silently auto-run a side effect.
fn risk_from_annotations(ann: &Value) -> RiskTier {
    if let Some(explicit) = ann.get("x-ark-risk").and_then(|v| v.as_str()) {
        match explicit.to_ascii_lowercase().as_str() {
            "read" => return RiskTier::Read,
            "observe" => return RiskTier::Observe,
            "mutate" => return RiskTier::Mutate,
            _ => {}
        }
    }
    let b = |k: &str| ann.get(k).and_then(|v| v.as_bool());
    if b("destructiveHint") == Some(true) {
        return RiskTier::Mutate;
    }
    if b("readOnlyHint") == Some(true) {
        return if b("openWorldHint") == Some(true) { RiskTier::Observe } else { RiskTier::Read };
    }
    RiskTier::Mutate
}

/// An adapter backed by one MCP server. Its actions are the server's tools, namespaced
/// under the plugin name.
pub struct McpAdapter {
    namespace: String,
    client: Arc<McpClient>,
    actions: Vec<ActionDescriptor>,
    label: String,
}

impl McpAdapter {
    /// Connect to a plugin server and enumerate its tools as actions.
    pub fn connect(
        namespace: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> crate::Result<Self> {
        let client = Arc::new(McpClient::connect(command, args, env)?);
        let label = client.server_info();
        let tools = client.list_tools()?;
        let actions = tools
            .into_iter()
            .map(|t| {
                ActionDescriptor::new(format!("{namespace}.{}", t.name), t.description, t.risk)
                    .with_input_schema(t.input_schema)
            })
            .collect();
        Ok(McpAdapter { namespace: namespace.to_string(), client, actions, label })
    }
}

#[async_trait]
impl Adapter for McpAdapter {
    fn name(&self) -> &str {
        &self.namespace
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        self.actions.clone()
    }

    async fn health(&self) -> AdapterHealth {
        if self.client.alive() {
            AdapterHealth::ok_with(format!("MCP plugin {} ({} tools)", self.label, self.actions.len()))
        } else {
            AdapterHealth::missing("MCP server is not running")
        }
    }

    async fn run(&self, action: &str, input: Value, ctx: &RunCtx<'_>) -> crate::Result<ActionOutput> {
        let tool = action.strip_prefix(&format!("{}.", self.namespace)).unwrap_or(action).to_string();

        // Bridge: if the call names a ARK artifact and the tool wants a `path`,
        // resolve the artifact to a real file path so file-oriented tools just work.
        let mut args = input.clone();
        if let Some(obj) = args.as_object_mut() {
            if let Some(aid) = obj.get("artifact").and_then(|v| v.as_str()) {
                let wants_path = self
                    .actions
                    .iter()
                    .find(|d| d.name == action)
                    .map(|d| d.input_schema.get("properties").and_then(|p| p.get("path")).is_some())
                    .unwrap_or(false);
                if wants_path && !obj.contains_key("path") {
                    if let Some(p) = ctx.blobs.path(&ark_core::ArtifactId(aid.to_string())) {
                        obj.insert("path".into(), json!(p.display().to_string()));
                    }
                }
            }
        }

        let client = self.client.clone();
        let tool_for_call = tool.clone();
        let result = tokio::task::spawn_blocking(move || client.call_tool(&tool_for_call, args))
            .await
            .map_err(|e| AdapterError::Tool(format!("join: {e}")))??;

        // MCP result: `content` (array of parts) + optional `structuredContent`, `isError`.
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            let text = collect_text(&result);
            return Err(AdapterError::Tool(format!("plugin tool `{tool}` reported an error: {text}")));
        }
        let mut output = json!({ "plugin": self.namespace, "tool": tool });
        if let Some(sc) = result.get("structuredContent") {
            output["structured"] = sc.clone();
        }
        let text = collect_text(&result);
        if !text.is_empty() {
            // If the text is itself JSON, surface it structured; else keep it as text.
            match serde_json::from_str::<Value>(text.trim()) {
                Ok(v) if v.is_object() || v.is_array() => output["result"] = v,
                _ => output["text"] = json!(text),
            }
        }
        Ok(ActionOutput::json(output).with_command(vec!["mcp".into(), self.namespace.clone(), tool]))
    }
}

/// Concatenate the text parts of an MCP tool result's `content` array.
fn collect_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_mapping_from_annotations() {
        assert_eq!(risk_from_annotations(&json!({ "readOnlyHint": true })), RiskTier::Read);
        assert_eq!(risk_from_annotations(&json!({ "readOnlyHint": true, "openWorldHint": true })), RiskTier::Observe);
        assert_eq!(risk_from_annotations(&json!({ "destructiveHint": true })), RiskTier::Mutate);
        assert_eq!(risk_from_annotations(&json!({ "x-ark-risk": "observe" })), RiskTier::Observe);
        // Unknown intent defaults to the gated tier, never silent auto-run.
        assert_eq!(risk_from_annotations(&Value::Null), RiskTier::Mutate);
    }

    #[test]
    fn parse_tool_reads_schema_and_name() {
        let t = parse_tool(&json!({
            "name": "entropy",
            "description": "Shannon entropy scan",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } } },
            "annotations": { "readOnlyHint": true }
        }));
        assert_eq!(t.name, "entropy");
        assert_eq!(t.risk, RiskTier::Read);
        assert!(t.input_schema["properties"]["path"].is_object());
    }

    #[test]
    fn collect_text_joins_parts() {
        let r = json!({ "content": [ { "type": "text", "text": "a" }, { "type": "text", "text": "b" } ] });
        assert_eq!(collect_text(&r), "a\nb");
    }
}
