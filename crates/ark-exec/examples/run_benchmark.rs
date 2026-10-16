//! Compare this identical driver on the baseline and candidate, through the public Exec API.
//! Arguments: new-owned-folder Python-home shell|python|snapshot|many|rewrite|change concurrency iterations.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ark_core::ArtifactId;
use ark_exec::{Exec, Launch, Python, Runtime, Sandbox, RUN};
use ark_tools::{Adapter, BlobResolver, RunCtx};
use serde_json::json;

struct NoBlobs;
impl BlobResolver for NoBlobs {
    fn path(&self, _: &ArtifactId) -> Option<PathBuf> {
        None
    }
    fn read(&self, id: &ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

struct Shell;
#[async_trait::async_trait]
impl Runtime for Shell {
    fn name(&self) -> &'static str {
        "sh"
    }
    fn says(&self) -> String {
        "benchmark shell".into()
    }
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    async fn ready(&self) -> Result<Launch, String> {
        Ok(Launch {
            program: "/bin/sh".into(),
            args: vec![],
            file: "main.sh",
            env: vec![],
            reads: vec![],
            home: vec![],
        })
    }
}

struct Measured {
    name: &'static str,
    inner: Arc<dyn Runtime>,
    submitted: Mutex<Instant>,
    ready_us: AtomicU64,
}
#[async_trait::async_trait]
impl Runtime for Measured {
    fn name(&self) -> &'static str {
        self.name
    }
    fn says(&self) -> String {
        self.inner.says()
    }
    fn available(&self) -> Result<(), String> {
        self.inner.available()
    }
    async fn ready(&self) -> Result<Launch, String> {
        self.ready_us.store(
            self.submitted.lock().unwrap().elapsed().as_micros() as u64,
            Ordering::Relaxed,
        );
        self.inner.ready().await
    }
}

const ANALYSIS: &str = r#"import pandas as pd, numpy as np, duckdb
import matplotlib.pyplot as plt
d = pd.read_csv('input.csv')
t = d.groupby('bucket')['value'].sum()
t.to_csv('totals.csv')
fig, ax = plt.subplots(figsize=(4, 2))
ax.bar(t.index, t.values)
fig.savefig('totals.png')
plt.close(fig)
assert int(np.sum(d['value'])) == 49995000
assert duckdb.sql("select sum(value) from read_csv('input.csv')").fetchone()[0] == 49995000
print('verified:10000:49995000')
"#;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(
        args.len(),
        5,
        "output-folder Python-home mode concurrency iterations"
    );
    let root = PathBuf::from(&args[0]);
    let mode = args[2].clone();
    let concurrency: usize = args[3].parse().unwrap();
    let iterations: usize = args[4].parse().unwrap();
    assert!((1..=4).contains(&concurrency));
    std::fs::create_dir(&root).expect("the benchmark output folder must be new");
    let inner: Arc<dyn Runtime> = if mode == "python" {
        Arc::new(Python::new(&args[1]))
    } else {
        Arc::new(Shell)
    };
    // Warm shared Python setup separately; no install is included in the measurement.
    inner.ready().await.unwrap();
    let names = ["a", "b", "c", "d"];
    let mut measured = Vec::new();
    let mut projects = Vec::new();
    for (i, name) in names.iter().enumerate().take(concurrency) {
        let project = root.join(format!("project-{i}"));
        std::fs::create_dir_all(&project).unwrap();
        if mode == "python" {
            let data = std::iter::once("bucket,value\n".to_string())
                .chain((0..10000).map(|n| format!("{},{}\n", n % 10, n)))
                .collect::<String>();
            std::fs::write(project.join("input.csv"), data).unwrap();
        } else if matches!(mode.as_str(), "snapshot" | "rewrite" | "change") {
            let file = std::fs::File::create(project.join("data.bin")).unwrap();
            file.set_len(32 * 1024 * 1024).unwrap();
        } else if mode == "many" {
            for n in 0..200 {
                std::fs::write(project.join(format!("file-{n:03}")), vec![b'x'; 12 * 1024])
                    .unwrap();
            }
        }
        projects.push(project.canonicalize().unwrap());
        measured.push(Arc::new(Measured {
            name,
            inner: inner.clone(),
            submitted: Mutex::new(Instant::now()),
            ready_us: 0.into(),
        }));
    }
    let runtimes: Vec<Arc<dyn Runtime>> = measured
        .iter()
        .cloned()
        .map(|r| r as Arc<dyn Runtime>)
        .collect();
    let exec = Arc::new(Exec::with_sandbox(
        Sandbox::native().expect("native sandbox"),
        root.join("runs"),
        runtimes,
    ));
    let code = if mode == "python" {
        ANALYSIS
    } else if mode == "rewrite" {
        "printf x | dd of=data.bin conv=notrunc 2>/dev/null; printf 'verified:rewrite\\n'"
    } else {
        "printf 'verified:shell\\n'"
    };
    let mut samples = Vec::new();
    let started = Instant::now();
    for iteration in 0..iterations + 2 {
        let batch_start = Instant::now();
        let mut jobs = Vec::new();
        for (runtime, project) in measured.iter().zip(&projects) {
            let (exec, runtime, project) = (exec.clone(), runtime.clone(), project.clone());
            // `rewrite` measures equality on a large file. `change` instead retains its
            // previous version every time, including after the warmup batches.
            let code = if mode == "change" {
                format!(
                    "printf {} | dd of=data.bin conv=notrunc 2>/dev/null; printf 'verified:change\\n'",
                    if iteration % 2 == 0 { "x" } else { "y" }
                )
            } else {
                code.to_owned()
            };
            let changed = mode == "change";
            *runtime.submitted.lock().unwrap() = Instant::now();
            jobs.push(tokio::spawn(async move {
                let start = Instant::now();
                let ctx = RunCtx::new(&NoBlobs).with_workspace(project);
                let out = exec
                    .run(
                        RUN,
                        json!({"code":&code,"runtime":runtime.name,"timeout":60}),
                        &ctx,
                    )
                    .await
                    .unwrap();
                assert_eq!(out.output["exit_code"], 0, "{}", out.output);
                assert!(out.output["stdout"].as_str().unwrap().contains("verified:"));
                if code == ANALYSIS {
                    assert!(out
                        .produced
                        .iter()
                        .any(|p| p.path.as_deref() == Some("totals.csv")));
                    assert!(out
                        .produced
                        .iter()
                        .any(|p| p.path.as_deref() == Some("totals.png")));
                }
                if changed {
                    let before = out
                        .produced
                        .iter()
                        .find(|p| p.before && p.path.as_deref() == Some("data.bin"))
                        .expect("the changed large file keeps its previous version");
                    assert_eq!(before.bytes.len(), 32 * 1024 * 1024);
                    let previous = if iteration == 0 {
                        0
                    } else if iteration % 2 == 0 {
                        b'y'
                    } else {
                        b'x'
                    };
                    assert_eq!(before.bytes[0], previous);
                    assert!(before.bytes[1..].iter().all(|b| *b == 0));
                }
                (
                    start.elapsed().as_micros() as u64,
                    runtime.ready_us.load(Ordering::Relaxed),
                )
            }));
        }
        let mut runs = Vec::new();
        for job in jobs {
            let (elapsed_us, ready_us) = job.await.unwrap();
            runs.push(json!({"elapsed_us":elapsed_us,"ready_entry_us":ready_us}));
        }
        if iteration >= 2 {
            samples.push(json!({"batch_us":batch_start.elapsed().as_micros() as u64,"runs":runs}));
        }
    }
    assert_eq!(std::fs::read_dir(root.join("runs")).unwrap().count(), 0);
    println!(
        "{}",
        json!({"mode":mode,"concurrency":concurrency,"iterations":iterations,"warmup_batches":2,"total_ms":started.elapsed().as_millis(),"samples":samples,"success":true})
    );
}
