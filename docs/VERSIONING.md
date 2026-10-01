# Versioning & Release Strategy

ARK is one canonical repository published as several independently-consumable crates.
Consumers depend on them as ordinary Cargo dependencies and **never copy the source**; when
ARK improves, publish a new version and consumers update intentionally.

## Packages

`ark-core`, `ark-model`, `ark-store`, `ark-tools`, `ark-runtime`, `ark-sdk`, and the
front-end crates `ark-interact`, `ark-cli`, `ark-ui`. Most consumers depend only on `ark-sdk`
(which pulls the rest at matching versions), plus a front-end if they want one. `ark-ui`'s
Tauri window is behind its default `desktop` feature; `default-features = false` keeps only
the browser server. A consumer that wants a
narrower surface (say, just the model abstraction) can depend on `ark-model` alone.

During `0.x`, all crates move in lockstep on one version (the workspace version). At `1.0`,
crates may version independently; a crate's public API change then follows its own semver.

## Semantic versioning

Standard Cargo semver. In `0.x`, the minor version is the breaking-change axis (`0.1 → 0.2`
may break), and the patch version is additive/fixes. From `1.0`:

- **MAJOR**: a breaking change to a public API (a trait signature, a public struct field, an
  enum without `#[non_exhaustive]`, a builder method's meaning).
- **MINOR**: additive: new methods, new optional builder knobs, new `Other`/`non_exhaustive`
  variants, new crates.
- **PATCH**: bug fixes and internal changes with no API impact.

### What counts as "public API"

The re-exports from each crate's root and the `ark_sdk::prelude`. Internal modules may change
freely. Prompt-string *defaults* are behavior, not API: a materially different default persona
or method block is a MINOR change and noted in the changelog, because it changes agent output.

## Compatibility guarantees

- **Persisted data.** The store schema and the JSON shape of persisted domain types
  (`Invocation`, `EvidenceCard`, `ResearchState`, …) are treated as a compatibility surface:
  a newer ARK reads an older store. New fields are added with `#[serde(default)]`; fields are
  never removed or repurposed within a major version.
- **The event stream.** `HarnessEvent` / `AgentEvent` are `#[non_exhaustive]` in spirit:
  match with a `_ =>` arm; new variants are MINOR.
- **The core invariant.** `ark-core` will never gain a tool or domain name. This is a
  permanent guarantee, not a version policy.

## A safe upgrade path

1. Read [`CHANGELOG.md`](../CHANGELOG.md) for the target version.
2. Change the tag in your `Cargo.toml` (`ark-sdk = { git = "...", tag = "v0.4.0" }`) and run `cargo update -p ark-sdk`.
3. `cargo build`: the compiler catches API breaks; the changelog explains each.
4. Run your agent's own test suite (drive the loops with `ark_model::MockProvider`, no live
   model needed).
5. Run your benchmark against your last baseline if the release touched the runtime
   ([`BENCH.md`](BENCH.md)).

A harness update **must not silently break every application**: breaking changes are gated
behind a version bump and documented, and defaults change only on a MINOR+ with a changelog
note.

## Tags are immutable

A release tag never moves. Once `vX.Y.Z` exists, anyone may have built against it, so
re-pointing it would change what an existing pin means. Any correction after a tag, even a
one-line doc fix, is a new version.

- Never `git tag -f` or delete and re-create a release tag.
- `v0.3.0` points at `d67b5b8`. The README fix that landed after it (`580655e`) ships in
  `v0.4.0`.
- Consumers pin a tag (`{ git = "<canonical remote>", tag = "vX.Y.Z" }`), never a branch.

## Where releases come from

The canonical repository is the remote named `origin` of the maintainer checkout. Consumers
depend on that URL and a tag. The public copy is
`https://github.com/0x00vb/ARK`; external consumers pin that URL. The maintainer's `origin`
is still a local bare repository that `main` and every tag are also pushed to; switching
`origin` to GitHub is a `git remote set-url` and a URL change in each consumer's
`Cargo.toml`. `main` is always releasable: it is what the
next tag is cut from.

## Release process (maintainers)

1. Add the version's entry to `CHANGELOG.md` and bump `workspace.package.version` (and the
   internal dependency versions) in the root `Cargo.toml`.
2. `scripts/ci.sh`: format check, clippy with warnings as errors, all tests, a release
   build, the version check, and the simulated benchmark compared with `bench/baseline.json`
   (a regression fails the run). `ARK_UI_PAGE_TESTS=1` adds the WebKit page tests.
3. Commit, then tag that exact commit: `git tag -a vX.Y.Z -m vX.Y.Z`. Push `main` and the tag
   to `origin`.
4. `scripts/verify-release.sh vX.Y.Z`: clones `origin` at the tag into an empty directory,
   checks the tag matches the version and points at the cloned commit, and runs every CI
   check there.
5. Record the release's simulated report as `bench/baseline.json` (in the next commit), so
   the next release is compared with this one.
6. Consumers change their tag, run `cargo update -p ark-sdk` (or the crates they use), and
   run their own tests and benchmark. SecRes does this with `secres-bench`.

The crates are not published to crates.io; a git tag is the release.
