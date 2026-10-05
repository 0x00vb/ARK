# Recorded benchmark runs

Reports written by `ark-bench` (format: `docs/BENCH.md`). Every report in a comparison was
produced by the same bench code: older runtimes are measured with `bench/at-commit.sh`.

| File | What |
|---|---|
| `baseline.json` | the simulated report of `main` (18 workloads since `claimed-schedule`; the older ones did not move with it); `scripts/ci.sh` compares against it. Regenerate it in the commit that moves the numbers on purpose |
| `cli-live/haiku-v0.8.0.json`, `cli-live/haiku-live.json`, `cli-live/codex-v0.8.0.json`, `cli-live/codex-app-server.json` | live, the subscription CLIs before (v0.8.0, a process per call) and after (kept running per session) |
| `cli-live/long-task/{claude-sonnet,claude-haiku,codex-default}.jsonl`, `long_task.py` | live, one longer task per CLI through `writer --serve` and the window protocol (2026-10-04): tools, a follow-up, a cancelled scene, recovery, `/review 2` with three agents, a reused process, `subscription_usage`. The `claude-sonnet` run counted CLI processes machine-wide, so its `cli_procs` values mean nothing; the other two count the writer's own |
| `skills/qwen-skill.json` | live, the `skill` and `simple` workloads on qwen3:4b-instruct, 3 repetitions |
| `web/qwen-web.json`, `web/qwen-web-naive.json` | live, the `web` workload (and `simple`) on qwen3:4b-instruct, 3 repetitions, ARK and the naive loop |
| `claims/qwen3-4b-{before,after}.json`, `claims/qwen3.5-27b-{before,after}.json` | live, the `claimed-files` workload before and after the check of claimed files, 5 repetitions on qwen3:4b-instruct and 3 on qwen3.5:27b |
| `claims/schedule-qwen3-4b.json`, `claims/schedule-qwen3.5-27b.json` | live, the `claimed-schedule` workload, 5 repetitions on qwen3:4b-instruct and 3 on qwen3.5:27b |
| `board/live-qwen3-4b.json`, `board/sim.json` | the `board` workload (ADR 0023), 3 repetitions live on qwen3:4b-instruct and simulated |
| `analysis/live-qwen3-4b.json`, `analysis/live-qwen3.5-27b.json`, `analysis/sim.json` | the `analysis` workload: 3 repetitions live on qwen3:4b-instruct, 2 on qwen3.5:27b, and simulated |
| `analysis/repeat-before-qwen3-4b.json`, `analysis/repeat-after-qwen3-4b.json` | the `analysis` workload, 5 repetitions live on qwen3:4b-instruct, before and after an identical change runs at most twice |
| `baseline-naive.json` | the same suite through the naive loop (`--harness naive`), for "ARK against a naive loop" in `RESULTS.md` |
| `m4/sim.json` | simulated, the M4 runtime (v0.3.0 plus measurement-only event fields, `035a06b`) |
| `m4/live-qwen3-4b.json` | live, M4 runtime, qwen3:4b-instruct through Ollama, 3 repetitions |
| `m5/steps/<commit>.json` | simulated, after each M5 change (the commit), with the final bench code |
| `m5/live-qwen3-4b.json` | live, the M5 candidate (`f01cef6`) |
| `m5/live-qwen3-4b-noplanner.json` | live, M5 with the planner off |
| `m5/live-qwen3-4b-fasttier.json` | live, M5 with planning and the critic on qwen2.5:3b-instruct |
| `m5/rejected-index-summaries-live.json` | live, a change measured and not adopted |
| `m5/live-qwen3-4b-final.json` | live, v0.4.0, run right after the M4 live run on the same idle machine |
| `orchestration/agent-team-sessions-{on,off}.log` | live, `examples/agent-team` on Claude and ChatGPT subscriptions, managed sessions on and off (`ARK_SESSION_REUSE=0`); the printed run |
| `orchestration/agent-team-sessions-{on,off}.graph.json` | the orchestration `Graph` at the end of each of those runs |
| `orchestration/repeat/sessions-{on,off}-{1,2,3}.log` | the same, three rounds with on and off alternating; `orchestration/summarize.py` makes the table |
| `orchestration/repeat/haiku-readers-{1,2,3}.log` | the same with both readers on `claude:haiku`, sessions on |
| `protocol/*.py`, `protocol/results.txt` | `claude -p` probes of the reply protocol wording and their recorded output |

`RESULTS.md` has the tables; `results.py` prints them from two reports.
