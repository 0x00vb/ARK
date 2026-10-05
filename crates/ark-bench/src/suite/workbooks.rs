//! The `workbooks` workload: an analyst's task over several files. The project folder has a
//! workbook of orders (a sheet per quarter, 54,000 rows), a CSV of customers and a workbook of
//! targets, with the problems real exports have:
//!
//! | Problem | Where |
//! |---|---|
//! | a title row and an empty row above the header | the Q2 sheet |
//! | a total row under the data | both order sheets |
//! | orders exported twice | about 1 in 100 |
//! | amounts stored as text (`1,234.50`) | about 1 in 20 |
//! | customer IDs in other case or with spaces | about 1 in 5 |
//! | a customer listed twice | customers.csv |
//! | customers not in customers.csv | 2 of 62 |
//! | more rows than Files reads | both order sheets |
//!
//! The task asks for a summary workbook (revenue per region per month, Q2 against target), a
//! chart, and three findings. A run succeeds when the saved workbook has every monthly figure
//! as a number, the chart is a PNG, the answer has the total and names the region that grew
//! most and the one that missed its target, and the three source files are unchanged. The
//! figures are computed here from the same data, not written down by hand.
//!
//! Live runs use ARK's Run code in its sandbox. The simulated model's program is run by a
//! stand-in that writes what [`PROGRAM`] writes; with `ARK_TEST_PYTHON=1` a test runs the real
//! program and checks it passes.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ark_sdk::prelude::ark_files::{self, Cell, Content, Sheet, Value as CellValue, Workbook};
use ark_sdk::prelude::*;
use serde_json::{json, Value};

use super::{answer, tools, Spec};
use crate::sim::{competent, Goal, Policy};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

const ORDERS: &str = "orders 2026.xlsx";
const CUSTOMERS: &str = "customers.csv";
const TARGETS: &str = "targets.xlsx";
const SUMMARY: &str = "summary.xlsx";
const CHART: &str = "revenue.png";
const SOURCES: [&str; 3] = [ORDERS, CUSTOMERS, TARGETS];
/// Where `prepare` keeps a copy of the sources, under the store root, to check them against.
const ORIGINALS: &str = "bench-originals";

const TASK: &str = "orders 2026.xlsx has this year's orders, a sheet per quarter. customers.csv says which region each customer is in, and targets.xlsx has each region's Q2 target. Each order counts once, and orders whose customer is not in customers.csv count under the region Unknown. Make summary.xlsx with the revenue per region for each month from January to June, and each region's Q2 revenue next to its target. Save a chart of the monthly revenue per region as revenue.png. Do not change the three files. Then tell me the total revenue for January to June, which region grew most from Q1 to Q2, and which region missed its Q2 target.";

const REGIONS: [&str; 4] = ["North", "South", "East", "West"];
const UNKNOWN: &str = "Unknown";
const MONTHS: [&str; 6] = ["January", "February", "March", "April", "May", "June"];
/// Customers in customers.csv; orders also name two more.
const CUSTOMERS_LISTED: u64 = 60;
const ROWS_Q1: usize = 26_000;
const ROWS_Q2: usize = 28_000;
/// The region whose Q2 orders are larger (it grows most), and the one whose target is above
/// its Q2 revenue.
const GROWS: &str = "West";
const MISSES: &str = "South";

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The program the simulated model writes; with `ARK_TEST_PYTHON=1` a test runs it for real.
const PROGRAM: &str = r#"import pandas as pd
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

def amount(x):
    return float(x.replace(",", "")) if isinstance(x, str) else float(x)

q1 = pd.read_excel("orders 2026.xlsx", sheet_name="Q1", engine="calamine")
q2 = pd.read_excel("orders 2026.xlsx", sheet_name="Q2", engine="calamine", skiprows=2)
orders = pd.concat([q1, q2])
orders = orders[orders["order_id"].astype(str).str.startswith("O-")]
orders = orders.drop_duplicates("order_id").copy()
orders["amount"] = orders["amount"].map(amount)
orders["customer_id"] = orders["customer_id"].str.strip().str.upper()
orders["month"] = pd.to_datetime(orders["order_date"]).dt.month
customers = pd.read_csv("customers.csv").drop_duplicates("customer_id")
df = orders.merge(customers[["customer_id", "region"]], on="customer_id", how="left")
assert len(df) == len(orders)
df["region"] = df["region"].fillna("Unknown")
monthly = df.pivot_table(index="month", columns="region", values="amount", aggfunc="sum").round(2)
monthly.index = [pd.Timestamp(2026, m, 1).strftime("%B") for m in monthly.index]
df["quarter"] = df["month"].map(lambda m: "Q1" if m <= 3 else "Q2")
q = df.pivot_table(index="region", columns="quarter", values="amount", aggfunc="sum")
targets = pd.read_excel("targets.xlsx", engine="calamine").set_index("Region")
vs = pd.DataFrame({"Q2 revenue": q["Q2"].round(2), "Q2 target": targets["Q2 target"]}).dropna()
vs["Difference"] = (vs["Q2 revenue"] - vs["Q2 target"]).round(2)
with pd.ExcelWriter("summary.xlsx", engine="xlsxwriter") as w:
    monthly.to_excel(w, sheet_name="Monthly revenue")
    vs.to_excel(w, sheet_name="Q2 vs target")
monthly.plot(marker="o", title="Revenue per region, January to June 2026")
plt.ylabel("Revenue")
plt.tight_layout()
plt.savefig("revenue.png", dpi=150)
growth = (q["Q2"] / q["Q1"] - 1).drop("Unknown")
print("total", round(df["amount"].sum(), 2))
print("grew most", growth.idxmax())
print("missed", list(vs[vs["Difference"] < 0].index))
"#;

/// A small deterministic generator, so every run and the check see the same data.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// One row of an order sheet as exported.
struct Row {
    id: String,
    date: String,
    customer: String,
    cents: i64,
    as_text: bool,
}

/// The sources and the right answers.
struct Data {
    q1: Vec<Row>,
    q2: Vec<Row>,
    targets: Vec<(&'static str, f64)>,
    /// Revenue in cents by region and month (1 to 6).
    monthly: BTreeMap<(String, u32), i64>,
    total: i64,
}

fn region_of(customer: u64) -> &'static str {
    REGIONS[((customer - 1) % 4) as usize]
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut rng = Rng(2026);
        let mut quarter = |q: u32, rows: usize| {
            let mut out = Vec::with_capacity(rows + rows / 50);
            let mut n = 0;
            while out.len() < rows {
                n += 1;
                let month = (q - 1) * 3 + 1 + rng.below(3) as u32;
                let customer = rng.below(CUSTOMERS_LISTED + 2) + 1;
                let mut cents = 2_000 + rng.below(198_000) as i64;
                if q == 2 && region_of(customer) == GROWS && customer <= CUSTOMERS_LISTED {
                    cents = cents * 3 / 2;
                }
                let id = format!("C-{customer:04}");
                let customer = match rng.below(10) {
                    0 => id.to_lowercase(),
                    1 => format!(" {id} "),
                    _ => id,
                };
                let row = Row {
                    id: format!("O-{q}{n:06}"),
                    date: format!("2026-{month:02}-{:02}", 1 + rng.below(28)),
                    customer,
                    cents,
                    as_text: rng.below(20) == 0,
                };
                let twice = rng.below(100) == 0;
                if twice {
                    out.push(Row {
                        id: row.id.clone(),
                        date: row.date.clone(),
                        customer: row.customer.clone(),
                        cents: row.cents,
                        as_text: row.as_text,
                    });
                }
                out.push(row);
            }
            out
        };
        let q1 = quarter(1, ROWS_Q1);
        let q2 = quarter(2, ROWS_Q2);

        let mut seen = HashSet::new();
        let mut monthly: BTreeMap<(String, u32), i64> = BTreeMap::new();
        for r in q1.iter().chain(&q2) {
            if !seen.insert(r.id.clone()) {
                continue;
            }
            let n: u64 = r.customer.trim()[2..].parse().unwrap();
            let region = if n <= CUSTOMERS_LISTED {
                region_of(n)
            } else {
                UNKNOWN
            };
            let month: u32 = r.date[5..7].parse().unwrap();
            *monthly.entry((region.to_string(), month)).or_default() += r.cents;
        }
        let total = monthly.values().sum();
        let q2_of = |region: &str| -> i64 {
            (4..=6)
                .map(|m| monthly[&(region.to_string(), m)])
                .sum::<i64>()
        };
        // Targets in whole thousands: a little under Q2 revenue, and above it for one region.
        let targets = REGIONS
            .iter()
            .map(|&r| {
                let q2 = q2_of(r) as f64 / 100.0;
                let t = if r == MISSES { q2 * 1.08 } else { q2 * 0.92 };
                (r, (t / 1000.0).round() * 1000.0)
            })
            .collect();
        Data {
            q1,
            q2,
            targets,
            monthly,
            total,
        }
    })
}

fn cell(value: CellValue) -> Cell {
    Cell {
        value,
        ..Cell::default()
    }
}

fn text(s: &str) -> Cell {
    cell(CellValue::Text(s.to_string()))
}

fn number(n: f64) -> Cell {
    cell(CellValue::Number(n))
}

fn sheet(name: &str, cells: BTreeMap<(u32, u32), Cell>) -> Sheet {
    Sheet {
        name: name.into(),
        cells,
        origin: None,
        unread: 0,
    }
}

/// `1,234.50`, as a spreadsheet exported as text writes it.
fn money_text(cents: i64) -> String {
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped}.{:02}", cents % 100)
}

/// An order sheet: its header at `top`, the rows, then a total row.
fn order_sheet(name: &str, rows: &[Row], title: Option<&str>) -> Sheet {
    let mut cells = BTreeMap::new();
    let top = match title {
        Some(t) => {
            cells.insert((0, 0), text(t));
            2
        }
        None => 0,
    };
    for (c, h) in ["order_id", "order_date", "customer_id", "amount"]
        .iter()
        .enumerate()
    {
        cells.insert((top, c as u32), text(h));
    }
    let mut sum = 0;
    for (i, r) in rows.iter().enumerate() {
        let at = top + 1 + i as u32;
        cells.insert((at, 0), text(&r.id));
        cells.insert((at, 1), text(&r.date));
        cells.insert((at, 2), text(&r.customer));
        let amount = match r.as_text {
            true => text(&money_text(r.cents)),
            false => number(r.cents as f64 / 100.0),
        };
        cells.insert((at, 3), amount);
        sum += r.cents;
    }
    let at = top + 1 + rows.len() as u32;
    cells.insert((at, 0), text("Total"));
    cells.insert((at, 3), number(sum as f64 / 100.0));
    sheet(name, cells)
}

fn customers_csv() -> String {
    let mut s = String::from("customer_id,name,region\n");
    for n in 1..=CUSTOMERS_LISTED {
        let line = format!("C-{n:04},Customer {n},{}\n", region_of(n));
        s.push_str(&line);
        if n == 7 {
            s.push_str(&line);
        }
    }
    s
}

fn targets_book() -> Workbook {
    let mut cells = BTreeMap::new();
    cells.insert((0, 0), text("Region"));
    cells.insert((0, 1), text("Q2 target"));
    for (i, (r, t)) in data().targets.iter().enumerate() {
        cells.insert((i as u32 + 1, 0), text(r));
        cells.insert((i as u32 + 1, 1), number(*t));
    }
    Workbook {
        sheets: vec![sheet("Targets", cells)],
    }
}

/// Write the three sources into `dir`.
fn write_sources(dir: &Path) -> anyhow::Result<()> {
    let d = data();
    let orders = Workbook {
        sheets: vec![
            order_sheet("Q1", &d.q1, None),
            order_sheet("Q2", &d.q2, Some("Orders, April to June 2026")),
        ],
    };
    ark_files::write(dir, ORDERS, &Content::Sheet(orders), None)?;
    std::fs::write(dir.join(CUSTOMERS), customers_csv())?;
    ark_files::write(dir, TARGETS, &Content::Sheet(targets_book()), None)?;
    Ok(())
}

fn folder(ark: &Ark, project: &ProjectId) -> anyhow::Result<PathBuf> {
    Ok(ark.engine().workspace_dir(project)?)
}

fn originals(ark: &Ark) -> PathBuf {
    ark.engine().store.root().join(ORIGINALS)
}

/// Every number in a workbook's sheets.
fn numbers(book: &Workbook) -> Vec<f64> {
    book.sheets
        .iter()
        .flat_map(|s| s.cells.values())
        .filter_map(|c| match c.value {
            CellValue::Number(n) => Some(n),
            _ => None,
        })
        .collect()
}

/// Whether the answer gives the total: its whole units in any grouping (`54,012,345`,
/// `54.012.345`, `54012345`), or in millions to one or two decimals.
fn has_total(answer: &str, cents: i64) -> bool {
    let digits: String = answer
        .chars()
        .filter(|c| !matches!(c, ',' | '.' | '\u{a0}' | '\u{202f}' | '\''))
        .collect();
    let whole = cents / 100;
    let rounded = (cents + 50) / 100;
    if digits.contains(&whole.to_string()) || digits.contains(&rounded.to_string()) {
        return true;
    }
    let millions = cents as f64 / 100.0 / 1e6;
    let lower = answer.to_lowercase();
    [format!("{millions:.1}"), format!("{millions:.2}")]
        .iter()
        .any(|m| lower.contains(&format!("{m} million")) || lower.contains(&format!("{m}m")))
}

fn check(dir: &Path, kept: &Path, answer: &str) -> Verdict {
    let d = data();
    let unchanged = SOURCES.iter().all(|s| {
        let now = std::fs::read(dir.join(s)).ok();
        now.is_some() && now == std::fs::read(kept.join(s)).ok()
    });
    let summary = ark_files::open(dir, SUMMARY)
        .ok()
        .and_then(|f| match f.content {
            Content::Sheet(b) => Some(b),
            _ => None,
        });
    let found = summary.as_ref().map(numbers).unwrap_or_default();
    let has = |cents: i64| {
        let want = cents as f64 / 100.0;
        found.iter().any(|n| (n - want).abs() <= 0.51)
    };
    let missing: Vec<String> = d
        .monthly
        .iter()
        .filter(|(_, &c)| !has(c))
        .map(|((r, m), _)| format!("{r} {}", MONTHS[*m as usize - 1]))
        .collect();
    let q2_missing: Vec<&str> = REGIONS
        .iter()
        .copied()
        .filter(|r| !has((4..=6).map(|m| d.monthly[&(r.to_string(), m)]).sum()))
        .collect();
    let chart = std::fs::read(dir.join(CHART)).unwrap_or_default();
    Verdict::all(&[
        (unchanged, "a source file was changed or removed"),
        (
            summary.is_some(),
            "no summary.xlsx that opens as a workbook",
        ),
        (
            summary.is_none() || missing.is_empty(),
            &format!(
                "summary.xlsx lacks the right monthly revenue (as a number) for {} of {}: {}",
                missing.len(),
                d.monthly.len(),
                missing
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        (
            summary.is_none() || q2_missing.is_empty(),
            &format!(
                "summary.xlsx lacks the right Q2 revenue for {}",
                q2_missing.join(", ")
            ),
        ),
        (
            chart.starts_with(PNG_MAGIC),
            "no PNG chart saved as revenue.png",
        ),
        (
            has_total(answer, d.total),
            "the answer does not give the total revenue",
        ),
        (
            answer.contains(GROWS),
            "the answer does not name the region that grew most",
        ),
        (
            answer.contains(MISSES),
            "the answer does not name the region that missed its target",
        ),
    ])
}

/// Whole currency units with thousands separators.
fn money(cents: i64) -> String {
    money_text((cents + 50) / 100 * 100)
        .trim_end_matches(".00")
        .to_string()
}

fn sim_answer() -> String {
    format!(
        "Total revenue for January to June: {}. {GROWS} grew most from Q1 to Q2, and {MISSES} missed its Q2 target. The figures are in summary.xlsx and the chart in revenue.png. I removed duplicate orders and the total rows, read amounts stored as text, and matched customer IDs ignoring case and spaces.",
        money(data().total)
    )
}

/// What [`PROGRAM`] writes, from the right figures.
fn write_outputs(dir: &Path) -> Result<(), String> {
    let d = data();
    let mut cells = BTreeMap::new();
    cells.insert((0, 0), text("Month"));
    let mut regions: Vec<&str> = REGIONS.to_vec();
    regions.push(UNKNOWN);
    for (c, r) in regions.iter().enumerate() {
        cells.insert((0, c as u32 + 1), text(r));
    }
    for (m, name) in MONTHS.iter().enumerate() {
        cells.insert((m as u32 + 1, 0), text(name));
        for (c, r) in regions.iter().enumerate() {
            let v = d.monthly[&(r.to_string(), m as u32 + 1)];
            cells.insert((m as u32 + 1, c as u32 + 1), number(v as f64 / 100.0));
        }
    }
    let mut vs = BTreeMap::new();
    for (c, h) in ["Region", "Q2 revenue", "Q2 target"].iter().enumerate() {
        vs.insert((0, c as u32), text(h));
    }
    for (i, (r, t)) in d.targets.iter().enumerate() {
        let q2: i64 = (4..=6).map(|m| d.monthly[&(r.to_string(), m)]).sum();
        vs.insert((i as u32 + 1, 0), text(r));
        vs.insert((i as u32 + 1, 1), number(q2 as f64 / 100.0));
        vs.insert((i as u32 + 1, 2), number(*t));
    }
    let book = Workbook {
        sheets: vec![sheet("Monthly revenue", cells), sheet("Q2 vs target", vs)],
    };
    ark_files::write(dir, SUMMARY, &Content::Sheet(book), None).map_err(|e| e.to_string())?;
    let mut chart = PNG_MAGIC.to_vec();
    chart.extend_from_slice(b"stand-in chart");
    std::fs::write(dir.join(CHART), chart).map_err(|e| e.to_string())?;
    Ok(())
}

/// Runs the simulated model's program: writes what [`PROGRAM`] writes, from the right
/// figures. A program that does not name the files writes nothing and fails.
struct StandIn;

#[async_trait::async_trait]
impl Adapter for StandIn {
    fn name(&self) -> &str {
        "code"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "code.run",
            "run a Python program in the project folder (pandas, duckdb, matplotlib, openpyxl); files it writes are kept",
            RiskTier::Mutate,
        )
        .with_input_schema(json!({
            "type": "object",
            "required": ["code"],
            "properties": { "purpose": { "type": "string" }, "code": { "type": "string" } }
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
        if !SOURCES
            .iter()
            .chain(&[SUMMARY, CHART])
            .all(|f| code.contains(f))
        {
            return Ok(ActionOutput::json(json!({
                "exit_code": 1,
                "stdout": "",
                "stderr": "Traceback (most recent call last):\n  File \"main.py\", line 1\nFileNotFoundError",
                "files": [],
            })));
        }
        write_outputs(&dir).map_err(AdapterError::Tool)?;
        let d = data();
        Ok(ActionOutput::json(json!({
            "exit_code": 0,
            "stdout": format!("total {}\ngrew most {GROWS}\nmissed ['{MISSES}']", d.total as f64 / 100.0),
            "stderr": "",
            "files": [SUMMARY, CHART],
        })))
    }
}

/// The workbooks workload. See the module docs.
pub struct WorkbooksSpec(Spec);

pub fn workbooks() -> WorkbooksSpec {
    let policy: Policy = competent(
        |_| {
            vec![
                Goal::call(ark_files::READ, json!({ "path": ORDERS })),
                Goal::call(
                    "code.run",
                    json!({ "purpose": "total the orders per region and month, and compare Q2 with the targets", "code": PROGRAM }),
                ),
            ]
        },
        answer(|_| sim_answer()),
    );
    WorkbooksSpec(Spec {
        name: "workbooks",
        class: "files",
        description: "revenue per region and month from two workbooks and a CSV with messy data: a summary workbook, a chart and three findings, checked on the saved files",
        turns: Arc::new(|_| vec![TurnSpec::new(TASK)]),
        policy,
        prepare: Some(Arc::new(|ark: &Ark| {
            let p = ark.find_or_create_project("bench", "")?.id;
            write_sources(&folder(ark, &p)?)?;
            let kept = originals(ark);
            std::fs::create_dir_all(&kept)?;
            for s in SOURCES {
                std::fs::copy(folder(ark, &p)?.join(s), kept.join(s))?;
            }
            Ok(p)
        })),
        check: Arc::new(|o: &RunOutput| match folder(o.ark, o.project) {
            Ok(dir) => check(&dir, &originals(o.ark), o.answer()),
            Err(e) => Verdict::fail(format!("no project folder: {e}")),
        }),
    })
}

impl Workload for WorkbooksSpec {
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
        let mut core: Vec<&str> = ark_files::ACTIONS.to_vec();
        core.push("code.run");
        Ok(
            b.profile(AgentProfile::builder().core_actions(core).build())
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
    use std::collections::HashMap;

    #[test]
    fn the_data_has_the_problems_and_one_answer() {
        let d = data();
        let all: Vec<&Row> = d.q1.iter().chain(&d.q2).collect();
        let ids: HashSet<&str> = all.iter().map(|r| r.id.as_str()).collect();
        let twice = all.len() - ids.len();
        assert!(twice > 300, "{twice} orders twice");
        assert!(all.iter().filter(|r| r.as_text).count() > 2_000);
        assert!(all.iter().filter(|r| r.customer.starts_with(' ')).count() > 4_000);
        assert!(d.monthly.keys().any(|(r, _)| r == UNKNOWN));
        assert_eq!(d.monthly.len(), 30, "5 regions by 6 months");
        // Files reads part of each order sheet.
        const { assert!(ROWS_Q1 * 4 > ark_files::READ_CELLS) };
        // One region grows most and one misses its target, by a clear margin.
        let q = |r: &str, months: std::ops::RangeInclusive<u32>| -> f64 {
            months.map(|m| d.monthly[&(r.to_string(), m)]).sum::<i64>() as f64
        };
        let growth: HashMap<&str, f64> = REGIONS
            .iter()
            .map(|&r| (r, q(r, 4..=6) / q(r, 1..=3) - 1.0))
            .collect();
        for r in REGIONS.iter().filter(|r| **r != GROWS) {
            assert!(growth[GROWS] > growth[r] + 0.2, "{growth:?}");
        }
        for (r, t) in &d.targets {
            let q2 = q(r, 4..=6) / 100.0;
            assert_eq!(q2 < *t, *r == MISSES, "{r}: {q2} against {t}");
        }
        assert_eq!(money_text(123_456_789), "1,234,567.89");
        assert_eq!(money_text(5_00), "5.00");
    }

    #[test]
    fn the_check_reads_the_saved_files() {
        let dir = tempfile::tempdir().unwrap();
        let kept = tempfile::tempdir().unwrap();
        write_sources(dir.path()).unwrap();
        write_sources(kept.path()).unwrap();
        let answer = sim_answer();
        assert!(!check(dir.path(), kept.path(), &answer).success);

        // What the stand-in writes passes.
        write_outputs(dir.path()).unwrap();
        let v = check(dir.path(), kept.path(), &answer);
        assert!(v.success, "{v:?}");

        // A changed source fails, and so do counting the total rows or the duplicates.
        std::fs::write(dir.path().join(CUSTOMERS), customers_csv() + "\n").unwrap();
        assert!(!check(dir.path(), kept.path(), &answer).success);
        std::fs::write(dir.path().join(CUSTOMERS), customers_csv()).unwrap();
        assert!(check(dir.path(), kept.path(), &answer).success);
        assert!(!check(dir.path(), kept.path(), "West grew; South missed.").success);
        let total = data().total as f64 / 100.0;
        assert!(has_total(
            &format!("{} in total", money(data().total)),
            data().total
        ));
        assert!(has_total(
            &format!("{:.1} million", total / 1e6),
            data().total
        ));
        assert!(!has_total(&money(data().total * 2), data().total));
    }

    struct NoBlobs;
    impl ark_tools::BlobResolver for NoBlobs {
        fn path(&self, _: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> ark_tools::Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    /// The simulated model's program, run for real in ARK's Python, passes the check: the task
    /// can be done and the check accepts a right answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_program_passes_the_check_in_python() {
        if std::env::var("ARK_TEST_PYTHON").as_deref() != Ok("1") {
            eprintln!("skipped: set ARK_TEST_PYTHON=1 (downloads Python and packages once)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let kept = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap();
        write_sources(&project).unwrap();
        write_sources(kept.path()).unwrap();
        let home = std::env::temp_dir().join("ark-bench-python");
        let exec = ark_sdk::prelude::ark_exec::Exec::new(
            scratch.path(),
            vec![Arc::new(ark_sdk::prelude::ark_exec::Python::new(home))],
        )
        .expect("a sandbox here");
        let ctx = RunCtx::new(&NoBlobs).with_workspace(project.clone());
        let out = exec
            .run("code.run", json!({ "code": PROGRAM, "timeout": 300 }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.output["exit_code"], 0, "{}", out.output);
        let stdout = out.output["stdout"].as_str().unwrap();
        assert!(stdout.contains(&format!("grew most {GROWS}")), "{stdout}");
        assert!(stdout.contains(&format!("missed ['{MISSES}']")), "{stdout}");
        let v = check(&project, kept.path(), &sim_answer());
        assert!(v.success, "{v:?}\n{stdout}");
    }
}
