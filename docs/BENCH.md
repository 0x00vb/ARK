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
| `--no-planner`, `--no-critic`, `--budget N` | runtime settings to compare |
| `--turn-timeout S` | a turn that takes longer fails its run instead of stalling the benchmark |
| `--label` | the name the comparison shows |

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
