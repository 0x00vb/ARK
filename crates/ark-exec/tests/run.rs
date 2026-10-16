//! `code.run` against the real sandbox (macOS: `sandbox-exec`, Linux: bubblewrap), with `sh` as
//! the runtime so no install is needed. The checks look at what happened, not at error texts,
//! which differ between the two. Python itself runs in `python_analysis_writes_a_chart_and_a_table`
//! when `ARK_TEST_PYTHON=1` (the first run downloads Python and packages with uv). Windows has
//! its own file, `run_windows.rs`.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_core::ArtifactId;
use ark_exec::{distill, Exec, Launch, Python, Runtime, Sandbox, RUN};
use ark_tools::{ActionOutput, Adapter, BlobResolver, RunCtx};
use serde_json::json;

struct Sh;

#[async_trait::async_trait]
impl Runtime for Sh {
    fn name(&self) -> &'static str {
        "sh"
    }
    fn says(&self) -> String {
        "a POSIX shell".into()
    }
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    async fn ready(&self) -> Result<Launch, String> {
        Ok(Launch {
            program: PathBuf::from("/bin/sh"),
            args: vec![],
            file: "main.sh",
            env: vec![],
            reads: vec![],
            home: vec![],
        })
    }
}

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ArtifactId) -> Option<PathBuf> {
        None
    }
    fn read(&self, id: &ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

struct Setup {
    _dirs: Vec<tempfile::TempDir>,
    project: PathBuf,
    outside: PathBuf,
    scratch: PathBuf,
    exec: Exec,
}

fn setup(runtime: Arc<dyn Runtime>) -> Setup {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let exec = Exec::with_sandbox(Sandbox::native().unwrap(), scratch.path(), vec![runtime]);
    Setup {
        project: project.path().canonicalize().unwrap(),
        outside: outside.path().canonicalize().unwrap(),
        scratch: scratch.path().to_path_buf(),
        exec,
        _dirs: vec![project, outside, scratch],
    }
}

async fn run(exec: &Exec, project: &Path, code: &str) -> ActionOutput {
    let blobs = NoBlobs;
    let ctx = RunCtx::new(&blobs).with_workspace(project.to_path_buf());
    exec.run(RUN, json!({ "code": code, "timeout": 20 }), &ctx)
        .await
        .unwrap()
}

async fn until(mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "run did not reach the expected state"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

struct HeldSetup {
    entered: std::sync::atomic::AtomicUsize,
    release: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl Runtime for HeldSetup {
    fn name(&self) -> &'static str {
        "sh"
    }
    fn says(&self) -> String {
        "held setup".into()
    }
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    async fn ready(&self) -> Result<Launch, String> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.release.acquire().await.unwrap().forget();
        Sh.ready().await
    }
}

#[tokio::test]
async fn queued_calls_do_no_setup_and_a_cancelled_setup_retains_its_project_slot() {
    use std::sync::atomic::Ordering;
    let held = Arc::new(HeldSetup {
        entered: 0.into(),
        release: tokio::sync::Semaphore::new(0),
    });
    let s = setup(held.clone());
    let exec = Arc::new(s.exec);
    let (e, p) = (exec.clone(), s.project.clone());
    let first = tokio::spawn(async move { run(&e, &p, "echo first > first").await });
    until(|| held.entered.load(Ordering::SeqCst) == 1).await;
    first.abort();
    let _ = first.await;
    let (e, p) = (exec.clone(), s.project.clone());
    let second = tokio::spawn(async move { run(&e, &p, "echo second > second").await });
    tokio::task::yield_now().await;
    assert_eq!(held.entered.load(Ordering::SeqCst), 1);
    held.release.add_permits(1);
    until(|| held.entered.load(Ordering::SeqCst) == 2).await;
    assert!(!s.project.join("first").exists());
    held.release.add_permits(1);
    assert_eq!(second.await.unwrap().output["exit_code"], 0);
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn two_active_calls_and_eight_waiters_are_shared_across_projects() {
    use std::sync::atomic::Ordering;
    let held = Arc::new(HeldSetup {
        entered: 0.into(),
        release: tokio::sync::Semaphore::new(0),
    });
    let s = setup(held.clone());
    let exec = Arc::new(s.exec);
    let mut projects = Vec::new();
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let project = tempfile::tempdir().unwrap();
        let (e, p) = (exec.clone(), project.path().canonicalize().unwrap());
        tasks.push(tokio::spawn(async move { run(&e, &p, "echo ok").await }));
        projects.push(project);
        tokio::task::yield_now().await;
    }
    until(|| held.entered.load(Ordering::SeqCst) == 2).await;
    let blobs = NoBlobs;
    let extra = tempfile::tempdir().unwrap();
    let ctx = RunCtx::new(&blobs).with_workspace(extra.path().to_path_buf());
    let result = exec.run(RUN, json!({"code":"echo extra"}), &ctx).await;
    assert!(
        matches!(&result, Err(ark_tools::AdapterError::ToolUnavailable { detail, .. })
            if detail.contains("too many code runs")),
        "{:?}",
        result.err()
    );
    assert_eq!(held.entered.load(Ordering::SeqCst), 2);
    held.release.add_permits(10);
    for task in tasks {
        assert_eq!(task.await.unwrap().output["exit_code"], 0);
    }
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn cancellation_stops_the_child_before_scratch_is_removed() {
    let s = setup(Arc::new(Sh));
    let exec = Arc::new(s.exec);
    let (e, p) = (exec.clone(), s.project.clone());
    let job = tokio::spawn(async move {
        run(
            &e,
            &p,
            "echo $$ > pid; while :; do echo tick >> heartbeat; sleep 0.02; done",
        )
        .await
    });
    until(|| s.project.join("heartbeat").exists()).await;
    // bubblewrap gives the shell a PID inside its own namespace. Only macOS's value
    // identifies a host process. The heartbeat check below applies to both platforms.
    #[cfg(target_os = "macos")]
    let pid = std::fs::read_to_string(s.project.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    job.abort();
    let _ = job.await;
    until(|| std::fs::read_dir(&s.scratch).unwrap().count() == 0).await;
    #[cfg(target_os = "macos")]
    assert!(!ark_os::process::alive(pid));
    let stopped = std::fs::metadata(s.project.join("heartbeat"))
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::metadata(s.project.join("heartbeat"))
            .unwrap()
            .len(),
        stopped
    );
    assert_eq!(
        run(&exec, &s.project, "echo recovered").await.output["exit_code"],
        0
    );
}

#[tokio::test]
async fn before_versions_are_outside_the_programs_writable_scratch() {
    let s = setup(Arc::new(Sh));
    std::fs::write(s.project.join("source"), b"original").unwrap();
    let out = run(
        &s.exec,
        &s.project,
        // Try both the copies' old place (beside the program) and the new one (beside its
        // writable folder): neither may change what Restore gets back.
        "d=$(dirname \"$0\"); for b in \"$d/before\" \"$d/../before\"; do \
         [ -f \"$b/source\" ] && echo poisoned > \"$b/source\"; done; echo changed > source",
    )
    .await;
    let original = out
        .produced
        .iter()
        .find(|p| p.before && p.path.as_deref() == Some("source"))
        .unwrap();
    assert_eq!(original.bytes, b"original");
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn a_scratch_folder_the_program_locks_is_removed_and_later_runs_work() {
    let s = setup(Arc::new(Sh));
    let locked = "d=$(dirname \"$0\")/locked; mkdir \"$d\"; touch \"$d/f\"; chmod 000 \"$d\"";
    assert_eq!(
        run(&s.exec, &s.project, locked).await.output["exit_code"],
        0
    );
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
    assert!(s.exec.health().await.available);
    assert_eq!(
        run(&s.exec, &s.project, "echo next").await.output["exit_code"],
        0
    );
}

#[tokio::test]
async fn a_completed_run_leaves_no_async_owner_or_output_readers() {
    let s = setup(Arc::new(Sh));
    let metrics = tokio::runtime::Handle::current().metrics();
    let before = metrics.num_alive_tasks();
    assert_eq!(
        run(&s.exec, &s.project, "echo complete").await.output["exit_code"],
        0
    );
    // An aborted reader can take a scheduler pass to be counted as gone; a leaked owner or a
    // reader held by the pipe would stay past this wait.
    until(|| metrics.num_alive_tasks() <= before).await;
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn a_run_returns_the_files_it_wrote_in_the_project() {
    let s = setup(Arc::new(Sh));
    std::fs::write(s.project.join("old.txt"), "before").unwrap();
    let out = run(
        &s.exec,
        &s.project,
        "mkdir -p out && echo a,b > out/table.csv && echo done",
    )
    .await;
    assert_eq!(out.output["exit_code"], 0);
    assert_eq!(out.output["files"], json!(["out/table.csv"]));
    assert_eq!(out.produced.len(), 1);
    assert_eq!(out.produced[0].name, "table.csv");
    assert_eq!(out.produced[0].path.as_deref(), Some("out/table.csv"));
    assert_eq!(out.produced[0].bytes, b"a,b\n");
    let card = distill()(RUN, &out.output).unwrap();
    assert_eq!(card.0, "exit 0\nwrote out/table.csv\nstdout:\ndone");
    assert_eq!(card.1, vec!["out/table.csv".to_string()]);
}

/// ADR 0034: a file the run changed or removed keeps its version before; a file it wrote again
/// with the same bytes, or made new, keeps none.
#[tokio::test]
async fn a_run_keeps_the_version_before_of_what_it_changed_or_removed() {
    let s = setup(Arc::new(Sh));
    std::fs::create_dir_all(s.project.join("data")).unwrap();
    std::fs::write(s.project.join("data/sales.csv"), "north,120\n").unwrap();
    std::fs::write(s.project.join("notes.txt"), "keep\n").unwrap();
    std::fs::write(s.project.join("old.txt"), "old\n").unwrap();
    std::fs::write(s.project.join("same.txt"), "same\n").unwrap();
    // Older than the run, so "written again" shows in the file's time.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = run(
        &s.exec,
        &s.project,
        "echo north,999 > data/sales.csv && rm old.txt && echo same > same.txt && echo new > new.txt",
    )
    .await;
    assert_eq!(out.output["exit_code"], 0);
    assert_eq!(out.output["removed"], json!(["old.txt"]));
    assert_eq!(out.output["no_copy"], json!([]));
    let mut before: Vec<(String, String)> = out
        .produced
        .iter()
        .filter(|a| a.before)
        .map(|a| {
            (
                a.path.clone().unwrap(),
                String::from_utf8(a.bytes.clone()).unwrap(),
            )
        })
        .collect();
    before.sort();
    assert_eq!(
        before,
        vec![
            ("data/sales.csv".into(), "north,120\n".into()),
            ("old.txt".into(), "old\n".into()),
        ]
    );
    // What the run wrote is still each file's snapshot, as before.
    let mut wrote: Vec<String> = out
        .produced
        .iter()
        .filter(|a| !a.before)
        .map(|a| a.path.clone().unwrap())
        .collect();
    wrote.sort();
    assert_eq!(wrote, ["data/sales.csv", "new.txt", "same.txt"]);
    let card = distill()(RUN, &out.output).unwrap();
    assert!(card.0.contains("\nremoved old.txt"), "{}", card.0);
    // The copies went with the run's folder.
    assert_eq!(std::fs::read_dir(&s.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn a_run_cannot_write_outside_the_project_or_into_its_kept_folders() {
    let s = setup(Arc::new(Sh));
    let escape = s.outside.join("escape.txt");
    std::fs::create_dir_all(s.project.join(".git")).unwrap();
    let code = format!(
        "echo x > '{}'; echo x > ../escape.txt; mkdir -p .git/hooks; echo x > .git/hooks/pre-commit; mkdir -p .vscode; echo x > .vscode/tasks.json; echo x > ok.txt",
        escape.display()
    );
    let out = run(&s.exec, &s.project, &code).await;
    assert!(!escape.exists());
    assert!(!s.project.parent().unwrap().join("escape.txt").exists());
    assert!(!s.project.join(".git/hooks/pre-commit").exists());
    // A kept folder that did not exist is not there after the run either.
    assert!(!s.project.join(".vscode").exists());
    assert!(s.project.join("ok.txt").exists());
    assert!(
        !out.output["stderr"].as_str().unwrap().is_empty(),
        "{}",
        out.output
    );
}

#[tokio::test]
async fn a_run_cannot_read_outside_the_project_or_reach_the_network() {
    let s = setup(Arc::new(Sh));
    std::fs::write(s.outside.join("secret.txt"), "the secret").unwrap();
    let home = std::env::var("HOME").unwrap();
    let code = format!(
        "cat '{}/secret.txt'; ls '{home}'; /usr/bin/curl -sS -m 5 https://example.com && echo REACHED",
        s.outside.display()
    );
    let out = run(&s.exec, &s.project, &code).await;
    let all = format!("{}{}", out.output["stdout"], out.output["stderr"]);
    assert!(!all.contains("the secret"), "{all}");
    assert!(!all.contains("REACHED"), "{all}");
    assert!(!out.output["stderr"].as_str().unwrap().is_empty(), "{all}");
}

#[tokio::test]
async fn a_run_past_its_timeout_is_stopped() {
    let s = setup(Arc::new(Sh));
    let blobs = NoBlobs;
    let ctx = RunCtx::new(&blobs).with_workspace(s.project.clone());
    let t = std::time::Instant::now();
    let out = s
        .exec
        .run(
            RUN,
            json!({ "code": "echo started >&2; sleep 30", "timeout": 1 }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(t.elapsed().as_secs() < 10);
    assert_eq!(out.output["timed_out"], true);
    assert!(distill()(RUN, &out.output)
        .unwrap()
        .0
        .starts_with("stopped"));
}

#[tokio::test]
async fn the_description_names_the_runtimes_and_what_the_window_shows() {
    let s = setup(Arc::new(Sh));
    let exec = s.exec.shows("tables as csv");
    let d = &exec.actions()[0];
    assert!(d.summary.contains("sh (a POSIX shell)"), "{}", d.summary);
    assert!(d.summary.contains("no network"));
    assert!(
        d.summary.ends_with("shown to the person: tables as csv."),
        "{}",
        d.summary
    );
    assert!(!d.risk.auto_runs());
}

/// The workflow the capability is for, with the real environment.
#[tokio::test]
async fn python_analysis_writes_a_chart_and_a_table() {
    if std::env::var("ARK_TEST_PYTHON").as_deref() != Ok("1") {
        eprintln!("skipped: set ARK_TEST_PYTHON=1 (downloads Python and packages once)");
        return;
    }
    let home = std::env::temp_dir().join("ark-exec-test-python");
    let s = setup(Arc::new(Python::new(&home)));
    std::fs::write(
        s.project.join("sales.csv"),
        "month,sales\nJan,3\nFeb,5\nMar,4\n",
    )
    .unwrap();
    let code = r#"
import pandas as pd, matplotlib.pyplot as plt
df = pd.read_csv("sales.csv")
print(df["sales"].sum())
df.plot(x="month", y="sales").get_figure().savefig("chart.png")
df.describe().to_excel("summary.xlsx")
"#;
    let out = run(&s.exec, &s.project, code).await;
    assert_eq!(out.output["exit_code"], 0, "{}", out.output);
    assert!(out.output["stdout"].as_str().unwrap().contains("12"));
    // matplotlib finds the cache it built at install: no warning, no 20 s rebuild.
    assert!(
        !out.output["stderr"]
            .as_str()
            .unwrap()
            .contains("not a writable directory"),
        "{}",
        out.output["stderr"]
    );
    let mut files: Vec<&str> = out
        .produced
        .iter()
        .filter_map(|p| p.path.as_deref())
        .collect();
    files.sort();
    assert_eq!(files, ["chart.png", "summary.xlsx"]);
    assert!(out.produced.iter().any(|p| p.bytes.starts_with(b"\x89PNG")));
}

/// The data tools of the environment: a workbook read fast (calamine) and in SQL (DuckDB's
/// `excel` extension, installed at setup since a run has no network), joined with a CSV, and a
/// new workbook written.
#[tokio::test]
async fn python_joins_a_workbook_and_a_csv_in_sql() {
    if std::env::var("ARK_TEST_PYTHON").as_deref() != Ok("1") {
        eprintln!("skipped: set ARK_TEST_PYTHON=1 (downloads Python and packages once)");
        return;
    }
    let home = std::env::temp_dir().join("ark-exec-test-python");
    let s = setup(Arc::new(Python::new(&home)));
    std::fs::write(
        s.project.join("regions.csv"),
        "id,region\n1,North\n2,South\n",
    )
    .unwrap();
    let make = r#"
import pandas as pd
pd.DataFrame({"id": [1, 2, 1], "amount": [10.5, 20.0, 4.5]}).to_excel("orders.xlsx", sheet_name="Orders", index=False)
"#;
    let out = run(&s.exec, &s.project, make).await;
    assert_eq!(out.output["exit_code"], 0, "{}", out.output);
    let code = r#"
import duckdb, pandas as pd
fast = pd.read_excel("orders.xlsx", sheet_name=None, engine="calamine")
print("sheets", list(fast))
t = duckdb.sql("""
  select r.region, sum(o.amount) as total
  from read_xlsx('orders.xlsx', sheet = 'Orders') o
  join read_csv('regions.csv') r using (id)
  group by r.region order by r.region
""").df()
print(t.to_string(index=False))
with pd.ExcelWriter("totals.xlsx", engine="xlsxwriter") as w:
    t.to_excel(w, sheet_name="Totals", index=False)
import pyarrow
"#;
    let out = run(&s.exec, &s.project, code).await;
    assert_eq!(out.output["exit_code"], 0, "{}", out.output);
    let stdout = out.output["stdout"].as_str().unwrap();
    assert!(stdout.contains("sheets ['Orders']"), "{stdout}");
    let row = |r: &str| {
        stdout
            .lines()
            .find(|l| l.contains(r))
            .unwrap_or("")
            .to_string()
    };
    assert!(row("North").ends_with("15.0"), "{stdout}");
    assert!(row("South").ends_with("20.0"), "{stdout}");
    assert!(s.project.join("totals.xlsx").exists());
}
