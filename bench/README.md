# Recorded benchmark runs

Reports written by `ark-bench` (format: `docs/BENCH.md`). Every report in a comparison was
produced by the same bench code: older runtimes are measured with `bench/at-commit.sh`.

| File | What |
|---|---|
| `baseline.json` | the simulated report of the current release (v0.4.0); `scripts/ci.sh` compares against it |
| `m4/sim.json` | simulated, the M4 runtime (v0.3.0 plus measurement-only event fields, `035a06b`) |
| `m4/live-qwen3-4b.json` | live, M4 runtime, qwen3:4b-instruct through Ollama, 3 repetitions |
| `m4/live-qwen3-4b-partial.log` | the first M4 live attempt, cut short by a full disk and an unbounded generation |
| `m5/steps/<commit>.json` | simulated, after each M5 change (the commit), with the final bench code |
| `m5/live-qwen3-4b.json` | live, the M5 candidate (`f01cef6`) |
| `m5/live-qwen3-4b-noplanner.json` | live, M5 with the planner off |
| `m5/live-qwen3-4b-fasttier.json` | live, M5 with planning and the critic on qwen2.5:3b-instruct |
| `m5/rejected-index-summaries-live.json` | live, a change measured and not adopted |
| `m5/live-qwen3-4b-final.json` | live, v0.4.0, run right after the M4 live run on the same idle machine |

`RESULTS.md` has the tables; `results.py` prints them from two reports.
