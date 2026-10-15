# Codex catalog startup

Board: `c3ed5945`. Worktree: `ark-codex-catalog`, branch `codex/catalog-startup`.

## Evidence and boundary

The preceding evaluation recorded two explicit-effort writer startup failures before any
model call. A read-only RPC diagnosis measured initialize at 0.116 s, optional account
usage at 5.004 s and model listing at 2.214 s. Total 7.334 s exceeds the existing six-second
catalog deadline. The model and requested medium effort were advertised.

The completed `ark-call-efficiency` tree remains separate. This tree starts at `11eee32`
with that evaluation's reviewed patch. `baseline-manifest.json` and `baseline/efficiency.patch`
preserve its identity; the two baseline model files distinguish the startup-only change.
No new workload matrix or cost comparison is part of this follow-up.

## Reviewed plan

Keep a single process/handshake implementation. Catalog discovery performs initialize,
initialized and paged model listing, without optional account telemetry or execution calls.
Ordinary sessions retain their existing account-usage query and recording. Keep the existing
six-second catalog timeout, advertised-model/effort validation and process-tree cancellation.
No public API, permissions, provider fallback or user configuration changes.

Independent plan/design QA passed. Tests come before the behavioral patch. Deterministic
fixtures cover a stalled optional query, call order, errors, timeout and cancellation,
including descendant cleanup. A child-isolated ordinary-start test checks recorded usage
without racing the process-global account snapshot. Existing tests retain effort rejection.

## Acceptance

- Focused fixtures fail on the original behavior, then pass with the narrow patch.
- Full required exact-source gate, only after the current dictation gate releases resources.
- Simulated metrics remain unchanged against the inherited baseline.
- One signed-in synthetic writing-app task on `codex:gpt-5.6-terra@medium`, with explicit
  effort, correct word counts and unchanged manuscript. Retain any failed attempts.
- Independent final QA, docs/state and board updated. No commit, merge, push or release.

Final model tests pass 99/99. The confirmed old behavior passed five catalog
checks and failed the stalled optional-usage regression. Initial fixture waits were too short
for fresh shell startup under parallel vendor tests; those failures remain in `validation/`.
Final fixtures use production's six-second contract, an eight-second watchdog and serialize
only the six new process fixtures. No executable warmup or production deadline increase.
The exact-source dev writing-app binary passes one signed-in synthetic task on explicit
`codex:gpt-5.6-terra@medium`, with two model calls, 4 and 3 words, total 7, no save and
unchanged hashes. Both raw model-call events name the requested model and effort. This is
an app behavior check, not a release-binary timing measurement. The exact-source full gate passes, including release compilation and the simulated gate.
All 20 reported non-timing metrics remain unchanged across the 22 simulated workloads.
No new baseline or live matrix was substituted.


The startup-only patch applies cleanly to the frozen validated efficiency source (`git apply
--check` against the parent worktree). It contains the two model files and their fixtures,
without rebasing or modifying the preceding evaluation. Ordinary-start usage is tested in
a child process because account snapshots are process-global. No user settings or credentials
are written by the patch or fixtures. Required catalog RPCs can still fail or time out; this
change removes optional telemetry from that deadline, rather than promising network availability.


## Final evidence

| Acceptance | Evidence |
|---|---|
| Reproduced old behavior | `validation/confirmed-red.log`: five pass, one expected stalled-usage failure before the startup split |
| Complete model suite | `validation/model-tests.log`: 99/99 pass; the workspace gate repeats these successfully |
| Exact-source full gate | `validation/full-ci.log`: format, protocol, version, clippy, workspace tests, release build and simulated gate pass |
| Model-request behavior unchanged | `validation/simulated-metrics.json`: all 20 reported non-timing metrics unchanged; 22/22 checked simulated outcomes |
| Exact-effort actual app | `dogfood-result.json`, `dogfood-events.jsonl`: explicit Terra@medium, two model calls, correct 4+3=7 words, no save, hashes unchanged |
| Independent review | Plan/design and pre-gate implementation pass; final verdict in `validation/qa.md` |
| Cleanup | `validation/clean-debug.log`, `validation/clean-final.log`: 3.8 GiB debug and 1.9 GiB remaining build output removed |

The consumer check used the dev binary produced by the frozen full test build. The release
build subsequently passed; no second signed-in task was needed. This validates the previously
failing explicit-effort path for the recorded setup. Required handshake/model-list failures
still reject an unverified choice within the same six-second deadline. No general availability
or latency guarantee is claimed. Ordinary-session usage collection remains unchanged.

`startup.patch` is the source/test delta after `baseline/efficiency.patch`. `startup-docs.patch`
is the matching documentation delta after the same frozen efficiency patch. Both are applied
after `baseline/efficiency.patch`, whose identity is recorded separately. The model source
hashes distinguish this correction from the inherited efficiency change.
Integration remains on `35c94426`; there is no commit, merge, push, release or publication.
The earlier benchmark results and their configuration-drift caveat remain unchanged.


Independent final QA passed. Its stale-artifact-reference nit was corrected before closing
the card; `startup.patch` and `startup-docs.patch` are the actual integration artifacts.
The temporary manuscript/store and copied dev executable were removed after validation.
The retained `dogfood-manuscript` snapshot and fixture hashes reproduce the synthetic input.
For a rerun, copy it to a new temporary folder, set fresh manuscript/store paths in
`dogfood-fixture.json` and run `python3 dogfood.py /path/to/writer`. The default model in
that script is explicitly `gpt-5.6-terra@medium`.
