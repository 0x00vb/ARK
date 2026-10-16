//! Explicit real-engine dogfood. No model downloads or automatic discovery.
use ark_model::{Inference, InferenceConfig, Message, Provider};
use std::{path::PathBuf, sync::Mutex};
#[tokio::test]
#[ignore = "requires explicitly configured local engine, CLI and GGUF paths"]
async fn real_engine_streams_reuses_and_enforces_limits() {
    let exe =
        PathBuf::from(std::env::var_os("ARK_INFERENCE_TEST_EXECUTABLE").expect("engine path"));
    let cli = PathBuf::from(std::env::var_os("ARK_INFERENCE_TEST_CLI").expect("CLI path"));
    let model = PathBuf::from(std::env::var_os("ARK_INFERENCE_TEST_MODEL").expect("GGUF path"));
    let mut config = InferenceConfig::new(&exe, &model);
    config.context = 128;
    config.output_tokens = 4;
    config.backend = std::env::var("ARK_INFERENCE_TEST_BACKEND").unwrap_or_else(|_| "cpu".into());
    let reference = std::process::Command::new(cli)
        .arg("generate")
        .arg("--model")
        .arg(&model)
        .args([
            "--prompt",
            "What is 2 + 2? Answer briefly.",
            "--context",
            "128",
            "--tokens",
            "4",
            "--threads",
            "2",
            "--backend",
            &config.backend,
        ])
        .output()
        .unwrap();
    assert!(reference.status.success(), "reference generation failed");
    let expected =
        String::from_utf8(reference.stdout).expect("explicit fixture must produce UTF-8");
    let provider = Inference::new(config.clone()).unwrap();
    assert_eq!(provider.context_window(), Some(124));
    let messages = [Message::user("What is 2 + 2? Answer briefly.")];
    for _ in 0..2 {
        let streamed = Mutex::new(String::new());
        let completion = provider
            .complete_streaming("", &messages, &[], &|text| {
                streamed.lock().unwrap().push_str(text)
            })
            .await
            .unwrap();
        assert_eq!(completion.content, expected);
        assert_eq!(*streamed.lock().unwrap(), expected);
        assert!(completion.usage.measured);
        assert!(completion.tool_calls.is_empty());
    }
    // Budget refusal cannot return a truncated successful completion.
    config.array_bytes = 1;
    let error = Inference::new(config)
        .unwrap()
        .complete("", &messages, &[])
        .await
        .unwrap_err();
    assert!(!error.is_transient());
    drop(provider);
}
