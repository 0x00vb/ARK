# M5 results

Every number here comes from a recorded report in this directory (`README.md` lists them).
`bench/results.py` prints the before/after tables from two reports.

## How it was measured

| | |
|---|---|
| Machine | Apple M4 Pro, 48 GB, macOS |
| Live model | `qwen3:4b-instruct` (Q4_K_M) through local Ollama; single tier; planner and critic on (the defaults) |
| Live runs | 13 workloads, 3 repetitions each, long session capped at 12 turns, 300 s turn timeout; M4 and M5 run back to back on an otherwise idle machine |
| M4 | the v0.3.0 runtime plus measurement-only event fields (`035a06b`), built with the same bench code (`bench/at-commit.sh`) |
| M5 | v0.4.0 |
| Cost | the reference price (Claude Sonnet 5.5 list: $2 in, $2.50 cache write, $0.20 cache read, $10 out per Mtok) applied to the measured tokens |
| Simulated | the deterministic simulated model (`docs/BENCH.md`) |

## Live, M4 against M5

| Metric | M4 (v0.3.0 runtime) | M5 (v0.4.0) | Delta |
|---|---:|---:|---:|
| Task success | 0.46 | 0.85 | +83% |
| Cost / successful task (USD) | 0.0857 | 0.0298 | -65% |
| Input tokens / run | 16887 | 10342 | -39% |
| Uncached input tokens / run | 16887 | 10342 | -39% |
| Cached input tokens / run | 0 | 0 |  |
| Output tokens / run | 577 | 450 | -22% |
| Model calls / run | 8.4 | 6.6 | -21% |
| Tool calls / run | 1.7 | 2.0 | +16% |
| Duplicate calls served from reuse / run | 0.1 | 0.0 | -75% |
| Repeated identical executions / run | 0.0 | 0.1 |  |
| Peak context (tokens) | 2423 | 1901 | -22% |
| Median turn latency (ms) | 4128 | 4225 | +2% |
| P95 turn latency (ms) | 20490 | 10346 | -50% |
| Median time to first token (ms) | 137 | 259 | +89% |
| Failed provider calls / run | 0.00 | 0.00 |  |
| Retried provider calls / run | 0.00 | 0.00 |  |
| Turns that hit the 300 s timeout | 1 | 0 | -100% |

| Workload | Success | Model calls | Tokens / run |
|---|---:|---:|---:|
| simple | 1.00 -> 1.00 | 1.0 -> 1.0 | 1808 -> 1349 |
| tool-lookup | 0.00 -> 0.00 | 4.0 -> 4.3 | 8262 -> 7175 |
| multi-step | 1.00 -> 1.00 | 8.7 -> 8.3 | 17569 -> 13874 |
| large-output | 0.00 -> 1.00 | 12.3 -> 5.7 | 25728 -> 9313 |
| long-session | 1.00 -> 1.00 | 23.3 -> 22.0 | 54186 -> 45660 |
| failure-recovery | 0.33 -> 1.00 | 6.0 -> 6.0 | 10775 -> 8616 |
| duplicate | 0.00 -> 1.00 | 8.7 -> 6.0 | 16285 -> 8348 |
| runaway | 0.33 -> 1.00 | 9.3 -> 4.0 | 17559 -> 4239 |
| resume | 1.00 -> 1.00 | 6.0 -> 7.3 | 10426 -> 10757 |
| retrieval | 0.00 -> 1.00 | 7.3 -> 5.3 | 22124 -> 8162 |
| approval | 0.00 -> 0.00 | 2.0 -> 2.0 | 3664 -> 2840 |
| parallel | 0.33 -> 1.00 | 11.3 -> 6.0 | 20428 -> 8172 |
| stale-read | 1.00 -> 1.00 | 9.0 -> 7.7 | 18213 -> 11791 |

Notes:

- Live cached tokens are 0 on both sides: Ollama does not report cache reads. Its KV cache
  reuse shows only as shorter prompt evaluation.
- Median time to first token rose (137 to 259 ms) while the median turn did not move. The
  likely cause, not proven: the working state is now the last message, after the prefix
  Ollama can reuse, so each step evaluates it before the first token. The same trade-off
  shows in simulation as about 2% more uncached input in workloads that never record state.
- The two workloads still failing (tool-lookup, approval) fail on both sides: the model picks
  a plausible neighbor (`market.quote`) or files the note in its own working state.
- 3 repetitions per workload: one run is 0.33 of a workload's success rate.

## Simulated, M4 against M5 (long session excluded from the means)

| Metric | M4 (v0.3.0 runtime) | v0.4.0 | Delta |
|---|---:|---:|---:|
| Task success | 0.75 | 1.00 | +33% |
| Cost / successful task (USD) | 0.0180 | 0.0094 | -48% |
| Input tokens / run | 11176 | 6735 | -40% |
| Uncached input tokens / run | 4422 | 3137 | -29% |
| Cached input tokens / run | 6754 | 3598 | -47% |
| Output tokens / run | 128 | 105 | -18% |
| Model calls / run | 6.8 | 5.7 | -17% |
| Tool calls / run | 1.5 | 1.5 | +0% |
| Duplicate calls served from reuse / run | 0.2 | 0.2 | -33% |
| Repeated identical executions / run | 0.2 | 0.2 | +0% |
| Peak context (tokens) | 3927 | 1739 | -56% |
| Median turn latency (ms) | 3 | 2 | -33% |
| P95 turn latency (ms) | 761 | 259 | -66% |
| Failed provider calls / run | 0.00 | 0.00 |  |
| Retried provider calls / run | 0.00 | 0.00 |  |
| Turns that hit the 300 s timeout | 0 | 0 |  |

| Workload | Success | Model calls | Tokens / run |
|---|---:|---:|---:|
| simple | 1.00 -> 1.00 | 1.0 -> 1.0 | 1477 -> 1124 |
| tool-lookup | 1.00 -> 1.00 | 4.0 -> 4.0 | 6676 -> 5272 |
| multi-step | 1.00 -> 1.00 | 9.0 -> 9.0 | 14798 -> 11963 |
| large-output | 0.00 -> 1.00 | 6.0 -> 5.0 | 14925 -> 6535 |
| failure-recovery | 1.00 -> 1.00 | 7.0 -> 7.0 | 10624 -> 8546 |
| duplicate | 1.00 -> 1.00 | 6.0 -> 6.0 | 8453 -> 6700 |
| runaway | 0.00 -> 1.00 | 18.0 -> 8.0 | 29755 -> 10156 |
| resume | 1.00 -> 1.00 | 9.0 -> 9.0 | 13577 -> 10773 |
| retrieval | 1.00 -> 1.00 | 7.0 -> 4.0 | 13569 -> 3740 |
| approval | 1.00 -> 1.00 | 4.0 -> 4.0 | 6438 -> 5035 |
| parallel | 1.00 -> 1.00 | 4.0 -> 4.0 | 5029 -> 3982 |
| stale-read | 0.00 -> 1.00 | 7.0 -> 7.0 | 10329 -> 8254 |

## Each change, in order (simulated, same bench code)

Means over 12 workloads (all but the long session), the parallel workload's turn time, and the
500-turn session separately.

| Step | Success | Model calls / run | Tokens / run | Uncached input / run | Cost / success (USD) | parallel turn (ms) | 500 turns: peak context | 500 turns: tokens / turn | 500 turns: uncached / turn | 500 turns: cost / turn (USD) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| M4 baseline (035a06b) | 0.75 | 6.8 | 11304 | 4422 | 0.0180 | 761 | 1983 | 5848 | 918 | 0.00370 |
| Stall guard counts only new information (c046edd) | 0.83 | 6.0 | 9763 | 4310 | 0.0153 | 766 | 1983 | 5848 | 918 | 0.00370 |
| get_evidence match and offset (ad3062d) | 0.92 | 5.9 | 9379 | 4018 | 0.0132 | 772 | 2018 | 5949 | 927 | 0.00374 |
| Cache-friendly request layout (e9929f8) | 0.92 | 5.9 | 9169 | 3963 | 0.0129 | 764 | 2011 | 5929 | 711 | 0.00329 |
| Anthropic cache breakpoints (4c4fe22) | 0.92 | 5.9 | 9169 | 3963 | 0.0129 | 762 | 2011 | 5929 | 711 | 0.00329 |
| Reuse semantics, no stale reads (ea903c5) | 1.00 | 5.9 | 9170 | 3964 | 0.0119 | 760 | 2011 | 5929 | 711 | 0.00329 |
| Parallel reads (7836a05) | 1.00 | 5.9 | 9170 | 3964 | 0.0119 | 258 | 2011 | 5929 | 711 | 0.00329 |
| Timeouts, caps, safe retries (805560b) | 1.00 | 5.9 | 9170 | 3964 | 0.0119 | 259 | 2011 | 5929 | 711 | 0.00329 |
| Runtime tool set, distiller and state fixes (f01cef6) | 1.00 | 5.9 | 7306 | 3228 | 0.0098 | 259 | 1638 | 4808 | 725 | 0.00310 |
| Search results show matching lines (6475a97) | 1.00 | 5.7 | 6840 | 3137 | 0.0094 | 263 | 1638 | 4808 | 725 | 0.00310 |
| Text tool-call guard (v0.4.0) | 1.00 | 5.7 | 6840 | 3137 | 0.0094 | 259 | 1638 | 4808 | 725 | 0.00310 |

Changes that are not visible here: provider timeouts and retries (the simulated model never
fails; see the live timeouts above and `crates/ark-runtime/tests/retry.rs`), Anthropic cache
breakpoints (provider wire format only), and the text tool-call guard (the simulated model
never writes a call as text; live it happened in 4 of 39 M5 runs before the guard and 0
after).

## Measured and not adopted

| Change | Live result (qwen3:4b, 3 reps) | Decision |
|---|---|---|
| Planner off | success 0.85 to 0.72, cost per success +29% (`m5/live-qwen3-4b-noplanner.json`) | planner stays on |
| Planning and critic on qwen2.5:3b (fast tier) | success 0.85 to 0.69, cost per success +19%; Ollama also swaps models on every plan call (`m5/live-qwen3-4b-fasttier.json`) | single tier by default; the router stays available |
| One-line summaries per action in the capability index | tool-lookup 0/3 to 0/3, approval 0/3 to 0/3, multi-step 3/3 to 2/3, tokens +15 to 40% (`m5/rejected-index-summaries-live.json`) | reverted |

## Where the model calls go (live M5)

| Call | Calls in 39 runs | Mean tokens per call | Why it needs a model | Decision |
|---|---:|---:|---|---|
| reason (each step) | 228 | 1808 | choosing the next action and writing the answer | kept; fewer per task (stall guard, filtered evidence, search lines) |
| plan | 24 | 140 | a short plan for non-trivial tasks | kept: turning it off lowered success |
| critic | 5 | 1078 | revising an answer that failed the deterministic self-check | kept: runs only after a failed check |
| synthesize (forced answer) | 0 | n/a | a final answer when the loop stops | kept: only when the stall guard or step limit ends a turn |

Done without a model: classifying a task as trivial, distilling tool results, dedup and reuse,
the self-check, the stall guard, session compaction, context accounting, choosing a model
tier.

## Long-running sessions

From `crates/ark-ui/tests/resource_bounds.rs` (2,000 turns through the workspace host, each
running a subprocess, then 300 browser connections dropped). Before the fix, RSS went from
20.2 to 30.9 MB and turn time from 4 to 13 ms over 1,000 turns and kept rising. After:

| turn | RSS (MB) | open fds | child processes | store (KB) | turn (ms) |
|---:|---:|---:|---:|---:|---:|
| 50 | 19.5 | 15 | 0 | 4340 | 3 |
| 1000 | 21.6 | 15 | 0 | 11597 | 5 |
| 2000 | 21.6 | 15 | 0 | 18901 | 5 |

## What these numbers do not show

- Provider-reported cache reads on Anthropic or OpenAI (no key in this environment).
- A larger model: every live number is one 4B local model.
- Cost on a hosted model: cost here is the reference price applied to tokens, not a bill.
