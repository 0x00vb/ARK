use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::{cli, live::LiveProcess, Cli, ModelOption};

pub(crate) async fn claude(
    account: Option<&crate::accounts::ModelAccount>,
) -> Result<Vec<ModelOption>, String> {
    let _lease = account
        .map(crate::accounts::Lease::acquire)
        .transpose()
        .map_err(|e| e.to_string())?;
    let bin = Cli::Claude.locate().ok_or("Claude Code is not installed")?;
    let mut cmd = cli::command(&bin);
    if let Some(account) = account {
        account.setup_command(&mut cmd).map_err(|e| e.to_string())?;
    }
    cmd.current_dir(std::env::temp_dir()).args([
        "--print",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
    ]);
    let mut proc = LiveProcess::spawn(cmd).map_err(|_| "Could not start Claude Code")?;
    let result = tokio::time::timeout(Duration::from_secs(6), async {
        proc.send(&json!({"type":"control_request", "request_id":"ark-models", "request":{"subtype":"initialize"}}).to_string())
            .await.map_err(|_| "Could not query Claude models".to_string())?;
        loop {
            let line = proc.next_line(Duration::from_secs(6)).await
                .map_err(|_| "Could not load Claude models. Retry in Models.".to_string())?;
            let value: Value = serde_json::from_str(&line).map_err(|_| "Invalid Claude model response".to_string())?;
            if value["type"] == "control_response" && value["response"]["request_id"] == "ark-models" {
                if value["response"]["subtype"] != "success" {
                    return Err("Claude model discovery was refused".into());
                }
                return claude_options(&value["response"]["response"]);
            }
        }
    }).await.map_err(|_| "Claude model discovery timed out".to_string())?;
    proc.stop().await;
    result
}

fn claude_options(response: &Value) -> Result<Vec<ModelOption>, String> {
    let models = response["models"]
        .as_array()
        .ok_or("Invalid Claude model list")?;
    let mut seen = HashSet::new();
    models
        .iter()
        .map(|model| {
            let id = model["value"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("Invalid Claude model identifier")?;
            let efforts = match model.get("supportedEffortLevels") {
                None => vec![],
                Some(value) => value
                    .as_array()
                    .ok_or("Invalid Claude reasoning levels")?
                    .iter()
                    .map(|e| {
                        e.as_str()
                            .map(str::to_string)
                            .ok_or("Invalid Claude reasoning level")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(ModelOption {
                id: id.into(),
                label: model["displayName"].as_str().unwrap_or(id).into(),
                efforts,
                default_effort: None,
            })
        })
        .filter(|result: &Result<ModelOption, &str>| match result {
            Ok(m) => seen.insert(m.id.clone()),
            Err(_) => true,
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(str::to_string)
}

pub(crate) async fn api(
    base: &str,
    key: &str,
    anthropic: bool,
) -> Result<Vec<ModelOption>, String> {
    tokio::time::timeout(Duration::from_secs(6), api_pages(base, key, anthropic))
        .await
        .map_err(|_| "Model discovery timed out".to_string())?
}

async fn api_pages(base: &str, key: &str, anthropic: bool) -> Result<Vec<ModelOption>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not query models")?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor = None;
    for _ in 0..20 {
        let mut request = client.get(format!("{}/models", base.trim_end_matches('/')));
        request = if anthropic {
            request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else if key.is_empty() {
            request
        } else {
            request.bearer_auth(key)
        };
        if let Some(cursor) = &cursor {
            request = request.query(&[(if anthropic { "after_id" } else { "after" }, cursor)]);
        }
        let response = request.send().await.map_err(|_| "Could not reach models")?;
        if !response.status().is_success() {
            return Err(format!(
                "Could not load models (HTTP {})",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Could not read model list")?;
            if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                return Err("Model list is too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid model list")?;
        let models = response["data"].as_array().ok_or("Invalid model list")?;
        for model in models {
            let id = model["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("Invalid model identifier")?;
            if model["lifecycle"] == "retired" || !seen.insert(id.to_string()) {
                continue;
            }
            out.push(ModelOption {
                id: id.into(),
                label: model["display_name"].as_str().unwrap_or(id).into(),
                efforts: vec![],
                default_effort: None,
            });
        }
        match response.get("has_more") {
            None | Some(Value::Bool(false)) => return Ok(out),
            Some(Value::Bool(true)) => {}
            _ => return Err("Invalid model pagination".into()),
        }
        let next = response["last_id"]
            .as_str()
            .or_else(|| models.last()?.get("id")?.as_str())
            .filter(|s| !s.is_empty())
            .ok_or("Incomplete model pagination")?
            .to_string();
        if !cursors.insert(next.clone()) {
            return Err("Repeated model pagination".into());
        }
        cursor = Some(next);
    }
    Err("Model list exceeds the supported page limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(
        bodies: Vec<&'static str>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let captured = seen.clone();
        let task = tokio::spawn(async move {
            for body in bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes[..n].to_vec()).unwrap());
                let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (base, seen, task)
    }

    #[tokio::test]
    async fn api_discovery_replaces_added_and_removed_models_without_a_static_list() {
        let (base, seen, task) = server(vec![
            r#"{"data":[{"id":"old"}]}"#,
            r#"{"data":[{"id":"new"},{"id":"new"}]}"#,
            r#"{"data":[]}"#,
        ])
        .await;
        assert_eq!(api(&base, "first", false).await.unwrap()[0].id, "old");
        let next = api(&base, "second", false).await.unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, "new");
        assert!(api(&base, "second", false).await.unwrap().is_empty());
        task.await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("Bearer first"));
        assert!(seen[1].contains("Bearer second"));
    }

    #[tokio::test]
    async fn anthropic_discovery_reads_complete_pages_and_filters_retired_models() {
        let (base, seen, task) = server(vec![
            r#"{"data":[{"id":"one","display_name":"One"}],"has_more":true,"last_id":"one"}"#,
            r#"{"data":[{"id":"gone","lifecycle":"retired"},{"id":"two"}],"has_more":false}"#,
        ])
        .await;
        let models = api(&base, "test-key", true).await.unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(models[0].label, "One");
        task.await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("x-api-key: test-key"));
        assert!(seen[1].starts_with("GET /v1/models?after_id=one "));
    }

    #[tokio::test]
    async fn discovery_rejects_malformed_or_incomplete_pages_without_partial_models() {
        for body in [
            r#"{}"#,
            r#"{"data":[{}]}"#,
            r#"{"data":[],"has_more":true}"#,
            r#"{"data":[],"has_more":"yes"}"#,
        ] {
            let (base, _, task) = server(vec![body]).await;
            assert!(api(&base, "key", false).await.is_err(), "{body}");
            task.await.unwrap();
        }
        let (base, _, task) = server(vec![
            r#"{"data":[{"id":"one"}],"has_more":true,"last_id":"same"}"#,
            r#"{"data":[{"id":"two"}],"has_more":true,"last_id":"same"}"#,
        ])
        .await;
        assert_eq!(
            api(&base, "key", false).await.unwrap_err(),
            "Repeated model pagination"
        );
        task.await.unwrap();
    }

    #[test]
    fn claude_discovery_uses_advertised_models_and_efforts() {
        let models = claude_options(&json!({"models":[{"value":"new-model","displayName":"New model","supportedEffortLevels":["low","max"]}]})).unwrap();
        assert_eq!(models[0].id, "new-model");
        assert_eq!(models[0].efforts, ["low", "max"]);
        assert!(claude_options(&json!({"models":[]})).unwrap().is_empty());
        assert!(claude_options(&json!({})).is_err());
        assert!(
            claude_options(&json!({"models":[{"value":"bad","supportedEffortLevels":[1]}]}))
                .is_err()
        );
    }
}
