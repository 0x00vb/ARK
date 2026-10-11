# 0057. An external client uses a versioned contract

Status: Accepted
Date: 2026-10-10

## Context

ARK's page, window and Rust client call `Host::call` with untyped JSON and follow one SSE
stream. Runtime-served pages stay compatible because the runtime serves their code. A native
iPhone app is the first separately released client. It cannot safely infer missing methods or
duplicate ARK's presentation words in Swift.

## Decision

ARK owns a domain-neutral external-client protocol in two new crates:

```text
ark-protocol  serde wire DTOs, schema and fixtures; no transport or product decisions
      ^                         ^
      |                         |
ark-interact              ark-client
shared meanings and       HTTP, SSE, resume and typed errors
transport-neutral port          ^
      ^                         |
      +-----------+-------------+
                  |
        ark-ui composition root
        and ark-cli
```

`ark-protocol` depends only on `serde` and schema support. `ark-interact` depends on it and maps
ARK's shared semantic state into the wire types. It defines a transport-neutral `RemoteRuntime`
port for calls and event streams. `ark-client` depends on `ark-protocol`, HTTP and SSE libraries,
never on `ark-interact`. The `ark-ui` composition root adapts `ark-client::Client` to
`RemoteRuntime` and injects it into `ark-interact::Links`. `ark-ui` serves the protocol using
meanings already computed by `ark-interact`; adapters never decide labels, state or behavior.

During migration, `ark-ui::remote::Client` may re-export `ark_client::Client` and keep its existing
constructors and methods. `ark-cli` moves to `ark-client` directly. The current
`ark-interact::remote::Client` moves out instead of becoming a reverse dependency. Its
`links.rs` caller accepts the injected transport-neutral port. Existing untyped calls remain
available in the upper transport layer while typed endpoints move one group at a time. There is
one wire model, never a second set in `ark-ui` or `ark-client`.

A small `/client/bootstrap` response is version-independent and additive forever. It lists the
protocol majors the runtime serves, each latest minor, the runtime version and an update URL.
After negotiation, `hello` publishes the selected major and minor, accepted client range, limits
and capabilities. Major changes may break compatibility. Minor changes only add optional fields,
events or capabilities. A client disables a missing capability.

Rust types generate checked-in JSON Schema and golden fixtures. A deterministic generator makes
Swift wire models from that released schema. Wording and display semantics remain in
`ark-interact` and cross the wire. ARK releases first; consumers pin an ARK protocol release.

Wire integers that may exceed JavaScript's exact range are decimal strings. Times are RFC 3339
UTC. Errors have stable codes, safe display text and optional retry data. Requests, pages, SSE
events and uploads publish bounds.

Every event carries a stable id and sequence. A client resumes from its last sequence. A retained
range replays in order. A gap emits `resync`; affected mutations stay disabled until fresh
snapshots arrive. Unknown fields are ignored. Unknown events keep the cursor and cause a snapshot
when they can affect visible state. Clients use bounded buffers and cancellation.

Capabilities belong to the runtime that owns the selected chat. Held-chat capability discovery
is routed to its destination. No action falls back to the origin when the destination is older.

Protocol v1 remains additive through the first native release. A later runtime that introduces a
new major must serve the current and prior major together. The prior major cannot retire until a
compatible iPhone build is approved and publicly available for at least 12 months. A delayed or
rejected App Store review therefore cannot make the shipping app unusable after a runtime update.
Release checks exercise the last approved app against the new runtime before that runtime ships.
Retirement is a separately documented breaking runtime release, never a minor update.

## Consequences

- ARK gains a public compatibility promise and schema release work.
- Swift, future clients and test fixtures share one contract instead of handwritten copies.
- The bundled page may adopt typed models incrementally but keeps its current transport.
- New phone-visible behavior must update types, schema, fixtures and compatibility tests together.

## Check

- Schema-drift CI regenerates into a temporary folder and compares it with the checked-in schema.
- Rust server conformance and Swift fixture decoding cover every public variant.
- Supported old-client/new-runtime and new-client/old-runtime pairs run in CI.
- Release tests include delayed and rejected iPhone review while the runtime serves both majors.
- Web, Desktop, CLI and iPhone parity tests compare chat, approval, result and error meaning.
