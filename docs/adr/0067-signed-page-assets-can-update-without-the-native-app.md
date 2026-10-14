# 0067. Signed page assets can update without the native app

Status: Accepted
Date: 2026-10-14

## Context

ARK's page is plain JavaScript and CSS, but a native consumer embeds it in the executable.
A one-line page fix therefore pays for Rust checks, two platform builds, Apple signing,
notarization and a full application download. Local development already serves both source trees
from disk and reloads after a save. The missing boundary is a safe release path for the same
unprivileged page code.

The page has the authority of the installed app's existing `window.ark` contract. A page update
must not add a native command, permission, dependency or protocol shape. A broken signed page
must not strand the app.

## Decision

`ark-ui` may load a consumer-supplied, signed page bundle in a local native macOS window. The
installed app supplies the endpoint, an asset-only public key, exact compiled ARK and consumer
versions, the asset contract and a private cache root. Browser, `--serve`, a remote runtime page,
Linux and Windows do not use this path in the first release.

The signed canonical manifest contains its schema and layer, an increasing mainline sequence,
both source identities, the exact native versions and asset contract, the immutable HTTPS bundle
URL, SHA-256, byte size and file count. The signing key is not the native updater key. Its private
copy lives outside Git and has its own recovery copy. A sequence below the stored high-water mark
is refused.

The archive accepts at most 512 regular files, 32 MiB compressed, 64 MiB extracted and 8 MiB per
file. Paths are ASCII relative paths below `web/` or `consumer/`. Absolute paths, `..`, links,
duplicates and unknown roots are refused. Download and verification happen in a private temporary
directory on the cache filesystem. Activation is an atomic rename. The embedded page and one
last-known-good bundle remain available.

An update is staged while the current page keeps running. The person may apply it from Updates,
which reloads the local page but not the runtime process. A normal later local-page load also uses
it. Drafts and scroll state already survive a reload. Startup must complete the shared page and
required extension handshake within seven seconds and report ready to the native shell. Otherwise
the shell restores the last-known-good or embedded page and reloads once.

An ARK asset release still comes from an immutable ARK tag. A later tag is compatible with an
installed ARK version only when a mechanical classifier proves the diff contains page assets,
their tests and release bookkeeping only. Version changes in the root manifest and lockfile are
allowed only for every local workspace member and must change uniformly. Rust, dependencies,
features, protocols, build scripts and unknown paths require a native release.

## Consequences

- UI and bounded client-side logic can be repaired without compiling or downloading the app.
- The first usable version still needs one native bootstrap release.
- Correctly signed page code remains trusted code. The separate key limits compromise to the
  installed app's existing page authority.
- Automatic publishing waits for a protected runner. Until then the release command signs and
  publishes locally after the repository checks pass.

## Check

Unit tests cover signature and digest failure, exact compatibility, rollback, archive limits,
unsafe paths, atomic activation and last-known-good recovery. A native dogfood stages a bundle,
applies it without replacing or restarting the app, then proves a broken bundle rolls back.
