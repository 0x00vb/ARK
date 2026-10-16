# 0072. Code runs have bounded admission and owned cleanup

Status: Accepted
Date: 2026-10-16

## Context

On `main` before this change (1bdcaa1), `code.run` had these faults:

| Fault | Where |
|---|---|
| A cancelled run left its scratch folder: the future was dropped, `remove_dir_all` never ran | `ark-exec` `Exec::run` |
| The program could overwrite its own Restore copies: `scratch/<id>/before` was inside its writable scratch | `ark-exec` |
| Background children outlived a successful run: the process group was disarmed on exit | `ark-tools` `subprocess::run_spec` |
| Capture checked a file's size, then read it with an unbounded `std::fs::read` by name: a growing file or a swapped symlink got past the check | `ark-exec`, `ark-runtime` `folder` |
| No limit on concurrent runs across chats; two chats in one project diffed the same folder at once | `ark-exec` |

The four-tool batch limit is per turn. Code mutations already run serially in a turn, but
several chats can run code at once.

## Decision

| Part | Behavior |
|---|---|
| Admission | One gate per `Exec`, shared by every view of that adapter: two active runs and at most eight waiting calls. A full queue returns a busy error. Separate adapters and processes have separate gates. |
| Projects | One run per canonical project path. A call first waits for its project, then for a slot, so a blocked project holds no slot. Slots are handed out in arrival order among calls whose project is free; there is no strict FIFO across busy projects. Built on a tokio `Semaphore`, one tokio `Mutex` per project and an atomic waiter count. |
| Waiting | Input is validated before queueing. No environment setup, copies or child process exists while a call waits. A dropped call leaves the queue. Nothing polls. |
| Ownership | An admitted run is owned by its own task. A dropped caller cancels it, and the task still joins setup, the program and capture before it removes scratch and frees the slot. Cancellation during shared environment setup waits for setup, then skips the launch. |
| Child lifetime | `run_contained` reaps the direct child and stops its process group or Job Object even on success. Other `run_spec` callers keep background processes as before. |
| Incomplete cleanup | Only a process group or Job Object that cannot be stopped closes admission until restart. A launch failure does not. A scratch folder the program made unremovable (for example `chmod 000`) is logged and left. |
| Files | Capture opens regular files through checked handles (descriptor-relative on Unix, a resolved-handle check on Windows), enforces byte limits on the open handle, compares with fixed buffers and keeps native clones. `Before` resolves its root first, so a project opened through a link keeps its copies. Restore copies live in `scratch/<id>/before`, outside the program's writable `scratch/<id>/work`. |
| CPU | No rate cap, priority change or numerical-library thread limit. |

## Consequences

Excess work waits, so latency under load grows. There is no limit on a program's RAM, the
host's total RAM or concurrency across processes. A cancelled run can leave project changes
without Restore versions, as before. Unix process groups are not a security boundary against
code that leaves its group. Abrupt process death can still leave scratch. Windows and Linux
behavior is compiled and linted here, not yet run on a machine of each.

Two active and eight waiting are the owner's call to change; one run per project applies
across chats.

## Check

| Check | Result (2026-10-16, macOS arm64) |
|---|---|
| New `tests/run.rs` tests on base | 4 of 5 fail: queued setup, the 2/8 limit, cancellation cleanup, Restore copies outside scratch. `a_completed_run_leaves_no_async_owner_or_output_readers` passes on base; it guards the owner task |
| Focused tests | `ark-os`, `ark-tools`, `ark-runtime`, `ark-exec`, `ark-orchestrate` pass; `ark-exec` and `ark-tools` 8 times in a row |
| Linked project root | `folder::tests::a_project_opened_through_a_link_keeps_its_versions_before` fails without the canonical root |
| Lint | clippy `-D warnings` on the four crates (host and `x86_64-pc-windows-gnu`), `ark-os` on `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`. The full Linux and MSVC checks stop in C build scripts (`ring`, `zstd-sys`) without a cross toolchain |

Timing: the `run_benchmark` example (`crates/ark-exec/examples/run_benchmark.rs`), same driver
on base and candidate, dev profile, one project, six alternating runs per side, other builds
running on the machine (load about 6):

| Workload | Base median | Candidate median | Base p95 | Candidate p95 |
|---|---:|---:|---:|---:|
| `shell`: `printf`, 180 runs | 10.4 ms | 9.7 ms | 12.9 ms | 11.5 ms |
| `many`: 200 files of 12 KiB in the project, 90 runs | 39.4 ms | 43.3 ms | 42.4 ms | 46.7 ms |

A small run shows no measurable change. A project of 200 files costs about 4 ms more per run:
each copy now opens its file through checked handles before the clone.
