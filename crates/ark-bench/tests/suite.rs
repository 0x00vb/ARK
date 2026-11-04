//! The simulated suite runs every workload and is deterministic: two runs of the same code
//! produce the same token and call counts, so a comparison shows only real changes.

use ark_bench::cassette::Cassette;
use ark_bench::faults::FaultPlan;
use ark_bench::{run_suite, suite, BenchSettings, HarnessKind, ModelSpec, Report};

#[tokio::test]
async fn managed_recordings_are_rejected_before_any_model_call() {
    let result = run_suite(
        &[],
        &ModelSpec {
            strong: "codex:unused".into(),
            fast: None,
        },
        &BenchSettings {
            cassette: Some(Cassette::record()),
            ..Default::default()
        },
        1,
        "unsupported",
        |_| {},
    )
    .await;
    if std::env::var("ARK_SESSION_REUSE")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
    {
        assert!(result.unwrap_err().to_string().contains("bypass recording"));
    }
}

async fn run() -> Report {
    run_suite(
        &suite::all(40),
        &ModelSpec::simulated(),
        &BenchSettings::default(),
        1,
        "test",
        |_| {},
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn simulated_runs_are_deterministic() {
    let a = run().await;
    let b = run().await;
    assert_eq!(a.workloads.len(), suite::all(40).len());
    for (x, y) in a.workloads.iter().zip(&b.workloads) {
        let (rx, ry) = (&x.runs[0], &y.runs[0]);
        assert_eq!(rx.success, ry.success, "{}", x.name);
        assert_eq!(rx.model_calls, ry.model_calls, "{}", x.name);
        assert_eq!(rx.total_tokens, ry.total_tokens, "{}", x.name);
        assert_eq!(rx.cached_input_tokens, ry.cached_input_tokens, "{}", x.name);
        assert_eq!(rx.tool_calls, ry.tool_calls, "{}", x.name);
    }
}

async fn run_with(settings: BenchSettings, only: &[&str]) -> Report {
    let mut workloads = suite::all(20);
    workloads.retain(|w| only.contains(&w.name()));
    run_suite(
        &workloads,
        &ModelSpec::simulated(),
        &settings,
        1,
        "test",
        |_| {},
    )
    .await
    .unwrap()
}

fn passed(r: &Report, name: &str) -> bool {
    r.workloads.iter().find(|w| w.name == name).unwrap().runs[0].success
}

/// The naive loop runs the same workloads and checks. It does what a plain loop can, and
/// fails where only a harness guard or ARK's evidence index helps.
#[tokio::test]
async fn the_naive_baseline_is_judged_by_the_same_checks() {
    let naive = BenchSettings {
        harness: HarnessKind::Naive,
        ..Default::default()
    };
    let r = run_with(
        naive,
        &[
            "multi-step",
            "long-session",
            "resume",
            "duplicate",
            "runaway",
            "retrieval",
            "team",
        ],
    )
    .await;
    for ok in ["multi-step", "long-session", "resume", "team"] {
        assert!(passed(&r, ok), "naive should pass {ok}");
    }
    for fails in ["duplicate", "runaway", "retrieval"] {
        assert!(!passed(&r, fails), "naive should fail {fails}");
    }
    assert_eq!(r.settings["harness"], "naive");
}

/// The team workload runs through the orchestrator: the lead delegates, each worker calls
/// only its own tools.
#[tokio::test]
async fn the_team_workload_delegates_under_ark() {
    let r = run_with(BenchSettings::default(), &["team"]).await;
    let run = &r.workloads[0].runs[0];
    assert!(run.success, "{}", run.verdict);
    assert!(
        run.tool_calls >= 5,
        "lead and workers called {} tools",
        run.tool_calls
    );
}

/// A recorded run replays offline with the same results and no divergence, for both
/// harnesses; the fault schedule is the same for the same seed.
#[tokio::test]
async fn record_replay_and_faults_are_reproducible() {
    let dir = tempfile::tempdir().unwrap();
    let only = ["multi-step", "large-output", "team", "stale-read"];
    for harness in [HarnessKind::Ark, HarnessKind::Naive] {
        let rec = Cassette::record();
        let a = run_with(
            BenchSettings {
                harness,
                cassette: Some(rec.clone()),
                ..Default::default()
            },
            &only,
        )
        .await;
        let path = dir.path().join(format!("{}.jsonl", harness.as_str()));
        rec.save(&path).unwrap();
        let b = run_with(
            BenchSettings {
                harness,
                cassette: Some(Cassette::load(&path).unwrap()),
                ..Default::default()
            },
            &only,
        )
        .await;
        for (x, y) in a.workloads.iter().zip(&b.workloads) {
            let (rx, ry) = (&x.runs[0], &y.runs[0]);
            assert_eq!(ry.replay_diverged, 0, "{} diverged", x.name);
            assert_eq!(rx.success, ry.success, "{}", x.name);
            assert_eq!(rx.total_tokens, ry.total_tokens, "{}", x.name);
            // Reports clip answers at 400 characters, which can cut an id in half.
            let head = |a: &str| strip_ids(&a.chars().take(350).collect::<String>());
            assert_eq!(head(&rx.answer), head(&ry.answer), "{}", x.name);
        }
    }

    let faults = FaultPlan::parse("overloaded=0.2,bad-tool=0.1", 7).unwrap();
    let with = |plan: FaultPlan| BenchSettings {
        faults: Some(plan),
        ..Default::default()
    };
    let a = run_with(with(faults.clone()), &only).await;
    let b = run_with(with(faults), &only).await;
    let injected = |r: &Report| -> Vec<u32> {
        r.workloads
            .iter()
            .map(|w| w.runs[0].faults_injected)
            .collect()
    };
    assert_eq!(injected(&a), injected(&b));
    assert!(injected(&a).iter().sum::<u32>() > 0);
}

/// Removes reference ids, including clipped ids, and long numeric hex runs from answers.
fn strip_ids(s: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_hexdigit() {
            run.push(c);
            continue;
        }
        let reference = ["inv:", "t-"].iter().any(|prefix| {
            out.strip_suffix(*prefix).is_some_and(|before| {
                !before
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
            })
        });
        if !reference && (run.len() < 8 || !run.chars().any(|x| x.is_ascii_digit())) {
            out.push_str(&run);
        }
        run.clear();
        out.push(c);
    }
    out
}

#[test]
fn clipped_reference_ids_do_not_change_answer_comparisons() {
    for (answer, expected) in [
        ("[inv:f57d9", "[inv:"),
        ("[inv:abcdefab]", "[inv:]"),
        ("inv:face", "inv:"),
        ("task t-abcd", "task t-"),
        ("decade faced", "decade faced"),
        ("format-decade print-faced", "format-decade print-faced"),
        ("myinv:face x.t-face", "myinv:face x.t-face"),
        ("value 1234abcd", "value"),
    ] {
        assert_eq!(strip_ids(answer).trim_end(), expected, "{answer}");
    }
}
