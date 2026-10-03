# 0006. A release is an immutable git tag; consumers pin it

Status: Accepted
Date: recorded 2026-10-03

## Context
ARK is not on crates.io. Consumers (SecRes, ark-desktop) build against git. Moving a tag or
following `main` would change their builds under them.

## Decision
A release is an annotated git tag pushed to both remotes (`origin`, `github`). Tags are never
moved, deleted or re-created; a fix after a tag is a new version. Consumers pin a tag, never
`main`, and never commit a `path =` dependency on ARK. In `0.x`, breaking a public API needs a
minor bump and a `Breaking` changelog entry.

## Consequences
- Consumers upgrade on purpose, following `docs/RELEASING.md`.
- Unreleased ARK work reaches a consumer only through a gitignored local `[patch]`.

## Check
`scripts/verify-release.sh vX.Y.Z` from a fresh clone; `docs/RELEASING.md`.
