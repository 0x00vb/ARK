//! ARK names no domain. ARK was extracted from a security app (SecRes, retired), so security
//! words are the likeliest to leak in. The front-end layers (ark-interact, ark-cli, ark-ui:
//! Rust sources and the page) must not contain one anywhere. The runtime crates must not contain one in code; their doc
//! comments and unit tests may, because they illustrate extension with a security consumer on
//! purpose. Writing words are allowed: the docs and tests use the example writing agent.

use std::path::Path;

const WORDS: &[&str] = &[
    "secres",
    "security",
    "exploit",
    "vulnerab",
    "decompil",
    "disassembl",
    "mach-o",
    "macho",
    "lldb",
    "frida",
    "ghidra",
    "radare",
    "jailbreak",
    "ipad",
    "iphone",
    "entitlement",
];

/// `code_only`: skip comment lines and stop at a file's `#[cfg(test)]` module.
fn scan(dir: &Path, code_only: bool, hits: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            // Third-party libraries shipped as they are (xterm.js), not ARK's words.
            if p.file_name().is_some_and(|n| n == "vendor") {
                continue;
            }
            scan(&p, code_only, hits);
            continue;
        }
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "rs" | "js" | "css" | "html") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap().to_lowercase();
        for (n, line) in text.lines().enumerate() {
            if code_only {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
            }
            // `IpAddr` (std::net) is not the tablet, nor is the HTTP header a domain.
            let line = line
                .replace("ipaddr", "")
                .replace("content-security-policy", "");
            for w in WORDS {
                if line.contains(w) {
                    hits.push(format!("{}:{}: `{w}`", p.display(), n + 1));
                }
            }
        }
    }
}

#[test]
fn front_end_layers_name_no_domain() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for dir in [
        "ark-interact/src",
        "ark-cli/src",
        "ark-ui/src",
        "ark-ui/web",
    ] {
        scan(&crates.join(dir), false, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "domain words in ARK's front-end layers:\n{}",
        hits.join("\n")
    );
}

#[test]
fn runtime_code_names_no_domain() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for dir in [
        "ark-core/src",
        "ark-model/src",
        "ark-store/src",
        "ark-tools/src",
        "ark-runtime/src",
        "ark-sdk/src",
        "ark-orchestrate/src",
        "ark-bench/src",
        "ark-reach/src",
    ] {
        scan(&crates.join(dir), true, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "domain words in ARK's runtime code:\n{}",
        hits.join("\n")
    );
}

/// Orchestration works the same on any provider: its code names no vendor. Vendors live in
/// `ark-model` only (`cli`, `anthropic`, `openai`, `ollama`).
#[test]
fn orchestration_names_no_vendor() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for path in files_under(&crates.join("ark-orchestrate/src")) {
        let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            for w in [
                "claude",
                "codex",
                "anthropic",
                "openai",
                "ollama",
                "chatgpt",
            ] {
                if line.contains(w) {
                    hits.push(format!("{}:{}: `{w}`", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "vendor names in orchestration code:\n{}",
        hits.join("\n")
    );
}

/// Reach sets up any app's program on a computer (ADR 0052): the app names the program
/// through its `ServeContract`, so `ark-reach` names no vendor or product, comments and tests
/// included.
#[test]
fn reach_names_no_vendor_or_product() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for path in files_under(&crates.join("ark-reach"))
        .into_iter()
        .filter(|p| !p.components().any(|c| c.as_os_str() == "target"))
    {
        let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
        for (n, line) in text.lines().enumerate() {
            for w in [
                "claude",
                "codex",
                "anthropic",
                "openai",
                "ollama",
                "chatgpt",
                "opencode",
                "ark-desktop",
                "ark desktop",
                "ark cloud",
            ] {
                if line.contains(w) {
                    hits.push(format!("{}:{}: `{w}`", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "vendor or product names in ark-reach:\n{}",
        hits.join("\n")
    );
}

/// The work model (ark-core's types, ark-orchestrate's agents and tasks) names no coding tool,
/// front-end panel or worker protocol. Terminals, diffs, worktrees and pull requests are an
/// app's surfaces or a worker's tools; an agent harness is a worker the work model does not
/// name (ADR 0036, 0039). Comments and tests may name them.
#[test]
fn the_work_model_names_no_coding_or_worker_concept() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let words = [
        "terminal",
        "worktree",
        "pull_request",
        "pullrequest",
        "diff_viewer",
        "diffviewer",
        "lsp",
        "debugger",
        "acp",
    ];
    let mut hits = Vec::new();
    for dir in ["ark-core/src", "ark-orchestrate/src"] {
        for path in files_under(&crates.join(dir)) {
            let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for w in words {
                    if line.contains(w) {
                        hits.push(format!("{}:{}: `{w}`", path.display(), n + 1));
                    }
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "coding or worker concepts in the work model:\n{}",
        hits.join("\n")
    );
}

/// The core model has no field for a concept that only some domains have: an application
/// that has no source code, processes or binaries should not see them in ARK's types.
#[test]
fn core_types_carry_no_code_concepts() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("ark-core/src");
    let fields = [
        "pub address:",
        "pub symbol:",
        "pub function:",
        "pub process:",
        "pub binary:",
    ];
    let mut hits = Vec::new();
    for e in std::fs::read_dir(&core).unwrap().flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap();
        for (n, line) in text.lines().enumerate() {
            for f in fields {
                if line.trim_start().starts_with(f) {
                    hits.push(format!("{}:{}: {}", e.path().display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "code-specific fields in ark-core:\n{}",
        hits.join("\n")
    );
}

/// The core's code names no file format: formats a tool stores (diagrams, documents) live
/// with the tool, in `ark-tools` or the app. Comments may name one as an example.
#[test]
fn core_names_no_file_format() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("ark-core/src");
    let mut hits = Vec::new();
    for e in std::fs::read_dir(core).unwrap().flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap().to_lowercase();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            for w in ["mermaid", "markdown", "html", "pdf"] {
                if line.contains(w) {
                    hits.push(format!("{}:{}: `{w}`", e.path().display(), n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "file formats in ark-core:\n{}",
        hits.join("\n")
    );
}

/// Every `.rs` file under `dir`, in its subfolders too.
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}
