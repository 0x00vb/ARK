# 0022. One place for what differs between operating systems

Status: Accepted
Date: 2026-10-05

## Context
ARK Desktop is going to Windows (ark-desktop `docs/WINDOWS.md`), and Linux is likely after it.
ARK grew on macOS, and what differs between systems was handled where it came up:

| What | Where it was | On Windows |
|---|---|---|
| Stopping a program with its children | `killpg` in `ark-tools` `subprocess.rs`; only `kill_on_drop` in `ark-model` and `ark-browser`; plain `kill` in `mcp.rs` | nothing stops the children: a cancelled call leaves Node, an MCP server or browser helpers running |
| Finding `claude`, `codex`, `npx`, tools | three lookups (`cli.rs`, `subprocess.rs`, `mcp.rs`) with Homebrew paths and `HOME` | no `PATHEXT`: npm's `claude.cmd` is missed, its extension-less shell script is found and fails to start |
| Running an npm launcher | `Command::new` on the `.cmd` | runs through `cmd.exe`: an 8191-character limit and `cmd`'s quoting for a multi-KB system prompt |
| Opening a link or a file | `cmd /C start` in two places | an OAuth link is cut at its first `&`; the rest runs as a command |
| Whether a process is alive | two copies, Unix only | always "no": the CLI's running turns are marked interrupted |
| Local time | `localtime_r`, Unix only | schedules run at UTC |
| Folders | `HOME` in five places | unset: data lands in the working folder |
| Atomic writes | `rename` in six places | fails while Excel or a virus scanner holds the file |
| Console windows | not handled | each started program flashes a console window over a windowed app |

Each fix in place would add one more `cfg` to a crate that is about something else, and Linux
would need the same pass again.

## Decision
A crate at the bottom, `ark-os`, holds every OS difference behind one API that does the same
thing everywhere: `dirs` (home, app data, Documents, paths without `\\?\`), `find` (programs
with `PATHEXT` and the places installers use; installed apps), `process` (`command` starts any
program in its own group with no console window and runs an npm launcher's target directly;
`Tree` stops a program with everything it started, by process group on Unix and Job Object on
Windows; `alive`), `open` (the system's opener, never a shell), `fs` (`replace` with retry,
`write_atomic`, `name_problem` and `clean_name` checked the same on every system,
`owner_only`), `time` (`local_offset`) and `shell` (the terminal's shell).

Rules:
- Outside `ark-os`, no crate tests for an OS to choose behavior. `cfg` stays only where code
  exists for one system (the Computer's macOS screen, the macOS sandbox for `code.run`) and
  in tests that use a system's own tools.
- A missing capability is not offered rather than offered broken: `ComputerService::native()`
  and `Sandbox::native()` return `None`, so their tools and pages do not appear.
- Credentials stay in `ark-connect`'s `Secrets` (the OS keychain through `keyring`, already
  Credential Manager on Windows). Apps keep keys there, not in their own store.
- Apps on ARK use the same crate (`ark_sdk::os`), so ARK Desktop's installers, data folder and
  updater follow the same rule.

## Consequences
- One place to read and test for each difference; Linux is the Unix code plus `xdg-open`.
- ARK starts every program the same way, so a fix (no console window, stopping a tree) reaches
  every caller at once.
- Behavior changes on macOS too, deliberately: a vendor CLI and an MCP server now start in
  their own process group, so a cancelled call stops what they started and a terminal's ctrl+c
  goes to ARK, which cancels the turn, not to the child directly. Programs are found only when
  executable, as a shell would.
- File names are refused by Windows' rules on every system, so a project folder moves between
  machines.
- Windows-only code paths are compiled for Windows in CI, but run only on a Windows machine:
  `cargo test -p ark-os` there covers the Job Object, `PATHEXT`, the npm launcher, the
  retried replace and the local offset.

## Check
- `crates/ark-os` tests: grandchildren stop with their tree (a shell's `sleep`, PowerShell's
  `ping`), npm launchers resolve, names are checked alike everywhere, a held file is still
  replaced (Windows), the local offset.
- `grep -rn 'cfg(windows)\|cfg(unix)\|cfg!(windows)\|target_os' crates --include='*.rs'`
  outside `ark-os` lists only one-system code (`ark-computer`, `ark-exec`) and tests.
- `.github/workflows/windows.yml` and `scripts/windows-check.ps1` run the whole suite on
  Windows.
