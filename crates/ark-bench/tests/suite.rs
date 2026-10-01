//! The simulated suite runs every workload and is deterministic: two runs of the same code
//! produce the same token and call counts, so a comparison shows only real changes.

use ark_bench::{run_suite, suite, BenchSettings, ModelSpec, Report};

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
