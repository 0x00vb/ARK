# 0071. Advanced files use one viewer seam

Status: Accepted. A1 to A3 are implemented and technically verified. CAD integration and IFC remain follow-up work.
Date: 2026-10-15

## Context

ARK shows text, sheets, documents, PDFs and images from the project file that the person and
agent share. Apps can add a lens over a stored artifact, but not a view over a live project file.
Maps, 3D models, CAD and BIM would otherwise create one-off panels, duplicate file transport and
add domain modes to the interface. Binary model files can also exceed the current 25 MB base64
media reply.

## Decision

Add one file-viewer extension selected by the kind that `ark-files` decides. The host copies
one authorized file handle into an immutable, hashed, expiring
snapshot and verifies a second pass plus source identity and change metadata before it returns a
token. An unstable copy is retried, then refused. Bounded chunks come only from the snapshot. The
viewer reports a bounded visible selection through `Focus`. It does not receive a filesystem path
or mutation authority. Registered viewer code is trusted page code; untrusted file parsing runs
in a worker or stronger sandbox under decoded-resource and network limits.

Implementation review on 2026-10-15 removed the proposed global path lease. Typed ARK writes
and Restore already replace atomically. Run code writes directly, so double-pass verification
also handles it. A Host-owned request registry provides immediate begin, progress, cancellation,
quota reservations and expiry. Blocking copies retain independent worker permits after cancellation
until they actually exit. A metadata probe detects external changes without hashing on every chunk.

Consumers may register trusted shipped HTML with `ArkUi::frame_asset`. Only a successful HTTP
GET of that exact asset receives `frame-ancestors 'self'` instead of `frame-ancestors 'none'`.
The current versioned asset prefix resolves by the same rules as other assets. Root pages,
ordinary assets, errors and path lookalikes retain the existing policy. Each response contains
one complete CSP. The consumer's frame also denies network connections in its own CSP.
This enables trusted renderer modules; it is not an authority sandbox for untrusted code.

After a page interruption, a bounded session-storage bookmark restores the last map/model tab
with one Retry. It stores only a version, file name and runtime/project/chat route. No file
contents, snapshot tokens or authority persist. A route mismatch discards it. Explicit close
clears it. Retry reads through the normal routed file API and obtains a new snapshot.

Prove the seam with two different kinds: an offline GeoJSON map and a GLB/STL 3D model. CAD
generation stays in a consuming app's contained runtime and writes standard files. IFC becomes a
later lens on the model kind. No profession, renderer or CAD package enters ARK Core.

## Consequences

- A person keeps one model: ask in chat, inspect the file beside it, continue from a selection.
- New viewers do not add navigation, a product mode, a tool system or a permission system.
- Files remain the source of truth. A renderer failure never hides or replaces the file.
- Binary transport becomes additive and chunked. Existing `file_media` and released clients keep
  their behavior.
- Renderers must ship offline, deny external resources, bound decoded work and dispose their
  memory. A connected basemap is a separate later capability.
- Editing a new format is not implied. It needs typed edits, fidelity tests and a real user.
- A consumer may be larger because it bundles renderers. Core users that do not register them do
  not pay that cost.

## Check

### IFC extension, 2026-10-15

The owner deferred the A5 architect/BIM observation gate explicitly. This permits implementation,
not a professional validation claim. `ark-files` classifies IFC as a read-only model with a
STEP header check and a 2 MiB source limit enforced before snapshot copying. It does not parse IFC.
`ViewOnly::max_bytes()` exposes source limits without adding a renderer to Core.
`has_text_source()` permits bounded read-only text recovery for opaque textual models. The
page exposes Source from metadata, without guessing a format. It never offers Save on that
source, and file writes remain refused by the host.

`ArkUi::wasm_worker_asset` registers one confined JavaScript worker entry. Only successful HTTP
GETs for that exact asset, including its current versioned path, receive `wasm-unsafe-eval`.
The worker policy denies connections. Same-origin worker imports remain permitted for WebKit.
The consumer disables nested worker creation before parsing. Root pages, siblings, errors and lookalikes
keep their original policy. JavaScript eval remains forbidden. Development and released asset
roots retain this exact registration. Only the HTTP serve path sends this policy. A native
window serves assets through the app's own protocol and CSP, which this method does not change.
Desktop ships the parser and supplies verified WASM bytes
after the person chooses Open IFC model. The worker never fetches bytes. No format-specific
parser, model tree or architecture vocabulary enters Core.

The file viewer's `api.confirm(label)` displays one text-only label of up to 80 characters in
the host-owned opening area beside Cancel. It resolves only after the person clicks. Abort,
replacement and generation checks prevent late clicks from starting stale work. It is local
viewer interaction, not an agent action, saved grant or filesystem authority. Consumers use it
before loading an optional parser. The pending replacement remains hidden until fully validated;
an existing good view is not overwritten by an unparsed preview.

The plan and acceptance gates are in `docs/ADVANCED_ARTIFACTS.md`. Tests prove
local and remote resource routing, no mixed-version snapshot, stale-version cancellation, no
renderer network request, hostile input and Focus bounds, keyboard and small-window recovery,
page and worker crash recovery, and cleanup after a viewer closes.
