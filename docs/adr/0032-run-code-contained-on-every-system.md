# 0032. Run code, contained on every system

Status: Accepted
Date: 2026-10-05

## Context
ADR 0020 contained runs with a macOS sandbox profile and offered Run code nowhere else. The
owner needs it on Windows and macOS for analysts and operators, and on Linux for ARK Cloud's
runtime (2026-10-05). The rules stay ADR 0020's: a run reads the system, its environment and the
project; it writes the project (not its kept folders: `.git`, `.agents`, ...) and its scratch
folder; it has no network; there is no unsandboxed fallback.

Each system has a different mechanism, and two of them cannot use the existing path as is:

| System | Mechanism | Fits `program args` started by `subprocess::run_spec`? |
|---|---|---|
| macOS | `sandbox-exec` with a profile | yes |
| Linux | bubblewrap (`bwrap`): namespaces, a root where only allowed folders are mounted, no network namespace | yes |
| Windows | an AppContainer: access only where the container's SID is granted, no network without the `internetClient` capability | no: it takes `CreateProcessW` with a security-capabilities attribute, which `std::process::Command` cannot pass (stable Rust) |

Landlock (Linux) applies to the calling process, so it also needs code in the child before
`exec`. bubblewrap is a program, is packaged by every distribution, and its rules are mounts,
which hide what is not allowed (a folder that is not mounted does not exist for the run).

## Decision
One `Sandbox` type with the same two calls on every system (`Sandbox::native`, `wrap`), a module
per system behind it, and nothing outside `ark_exec::sandbox` that knows which one runs.

| Question | Decision |
|---|---|
| Linux | bubblewrap: `--unshare-all` (network too), `--die-with-parent`, `--new-session`; system folders, the environment and kept folders read-only, the project and scratch folder writable. `native()` probes `bwrap` once; without it, or without user namespaces, Run code is not offered |
| Windows | An AppContainer named `ark.run`. `wrap` gives its SID access to the project and scratch folder (read, write, delete; not `FILE_DELETE_CHILD`, so a kept folder's own deny of `DELETE` holds), read access to the environment, and denies writes on the kept folders that exist. The entries are inherited and stay; a later run that finds them writes nothing |
| Starting a Windows run | Through the program itself: `wrap` returns the current program with `--ark-contained-run <folder> -- <program> <args>`. `ark_exec::entry()`, called first in `main`, sees the flag, starts the run with `CreateProcessW` in the container with its own standard streams (and no other handle), puts it in a job object that ends it when the launcher ends, waits and exits with its exit code. So timeouts, cancellation and output capture stay `subprocess::run_spec`'s, the same on every system |
| A program that forgets `entry()` | On Windows `Sandbox::native()` is `None` unless `entry()` ran, so Run code is not offered. It never starts a second copy of an app by mistake |
| Kept folders that do not exist | macOS denies a path whether or not it exists. bubblewrap and AppContainer protect only what exists, so a run could create `.vscode/tasks.json`. `code.run` notes which kept folders are absent before a run and removes any the run created, and says so in the run's stderr |
| The environment | `base_env` gives each system what its programs expect (Windows: `SystemRoot`, `USERPROFILE`, `TEMP`, `APPDATA` and others, all inside the run's scratch folder; Unix: `HOME`, `TMPDIR`) and `PATH` with the runtime's folder first. Python finds its interpreter from `pyvenv.cfg` on every system |

## Consequences
- Run code is offered on macOS, on Linux with bubblewrap, and on Windows in programs that call
  `ark_exec::entry()`. Each app on Windows adds that line (ARK Desktop, `writing-agent` and
  `ark-bench` have it).
- `Sandbox::wrap` returns a `Result` (granting access on Windows can fail, for example on a
  drive without access lists) and may take a while the first time on a large folder on
  Windows, so `code.run` calls it on a blocking thread.
- Windows leaves access entries for `ark.run` on the project folders it ran in and on ARK's
  Python environment. They grant access to that container only.
- A Linux container that runs ARK (ARK Cloud) needs `bwrap` installed and user namespaces, or
  runs as a VM (Fly machines are).
- Verified on macOS (`cargo test -p ark-exec`, with Python). Linux and Windows compile and lint
  here; their tests need a machine of each (`tests/run.rs` on Linux,
  `tests/run_windows.rs` on Windows).
- Still not limited: memory and CPU per run. A job object on Windows and cgroups on Linux could
  do it later; macOS has no equivalent in `sandbox-exec`.

## Check
`crates/ark-exec/tests/run.rs` (macOS and Linux): writes outside the project, into `.git` and a
new `.vscode` fail or are undone, reads outside the project and the network fail, a timeout
stops a run, and with `ARK_TEST_PYTHON=1` Python reads a workbook in SQL.
`crates/ark-exec/tests/run_windows.rs` (Windows, its own `main` that calls `entry()`): the same
checks with `cmd.exe`, and Python with `ARK_TEST_PYTHON=1`. `sandbox::linux` and
`sandbox::windows` unit tests: the bubblewrap arguments, and Windows command-line quoting.
