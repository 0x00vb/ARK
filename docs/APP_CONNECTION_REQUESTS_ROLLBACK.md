# Contextual connection rollback fixture

Status: both external-root Interaction fixture phases, exact R3 reads of all three database
boundaries, and new-reader recovery of all three returned roots passed. Focused SDK 13/13,
connection HTTP 7/7 and serializer 1/1 checks passed. Two later reviewed Host lock-order and
idempotent retry corrections pass the unchanged broader native-client HTTP test. Connection
HTTP 7/7 has not been rerun after those corrections. The full repository gate remains pending.
The later optional metadata retry payload passed focused new-reader tests but needs fresh
exact-R3 read-back.
The retained proofs below predate that payload.

## Encoding

| New state | Persisted representation | R3 behavior |
|---|---|---|
| Acquisition not performed | Existing `pending_approval` invocation status, optional `connection_request` ID | Reads the known status. Ignores the new field. No recovery dispatch. |
| Acquisition completed | Existing `ok` status | Reads the known completed call. |
| Waiting event | Existing `state_update` event, kind `connection_waiting` | Reads the known event. |
| Durable requirement and turn owner | New opaque record kind and optional turn field | Ignores unsupported data. |

Every `Timed` event serialization normalizes the waiting event. This includes running Journal
snapshots, not only final traces. Live events remain typed in the new process.

New Core does not infer authority from `pending_approval`. It requires the request ID on the
invocation, the matching durable request and work, the exact current turn marker, and the
phase's expected invocation status. A missing or mismatched marker closes the orphan request
by CAS. It does not restore a marker discarded by an old writer. An `ok` or `failed` invocation
cannot authorize a waiting requirement.

## Old to new to old fixture

The consumer owner first starts exact R3 in an isolated disposable volume, creates a dummy
fixture credential and one empty chat without inference, then stops R3. Copy that entire
closed store into two independent disposable directories. Preserve an immutable seed copy
and its evidence. This is the old stage. A new-created store is not equivalent evidence.

Coordinate the exclusive compiler window first. Use the allocated Cargo environment and
existing target directory. Set `ARK_ROLLBACK_FIXTURE_ROOT` to one exact disposable copied
store root and `ARK_ROLLBACK_SESSION_ID` to the R3-created empty chat ID. Then run:

```sh
ARK_ROLLBACK_PHASE=paused cargo test --locked -p ark-ui --no-default-features --test connection_requests rollback_external_old_root_preserves_seed_and_exports_real_journal -- --nocapture
```

The supplied exact R3 seed is `/tmp/ark-contextual-old-seed-20261026/store`. Never open that
original through the new runtime. Its already-created independent copies are `/paused` and
`/claimed` under the same parent. The selected empty session ID is:
`client-6aabf1f3cd69141ac057aa2da4f2285873e86af0f9d365e011d80929a3b11c72`.
Session IDs are strings, not restricted UUIDs. Set the exact environment for the paused copy:

```sh
export ARK_ROLLBACK_FIXTURE_ROOT=/tmp/ark-contextual-old-seed-20261026/paused
export ARK_ROLLBACK_SESSION_ID=client-6aabf1f3cd69141ac057aa2da4f2285873e86af0f9d365e011d80929a3b11c72
```

For the second run change only the root to `/tmp/ark-contextual-old-seed-20261026/claimed`
and the phase to `claimed`. Use the allocated compiler guard and Cargo target settings.
The read-only preflight confirmed both copies match the original `meta.db`, `access.json`
and `secrets.enc` hashes recorded in `seed-proof.json`. Credential contents were not printed.

Both supplied-copy runs passed on 2026-10-26. Paused request:
`8c697d77-29bb-45b4-8cb4-9ae4134b7a54`; claimed request:
`0bfd2336-ebec-4d32-8f57-6ef7df5859d7`. Logs are
`/tmp/ark-app-requests-external-paused.log` and
`/tmp/ark-app-requests-external-claimed.log`. Each output root contains its standalone
database snapshots and `rollback-baseline-proof.json`. Do not rerun against these now-used
chats. Their original immutable `/store` seed remains available for independent comparison.

Use the second untouched R3 seed copy with `ARK_ROLLBACK_PHASE=claimed`. Never run the two
phases sequentially against the same already-used chat. The test refuses a non-empty chat.
With no external-root variables, its normal test creates its own temporary empty seed. That
mode checks the harness but does not count as old-to-new-to-old evidence.

The Interaction runs one mock-provider prerequisite call in the existing chat. Its actual
Journal write is captured inside the typed waiting callback, before finalization. The test
asserts both live Journal and final stored trace contain legacy-readable StateUpdate events.
At that callback a read-only SQLite connection executes `VACUUM INTO` to create a standalone,
transaction-consistent database snapshot. It is not a JSON reconstruction or a later final
database with a substituted event. The snapshot destination must not already exist.
No compatibility-shaped trace JSON is written manually. The claimed phase uses the real
runtime broker-claim transition, then intentionally fails the metadata adapter before any
completion, retaining the durable claimed boundary. No external provider operation occurs.

The test captures every baseline SQL table row before opening new Core for an external seed.
It excludes only the target session header row, which must change for the requested turn.
Every other baseline row must remain byte-for-byte equal in its typed column encoding after
the test. For the target header, every original JSON field outside the explicit turn usage,
title, carry and update fields must remain equal. Credential file SHA-256 values must remain
equal. Counts are additional checks, not the preservation proof.

`rollback-baseline-proof.json` records the canonical sorted baseline rows, their SHA-256,
the unchanged credential hashes, and the exact allowed target session. The row encoding tags
NULL, decimal integer, real IEEE bits, text bytes and blob bytes. It does not normalize JSON
stored inside a text column. An external verifier can independently check every recorded row
still exists in each returned SQLite snapshot and recompute the canonical baseline hash.

The test adds one original person turn and one invocation, closes all store handles, and
prints one `ROLLBACK_FIXTURE` line with the
exact root, session ID, request ID, `realJournal=true`, and `externalOldSeed=true`. Fixture
credentials come only from the isolated old seed. No live grant or provider credential is
used. The mock provider performs no network request.

Read these three snapshots independently with exact R3:

| Snapshot file | Actual capture boundary |
|---|---|
| `rollback-live-journal.db` | Live Journal persisted the waiting event. The pause transaction has not yet materialized the counted waiting turn/request. |
| `rollback-paused-final.db` | Counted waiting turn and request committed. Final trace saved. No metadata claim. |
| `rollback-claimed.db` | Runtime committed the immutable metadata claim. Adapter deliberately stopped before completion. Only present for the claimed phase. |

For each snapshot, import the whole closed retained root into a different fresh disposable
Linux volume. Use the selected standalone snapshot as that volume's `/data/store/meta.db`.
Do not include a `meta.db-wal` or `meta.db-shm` belonging to another database version. Preserve
all original credential and object files and the immutable retained fixture. Run each R3
read against a separate copy because R3 recovery may legitimately update its disposable copy.
Do not mount a production volume or rewind a live store.

## Exact old image read

Exact R3 image:
`sha256:5d92a807b674f4b0d09bf264112ee411980975a0333fdaec8edb853c923a9c05`.

Exported Linux binary SHA-256:
`425b71afca05606f286f7d64ea65df4f15fc1e46307f223100a590b9ad65ba65`.

Provenance is in the workspace evidence folder's `r3-binary-provenance.json`. The inherited
source manifest is an audit reference, not an attestation of the deployed image.

The consumer owner completed the confined read using `contextual-rollback-old-reader.py` in
the workspace evidence folder. Do not run the older `runtime-rollback-smoke.py` unchanged:
it creates chats with POST and uses a different old/candidate image pair. The read procedure is:

1. Use a fresh fixture volume and container name per retained store. Refuse existing names.
2. Set `OLD` to the exact R3 image above. Import only the disposable fixture.
3. Preserve the original R3 fixture-only token. Verify it still authenticates after the new
   stage. Do not substitute a new credential when checking continuity.
4. Run R3 with `--serve 0.0.0.0:8420 --tokens` under `--network none`, read-only container
   filesystem, `--cap-drop ALL`, and the reviewed CPU, memory, PID and tmpfs bounds.
5. Set only fixture `HOME=/data/home`, `ARK_DESKTOP_HOME=/data/store`, and generated
   `ARK_SECRETS_KEY`. Pass no model/provider/deployment environment.
6. From the reviewed Node helper in the container network namespace, issue only authenticated
   GET `/client/v1/projects`, `/client/v1/chats`, and `/client/v1/chats/{fixtureSessionId}`.
   Require HTTP 200 and exactly one original person message with its original text.
7. Stop R3. Inspect the disposable SQLite data. At paused-final and claimed boundaries require
   the same invocation ID, status `pending_approval`, no `finished_at`, no effect, unchanged
   invocation count, and no added person message. At live-Journal boundary the pending
   invocation/request did not exist yet, so require no added invocation or dispatch; R3 may
   recover the begun placeholder as interrupted. Independently compare preserved baseline
   rows and credential hashes with `rollback-baseline-proof.json`. Keep logs, selected snapshot
   SHA-256, database comparisons and JSON responses for all three stages.
8. Reopen another disposable copy with new Core. Verify the owned request remains usable when
   all additive markers survived. Separately remove or mismatch the turn marker and verify
   the orphan closes without a claim, metadata dispatch, or user message.

Never call send, start, resume, authorization, or mutation endpoints in the R3 stage. Never
execute the Linux binary natively on macOS. The owner runs the confined Linux image only
after reviewing the adapted command.

## Completed exact-image evidence

The workspace evidence folder contains `contextual-r3-reader-paused.json`,
`contextual-r3-reader-claimed.json`, and `contextual-r3-reader-live-journal.json`. Each records
the exact image above, acceptance of the original credential, successful conversation and
typed trace reads, and preservation of baseline rows except the explicitly allowed interrupted
turn normalization. Paused-final and claimed reads also parsed the pending invocation through
the typed call path. Their invocation remained `pending_approval`, unfinished and without an
effect. The live-Journal boundary had no persisted invocation yet. R3 normalized its begun
placeholder as interrupted without dispatch. No production store was modified or rewound.

### New reader after exact R3

Use only a disposable root returned by the exact R3 read stage, not the original seed or a
live store. Coordinate the compiler window and set:

- `ARK_ROLLBACK_RETURNED_ROOT`: exact returned disposable store directory.
- `ARK_ROLLBACK_RETURNED_SESSION_ID`: the original selected `client-...` session ID above.
- `ARK_ROLLBACK_RETURNED_REQUEST_ID`: the request ID belonging to that snapshot. Paused-final
  uses `8c697d77-29bb-45b4-8cb4-9ae4134b7a54`; claimed uses
  `0bfd2336-ebec-4d32-8f57-6ef7df5859d7`.

Then run with the allocated target and compiler settings:

```sh
cargo test --locked -p ark-sdk --test connection_requests rollback_returned_root_recovery_preserves_owned_or_retires_orphan -- --nocapture
```

This check does not require an empty chat and does not run a user request. It constructs an
Engine over the returned Store with an empty registry. It does not load plugins or configure
a provider or broker. It calls only synchronous contextual-request recovery. Matching owned
markers must preserve request phase and revision. Missing or mismatched markers must close
the orphan as replaced. It compares the exact before/after session header, turn rows, input,
trace and invocation rows, and requires no external-effect metadata.

`rollback-new-reader-proof.json` records the outcome. No refresh, claim, continuation, model
call, adapter dispatch, user message or credential mutation is part of this reader check.
For the live-Journal boundary, set `ARK_ROLLBACK_RETURNED_REQUEST_ID` to an empty string.
That mode requires no durable request or invocation and compares the old-recovered header,
turns and trace without further changes. A normal test run without these environment variables
checks a synthetic waiting root only; it is not returned-R3 evidence.

All three actual returned-root checks passed on 2026-10-26. Fresh disposable copies are under
`/tmp/ark-contextual-new-readers-Xudtuz/{paused-final,claimed,live-journal}`. Each contains
`rollback-new-reader-proof.json`. Paused-final and claimed retained their owned markers and
request phases. All three checks preserved the original input, trace and invocation rows,
with no provider, broker or adapter configured. Logs are
`/tmp/ark-app-requests-returned-paused.log`, `/tmp/ark-app-requests-returned-claimed.log`, and
`/tmp/ark-app-requests-returned-live-journal.log`. These checks did not refresh or resume work.

## Evidence limits

The focused SDK tests exercise missing/mismatched turn and invocation markers, wrong terminal
invocation statuses, and the paused/claimed storage shapes. The trace test checks full,
summary, and Journal-equivalent serialization. These tests are not proof that the exact R3
image read the stores. The completed exact-image evidence above includes separate typed trace
and call reads, not only conversation GET. It covers these fixture boundaries, not every old
rendering path or arbitrary future persistence shape. The full repository gate remains pending;
the retained rollback proofs do not certify later changes by themselves. The current required
debug prefix has since passed; release build and benchmark remain pending.

## Stopped unfinalized boundary

The stopped fixture exports `rollback-stopped-unfinalized.db` from a fresh, separately approved
producer root. It preserves Closed/cancelled request JSON and the existing Started turn marker,
with the outer pid cleared. It adds no persistence tag. Existing immutable proof roots are not
inputs or destinations for this check. The export and exact-R3 round trip remain pending.

The exact-R3 comparison may permit one genuine `runtime/self` creation only if absent. Every
pre-existing row and both credential files must remain unchanged. This is not permission to
ignore an arbitrary changed table, invocation, trace or request.

The stopped-specific SDK return reader requires all three environment inputs together:
`ARK_STOPPED_ROLLBACK_RETURNED_ROOT`, `ARK_STOPPED_ROLLBACK_RETURNED_SESSION_ID` and
`ARK_STOPPED_ROLLBACK_RETURNED_REQUEST_ID`. The root must be a fresh disposable copy returned
by the old reader. The test configures no provider, broker or adapters. It rejects a pre-existing
proof before opening the Store and requires both credential files on the external branch.

Recovery requires the exact Closed/cancelled request, session/project/work/count, existing
Started marker and dead writer. Header JSON and request JSON remain unchanged. The sole turn
changes only by removing its connection marker; input, metrics, citations, trace, invocations
and credentials remain unchanged. The Store may refresh its SQL session bookkeeping timestamp.
Admission is refused and repeated reopen is idempotent. Missing or changed markers cannot
authorize a late finalizer.

The SDK proof labels its full content identifiers as BLAKE3. Credential bytes are compared
privately in memory. The external artifact wrapper records SHA-256 independently for source
manifests, exact executables and the immutable database. No scope ref or credential is printed.
