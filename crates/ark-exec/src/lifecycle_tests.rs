//! Test-only barriers inside the actual blocking phases. No hook exists in production builds.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Prepare,
    Capture,
}

pub(super) struct Hook {
    phase: Phase,
    hit: AtomicBool,
    entered: tokio::sync::Notify,
    resume: std::sync::Mutex<mpsc::Receiver<()>>,
}

impl Hook {
    pub(super) fn pause(&self, phase: Phase) {
        if self.phase == phase && !self.hit.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .expect("the test must release its blocking phase");
        }
    }
}

struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

struct Shell(AtomicUsize);
#[async_trait]
impl Runtime for Shell {
    fn name(&self) -> &'static str {
        "sh"
    }
    fn says(&self) -> String {
        "test shell".into()
    }
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    async fn ready(&self) -> Result<Launch, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
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

async fn cancellation_during(phase: Phase) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let scratch = root.path().join("runs");
    let shell = Arc::new(Shell(0.into()));
    let (send, recv) = mpsc::channel();
    let hook = Arc::new(Hook {
        phase,
        hit: false.into(),
        entered: tokio::sync::Notify::new(),
        resume: std::sync::Mutex::new(recv),
    });
    let release = Release(send);
    let mut exec = Exec::with_sandbox(Sandbox::native().unwrap(), &scratch, vec![shell.clone()]);
    exec.hook = Some(hook.clone());
    let exec = Arc::new(exec);
    let (owner, path) = (exec.clone(), project.clone());
    let first = tokio::spawn(async move {
        owner
            .run(&json!({"code":"echo first > first"}), &path)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
        .await
        .unwrap();
    first.abort();
    let _ = first.await;
    let (owner, path) = (exec.clone(), project.clone());
    let second = tokio::spawn(async move {
        owner
            .run(&json!({"code":"echo second > second"}), &path)
            .await
    });
    // Observe actual queue entry, rather than assuming the replacement was scheduled.
    tokio::time::timeout(Duration::from_secs(3), async {
        while exec.admission.queued() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(shell.0.load(Ordering::SeqCst), 1);
    assert!(!second.is_finished());
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 1);
    assert_eq!(project.join("first").exists(), phase == Phase::Capture);
    drop(release);
    let out = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(out.output["exit_code"], 0);
    assert_eq!(shell.0.load(Ordering::SeqCst), 2);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    assert!(project.join("second").exists());
}

#[tokio::test]
async fn cancelled_preparation_is_joined_before_scratch_and_admission_are_released() {
    cancellation_during(Phase::Prepare).await;
}

#[tokio::test]
async fn cancelled_capture_is_joined_before_scratch_and_admission_are_released() {
    cancellation_during(Phase::Capture).await;
}
