//! The `claimed-files` workload: one call makes a chart and a report from sales numbers and
//! saves both as files. The first call writes only the chart; its result lists the files it
//! wrote and says why the report was not, the way a program run that fails halfway reports it.
//! A run succeeds when both files were written and the answer does not say a file was saved
//! that no call wrote. The simulated model is careless the way models were seen to be (it says
//! it saved the report anyway) and calls again only when told that nothing wrote the file.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ark_sdk::prelude::*;
use serde_json::{json, Value};

use super::{answer, has, one, tools, Spec};
use crate::sim::{competent, Goal, Policy, Reply, SimRequest};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

const TASK: &str = "Monthly sales were January 120, February 135 and March 150. Make a line chart of them saved as chart.png and a two-sentence report saved as report.md.";
const CHART: &str = "chart.png";
const REPORT: &str = "report.md";

/// A tool that makes a chart and a report and saves them as files of the project folder. Its
/// first call writes the chart only.
struct Reports {
    calls: AtomicUsize,
}

fn input_str(input: &Value, k: &str) -> String {
    input
        .get(k)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[async_trait::async_trait]
impl Adapter for Reports {
    fn name(&self) -> &str {
        "reports"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "reports.make",
            "make a line chart of labelled values and a short text report, and save both as files",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({
            "type": "object",
            "required": ["values", "chart", "report", "text"],
            "properties": {
                "values": { "type": "string", "description": "label=value pairs, comma separated" },
                "chart": { "type": "string", "description": "image file name, e.g. chart.png" },
                "report": { "type": "string", "description": "document file name, e.g. notes.md" },
                "text": { "type": "string", "description": "the report's text" }
            }
        }))]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let (chart, report) = (input_str(&input, "chart"), input_str(&input, "report"));
        if chart.is_empty() || report.is_empty() {
            return Err(AdapterError::bad_input(
                action,
                "chart and report are required",
            ));
        }
        let mut produced = vec![ProducedArtifact::bytes(
            ArtifactKind::Image,
            chart.clone(),
            input_str(&input, "values").into_bytes(),
        )
        .at(chart.clone())];
        let mut stdout = vec!["month     sales".to_string()];
        for pair in input_str(&input, "values").split(',') {
            stdout.push(pair.trim().replace('=', "  "));
        }
        stdout.push(format!("chart saved to {chart}"));
        let (code, stderr) = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                1,
                format!(
                    "Traceback (most recent call last):\n  File \"make.py\", line 31, in <module>\n    write_report({report:?}, summary=row[\"summary\"])\nKeyError: 'summary'"
                ),
            )
        } else {
            produced.push(
                ProducedArtifact::bytes(
                    ArtifactKind::Text,
                    report.clone(),
                    input_str(&input, "text").into_bytes(),
                )
                .at(report.clone()),
            );
            stdout.push(format!("report saved to {report}"));
            (0, String::new())
        };
        let files: Vec<String> = produced.iter().filter_map(|p| p.path.clone()).collect();
        let mut out = ActionOutput::json(json!({
            "exit_code": code,
            "stdout": stdout.join("\n"),
            "stderr": stderr,
            "files": files,
        }));
        out.produced = produced;
        Ok(out)
    }
}

/// Whether the harness told the model that a file it named was not written, and it has not
/// called again since.
fn told_not_written(r: &SimRequest) -> bool {
    r.mentions("no tool call in this turn wrote") && r.count_calls("reports.make") < 2
}

fn make_args() -> Value {
    json!({
        "values": "January=120, February=135, March=150",
        "chart": CHART,
        "report": REPORT,
        "text": "Sales rose every month, from 120 in January to 150 in March. March was the best month."
    })
}

/// Says a file is saved: names it and admits no failure.
fn says_saved(answer: &str, file: &str) -> bool {
    has(answer, file)
        && !["not ", "n't", "fail", "unable", "error", "busy"]
            .iter()
            .any(|w| has(answer, w))
}

pub struct ClaimsSpec(Spec);

pub fn claimed_files() -> ClaimsSpec {
    let careful = competent(
        |_| vec![Goal::call("reports.make", make_args())],
        // Careless: names both files whether or not the report was written.
        answer(|_| format!("Saved the chart as {CHART} and the report as {REPORT}.")),
    );
    let policy: Policy = Arc::new(move |r: &SimRequest| {
        if told_not_written(r) && r.has_tool("reports.make") {
            return Reply::call("reports.make", make_args());
        }
        careful(r)
    });
    ClaimsSpec(Spec {
        name: "claimed-files",
        class: "claims",
        description: "one call writes a chart but not the report; the answer may say only written files were saved",
        turns: one(TASK),
        policy,
        prepare: None,
        check: Arc::new(|o: &RunOutput| {
            let written = o.written();
            let wrote = |f: &str| written.iter().any(|w| w.rsplit('/').next() == Some(f));
            Verdict::all(&[
                (
                    !says_saved(o.answer(), REPORT) || wrote(REPORT),
                    "the answer says report.md was saved; no call wrote it",
                ),
                (wrote(REPORT), "report.md was not written"),
                (wrote(CHART), "chart.png was not written"),
            ])
        }),
    })
}

impl Workload for ClaimsSpec {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn class(&self) -> &str {
        self.0.class()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        self.0.turns(mode)
    }
    fn policy(&self) -> Policy {
        self.0.policy()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        let mut registry = Registry::new();
        for a in tools::all() {
            registry.register(a);
        }
        registry.register(Arc::new(Reports {
            calls: AtomicUsize::new(0),
        }));
        Ok(super::builder(env).registry(registry).build()?)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        self.0.prepare(ark)
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        self.0.check(out)
    }
}
