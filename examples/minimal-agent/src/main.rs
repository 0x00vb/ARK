//! # minimal-agent
//!
//! The smallest useful ARK agent: two in-process tools (`calc.eval`, `clock.now`) and a
//! one-line persona. It touches no files, no network, and no domain — its entire reason for
//! existing is to show how little boilerplate a new agent needs, and that ARK's runtime is
//! genuinely domain-agnostic.
//!
//! Run it against any model you have configured:
//! ```text
//! ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct cargo run -p minimal-agent -- "what is 6 * 7?"
//! ```

use std::sync::Arc;

use ark_sdk::prelude::*;

/// A calculator tool: evaluates a simple `<number> <op> <number>` expression.
struct Calc;

#[async_trait::async_trait]
impl Adapter for Calc {
    fn name(&self) -> &str {
        "calc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("calc.eval", "evaluate a simple arithmetic expression like '6 * 7'", RiskTier::Read)
            .with_capabilities([Capability::new("computation")])
            .with_input_schema(serde_json::json!({
                "type": "object",
                "required": ["expr"],
                "properties": { "expr": { "type": "string", "description": "e.g. '12 + 30' or '6 * 7'" } }
            }))]
    }
    async fn run(
        &self,
        _action: &str,
        input: serde_json::Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let expr = input
            .get("expr")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        match eval(&expr) {
            Some(value) => Ok(ActionOutput::json(
                serde_json::json!({ "expr": expr, "value": value }),
            )),
            None => Ok(ActionOutput::json(
                serde_json::json!({ "expr": expr, "error": "could not parse; use '<number> <+|-|*|/> <number>'" }),
            )),
        }
    }
}

/// A clock tool: reports the current time as a Unix timestamp.
struct Clock;

#[async_trait::async_trait]
impl Adapter for Clock {
    fn name(&self) -> &str {
        "clock"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "clock.now",
            "get the current Unix timestamp (seconds)",
            RiskTier::Read,
        )
        .with_capabilities([Capability::new("time")])]
    }
    async fn run(
        &self,
        _action: &str,
        _input: serde_json::Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(ActionOutput::json(serde_json::json!({ "unix": now })))
    }
}

/// Evaluate `<number> <op> <number>`. Deliberately tiny — a real tool, not a toy DSL.
fn eval(expr: &str) -> Option<f64> {
    for op in ['+', '-', '*', '/'] {
        if let Some((l, r)) = expr.split_once(op) {
            let a: f64 = l.trim().parse().ok()?;
            let b: f64 = r.trim().parse().ok()?;
            return Some(match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' => a / b,
                _ => unreachable!(),
            });
        }
    }
    expr.trim().parse().ok()
}

/// Build the agent. This is the whole wiring: two tools, a persona, a capability catalog.
fn build_ark(
    store_root: impl Into<std::path::PathBuf>,
    provider: Arc<dyn ark_model::Provider>,
) -> Result<Ark, ArkError> {
    let profile = AgentProfile::builder()
        .instructions(
            "You are a precise assistant that answers arithmetic and time questions by calling tools, \
             never by guessing. Do the arithmetic with calc.eval and read the time with clock.now.",
        )
        .capabilities(CapabilityCatalog::new([
            CapabilitySpec::new("computation", "evaluate arithmetic").with_synonyms(["math", "arithmetic", "calculate"]),
            CapabilitySpec::new("time", "read the current time").with_synonyms(["clock", "now", "date"]),
        ]))
        .core_actions(["calc.eval", "clock.now"])
        .build();

    Ark::builder()
        .store_root(store_root)
        .tool(Arc::new(Calc))
        .tool(Arc::new(Clock))
        .profile(profile)
        .provider(provider)
        .planner(false) // trivial tasks; skip the planner
        .build()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let task = if task.is_empty() {
        "what is 6 * 7?".to_string()
    } else {
        task
    };

    let provider = ark_model::from_env_arc()?;
    let ark = build_ark("./minimal-agent-workspace", provider)?;
    let project = ark.create_project("scratch", "arithmetic and time")?;

    println!(
        "model: {}\n",
        ark.profile()
            .prompts
            .instructions
            .lines()
            .next()
            .unwrap_or("")
    );
    let outcome = ark
        .ask_with(&project.id, &task, |ev| {
            if let HarnessEvent::ToolResult {
                action, summary, ..
            } = ev
            {
                eprintln!("  · {action}: {summary}");
            }
        })
        .await?;

    println!("\n{}", outcome.answer);
    println!(
        "\n[{} tokens, {} tool call(s)]",
        outcome.metrics.total_tokens(),
        outcome.metrics.tool_calls
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Completion, MockProvider, ToolCall, Usage};

    #[test]
    fn eval_handles_basic_ops() {
        assert_eq!(eval("6 * 7"), Some(42.0));
        assert_eq!(eval("12 + 30"), Some(42.0));
        assert_eq!(eval("100 / 4"), Some(25.0));
        assert_eq!(eval("nonsense"), None);
    }

    #[tokio::test]
    async fn agent_uses_the_calculator_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        // Script the model: call calc.eval(6*7), then answer.
        let call = ToolCall {
            id: "c1".into(),
            name: "calc.eval".into(),
            arguments: serde_json::json!({ "expr": "6 * 7" }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![call],
                usage: Usage::measured(80, 8),
            },
            Completion {
                content: "6 * 7 = 42 [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(90, 9),
            },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));
        let ark = build_ark(dir.path(), provider).unwrap();
        let project = ark.create_project("t", "").unwrap();
        let outcome = ark.ask(&project.id, "what is 6 * 7?").await.unwrap();
        assert!(outcome.answer.contains("42"), "answer: {}", outcome.answer);
        assert_eq!(outcome.metrics.tool_calls, 1);
    }
}
