//! Run a supplied Python profile probe through the real native sandbox, without model calls.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    assert!(
        args.len() >= 5,
        "environment python script project scratch [script arguments]"
    );
    let sandbox = ark_exec::Sandbox::native().expect("native sandbox");
    let bounds = ark_exec::Bounds {
        project: args[3].clone(),
        scratch: args[4].clone(),
        reads: vec![args[0].clone()],
    };
    let (exe, argv) = sandbox
        .wrap(
            &bounds,
            &args[1],
            &std::iter::once(&args[2])
                .chain(args.iter().skip(5))
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let status = Command::new(exe)
        .args(argv)
        .current_dir(&args[3])
        .env_clear()
        .env("HOME", &args[4])
        .env("TMPDIR", &args[4])
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("ARK_PROBE", "1")
        .status()
        .unwrap();
    std::process::exit(status.code().unwrap_or(1));
}
