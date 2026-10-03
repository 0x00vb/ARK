# 0008. The planner is off by default

Status: Accepted
Date: 2026-10-03

## Context
After P2, ARK still cost 1.1 to 1.5 times the naive loop per success on `multi-step`,
`resume`, `parallel` and `team` (live, qwen3:4b). The traces put most of that on the
planner, not on the plan call itself (about 130 tokens):

| Effect of the plan's steps | Where |
|---|---|
| One reply per step: three lookups became three calls | `parallel`, `team` workers |
| Steps the task did not need ("sum the amounts" ran `calc.eval`; "verify" made a `record_state` written as text, then a retry) | `multi-step`, `duplicate`, `resume` |
| The lead delegated one task per step (three tasks for two workers) | `team` |
| The steps stay in the working state's NEXT ACTIONS into the next turn, so the model searched again after a restart | `resume` |

In M5 (v0.4.0) the planner was measured off and kept: success fell 0.85 to 0.72
(`retrieval`, `stale-read`). Since then preloading and the ready-to-call list give the first
request the actions a task needs, which is what the plan gave a small model.

## Decision
`planner` defaults to false in `Harness`, `ArkBuilder`, orchestrated agents and
`BenchSettings`. The planner stays available (`.planner(true)`, `AgentSpec::planner(true)`,
`ark-bench --planner`).

## Consequences
- Live qwen3:4b, 14 workloads x 3: 42/42 against 41/42, cost per success 0.0181 against
  0.0189, 4.2 model calls per run against 5.3. `retrieval` and `stale-read` pass 3/3
  without it. Simulated: 0.0075 to 0.0071 per success, no lost success.
- Not measured on Claude Haiku yet: the 0.7 release gate runs Haiku once on all P2 defaults.
- An app with long tasks that benefit from a plan turns it on and measures it.

## Check
`bench/baseline.json` (simulated, `scripts/ci.sh`); live reports in `bench/p2b/`.
