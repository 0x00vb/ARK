# Benchmarks

`ark-bench` answers one question about a runtime change: does it make the agent better or
cheaper? It runs workloads against an ARK agent, records what every model request contained
and cost, and compares two runs metric by metric.

## Run it

```sh
cargo build --release -p ark-bench
./target/release/ark-bench list                                   # the workloads
./target/release/ark-bench run --out candidate.json               # simulated, deterministic
./target/release/ark-bench run --model ollama:qwen3:4b-instruct --reps 3 \
    --long-turns 12 --turn-timeout 300 --out live.json            # a real model
./target/release/ark-bench compare baseline.json candidate.json --md
./target/release/ark-bench compare baseline.json candidate.json --out comparison.json
./target/release/ark-bench run --harness naive --out naive.json  # the naive baseline loop
./target/release/ark-bench run --faults overloaded=0.1,bad-tool=0.05 --seed 7 --out faults.json
./target/release/ark-bench run --model claude:haiku --record live.jsonl --out live.json
./target/release/ark-bench run --model claude:haiku --replay live.jsonl --out replay.json
```

`compare` exits with status 2 when it finds a regression, so CI can gate on it
(`scripts/ci.sh` compares against `bench/baseline.json`).

| Flag | Meaning |
|---|---|
| `--model sim` / `provider:model` | the simulated model (default) or a real one |
| `--fast provider:model` | a second, cheaper tier for planning and the critic |
| `--reps N` | repetitions per workload |
| `--only a,b` | run some workloads |
| `--long-turns N` | turns in the long-session workload (live runs cap it at 12) |
| `--planner`, `--no-critic`, `--budget N` | runtime settings to compare (the planner is off by default) |
| `--turn-timeout S` | a turn that takes longer fails its run instead of stalling the benchmark |
| `--label` | the name the comparison shows |
| `--harness ark` / `naive` | ARK's harness (default) or the naive baseline loop |
| `--faults spec`, `--seed N` | inject faults into model calls (below) |
| `--record file` / `--replay file` | keep every model reply, or serve them back (below) |
| `--traces dir` | write each run's requests and events to `<dir>/<workload>-<rep>.jsonl` |

The report is rewritten after every run, so an interrupted live run keeps what it measured.

## Simulated and live

**Simulated** runs use a model whose policy reads each request (which tools are offered, which
results are still in the window, the working state) and decides the next move like a careful
agent: load an action before calling it, run each needed call once, dig into a large result
with `get_evidence`, record a finding, cite, answer. A runtime change that hides a result or
offers a tool earlier changes what the policy does. Usage is simulated: the prompt is the
estimated size of system, messages and tool schemas; the cached part is the prefix shared with
a recent request when it is at least 1,024 tokens; cache writes are the uncached part before
the last message. Two runs of the same code give the same numbers.

**Live** runs measure a real model: success, provider-reported tokens, latency and time to
first token. They are noisy; use `--reps` and compare means.

What each mode can tell you:

| Question | Simulated | Live |
|---|---|---|
| Did context, schema or cache-prefix tokens change? | yes, exactly | yes, with noise |
| Did the number of model calls a task needs change? | for the simulated policy | yes |
| Does a real model still pick the right tool? | no | yes |
| Latency, time to first token | tool time only | yes |
| Provider cache hits | estimated | when the provider reports them (Anthropic, OpenAI; Ollama reports none) |

## The generic suite

Every workload is neutral: a calculator, a unit converter, a small customer database, a log
source, product documents, a mirror whose primary is down, a slow weather service, a notes
store and a settings store, among filler adapters so tools are found among realistic
neighbors.

| Workload | What it exercises |
|---|---|
| simple | trivial work: no tool, no planning |
| tool-lookup | finding and loading one tool among 25 actions |
| multi-step | dependent calls, each using a value from the last |
| large-output | one line among 3,000 (`get_evidence` with `match`) |
| long-session | many turns; the prompt must stay flat |
| failure-recovery | a tool that always fails; retry once, then switch |
| duplicate | the model repeats an identical call |
| runaway | the model repeats one search forever; the loop must end |
| resume | a fact from turn 1 needed after a process restart |
| retrieval | one relevant document among 120 |
| approval | a state-changing call through the approval gate |
| parallel | three slow independent lookups in one reply |
| stale-read | read, write, read again: no stale reuse |
| team | a lead delegates two independent lookups to two workers (`Workload::team`) |

## The naive baseline

`--harness naive` runs the suite through the loop an application usually starts with
(`ark_bench::naive`): one short system prompt, the whole transcript and every tool's full
schema on every call, raw tool results, transient errors retried as a provider SDK does, and
a 25-step cap. No planner, critic, dedupe, distilling, compaction or stall guard. The
transcript survives a restart, as an app that saves it would keep it. A team workload is done
alone, with every tool.

Same tools, same store, same model, same checks: a difference between the two reports is the
harness. Two things keep it fair in simulated runs:

- The simulated model reads a result the same whether it arrives as ARK's card (`id=C-1042`)
  or raw JSON (`"id":"C-1042"`), and finds the task in a plain transcript (the first user
  message after the last finished turn).
- Where the naive loop fails, the reason is a missing guard or capability, never the
  simulated model: `duplicate` (no dedupe), `runaway` (no stall guard), `retrieval` (no
  search over stored documents, which ARK's evidence index gives).

`bench/RESULTS.md` has the numbers.

## Several agents

A workload whose `team()` lists agents runs each turn as a task for the first one, through an
`Orchestrator` over the workload's store and tools; the report counts every agent's model and
tool calls. The simulated model tells agents apart by the system prompt ("You are the agent
\"lead\""). `suite::Spec::with_team` builds one.

## Faults

`--faults overloaded=0.1,malformed=0.05,bad-tool=0.05` fails that share of model calls:

| Fault | What the harness sees |
|---|---|
| `overloaded` | a temporary provider error, worth a retry |
| `malformed` | a reply that cannot be used as sent |
| `bad-tool` | the reply's first tool call names a tool that does not exist |

The schedule depends only on `--seed` and the repetition, so a rerun sees the same faults.
`faults_injected` counts them per run. Faults land per model call, so two harnesses that make
different calls meet different faults: compare several seeds or repetitions, not one run.

## Record and replay

`--record file` keeps every model reply of a run (JSON lines); `--replay file` serves them
back instead of calling the model, so a live run can be reproduced, re-graded or inspected
offline at no cost. Replies are matched by a fingerprint of the request with run-specific ids
normalized out, and ids a reply quotes are translated to the replaying run's. A request the
recording never saw counts in `replay_diverged`; 0 means the replay was faithful.

Pass the same `--model` as the recording, so cost is priced the same. Replaying a simulated
recording loses the simulated cache-write cost (the simulated model reports it outside the
reply); a live recording carries it in the reply's usage.

## Your application's workloads

Implement `ark_bench::Workload` (or assemble one from closures like `suite::Spec`) and run it
with `ark_bench::run_suite`. SecRes does this in its `secres-bench` crate with its real
profile and adapters; ARK's own suite never names a domain.

```rust
pub trait Workload: Send + Sync {
    fn name(&self) -> &str;
    fn class(&self) -> &str;
    fn description(&self) -> &str;
    fn turns(&self, mode: Mode) -> Vec<TurnSpec>;      // TurnSpec::after_restart for a process boundary
    fn policy(&self) -> Policy;                        // the simulated model
    fn build(&self, env: &Env) -> anyhow::Result<Ark>; // env.builder() has the metered models
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId>;
    fn check(&self, out: &RunOutput) -> Verdict;
}
```

`ark_bench::sim::competent(goals, answer)` builds the careful-agent policy from a list of
goals (`Goal::call`, `Goal::find`) that may depend on what earlier results showed.

## Comparing commits

```sh
bench/at-commit.sh <commit> /tmp/bench-old   # the current bench, built against that runtime
/tmp/bench-old run --out old.json
./target/release/ark-bench run --out new.json
./target/release/ark-bench compare old.json new.json --md
```

Both sides then use the same workloads and the same simulated model, so the difference is
the runtime.

## What a report holds

Per run (`RunRecord`): success and the reason, the answer (clipped), turns and steps, model
calls by role and by model, input / cached / uncached / cache-write / output tokens, cost at
the report's pricing, wall time per turn, time to first token and latency per call, tool calls
(ok, failed, deduplicated, repeated executions), runtime-tool calls by name, provider errors
and retries, approvals, stall nudges and stops, peak and mean context, tokens per context
source, tool schema tokens, tools exposed and used, result tokens in context against the raw
output behind them, compressed rounds, and the prefix tokens shared with recent requests.

Per workload and in total (`Summary`): means of the above, success rate, cost per successful
run (total cost over successes), median and p95 turn time, median time to first token.

Cost uses the model's list price when it is a known hosted model, otherwise the reference
price (Claude Sonnet 5.5: $2 in, $2.50 cache write, $0.20 cache read, $10 out per million
tokens). The report says which.
