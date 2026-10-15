# 0071. Advanced files use one viewer seam

Status: Proposed
Date: 2026-10-15

## Context

ARK shows text, sheets, documents, PDFs and images from the project file that the person and
agent share. Apps can add a lens over a stored artifact, but not a view over a live project file.
Maps, 3D models, CAD and BIM would otherwise create one-off panels, duplicate file transport and
add domain modes to the interface. Binary model files can also exceed the current 25 MB base64
media reply.

## Decision

Add one file-viewer extension selected by the kind that `ark-files` decides. Under a per-path
read lease, the host copies one authorized file handle into an immutable, hashed, expiring
snapshot and verifies a second pass plus source identity and change metadata before it returns a
token. An unstable copy is retried, then refused. Bounded chunks come only from the snapshot. The
viewer reports a bounded visible selection through `Focus`. It does not receive a filesystem path
or mutation authority. Registered viewer code is trusted page code; untrusted file parsing runs
in a worker or stronger sandbox under decoded-resource and network limits.

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

The plan and acceptance gates are in `docs/ADVANCED_ARTIFACTS.md`. Before acceptance, tests prove
local and remote resource routing, no mixed-version snapshot, stale-version cancellation, no
renderer network request, hostile input and Focus bounds, keyboard and small-window recovery,
page and worker crash recovery, and cleanup after a viewer closes.
