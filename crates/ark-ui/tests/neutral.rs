//! ARK names no domain. SecRes is the largest consumer, so security words are the likeliest
//! to leak in. The front-end layers (ark-interact, ark-cli, ark-ui: Rust sources and the page)
//! must not contain one anywhere. The runtime crates must not contain one in code; their doc
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
        "ark-bench/src",
    ] {
        scan(&crates.join(dir), true, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "domain words in ARK's runtime code:\n{}",
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
