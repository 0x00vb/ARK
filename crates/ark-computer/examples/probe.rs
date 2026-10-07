//! Try the real screen by hand, without a model or approvals.
//!
//! ```text
//! cargo run -p ark-computer --example probe -- access
//! cargo run -p ark-computer --example probe -- apps
//! cargo run -p ark-computer --example probe -- look <app>
//! cargo run -p ark-computer --example probe -- do computer.menu '{"app":"Finder"}'
//! ```
//!
//! `do` runs an action as an agent would (the screen held, the person's pause awaited), with a
//! look first so element numbers are known; the approval is the person running this.

struct NoBlobs;
impl ark_tools::BlobResolver for NoBlobs {
    fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
        None
    }
    fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
        Err(ark_tools::AdapterError::ArtifactNotFound(id.to_string()))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(svc) = ark_computer::ComputerService::native() else {
        eprintln!("no Computer on this platform");
        std::process::exit(1);
    };
    let screen = svc.screen();
    match args.first().map(String::as_str) {
        Some("access") => println!("{:?}", screen.access()),
        Some("apps") => {
            for a in screen.apps().unwrap() {
                println!("{}{} ({})", a.name, if a.front { " *" } else { "" }, a.pid);
            }
        }
        Some("look") => {
            let app = args.get(1).expect("look <app>");
            let t = std::time::Instant::now();
            let look = screen.look(app).unwrap();
            println!("{} · \"{}\" ({:?})", look.app, look.window, t.elapsed());
            for e in &look.elements {
                println!("[{}] {} \"{}\" {:?}", e.number, e.role, e.name, e.value);
            }
            println!("--- text ({} chars)\n{}", look.text.len(), look.text);
        }
        Some("shot") => {
            let shot = screen.capture(&args[1], 1280).unwrap();
            let out = args.get(2).cloned().unwrap_or_else(|| "shot.png".into());
            std::fs::write(&out, &shot.png).unwrap();
            println!(
                "{}x{} of {:?} -> {out}",
                shot.width, shot.height, shot.frame
            );
        }
        Some("do") => {
            use ark_tools::Adapter;
            let input: serde_json::Value = serde_json::from_str(&args[2]).expect("json input");
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let c = svc.for_owner("probe");
                let ctx = ark_tools::RunCtx::new(&NoBlobs);
                if let Some(app) = input["app"].as_str() {
                    if args[1] != ark_computer::LOOK && input.get("ref").is_some() {
                        c.run(ark_computer::LOOK, serde_json::json!({ "app": app }), &ctx)
                            .await
                            .unwrap();
                    }
                }
                match c.run(&args[1], input, &ctx).await {
                    Ok(out) => match ark_computer::distill(&args[1], &out.output) {
                        Some((card, _)) => println!("{card}"),
                        None => println!("{:#}", out.output),
                    },
                    Err(e) => println!("error: {e}"),
                }
            });
        }
        #[cfg(target_os = "macos")]
        Some("tree") => {
            let depth = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(30);
            print!("{}", ark_computer::tree(&args[1], depth).unwrap());
        }
        _ => eprintln!("access | apps | look <app>"),
    }
}
