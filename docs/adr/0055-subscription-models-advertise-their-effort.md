# 0055. Subscription models advertise their effort

Status: Accepted
Date: 2026-10-09

## Context

ARK could pass a named model to Claude Code and Codex, but the Codex picker offered only
`Default`. Both CLIs also accept reasoning effort. A static list would become stale and could
offer a model or effort that the signed-in account cannot use.

## Decision

Subscription model specs may end in `@effort`, for example
`codex:gpt-5.6-terra@medium` or `claude:sonnet@high`. A spec without effort keeps the CLI's
default, so stored specs keep their behavior.

Codex models, labels, default effort and supported efforts come from `codex app-server`'s
paginated `model/list`. Listing has a short timeout and falls back to `Default`, so discovery
never blocks chat. Claude Code uses its documented aliases and accepted effort values.

Effort is passed on every route: Claude Code's one-shot, search, open and resumed sessions;
Codex exec configuration; and app-server `turn/start`. An unsupported typed effort is refused
before the active model changes.

`Backend.models` remains for clients that only need identifiers. `Backend.model_options` adds
the label, effort list and provider default used by richer pickers.

## Consequences

The picker shows only choices the provider advertises when that catalog is available. Adding
`model_options` is a public struct-literal break and therefore ships in a minor `0.x` release.
The `@` suffix is reserved for subscription effort and is not applied to API, endpoint or local
model identifiers.

## Check

`cargo test -p ark-model` covers parsing, Claude arguments, Codex exec and app-server effort,
and live catalog shape. ARK's page tests cover the model picker and recoverable selection error.
