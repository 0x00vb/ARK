# 0003. Code decides what code can decide; a model call needs a benchmark

Status: Accepted
Date: recorded 2026-10-03

## Context
The target metric is cost per successful task. Extra model calls add cost and latency and can
lower success, especially on small models.

## Decision
Classification, distillation, dedupe, stall detection, reuse, compaction and request layout
are code. A new model call, prompt change or request-layout change ships only with
before-and-after `ark-bench` numbers (simulated always, live when model behavior could
change). A change that saves tokens and loses success is a regression.

## Consequences
- Ideas measured and rejected are listed in `ROADMAP.md` and `bench/RESULTS.md` (action
  summaries in the index, planner on a smaller model, planner off). Do not retry them without
  a new reason.
- `bench/baseline.json` moves only on purpose, in the same commit as the change.

## Check
Bench gate in `scripts/ci.sh`; `docs/BENCH.md`.
