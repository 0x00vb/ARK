# Bounded call and token evaluation

Date: 2026-10-15. Board: `958effaa`. Owner authorized evaluation and measured fixes.
Baseline: ARK `11eee32`. Worktree: `ark-call-efficiency`, branch `codex/call-efficiency`.

## Protocol

Ten fixed existing workloads, three attempts each. See `manifest.json` for fixture hashes,
CLI version, configuration hash, model and settings. The live model is
`codex:gpt-5.6-terra@medium`, explicitly advertised by the installed CLI. The first requested
6.1 model failed catalog validation before any workload or model call. There are no new API
credits or purchases. Dollar values use the same frozen reference weights on every condition,
not subscription billing or verified current list prices.

| Condition | Difference from the preceding condition |
|---|---|
| Baseline | Released ARK source, no product changes |
| Isolation | Only the host-model adapter's unused native tools are disabled. Benchmark reporting is corrected separately |
| Prompt, rejected | The default instructions discourage a separate state update solely to repeat a completed result |
| Approval mapping | Adds an explicit route through the existing approval gate for actions the model is permitted to propose |

The isolation executable was copied before the prompt edit. It stays frozen while the next
condition builds. `isolation.patch` and `final.patch` record source differences from the
baseline. Tests added later do not change the frozen executable's production behavior.

The original prompt condition starts with the four affected workloads. It expands to the other six
only if checked results hold and the call/cost evidence supports keeping the prompt change.
Keep every failed attempt. Do not replace a failure with a successful rerun.

That prompt condition failed one approval attempt and is rejected. The model returned a
text-only request for permission without reaching the gate. Its 12-task focus report and
18-task remainder remain separate, disjoint parts of that condition. One scoped mapping
correction completed a new full 30-attempt condition with 30/30 checked outcomes. No failed attempt is
relabelled or removed. A further checked regression rejects the prompt changes; retain the
verified isolation-only change rather than extend the experiment.

## Commands

Build with `CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 cargo build --release -p ark-bench`.

```sh
target/release/ark-bench run --label baseline-sim --out baseline-sim.json --traces baseline-sim-traces
target/release/ark-bench run --model codex:gpt-5.6-terra@medium \
  --only simple,tool-lookup,multi-step,large-output,failure-recovery,resume,approval,parallel,stale-read,analysis \
  --reps 3 --turn-timeout 120 --label CONDITION \
  --traces CONDITION-live-traces --out CONDITION-live.json
python3 summarize.py CONDITION-live.json --traces CONDITION-live-traces
```

Run the benchmark command from the repository root, with output paths prefixed by this
directory. The baseline command initially included `--record`. Its empty output exposed the
managed-session interception gap described below. It was removed. Do not use that option in
the corrected managed-session evaluation.

## Measurement limits

- ModelCall events report vendor input, cached input, cache writes and output. They can
  include the vendor's own internal context and calls. They are not ARK prompt estimates.
- Context events estimate the canonical ARK request by source. They do not measure vendor
  cache eligibility or the vendor's complete serialized request.
- Managed sessions bypass the direct provider meter and cassette. Old reports consequently
  showed zero request metrics and produced an empty recording. The fix marks coverage,
  restores available context and cache-write counts from runtime events, warns on the CLI,
  and rejects unsupported recording, replay and fault injection before a call.
- Raw managed-request definitions, exact metadata-call counts and prefix reuse remain
  unavailable. Do not use their zero fields as observations. Compare schema sizes through
  Context events on both conditions, including the original baseline.
- The original baseline's outcomes, ModelCall counts, provider usage and Context events are
  still valid. No raw request interception claim is made for it or later managed runs.
- State-update events may accompany useful work. The summarizer separately counts steps
  with state updates and no adapter or retrieval call. Other metadata operations are not
  fully observable, so this is a diagnostic proxy, not an exact count of wasted calls.
- The local vendor configuration hash changed during the runs, outside this evaluation.
  Its current file mtime is 20:21:21 UTC, after baseline ended at 20:12:44 and before
  isolation ended at 20:26:41. Raw settings were not retained because they can include
  credentials. The manifest preserves both hashes. This prevents a strict causal attribution
  across conditions. Explicit model, medium effort, workload hashes and 16,000-token ARK
  budgets remained fixed. No user configuration was changed or restored by this work. The observed mtime does not
  prove the file changed only once, or identify the setting that changed.
- Three attempts are directional evidence. Model variability, provider load and concurrent
  compilation prevent a controlled latency claim. No Reason comparison or delegation
  efficiency claim is supported by these ten tasks.
- Existing grades are limited. Trace inspection and runtime approval, refusal, stale-read,
  cancellation and crash-recovery tests supplement them. Analysis checks the saved files.
- Failed or cancelled provider requests can consume unreported usage. Known successful-call
  token totals are not a billing reconciliation if a condition has such failures.

## Native mechanism probe

`probe_vendor.py` uses the same short synthetic greeting and output schema. It alternates
inherited configuration and process/thread overrides without changing user settings. It
archives its own threads and stores only usage, answer, timing and server count. The raw
configuration can contain secrets and is never logged or written.

Three completed inherited observations each reported 15,246 input tokens. Three completed
isolated observations reported 11,127, 11,125 and 11,241. One earlier isolated attempt timed
out in the probe's buffered reader, with no recovered usage. The reader was fixed before
the final completed observation. Three configuration preflights also failed before model
submission while determining the RPC overlay's supported shape. These are disclosed setup
events, not replaced benchmark failures. This probe identifies a mechanism; the ten-task
comparison determines whether the production change helps.

Official references checked on this date:
[configuration](https://learn.chatgpt.com/docs/config-file/config-reference) and
[app server](https://learn.chatgpt.com/docs/app-server). The installed CLI generated its own
schema locally for verification. Dotted MCP server names that the RPC cannot safely override
select ARK's existing isolated exec fallback. Model catalog discovery remains available.

## Results

The baseline completed 30/30 checked attempts in 104 model calls and 2,012,191 total tokens.
Its total reference-price estimate is $1.8234412.

| Condition | Checked successes | Model calls | Total tokens | Reference estimate |
|---|---:|---:|---:|---:|
| Baseline | 30/30 | 104 | 2,012,191 | $1.8234412 |
| Isolation only | 30/30 | 102 | 1,526,988 | $1.4472496 |
| Prompt candidate, rejected | 29/30 | 91 | 1,382,148 | $1.3137264 |
| Corrected approval mapping | 30/30 | 87 | 1,290,472 | $1.2120544 |

Isolation observed 24.1% fewer total tokens and a 20.6% lower common reference estimate.
Configuration drift and model variability prevent attributing that exact difference to code.
The rejected prompt candidate's lower totals include an incomplete approval task and cannot
justify accepting it. In the focused 12 attempts, calls fell from 49 to 36 and total tokens
from 966,034 to 509,854 against original baseline. Against isolation-only on the same subset,
47 calls and 699,128 tokens fell to 36 and 509,854. That promising subset did not establish
full-suite reliability. Isolation recorded 14 state updates, 13 in steps without an adapter
or retrieval call; baseline recorded 19 and 16 respectively. These are diagnostic proxies.

The simulated correction passes all 22 workloads with the same call counts. Its extra
instructions increase mean simulated total tokens by 3.5% and the reference estimate by
1.2% against the original baseline. `approval-mapping-sim-comparison.md` retains the full
comparison. `bench/baseline.json` was explicitly regenerated for this instruction change;
that reset does not establish an efficiency gain. Live measurements and compatibility
checks decide acceptance.

The corrected condition passes 30/30, with 16.3% fewer calls, 35.9% fewer total tokens
and a 33.5% lower reference estimate observed against baseline. Those aggregate differences
are provisional because of configuration drift and model variability. The trace audit passes
all three attempts each for greeting restraint, once-only approval, fresh reads and restart
fact retention. State-update events fall from 19 to zero; this does not establish the right
state policy for a long live investigation.

Approvals requested are 10 at baseline and 11 in the corrected condition, including extra
analysis verification. Fewer calls do not imply fewer questions on every task. Stale-read
calls increase from 8 to 10, even while its total tokens fall from 146,095 to 135,241.

| Workload | Baseline calls | Corrected calls | Baseline tokens | Corrected tokens |
|---|---:|---:|---:|---:|
| simple | 3 | 3 | 51,362 | 39,137 |
| tool-lookup | 6 | 6 | 104,647 | 85,282 |
| multi-step | 15 | 12 | 288,825 | 193,976 |
| large-output | 12 | 9 | 241,512 | 130,606 |
| failure-recovery | 9 | 9 | 178,696 | 131,565 |
| resume | 14 | 9 | 274,544 | 141,990 |
| approval | 7 | 6 | 122,409 | 85,101 |
| parallel | 8 | 6 | 161,153 | 80,244 |
| stale-read | 8 | 10 | 146,095 | 135,241 |
| analysis | 22 | 17 | 442,948 | 267,330 |

Exact-source `scripts/ci.sh` passes. The real writing app also passes a controlled task
using `codex:gpt-5.6-terra` with unforced effort: it reads the two synthetic chapters, reports
4 and 3 words and a total of 7, and preserves both file hashes. This validates the same
Codex host adapter in a consumer app, with a different effort-selection setup from the
matched benchmark. No release or public claim is authorized by this evidence.

## Acceptance and remaining work

| Check | Evidence |
|---|---|
| Exact-source full gate | `validation/accepted-ci.log`: format, protocol, version, clippy, workspace tests, release build and simulated regression gate pass |
| Focused safety and request capture | `validation/focused-safety.log`: 18/18 pass |
| Live checked outcomes | `approval-mapping-live.json`: 30/30, no reported provider errors or retries |
| Approval, fresh read, restart and greeting audit | `approval-mapping-audit.json`: 12 cases, no errors |
| Actual consumer app | `dogfood-events.jsonl`, `dogfood-result.json`, `dogfood-fixture.json`; two model calls, correct answer and unchanged manuscript |
| First gate failure retained | `validation/first-ci.log`: prompt-capture failed while the gate started before the footer edit. Final-source focused tests and full gate supersede it |
| Setup failures retained | Two benchmark catalog failures in `approval-mapping-setup.json`, two writer failures in `dogfood-preflight-1-*` and `dogfood-preflight-2-*`; each stopped before any task/model call |

Codex explicit-effort startup remains an open reliability issue. The read-only diagnosis
(`catalog-diagnosis.json`) measured initialization at 0.116 s, the usage-limit query at
5.004 s and model listing at 2.214 s. Their total, 7.334 s, exceeds the existing six-second
catalog wrapper. The target model and medium effort were advertised. The successful
unforced-effort consumer check does not fix or validate that constructor path. Track this
separately on `d1bb1e0c` before claiming startup reliability. No additional blind retries or
product edits were used to conceal it.

Independent QA reviewed the plan, source, totals, trace audit, configuration drift and
per-task tradeoffs. Its final review is saved in `validation/qa.md`. The scoped changes are
uncommitted in `ark-call-efficiency`, branch `codex/call-efficiency`; integration is tracked
on `06ad26dd`. The original host-model API-key evidence gate is separate and remains open.
No commit, merge, push, publication or release occurred. All own build output was cleaned
after checks, removing 3.9 GiB of debug output and 2.3 GiB of remaining output.


## What changed

| Gap | Change | Boundary |
|---|---|---|
| Codex host calls inherited tools that ARK does not execute | Disable native shell, apps, web search and configured MCP servers through process/thread overrides | User settings remain untouched. ACP workers retain their own execution behavior. Unsupported overlays select the existing isolated exec fallback |
| The default prompt encouraged separate updates that repeated completed results | Reserve state updates for findings, decisions or next steps useful to ongoing or later work | Keep useful durable state. Live restart and existing recovery checks remain required |
| The model sometimes asked permission in text and stopped | Tell it to propose a permitted action through the host approval gate | Plan, denial, missing gate and restricted Auto actions still refuse execution |
| Managed-session requests bypassed the benchmark recorder | Recover available usage/context metrics from runtime events, mark request coverage and reject unsupported recording/replay/faults | Exact raw request, prefix reuse and metadata counts remain unavailable |

## Launch technical story

The internal lesson from Reason's data is to measure the complete attempt and inspect what
each call achieves. ARK found avoidable tool context and separate bookkeeping steps in its
own traces. The patch removes those sources of overhead while keeping the host responsible
for execution and approval. A lower-token candidate that failed approval was rejected.

Use the observed result only with its model, sample size and configuration-drift caveat.
This is evidence for a narrow unreleased engineering change. It does not establish a Reason
comparison, a general cost guarantee, market demand, launch readiness or a latency advantage.
No announcement, publication, purchase, commit or release is part of this evaluation.


To reproduce the consumer check, copy `dogfood-manuscript` to a temporary manuscript folder
and choose a separate temporary store. Update those two paths in `dogfood-fixture.json`,
then run `python3 dogfood.py /path/to/writer gpt-5.6-terra`. The original temporary store
and all copied executables were removed after validation. Their output and binary hashes
remain in the evidence files. The successful check deliberately leaves effort unforced.
