//! Real local Provider consumer. Prompt bytes come from stdin and remain off the command line.
use ark_model::{Inference, InferenceConfig, Message, Provider};
use std::io::{Read, Write};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        return Err("Use: local_inference ABSOLUTE_EXECUTABLE ABSOLUTE_GGUF [cpu|metal]".into());
    }
    let mut config = InferenceConfig::new(&args[1], &args[2]);
    if let Some(backend) = args.get(3) {
        config.backend.clone_from(backend);
    }
    config.output_tokens = 12;
    let provider = Inference::new(config)?;
    let mut bytes = Vec::new();
    std::io::stdin().take(1048577).read_to_end(&mut bytes)?;
    if bytes.len() > 1048576 {
        return Err("Input byte limit exceeded.".into());
    }
    let prompt = String::from_utf8(bytes)?;
    let messages = [Message::user(prompt)];
    let output_failed = std::sync::atomic::AtomicBool::new(false);
    let sink = |text: &str| {
        let mut output = std::io::stdout().lock();
        if output
            .write_all(text.as_bytes())
            .and_then(|_| output.flush())
            .is_err()
        {
            output_failed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    };
    let result = tokio::select! {
        result = provider.complete_streaming("", &messages, &[], &sink) => result?,
        _ = tokio::signal::ctrl_c() => return Err("Local generation cancelled.".into()),
    };
    if output_failed.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Output destination failed.".into());
    }
    eprintln!(
        "prompt_tokens={} output_tokens={}",
        result.usage.prompt_tokens, result.usage.completion_tokens
    );
    Ok(())
}
