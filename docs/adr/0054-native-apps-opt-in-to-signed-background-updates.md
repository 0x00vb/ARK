# 0054. Native apps opt in to signed background updates

Status: Accepted
Date: 2026-10-08

## Context

`ark-ui` owns the Tauri builders for both a local window and a window that opens directly on
another runtime. A consumer cannot add the official Tauri updater in both paths without copying
the window shell or opening a general builder hook. A custom downloader would duplicate package
replacement and signature checks that Tauri already maintains.

An update must not interrupt an agent or a schedule. ARK already lets work reach a safe point
when the app quits, but an automatic restart could still arrive while a person or agent is in
the middle of work.

## Decision

`ark-ui` has an opt-in `desktop-updater` feature. It registers Tauri's updater in both native
window paths, checks after startup, and downloads and installs a signed update in the
background on macOS and Linux. It never restarts the app. The new version opens after the person
next quits and reopens it. Updater 2.12 or later is used with signed-version verification, HTTPS
transport and the normal no-downgrade version comparison. Windows stays disabled because the
Tauri updater must exit the running process to launch its installer; it needs a separate design
before ARK releases a Windows updater.

The own page gets three native commands for state, a manual check and state events. The
commands use the same origin guard as the rest of the window IPC. `transport.js` exposes them
as `window.ark.updates`; it is absent in a browser and reports no updater in development. They
are not runtime protocol calls. A page on another runtime cannot update this app. JavaScript
cannot supply a URL, request header, version or downgrade flag, and gets no updater plugin
permission.

The consumer owns the release contract:

| Concern | Owner |
|---|---|
| HTTPS endpoint and public key | the consumer's Tauri config |
| Private key and recovery copy | the consumer's release operator, outside Git |
| Versioned package, signature and manifest | the consumer's release process |
| Update status in its own Settings | the consumer, through `window.ark.updates` |
| Package replacement and signature verification | Tauri's updater plugin |

The release publishes immutable versioned packages first and the mutable manifest last. A bad
network response, malformed manifest or invalid signature leaves the running app unchanged.
Only platforms with a published package appear in the manifest.

## Consequences

Browser, server and Windows builds do not run the updater. A consumer must ship one
updater-capable version through its old download path before later versions can arrive
automatically. Losing the private key strands those installed versions, so the encrypted key
needs a separate recovery copy.

## Check

Run `cargo test -p ark-ui --features desktop-updater updates::tests` and `scripts/ci.sh`. A
consumer proves the release path by installing its first updater-capable version manually,
then updating it to one staged version through the published endpoint without a forced restart.
