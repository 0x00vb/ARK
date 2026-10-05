//! The `analysis` workload: a CSV of sales by region in the project folder; the task asks for a
//! chart and a short report made with Python (`code.run`). A run succeeds when the project folder
//! has a PNG chart and a report that names the region with the highest total and that total,
//! read from the saved files, not from the answer. Live runs use ARK's Run code in its sandbox,
//! with one Python environment kept between runs; the simulated model's program is run by a
//! stand-in that writes what that program would write, so the check needs no Python.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_sdk::prelude::*;
use serde_json::{json, Value};

use super::{answer, tools, Spec};
use crate::sim::{competent, Goal, Policy};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

const DATA: &str = "sales.csv";
const CHART: &str = "chart.png";
const REPORT: &str = "report.md";
const TASK: &str = "sales.csv in the project folder has revenue by region and month. With Python, save a bar chart of the total revenue per region as chart.png, and a two-sentence report as report.md that names the region with the highest total revenue and gives that total.";

/// Region, month, revenue. North totals 4,300, South 3,950, East 3,600.
const ROWS: [(&str, &str, u32); 9] = [
    ("North", "January", 1300),
    ("North", "February", 1450),
    ("North", "March", 1550),
    ("South", "January", 1200),
    ("South", "February", 1300),
    ("South", "March", 1450),
    ("East", "January", 1100),
    ("East", "February", 1200),
    ("East", "March", 1300),
];
const TOP: (&str, u32) = ("North", 4300);

fn csv() -> String {
    let mut s = String::from("region,month,revenue\n");
    for (r, m, v) in ROWS {
        s.push_str(&format!("{r},{m},{v}\n"));
    }
    s
}

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The program the simulated model writes.
const PROGRAM: &str = r#"import pandas as pd
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
df = pd.read_csv("sales.csv")
totals = df.groupby("region")["revenue"].sum().sort_values(ascending=False)
totals.plot(kind="bar", title="Total revenue by region")
plt.tight_layout()
plt.savefig("chart.png")
top, value = totals.index[0], int(totals.iloc[0])
open("report.md", "w").write(f"{top} had the highest total revenue, {value}. The chart compares the three regions.\n")
print(totals.to_string())"#;

/// Runs the simulated model's program: writes what it writes. A program that does not name both
/// files writes nothing and fails, as Python would.
struct StandIn;

#[async_trait::async_trait]
impl Adapter for StandIn {
    fn name(&self) -> &str {
        "code"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "code.run",
            "run a Python program in the project folder (pandas, matplotlib, openpyxl); files it writes are kept",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({
            "type": "object",
            "required": ["code"],
            "properties": { "code": { "type": "string" } }
        }))]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let code = input.get("code").and_then(Value::as_str).unwrap_or("");
        let dir = ctx
            .workspace
            .clone()
            .ok_or_else(|| AdapterError::bad_input(action, "no project folder"))?;
        if !(code.contains(CHART) && code.contains(REPORT) && code.contains(DATA)) {
            return Ok(ActionOutput::json(json!({
                "exit_code": 1,
                "stdout": "",
                "stderr": "Traceback (most recent call last):\n  File \"main.py\", line 1\nFileNotFoundError",
                "files": [],
            })));
        }
        let mut chart = PNG_MAGIC.to_vec();
        chart.extend_from_slice(b"stand-in chart");
        std::fs::write(dir.join(CHART), chart).map_err(|e| AdapterError::Tool(e.to_string()))?;
        std::fs::write(
            dir.join(REPORT),
            format!(
                "{} had the highest total revenue, {}. The chart compares the three regions.\n",
                TOP.0, TOP.1
            ),
        )
        .map_err(|e| AdapterError::Tool(e.to_string()))?;
        Ok(ActionOutput::json(json!({
            "exit_code": 0,
            "stdout": "region\nNorth    4300\nSouth    3950\nEast     3600",
            "stderr": "",
            "files": [CHART, REPORT],
        })))
    }
}

fn folder(ark: &Ark, project: &ProjectId) -> anyhow::Result<PathBuf> {
    Ok(ark.engine().workspace_dir(project)?)
}

/// Whether `report` names the top region and its total (`4300`, `4,300` or `4.300`).
fn report_is_right(report: &str) -> bool {
    let digits: String = report
        .chars()
        .filter(|c| c.is_ascii_digit() || c.is_whitespace() || c.is_alphabetic())
        .collect();
    report.contains(TOP.0) && digits.contains(&TOP.1.to_string())
}

fn check(dir: &Path) -> Verdict {
    let chart = std::fs::read(dir.join(CHART)).unwrap_or_default();
    let report = std::fs::read_to_string(dir.join(REPORT)).unwrap_or_default();
    Verdict::all(&[
        (
            chart.starts_with(PNG_MAGIC),
            "no PNG chart was saved as chart.png",
        ),
        (
            !report.trim().is_empty(),
            "no report was saved as report.md",
        ),
        (
            report.is_empty() || report_is_right(&report),
            "the report does not name North with 4,300",
        ),
    ])
}

/// The analysis workload. See the module docs.
pub struct AnalysisSpec(Spec);

pub fn analysis() -> AnalysisSpec {
    let policy: Policy = competent(
        |_| vec![Goal::call("code.run", json!({ "code": PROGRAM }))],
        answer(|_| {
            "North had the highest total revenue, 4,300. The chart is in chart.png and the report in report.md.".into()
        }),
    );
    AnalysisSpec(Spec {
        name: "analysis",
        class: "files",
        description:
            "a chart and a report from a CSV with Python (code.run), checked on the saved files",
        turns: Arc::new(|_| vec![TurnSpec::new(TASK)]),
        policy,
        prepare: Some(Arc::new(|ark: &Ark| {
            let p = ark.find_or_create_project("bench", "")?.id;
            std::fs::write(folder(ark, &p)?.join(DATA), csv())?;
            Ok(p)
        })),
        check: Arc::new(|o: &RunOutput| match folder(o.ark, o.project) {
            Ok(dir) => check(&dir),
            Err(e) => Verdict::fail(format!("no project folder: {e}")),
        }),
    })
}

impl Workload for AnalysisSpec {
    fn name(&self) -> &str {
        self.0.name
    }
    fn class(&self) -> &str {
        self.0.class
    }
    fn description(&self) -> &str {
        self.0.description
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        (self.0.turns)(mode)
    }
    fn policy(&self) -> Policy {
        self.0.policy.clone()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        let mut registry = Registry::new();
        for a in tools::all().into_iter().filter(|a| a.name() != "files") {
            registry.register(a);
        }
        let b = env.builder().files(true);
        let b = match env.mode {
            Mode::Live => {
                // One Python environment for every run, so only the first one installs it.
                let home = std::env::temp_dir().join("ark-bench-python");
                let exec = ark_sdk::prelude::ark_exec::Exec::new(
                    env.root().join("runs"),
                    vec![Arc::new(ark_sdk::prelude::ark_exec::Python::new(home))],
                )
                .ok_or_else(|| anyhow::anyhow!("no sandbox for runs on this platform"))?;
                b.registry(registry).code_with(exec)
            }
            Mode::Simulated => {
                registry.register(Arc::new(StandIn));
                b.registry(registry)
            }
        };
        Ok(
            b.profile(AgentProfile::builder().core_actions(["code.run"]).build())
                .build()?,
        )
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        match &self.0.prepare {
            Some(p) => p(ark),
            None => Ok(ark.find_or_create_project("bench", "")?.id),
        }
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        (self.0.check)(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_reads_the_saved_files_and_accepts_any_number_format() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!check(dir.path()).success);
        std::fs::write(dir.path().join(CHART), PNG_MAGIC).unwrap();
        std::fs::write(dir.path().join(REPORT), "South led with 3,950.").unwrap();
        assert!(!check(dir.path()).success);
        for good in [
            "North led with 4,300.",
            "North: 4300 total",
            "North had 4.300 in total",
        ] {
            std::fs::write(dir.path().join(REPORT), good).unwrap();
            assert!(check(dir.path()).success, "{good}");
        }
        let total: u32 = ROWS.iter().filter(|r| r.0 == TOP.0).map(|r| r.2).sum();
        assert_eq!(total, TOP.1);
    }
}
