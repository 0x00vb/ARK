//! `code.run` in an AppContainer, with `cmd.exe` as the runtime so no install is needed. The
//! launcher starts through this program's `main`, so the test has its own (`harness = false`)
//! and calls [`ark_exec::entry`] first, as an app does. Python runs too when
//! `ARK_TEST_PYTHON=1` (the first run downloads Python and packages with uv).
//!
//! Run on Windows: `cargo test -p ark-exec --test run_windows`.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    ark_exec::entry();
    win::main();
}

#[cfg(windows)]
mod win {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use ark_core::ArtifactId;
    use ark_exec::{Exec, Launch, Python, Runtime, Sandbox, RUN};
    use ark_tools::{ActionOutput, Adapter, BlobResolver, RunCtx};
    use serde_json::{json, Value};

    struct Cmd;

    #[async_trait::async_trait]
    impl Runtime for Cmd {
        fn name(&self) -> &'static str {
            "cmd"
        }
        fn says(&self) -> String {
            "the Windows command interpreter".into()
        }
        fn available(&self) -> Result<(), String> {
            Ok(())
        }
        async fn ready(&self) -> Result<Launch, String> {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            Ok(Launch {
                program: PathBuf::from(format!(r"{root}\System32\cmd.exe")),
                args: vec!["/d".into(), "/c".into()],
                file: "main.cmd",
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
        exec: Exec,
    }

    fn setup(runtime: Arc<dyn Runtime>) -> Setup {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::native().expect("an AppContainer sandbox after entry()");
        let exec = Exec::with_sandbox(sandbox, scratch.path(), vec![runtime]);
        Setup {
            project: project.path().to_path_buf(),
            outside: outside.path().to_path_buf(),
            exec,
            _dirs: vec![project, outside, scratch],
        }
    }

    async fn run(exec: &Exec, project: &Path, code: &str, timeout: u64) -> ActionOutput {
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs).with_workspace(project.to_path_buf());
        exec.run(RUN, json!({ "code": code, "timeout": timeout }), &ctx)
            .await
            .unwrap()
    }

    fn text(out: &ActionOutput) -> String {
        let s = |k: &str| out.output[k].as_str().unwrap_or("").to_string();
        format!("{}{}", s("stdout"), s("stderr"))
    }

    async fn writes_only_the_project() {
        let s = setup(Arc::new(Cmd));
        std::fs::create_dir_all(s.project.join(".git")).unwrap();
        let code = format!(
            "@echo off\r\nmkdir out\r\necho a,b> out\\table.csv\r\necho x> \"{}\\escape.txt\"\r\nmkdir .git\\hooks\r\necho x> .git\\hooks\\pre-commit\r\nmkdir .vscode\r\necho x> .vscode\\tasks.json\r\necho done\r\n",
            s.outside.display()
        );
        let out = run(&s.exec, &s.project, &code, 20).await;
        assert_eq!(
            out.output["files"],
            json!(["out/table.csv"]),
            "{}",
            text(&out)
        );
        assert!(!s.outside.join("escape.txt").exists(), "{}", text(&out));
        assert!(!s.project.join(".git/hooks/pre-commit").exists());
        assert!(!s.project.join(".vscode").exists());
    }

    async fn reads_only_the_project_and_has_no_network() {
        let s = setup(Arc::new(Cmd));
        std::fs::write(s.outside.join("secret.txt"), "the secret").unwrap();
        let profile = std::env::var("USERPROFILE").unwrap();
        let code = format!(
            "@echo off\r\ntype \"{}\\secret.txt\"\r\ndir \"{profile}\\Documents\"\r\ncurl.exe -sS -m 5 https://example.com && echo REACHED\r\n",
            s.outside.display()
        );
        let out = run(&s.exec, &s.project, &code, 20).await;
        let all = text(&out);
        assert!(!all.contains("the secret"), "{all}");
        assert!(!all.contains("REACHED"), "{all}");
    }

    async fn stops_past_its_timeout() {
        let s = setup(Arc::new(Cmd));
        let t = std::time::Instant::now();
        let out = run(
            &s.exec,
            &s.project,
            "@echo off\r\nfor /L %%i in (1,1,2000000000) do @rem\r\n",
            1,
        )
        .await;
        assert!(t.elapsed().as_secs() < 15);
        assert_eq!(out.output["timed_out"], Value::Bool(true));
    }

    async fn python_joins_a_workbook_and_a_csv_in_sql() {
        let home = std::env::temp_dir().join("ark-exec-test-python");
        let s = setup(Arc::new(Python::new(&home)));
        std::fs::write(
            s.project.join("regions.csv"),
            "id,region\n1,North\n2,South\n",
        )
        .unwrap();
        let code = r#"
import duckdb, pandas as pd, matplotlib.pyplot as plt
pd.DataFrame({"id": [1, 2, 1], "amount": [10.5, 20.0, 4.5]}).to_excel("orders.xlsx", sheet_name="Orders", index=False)
print("sheets", list(pd.read_excel("orders.xlsx", sheet_name=None, engine="calamine")))
t = duckdb.sql("select r.region, sum(o.amount) as total from read_xlsx('orders.xlsx', sheet = 'Orders') o join read_csv('regions.csv') r using (id) group by r.region order by r.region").df()
print(t.to_string(index=False))
t.plot(x="region", y="total", kind="bar").get_figure().savefig("chart.png")
"#;
        let out = run(&s.exec, &s.project, code, 300).await;
        assert_eq!(out.output["exit_code"], 0, "{}", text(&out));
        let stdout = out.output["stdout"].as_str().unwrap();
        let north = stdout.lines().find(|l| l.contains("North")).unwrap_or("");
        assert!(north.ends_with("15.0"), "{stdout}");
        assert!(s.project.join("chart.png").exists());
    }

    pub fn main() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writes_only_the_project().await;
            println!("ok: writes_only_the_project");
            reads_only_the_project_and_has_no_network().await;
            println!("ok: reads_only_the_project_and_has_no_network");
            stops_past_its_timeout().await;
            println!("ok: stops_past_its_timeout");
            if std::env::var("ARK_TEST_PYTHON").as_deref() == Ok("1") {
                python_joins_a_workbook_and_a_csv_in_sql().await;
                println!("ok: python_joins_a_workbook_and_a_csv_in_sql");
            } else {
                println!("skipped: python (set ARK_TEST_PYTHON=1)");
            }
        });
    }
}
