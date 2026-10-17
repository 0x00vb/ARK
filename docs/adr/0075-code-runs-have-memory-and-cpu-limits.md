# 0075. Code runs can have memory and processor limits

Status: Proposed
Date: 2026-10-16

Amends 0032 › Consequences ("Still not limited: memory and CPU per run").

## Context
A `code.run` program can use most of the machine's memory or spin a processor for its whole
timeout (ROADMAP › Known limitations). On an ARK Cloud machine that also serves the person's
window, one run can starve the runtime (`board/evidence/ax-research-2026-10-16.md`, R2).
Measured on macOS 26.4: `setrlimit` refuses `RLIMIT_AS`, `RLIMIT_DATA` and `RLIMIT_RSS` (EINVAL,
"current limit exceeds maximum limit"), and `ulimit -v` fails the same way. `RLIMIT_CPU` works: a
spinning program ends with `SIGXCPU` (exit 152 from `sh`).

## Decision
An app sets `Exec::limits(Limits { memory_bytes, cpu_seconds })`; off by default. The process
tree that owns the run enforces them (`ark_os::process`), and the run's result says which one
stopped it.

| System | Memory | Processor time |
|---|---|---|
| macOS | the group's physical footprint (`proc_pid_rusage`), measured every 250 ms; past the limit the group is stopped | `RLIMIT_CPU` per process, set before the program runs; `SIGXCPU` is the reason |
| Linux | the group's resident memory from `/proc`, every 250 ms | `RLIMIT_CPU`, as macOS |
| Windows | the job's peak memory, every 250 ms | the job's user and kernel time, every 250 ms |

`subprocess::Run::stopped` is `Stop::Memory(limit)` or `Stop::Cpu(limit)`. `code.run` returns
`stopped: {limit, bytes|seconds}`, its card and the person's line read "stopped: it used more
than 512 MB of memory" or "stopped: it used more than 60 s of processor time", and stderr ends
with the same words in parentheses.

## Consequences
- Memory is checked, not reserved: a program can pass the limit for up to 250 ms before it is
  stopped. Linux counts shared pages in each process's resident memory, so it can stop early.
  A process that leaves the group (`setsid`) is not counted.
- The processor reason is given when the started process, or a shell's direct child, takes
  `SIGXCPU`. A program that catches it is killed a second later, and the reason is lost.
- Windows uses the job's peak memory and whole seconds of processor time; not yet run on a
  Windows machine.
- On Unix the processor limit is per process: a run that starts several busy processes can use
  more in all.
- Only `code.run` has limits. Agent programs (ADR 0074) and `process.start` do not.
- `subprocess::Spec` and `Run` gain fields (Breaking for struct literals).

## Check
`ark-exec/tests/run.rs` (macOS and Linux): `a_run_past_its_memory_limit_is_stopped_and_says_why`
(perl holds 600 MB under a 200 MB limit) and `a_run_past_its_processor_time_is_stopped_and_says_why`
(`sh` spins under a 1 s limit). Windows: `cargo clippy --target x86_64-pc-windows-gnu`; a run on
a Windows machine is left for the owner.
