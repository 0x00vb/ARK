# Results

Every number here comes from a recorded report in this directory (`README.md` lists them).
`bench/results.py` prints the before/after tables from two reports.

## The web (2026-10-03)

The `web` workload: find a page with `web.search`, read it with `web.fetch`, answer a fact that
sits near the end of a 95-line page. Pages come from a loopback server and search from a fixed
engine, so no run touches the network. Reports: `bench/web/qwen-web.json` (`--only web,simple
--reps 3`), `bench/web/qwen-web-naive.json` (`--harness naive`), `ollama:qwen3:4b-instruct`.

| Run | Success | Calls | Tokens | Cost per success |
|---|---:|---:|---:|---:|
| `web`, ARK, live qwen3:4b | 3/3 | 4.3 | 10,086 | $0.0227 |
| `web`, naive loop, live qwen3:4b | 3/3 | 3.0 | 10,835 | $0.0229 |
| `web`, ARK, simulated | 1/1 | 5.0 | 9,458 | $0.0086 |
| `web`, naive loop, simulated | 1/1 | 3.0 | 7,424 | $0.0106 |

ARK reads the page's first 1,500 characters in the card and finds the passage with
`get_evidence`, so it makes one or two more calls; the naive loop gets the whole page in its
transcript once. On this 95-line page the two cost about the same live. A real page is often
tens of thousands of tokens, which the naive loop then resends on every later call; this
workload does not measure that. The other 15 simulated workloads did not move
(`bench/baseline.json`).

Outside the bench, one question through `examples/writing-agent` (`writer -p`, "On what date
and at about what time of day did the great Lisbon earthquake strike?"):

| Model | Search source | Result |
|---|---|---|
| `claude:haiku` (Claude Code) | Claude Code's own search | 1 November 1755, 9:40, cites Wikipedia; 32 s |
| `ollama:qwen3:4b-instruct` | Brave answered 429 (rate limit from earlier probes); Wikipedia | read the Wikipedia article, quoted "approximately 09:40 local time"; 8.7 s |
| `ollama:qwen3:4b-instruct`, before Wikipedia was a source | Brave 429 only | no result; it said so and gave the date from memory, marked unverified |

## Skills read on demand (2026-10-03)

The `skill` workload: write release notes in a format only a stored skill gives. The request
carries one catalog line; the instructions come back from `skills.read`. Report:
`bench/skills/qwen-skill.json` (`--only skill,simple --reps 3`, `ollama:qwen3:4b-instruct`).

| Run | Success | Calls | Tokens | Cost per success |
|---|---:|---:|---:|---:|
| `skill`, live qwen3:4b | 3/3 | 2.0 | 3,495 | $0.00732 |
| `simple`, live qwen3:4b (same run) | 3/3 | 1.0 | 1,387 | $0.00284 |
| `skill`, simulated | 1/1 | 3.0 | 4,540 | $0.00549 |

In every live run the model read the skill first and its answer began with the format line
the skill sets. The other 14 simulated workloads have the same calls, tokens and cost with
skills in the build: a turn with no skills carries no catalog (`bench/baseline.json`).

## Overhead on small tasks: the planner (P2, 2026-10-03)

Target: after tool selection, ARK still cost 1.1 to 1.5x the naive loop per success on
`multi-step`, `resume`, `parallel` and `team` (qwen3:4b). Local qwen and simulated runs
only. Reports in `bench/p2b/`; `qwen-noplan.json` joins the two live runs.

Where the extra cost came from on those four (live qwen3:4b, mean per run, cost at the
reference price; from the traces in `bench/p2/traces/qwen-B`):

| Source | multi-step | resume | parallel | team | Cause |
|---|---:|---:|---:|---:|---|
| Plan calls | 1 | 1 | 1 | 4 | one per agent and task |
| Extra reasoning calls the plan's steps caused | 1 (`calc.eval` for "sum the amounts") | 1.3 (a search repeated after the restart; a `record_state` written as text) | 2 (one city per reply) | 2 (one task per step: worker-b ran two) | the steps |
| Answer length (output tokens, ARK / naive) | 172 / 51 | 90 / 14 per turn | 45 / 29 | 299 / 74 | the instructions ask for facts, inference and a next step |
| Prompt per reasoning call (ARK / naive) | 1,790 / 2,350 | 1,720 / 2,320 | 1,620 / 2,320 | 1,660 / 2,420 | ARK is cheaper per call |
| Cost per run (ARK / naive, USD x 1000) | 17.5 / 15.1 | 18.0 / 14.4 | 14.5 / 10.2 | 37.8 / 25.9 | |

Repeated context, tool discovery and result summaries were not the cause: every ARK call is
24 to 31% cheaper than a naive call (index plus meta tools are smaller than 30 full schemas,
and preloading leaves no `load_action` round trip). The cause is the number of calls, and
the planner made most of them. Its steps also stay in the working state into the next turn,
which is why `resume` searched again after the restart.

Planner off (14 workloads x 3, qwen3:4b):

| Configuration | Success | Cost / success (USD) | Model calls / run | Tokens / run |
|---|---:|---:|---:|---:|
| P2 tool selection (planner on) | 41/42 | 0.0189 | 5.3 | 8,085 |
| **Planner off (now the default)** | **42/42** | **0.0181** | **4.2** | **7,508** |
| naive loop (P1) | 36/42 | 0.0268 | 4.3 | 11,014 |

| Workload, cost per success | Planner on | Planner off | Naive |
|---|---:|---:|---:|
| multi-step | 0.0175 | 0.0139 | 0.0151 |
| resume | 0.0180 | 0.0126 | 0.0144 |
| parallel | 0.0145 | 0.0145 | 0.0102 |
| team | 0.0378 | 0.0355 | 0.0259 |
| duplicate | 0.0121 | 0.0075 | 0.0095 |
| retrieval | 0.0184 | 0.0135 | failed |

- `multi-step` and `resume` now cost less than the naive loop; `retrieval` and `stale-read`
  pass 3/3 (they failed without the planner in M5, before preloading and the ready list).
- `large-output` (3/3 against 2/3) and the 12-turn `long-session` cost 6 to 24% more per
  success: one large-output run read the 3,000 lines in three tries.
- Still dearer than naive: `parallel` (1.42x: qwen calls one city per reply even without a
  plan; naive's every-schema request gets all three in one), `team` (1.37x: a lead and two
  workers each pay a final answer), `tool-lookup` (1.16x).
- Simulated (13 workloads without the long session): 13/13 both ways, cost per success
  0.0075 to 0.0071, dearer on none.
- Not run on Claude Haiku: the 0.7 release gate measures all P2 defaults there once.

## Tool selection on a small model (P2, 2026-10-03)

Target from P1: on qwen3:4b the naive loop beat ARK. Local qwen and simulated runs only.
Reports in `bench/p2/` (`bench/harness.py` prints the tables).

What the traces showed, and the change for each:

| Failure (qwen3:4b) | Cause | Change |
|---|---|---|
| `tool-lookup` 0/3: called `market.quote` for "the converter's current rate" | The index lists names only; preloading also loaded a distractor (`mirror.backup`, "always current") and the model followed the index's "load an action first" | Preloading keeps only matches near the best score (siblings of the best match's adapter excepted), and the first message names the loaded actions as ready to call |
| `team` 0/3: a worker answered "she has 4 orders" with no total | The generic summary said `orders=4 items`; the model did not open the result | A small array is shown whole in the generic summary |
| `team`: a worker guessed a customer id | The floor alone dropped `crm.find_customer` next to `crm.orders` | An adapter's weaker matches stay next to its strong one |
| `large-output` 1/3 once the model stopped flailing | `lines=3000 items` gave no way to read them; the model searched stored documents instead | A counted array says how to read its rows (`get_evidence` with `match`) |

14 workloads x 3 on qwen3:4b (A and B ran at the same time on one Ollama):

| Configuration | Success | Cost / success (USD) | Model calls / run | Tokens / run |
|---|---:|---:|---:|---:|
| P1: ARK, preload 2 (the default before this change) | 35/42 | 0.0324 | 7.0 | 11,662 |
| P1: naive loop | 36/42 | 0.0268 | 4.3 | 11,014 |
| A: + preload floor + ready list | 37/42 | 0.0228 | 5.6 | 8,694 |
| **B: A + small arrays whole + read hint (now the default)** | **41/42** | **0.0189** | **5.3** | **8,085** |

- B closes the gap and passes it: 5 more successes than the naive loop at 29% less per
  success; against P1's default, 6 more successes at 42% less.
- `tool-lookup` 0/3 to 3/3 (3 calls, naive 2) and `team` 0/3 to 3/3. Neither the floor nor
  the ready list fixed `tool-lookup` alone; together they did.
- `large-output` is the one failure left (2/3); it was 3/3 in P1 after six search calls, and
  1/3 in A. The read hint is what makes it pass without flailing (3/3 in a focused run).
- Still dearer than naive per success on `multi-step`, `resume`, `parallel` and `team`
  (1.1 to 1.5x): discovery and the planner still cost calls on small tasks.
- Simulated, the defaults cost the same as before (cost per success 0.0074 to 0.0075 without
  the long session); the ready list adds about 60 tokens per turn.
- Not run live on Claude Haiku: these defaults are not measured there yet.

"Pass small results whole" (`ARK_WHOLE_RESULT_TOKENS`) stays off: the narrower inline-rows
rule fixed what it fixed on qwen, without passing whole outputs.

## ARK against a naive loop, live (P1, 2026-10-03)

14 workloads, 3 repetitions, long session capped at 12 turns, 300 s turn timeout. Reports and
recordings in `bench/p1/` (`bench/harness.py` prints the per-workload tables). Cost uses the
reference price for both models (Haiku is priced as Sonnet here; the ratios hold). Subscription
calls go through the `claude` CLI.

| Model, configuration | Success | Cost / success (USD) | Model calls / run |
|---|---:|---:|---:|
| Claude Haiku, ARK as shipped (vendor sessions on) | **42/42** | **0.0703** | 7.3 |
| Claude Haiku, naive loop | 37/42 | 0.1068 | 5.0 |
| Claude Haiku, ARK with sessions off | 40/42 | 0.1200 | 6.5 |
| qwen3:4b, ARK (v0.6.0 defaults) | 33/42 | 0.0398 | 7.5 |
| qwen3:4b, naive loop | 36/42 | **0.0268** | 4.3 |
| qwen3:4b, ARK, preload 2 | 35/42 | 0.0324 | 7.0 |
| qwen3:4b, ARK, preload 2 and whole results under 200 tokens | **37/42** | 0.0338 | 7.1 |

What it says:

- On Haiku, ARK as shipped beats the naive loop on both counts: 5 more successes, 34% less per
  success. Most of the cost win is vendor session reuse (cached reads); with sessions off ARK
  still succeeds more often (40 against 37) but costs 12% more.
- On qwen3:4b the naive loop beat v0.6.0's defaults. Seeing every tool at once suits a small
  model better than finding them in the index: `tool-lookup` failed 3 of 3 in every ARK
  configuration and passed 3 of 3 naive.
- ARK wins wherever context grows or a guard matters: large output (0.19x naive's cost on
  Haiku; the naive loop overflows qwen's context), the 12-turn session (0.30x on Haiku,
  0.76x on qwen), and retrieval, which the naive loop cannot do.
- ARK loses on small tasks: 1.2 to 1.8 times naive's cost on most, 4.5x on `tool-lookup` and
  5x on `team` (three agents each pay for discovery).
- Live models did not repeat calls or loop as the simulated `duplicate` and `runaway`
  workloads script, so those guards mattered less live (one Haiku `runaway` failure).

Preloading (P2, first change): on qwen, preload 2 fixed `approval` (0/3 to 3/3) and cut small
tasks' cost by 20 to 40%; on Haiku with sessions off it cut cost per success 18% (0.1045 to
0.0862) over the 11 workloads it finished before the run was stopped to save quota, with no
failure. **Adopted: preload 2 is the default.** Whole small results were measured on qwen only,
with mixed results (`team` 0/3 to 3/3, `large-output` 3/3 to 2/3), so they stay off.

## ARK against a naive loop (simulated, 2026-10-03)

The measured-runtime milestone, P0. `ark-bench run --harness naive` runs the same 14
workloads with a plain agent loop: the whole transcript and every tool schema on every call,
raw tool results, no guards (`docs/BENCH.md`). Same tools, store, simulated model and checks.
Reports: `baseline.json` (ARK) and `baseline-naive.json`.

| Total (14 workloads, 1 run each) | ARK | Naive |
|---|---:|---:|
| Task success | 1.00 | 0.79 |
| Cost / successful task (USD) | 0.121 | 1.172 |
| Tokens / run | 179,106 | 1,273,778 |
| Model calls / run | 113.3 | 75.7 |
| Peak context (tokens) | 1,739 | 44,137 |

Per workload, ARK / naive:

| Workload | Success | Model calls | Tokens | Peak context | Cost / success (USD) |
|---|---:|---:|---:|---:|---:|
| simple | 1 / 1 | 1 / 1 | 1,124 / 1,344 | 1,121 / 1,341 | 0.0028 / 0.0034 |
| tool-lookup | 1 / 1 | 4 / 2 | 5,272 / 2,776 | 1,412 / 1,396 | 0.0079 / 0.0041 |
| multi-step | 1 / 1 | 9 / 4 | 11,963 / 5,798 | 1,716 / 1,505 | 0.0152 / 0.0054 |
| large-output | 1 / 1 | 5 / 2 | 6,535 / 45,515 | 1,421 / 44,137 | 0.0086 / 0.0896 |
| long-session (500 turns) | 1 / 1 | 1500 / 1000 | 2,404,142 / 17,694,977 | 1,638 / 34,145 | 1.5496 / 12.7277 |
| failure-recovery | 1 / 1 | 7 / 4 | 8,546 / 5,802 | 1,587 / 1,504 | 0.0131 / 0.0054 |
| duplicate | 1 / 0 | 6 / 4 | 6,700 / 5,710 | 1,420 / 1,472 | 0.0087 / failed |
| runaway | 1 / 0 | 8 / 25 | 10,156 / 45,150 | 1,739 / 2,220 | 0.0112 / failed |
| resume | 1 / 1 | 9 / 3 | 10,773 / 4,197 | 1,441 / 1,416 | 0.0115 / 0.0045 |
| retrieval | 1 / 0 | 4 / 1 | 3,740 / 1,372 | 1,412 / 1,358 | 0.0064 / failed |
| approval | 1 / 1 | 4 / 2 | 5,035 / 2,759 | 1,332 / 1,385 | 0.0076 / 0.0040 |
| parallel | 1 / 1 | 4 / 2 | 3,982 / 2,844 | 1,403 / 1,436 | 0.0078 / 0.0045 |
| stale-read | 1 / 1 | 7 / 4 | 8,254 / 5,681 | 1,498 / 1,455 | 0.0124 / 0.0052 |
| team | 1 / 1 | 18 / 6 | 21,258 / 8,972 | 1,689 / 1,572 | 0.0355 / 0.0068 |

What it says:

- ARK wins where context grows or the model misbehaves: a long session (flat context,
  -88% cost), a large output (-90%), and the three runs the naive loop fails (a repeated
  call executed twice, a loop that never ends, a document it cannot search for).
- **ARK loses on small tasks**: 1.5 to 5 times the calls and cost on 9 of 14 workloads. Most
  of it is tool discovery (find the action in the index, load it, then call it) and the
  planner and critic calls a short task does not need. The team workload pays it in three
  agents. This is the first P2 target.
- The totals are means, so the 500-turn session dominates them. Read the rows.

Limits: simulated. The policy is a careful scripted agent, so it never wanders the way a
real model can without guards, and the naive loop's lack of guards costs it less here than
it would live. P1 measures both live.

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
| Planner off | success 0.85 to 0.72, cost per success +29% (`m5/live-qwen3-4b-noplanner.json`) | planner stayed on; after P2 it measured the other way and is off (ADR 0008) |
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

## Orchestration on subscriptions (live, 2026-10-03)

`examples/agent-team`: a coordinator on `claude:sonnet` delegates two document readings in
one reply to `reader-a` on `codex` and `reader-b` on `claude:haiku`, then stores a diagram and
answers. Managed vendor sessions on (the default) against off (`ARK_SESSION_REUSE=0`: every
call opens a fresh vendor session, the earlier behavior). Same binary, three rounds, on and
off alternating. Logs: `bench/orchestration/repeat/`; table: `bench/orchestration/summarize.py`.

| Run | Result | Wall time | Model calls | Input tokens | Uncached input | Output |
|---|---|---:|---:|---:|---:|---:|
| on 1 | done | 75.9 s | 7 | 67,493 | 37,783 | 3,354 |
| on 2 | done | 57.6 s | 7 | 62,081 | 30,273 | 3,313 |
| on 3 | done | 129.1 s | 9 | 148,128 | 56,638 | 6,195 |
| off 1 | done | 60.2 s | 8 | 81,842 | 73,685 | 5,286 |
| off 2 | done | 67.8 s | 7 | 67,415 | 59,258 | 4,496 |
| off 3 | done | 59.3 s | 7 | 61,500 | 53,343 | 3,652 |

| Metric (3 runs) | Sessions on | Sessions off | Delta |
|---|---:|---:|---:|
| Tasks done | 3 / 3 | 3 / 3 | |
| Uncached input, mean | 41.6k | 62.1k | -33% |
| Uncached input, median | 37.8k | 59.3k | -36% |
| Input tokens, median | 67.5k | 67.4k | 0% |
| Wall time, median | 75.9 s | 60.2 s | +26% |

What the numbers say:

- Sessions cut uncached input in every pairing. The vendor reads its whole context on every
  call either way, so total input does not move; what changes is how much of it is served
  from cache. A resumed Codex session serves its 20k base prompt from cache (20,096 of 20,719
  tokens on the reader's second call; 0 without a session).
- Wall time is not better with sessions on this evidence. "on 3" was slow because the Haiku
  reader gave up (below) and the lead delegated its document again, to the Codex reader.
- In every run both readers ran at the same time and each saw only its brief and its document.
- An earlier single pair (`bench/orchestration/agent-team-sessions-{on,off}.log`) showed
  40.0k against 62.2k uncached input (69.7k against 70.5k input).
- Before the protocol changes below, two runs took 102 s and 94 s and the lead used 40k and
  41k tokens: Sonnet first tried to call host tools natively and Claude Code refused them.

### The Claude Code reply protocol

Claude Code returns ARK's structured reply through a `StructuredOutput` tool. A model that
sees ARK's tools described like native ones tries to call them; Claude Code refuses, inside
the same call. Probes: `bench/protocol/` (`claude -p`, recorded output in `results.txt`).

| Change | Model | Internal turns | Input tokens | Result |
|---|---|---:|---:|---|
| Opening "You have no tools of your own" (before) | Sonnet | 4 | 3,049 | right call |
| Opening "Your only tool is StructuredOutput" | Sonnet | 2 | 1,384 | right call |
| Tools as Markdown headings (before) | Haiku | 4 | 5,500 | gave up, 2 of 2 |
| Tools as bullets, "use them only inside StructuredOutput" | Haiku | 2 | 1,520 | right call, 2 of 2 |
| Same, Sonnet | Sonnet | 2 | 1,462 | right call, 2 of 2 |
| No `--json-schema`, reply as plain JSON text | Haiku | 1 | 1,218 | 0 of 3 (writes fake `<function_calls>`) |
| No `--json-schema`, reply as plain JSON text | Sonnet | 1 | 1,554 | 3 of 3 |

With bullets, the full agent prompt still made Haiku give up in 1 of 3 runs ("on 3": it tried
`doc.read`, `search_evidence`, `load_action` and `rules.lookup` natively). So ARK now also
checks each Claude reply: more than 2 internal turns ending in a plain answer while tools
were offered means the model gave up, and ARK asks once more in the same vendor session with
a one-line correction.

Then three more runs with both readers on `claude:haiku` (`bench/orchestration/repeat/haiku-readers-*.log`):
6 of 6 reader tasks done, 3 of 3 runs done, mean 42.7 s, 7 calls and 24.6k uncached input per
run. In 3 of the Haiku sessions the session file still shows refused native calls, but the
model recovered inside the same call; the give-up check never fired. It is covered by a test
with a scripted CLI (`a_model_that_gave_up_after_refused_native_calls_is_asked_once_more`),
not yet by a live run.

## What these numbers do not show

- Provider-reported cache reads on Anthropic or OpenAI (no key in this environment).
- A larger model: every live number is one 4B local model.
- Cost on a hosted model: cost here is the reference price applied to tokens, not a bill.
