# Versioning & Release Strategy

ARK is one canonical repository published as several independently-consumable crates.
Consumers depend on them as ordinary Cargo dependencies and **never copy the source**; when
ARK improves, publish a new version and consumers update intentionally.

## Packages

`ark-core`, `ark-model`, `ark-store`, `ark-tools`, `ark-runtime`, `ark-sdk`. Most consumers
depend only on `ark-sdk` (which pulls the rest at matching versions). A consumer that wants a
narrower surface — say, just the model abstraction — can depend on `ark-model` alone.

During `0.x`, all crates move in lockstep on one version (the workspace version). At `1.0`,
crates may version independently; a crate's public API change then follows its own semver.

## Semantic versioning

Standard Cargo semver. In `0.x`, the minor version is the breaking-change axis (`0.1 → 0.2`
may break), and the patch version is additive/fixes. From `1.0`:

- **MAJOR** — a breaking change to a public API (a trait signature, a public struct field, an
  enum without `#[non_exhaustive]`, a builder method's meaning).
- **MINOR** — additive: new methods, new optional builder knobs, new `Other`/`non_exhaustive`
  variants, new crates.
- **PATCH** — bug fixes and internal changes with no API impact.

### What counts as "public API"

The re-exports from each crate's root and the `ark_sdk::prelude`. Internal modules may change
freely. Prompt-string *defaults* are behavior, not API: a materially different default persona
or method block is a MINOR change and noted in the changelog, because it changes agent output.

## Compatibility guarantees

- **Persisted data.** The store schema and the JSON shape of persisted domain types
  (`Invocation`, `EvidenceCard`, `ResearchState`, …) are treated as a compatibility surface:
  a newer ARK reads an older store. New fields are added with `#[serde(default)]`; fields are
  never removed or repurposed within a major version.
- **The event stream.** `HarnessEvent` / `AgentEvent` are `#[non_exhaustive]` in spirit —
  match with a `_ =>` arm; new variants are MINOR.
- **The core invariant.** `ark-core` will never gain a tool or domain name. This is a
  permanent guarantee, not a version policy.

## A safe upgrade path

1. Read [`CHANGELOG.md`](../CHANGELOG.md) for the target version.
2. Bump the range in your `Cargo.toml` (e.g. `ark-sdk = "0.2"`).
3. `cargo build` — the compiler catches API breaks; the changelog explains each.
4. Run your agent's own test suite (drive the loops with `ark_model::MockProvider`, no live
   model needed).
5. Compare token/metrics on a representative task against your baseline if the release touched
   the runtime (see the roadmap's benchmark harness).

A harness update **must not silently break every application**: breaking changes are gated
behind a version bump and documented, and defaults change only on a MINOR+ with a changelog
note.

## Release process (maintainers)

1. Update `CHANGELOG.md` (Keep-a-Changelog format).
2. Bump `workspace.package.version` in the root `Cargo.toml`.
3. `cargo test` (all suites green) and `cargo publish` each crate in dependency order:
   core → model, store, tools → runtime → sdk.
4. Tag `vX.Y.Z` and push.
