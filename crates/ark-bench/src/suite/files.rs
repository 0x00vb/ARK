//! The `files-edit` workload: a spreadsheet in the project folder gets a computed column and a
//! total row through ARK's Files tools (`files.read`, `files.edit`), and keeps what the task does
//! not touch. The run succeeds when the saved file has the values, not when the answer says so.
//! A small model was seen reading the file and adding a sheet, then stopping before it wrote any
//! values (`bench/RESULTS.md`); this workload measures that. The simulated model reads the file,
//! then stops with "I will now call files.edit" as that model did; told that nothing ran, it
//! sets the cells in one edit and answers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_sdk::prelude::ark_files::{self, Cell, Content, Sheet, Value as CellValue, Workbook};
use ark_sdk::prelude::*;
use serde_json::{json, Value};

use super::{answer, one, tools, Spec};
use std::sync::Arc;

use crate::sim::{competent, Goal, Policy, Reply, SimRequest};
use crate::workload::{RunOutput, Verdict};

const FILE: &str = "sales.xlsx";
const TASK: &str = "In sales.xlsx, add a Revenue column (Units times Price) for every month, and a Total row under the months with the total Units and total Revenue.";

/// Month, units, price.
const MONTHS: [(&str, f64, f64); 3] = [
    ("January", 120.0, 10.0),
    ("February", 135.0, 10.0),
    ("March", 150.0, 12.0),
];

fn cell(value: CellValue) -> Cell {
    Cell {
        value,
        ..Cell::default()
    }
}

/// The workbook the person had: a Sales sheet and a Notes sheet the task must leave alone.
fn seed() -> Workbook {
    let mut sales = BTreeMap::new();
    for (c, h) in ["Month", "Units", "Price"].iter().enumerate() {
        sales.insert((0, c as u32), cell(CellValue::Text(h.to_string())));
    }
    for (r, (m, u, p)) in MONTHS.iter().enumerate() {
        let r = r as u32 + 1;
        sales.insert((r, 0), cell(CellValue::Text(m.to_string())));
        sales.insert((r, 1), cell(CellValue::Number(*u)));
        sales.insert((r, 2), cell(CellValue::Number(*p)));
    }
    let mut notes = BTreeMap::new();
    notes.insert(
        (0, 0),
        cell(CellValue::Text("Prices from the Q1 list".into())),
    );
    Workbook {
        sheets: vec![
            Sheet {
                name: "Sales".into(),
                cells: sales,
                origin: None,
                unread: 0,
            },
            Sheet {
                name: "Notes".into(),
                cells: notes,
                origin: None,
                unread: 0,
            },
        ],
        ..Workbook::default()
    }
}

fn folder(ark: &Ark, project: &ProjectId) -> anyhow::Result<PathBuf> {
    Ok(ark.engine().workspace_dir(project)?)
}

fn edit_args() -> Value {
    json!({
        "path": FILE,
        "edits": [
            { "cell": "D1", "rows": [["Revenue"], ["=B2*C2"], ["=B3*C3"], ["=B4*C4"]] },
            { "cell": "A5", "rows": [["Total", "=SUM(B2:B4)", "", "=SUM(D2:D4)"]] }
        ]
    })
}

fn number(v: &CellValue) -> Option<f64> {
    match v {
        CellValue::Number(n) => Some(*n),
        CellValue::Text(t) => t.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

/// What the saved file says: each month's revenue found in its row, the totals row, and
/// whether the Notes sheet is still there.
struct Saved {
    revenue: Vec<bool>,
    total_units: bool,
    total_revenue: bool,
    notes: bool,
}

fn saved(root: &std::path::Path) -> Option<Saved> {
    let file = ark_files::open(root, FILE).ok()?;
    let Content::Sheet(book) = file.content else {
        return None;
    };
    let sales = book.sheets.iter().find(|s| s.name == "Sales")?;
    let row_has = |r: u32, want: f64| {
        sales
            .cells
            .iter()
            .any(|((rr, c), v)| *rr == r && *c >= 3 && number(&v.value) == Some(want))
    };
    let revenue = MONTHS
        .iter()
        .enumerate()
        .map(|(i, (_, u, p))| row_has(i as u32 + 1, u * p))
        .collect();
    let units: f64 = MONTHS.iter().map(|m| m.1).sum();
    let total: f64 = MONTHS.iter().map(|m| m.1 * m.2).sum();
    // The totals row: any row under the months whose first cell says total.
    let total_row = sales.cells.iter().find_map(|((r, c), v)| {
        (*r > MONTHS.len() as u32
            && *c == 0
            && matches!(&v.value, CellValue::Text(t) if t.to_lowercase().contains("total")))
        .then_some(*r)
    });
    let in_total = |want: f64| {
        total_row.is_some_and(|r| {
            sales
                .cells
                .iter()
                .any(|((rr, _), v)| *rr == r && number(&v.value) == Some(want))
        })
    };
    Some(Saved {
        revenue,
        total_units: in_total(units),
        total_revenue: in_total(total),
        notes: book.sheets.iter().any(|s| s.name == "Notes"),
    })
}

/// Edit a spreadsheet the person keeps in the project folder.
pub fn files_edit() -> FilesSpec {
    let careful = competent(
        |_| {
            vec![
                Goal::call(ark_files::READ, json!({ "path": FILE })),
                Goal::call(ark_files::EDIT, edit_args()),
            ]
        },
        answer(|_| {
            format!("Added a Revenue column (D) and a Total row (row 5) to {FILE}. Total revenue is 4,350.")
        }),
    );
    // Stops after reading the way qwen3:4b did ("I will now call files.edit"), and makes the
    // edit once told that nothing ran.
    let policy: Policy = Arc::new(move |r: &SimRequest| {
        let read = r.result_of(ark_files::READ).is_some();
        let edited = r.count_calls(ark_files::EDIT) > 0;
        let answering = !r.is_planner() && !r.is_critic() && !r.tools.is_empty();
        if answering && read && !edited && !r.mentions("called no tool") {
            return Reply::Text(format!(
                "{FILE} has Month, Units and Price for three months.\n\nI will now call files.edit to add the Revenue column and the Total row."
            ));
        }
        careful(r)
    });
    FilesSpec(Spec {
        name: "files-edit",
        class: "files",
        description:
            "a computed column and a total row in a real spreadsheet; the saved file is checked",
        turns: one(TASK),
        policy,
        prepare: Some(Arc::new(|ark: &Ark| {
            let id = ark.find_or_create_project("bench", "")?.id;
            let root = folder(ark, &id)?;
            ark_files::write(&root, FILE, &Content::Sheet(seed()), None)?;
            Ok(id)
        })),
        check: Arc::new(|o: &RunOutput| {
            let Some(s) = folder(o.ark, o.project).ok().and_then(|r| saved(&r)) else {
                return Verdict::all(&[(false, "sales.xlsx is missing or not a spreadsheet")]);
            };
            Verdict::all(&[
                (
                    s.revenue.iter().all(|r| *r),
                    "a month's revenue is missing from its row",
                ),
                (s.total_units, "no Total row with the total units"),
                (s.total_revenue, "no Total row with the total revenue"),
                (s.notes, "the Notes sheet was lost"),
            ])
        }),
    })
}

/// The suite's tools with ARK's Files in place of the stand-in, preloaded the way an app with a project folder offers them.
pub struct FilesSpec(Spec);

impl crate::workload::Workload for FilesSpec {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn class(&self) -> &str {
        self.0.class()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn turns(&self, mode: crate::workload::Mode) -> Vec<crate::workload::TurnSpec> {
        self.0.turns(mode)
    }
    fn policy(&self) -> crate::sim::Policy {
        self.0.policy()
    }
    fn build(&self, env: &crate::workload::Env) -> anyhow::Result<Ark> {
        // The suite's neighbors, without its stand-in `files` adapter: ARK's Files is the real one.
        let mut registry = Registry::new();
        for a in tools::all().into_iter().filter(|a| a.name() != "files") {
            registry.register(a);
        }
        Ok(env
            .builder()
            .registry(registry)
            .files(true)
            .profile(
                AgentProfile::builder()
                    .core_actions(ark_files::ACTIONS.iter().copied())
                    .build(),
            )
            .build()?)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        self.0.prepare(ark)
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        self.0.check(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_reads_the_saved_values() {
        let dir = tempfile::tempdir().unwrap();
        ark_files::write(dir.path(), FILE, &Content::Sheet(seed()), None).unwrap();
        let before = saved(dir.path()).unwrap();
        assert!(before.notes && !before.revenue.iter().any(|r| *r) && !before.total_revenue);

        // What a model that stops after adding a sheet leaves: still a failure.
        let sheet_only = ark_files::Edit::parse_all(
            &json!([{ "add_sheet": "Revenue", "rows": [["Month", "Revenue"]] }]),
        )
        .unwrap();
        ark_files::edit(dir.path(), FILE, &sheet_only, None).unwrap();
        assert!(!saved(dir.path()).unwrap().total_revenue);

        let edits = ark_files::Edit::parse_all(&edit_args()["edits"]).unwrap();
        ark_files::edit(dir.path(), FILE, &edits, None).unwrap();
        let after = saved(dir.path()).unwrap();
        assert!(after.revenue.iter().all(|r| *r));
        assert!(after.total_units && after.total_revenue && after.notes);
    }
}
