# 0020. Running code, contained in the project folder

Status: Accepted
Date: 2026-10-04

## Context
Agents could reason over text but could not compute: no tool ran a program. The owner wants
real analysis ("analyze this spreadsheet, make a chart, write a report"), with Python first
but not as the abstraction, and with containment that a person can allow for a whole project
(2026-10-04). Network access must stay an explicit permission, never a side effect of Python
being able to open a socket. No "always allow everything" path.

What existed: `ark_tools::subprocess` (process-group kill, no output bound; `run_timeout`
could hang on a full pipe), the person's terminal (not the agent's), the approval gate with
grants per project (C3), and `ProducedArtifact` for files a tool makes. What was measured on
macOS (2026-10-04): a `sandbox-exec` profile runs uv's Python 3.12 with pandas, numpy,
matplotlib and openpyxl. A run starts in 1.3 s once warm. Reads of `~`, other projects and
`~/.ssh`, writes outside the project folder, and every network connection fail with
"Operation not permitted".

## Decision
A crate, `ark-exec`, with one action, `code.run {code, runtime?, timeout?}`, and one trait,
`Runtime`, for what a program runs in. Python through uv is the first runtime.

| Question | Decision |
|---|---|
| Where a run happens | The project folder is the working folder. Each run gets a scratch folder of its own (its program file, `HOME`, `TMPDIR`), deleted after |
| What contains it | A `Sandbox`. It reads the system folders, its runtime's environment and the project; it writes only the project and its scratch folder; it has no network. Inside the project, the folders tools run or read as instructions later (`.git`, `.agents`, `.ark`, `.claude`, `.codex`, `.vscode`, `.idea`, `.envrc`, `.github`) are read-only. Paths go to the profile as parameters, never into its text |
| Where it is offered | Only where ARK has a `Sandbox` (macOS today, `sandbox-exec`). Elsewhere `Exec::new` returns `None` and the capability is not listed. There is no unsandboxed fallback |
| What asks | `Mutate`. The person sees the program in the approval. Because the run is contained, "Always in this project" is allowed: it covers only runs that are contained like this one |
| Network | None in a run. If a workflow needs it, it becomes its own explicit permission (its own approval class), never a flag on the same grant |
| The environment | ARK keeps it, not the person: `uv` makes a venv with Python 3.12 and exact package versions under the store (`exec/python`). The first run installs it (ARK's own step, outside the sandbox, with network). A version change makes it again. Without uv, Python is unavailable, with the reason |
| Outputs | Every file the run wrote in the project comes back as a `ProducedArtifact` with its path (up to 20 files, 25 MB each; up to 50 listed). `code.run` names no kind: the engine's sniffers decide it (ADR 0019) |
| Output bounds | `subprocess::run_spec` reads both streams while the program runs and keeps the last 16 KB of each. The agent's card shows the exit, the files and the last 4,000 characters of stdout (2,000 of stderr when it failed) |
| What the agent is told | The action's description names the runtimes and what the window shows when a run writes it, generated from the formats ARK reads (ADR 0019). Nothing goes in the base prompt |

## Consequences
- A new runtime (Node, R, a shell) is one `Runtime`: an environment and a launch line. The
  sandbox, outputs, approvals and views do not change.
- Packages are fixed. A program that needs another package fails with an import error. Adding
  packages per project is later, and needs the network as its own permission.
- A first run waits for the install: seconds when uv's cache has the files, a minute or more
  on a clean machine.
- ARK Desktop must ship uv or find it. Without uv, "Run code" is not available on a clean Mac.
- Linux and Windows need their own `Sandbox` (bubblewrap or Landlock, AppContainer) before
  they get the capability.
- `sandbox-exec` is deprecated by Apple but still works in macOS 26, and other agent CLIs use
  it. If it goes away, the `Sandbox` changes and the rest stays.

## Check
`crates/ark-exec/tests/run.rs` against the real sandbox: files written come back, writes
outside the project and into `.git` fail, reads outside the project and the network fail, a
run past its timeout stops. `python_analysis_writes_a_chart_and_a_table` runs the real
environment with `ARK_TEST_PYTHON=1`. `crates/ark-tools/src/subprocess.rs`:
`a_process_that_prints_more_than_a_pipe_holds_finishes`.
