# Contributing to ARK

Thanks for helping build the runtime so nobody has to rebuild orchestration, context
management, and LLM optimization from scratch.

## The one rule

**No tool name and no domain name may appear in `ark-core`.** Not a platform, not a file
format, not a domain concept. This invariant is what makes ARK reusable across unrelated
products. If you need domain-specific behavior, it belongs in a consumer via an extension seam
(see `docs/EXTENSION.md`), not in the core.

A useful check for any change: *would this still make sense for an agent that writes novels?*
If not, it's domain-specific — push it out to the SDK's extension points.

## Principles

- Composition over inheritance; interfaces over domain assumptions; headless core over UI
  coupling; small stable public APIs over exposing internals.
- Abstract because *multiple domains* need it, not because it might be useful. Avoid
  speculative abstractions.
- Keep the public API small. New surface needs a real second consumer.

## Workflow

1. `cargo test` must stay green (all suites; no live model required — use `MockProvider`).
2. `cargo clippy --all-targets` clean; `cargo fmt`.
3. Add or update tests for any behavior change; drive loops with the mock provider.
4. If you change a public API or a default prompt, update `CHANGELOG.md` and note the semver
   impact (see `docs/VERSIONING.md`).
5. Small, inspectable commits with a clear message.

## Adding a provider / tool / distiller

These are the common contributions and all live behind existing traits — see
`docs/EXTENSION.md`. A new *built-in* provider goes in `ark-model`; domain tools and distillers
generally belong in *your* crate, not ARK.
