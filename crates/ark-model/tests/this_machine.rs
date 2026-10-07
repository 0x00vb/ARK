//! What this machine offers: Claude Code, Codex and Ollama, each found or not, signed in or
//! not. Spends nothing: each CLI is only asked whether it is signed in, and Ollama for its
//! list of models. Used by `scripts/windows-smoke.ps1` to check finding the CLIs on Windows.
//!
//! ```text
//! cargo test -p ark-model --test this_machine -- --ignored --nocapture
//! ```

#[tokio::test]
#[ignore]
async fn lists_what_this_machine_offers() {
    for b in ark_model::catalog().await {
        let found = match &b.fix {
            Some(fix) if fix.contains("install") => "not found",
            _ => "found",
        };
        println!(
            "backend {}: {found}, ready {}, {}",
            b.provider, b.ready, b.detail
        );
    }
    for cli in [ark_model::Cli::Claude, ark_model::Cli::Codex] {
        println!(
            "path {}: {}",
            cli.name(),
            cli.locate()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".into())
        );
    }
}
