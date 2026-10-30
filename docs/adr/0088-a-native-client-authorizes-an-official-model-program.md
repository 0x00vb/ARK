# 0088. A native client authorizes an official model program

Status: Accepted
Date: 2026-10-23
Amends: 0052 (runtime setup), 0057 (external-client protocol), 0061 (subscription credentials)

## Context

A phone can start or continue a chat on an account-bound ARK runtime, but that runtime may not
yet be signed in to its model program. The existing setup surface is a terminal. Exposing that
terminal to a phone would also expose unrelated process output and generic input. Asking the
person to copy a token would move credential ownership from the vendor program into ARK.

Claude Code has an official account authorization flow. It prints an official HTTPS address,
accepts the returned one-time code on standard input and keeps its own credentials. Codex has an
official device authorization flow. It prints an official HTTPS address and user code, then polls
the provider itself. Both programs have official status and logout commands.

## Decision

Protocol v1.6 adds a narrow connection surface for account-bound external clients. The runtime
starts Claude Code directly with `claude auth login --claudeai`. It gives the native client the
unchanged, validated
`https://claude.com/cai/oauth/authorize` address and accepts one bounded code once. ARK never
reads or stores the resulting credential.

Protocol v1.7 adds `codex`. The runtime starts the official program with `codex login
--device-auth`. It accepts only the exact `https://auth.openai.com/codex/device` address and a
bounded uppercase user code. The native client displays the code. Codex polls and stores its own
credential. ARK never submits the code or reads a credential file.

Only one authorization process may run across all providers because every process shares the
same bounded runtime. Only the paired device that began an attempt may read it, submit a Claude
code or cancel it. Other devices see only that the provider is being connected. Attempts live in
memory, expire after ten minutes and keep terminal results long enough for a client to recover a
dropped response.

The program runs without a shell, in its own process group and with a cleared environment. Only
the home, path, temporary directory, locale and TLS certificate paths are retained. API keys,
OAuth tokens, endpoint overrides and proxy variables are not inherited. Output is bounded and
never enters an error, trace, event or durable store. Cancellation, expiry and device revocation
stop the whole process group.

Claude success requires a fresh `claude auth status` result with `loggedIn: true` and
`authMethod: "claude.ai"`. Codex success requires exact output from `codex login status` that says
`Logged in using ChatGPT`. API-key authentication does not satisfy either connection. Disconnect
runs only `claude auth logout` or `codex logout` and verifies the logged-out status. ARK never
deletes a credential file.

The route is not a generic program or terminal protocol. Each provider needs its own verified
command, URL and status contract before it can use the typed connection shape.

## Consequences

- A phone opens the vendor's page and returns to the same chat without seeing runtime setup.
- A dropped code-submission response is resolved by polling. The code is never submitted twice.
- Runtime restart loses an active attempt safely. The client starts a new attempt.
- An unavailable or changed vendor CLI fails closed and needs a Core update, not relaxed URL
  validation.

## Check

`ark-model` fixtures prove the exact commands, cleared environment, official URL, one-shot code,
device ownership, idempotent recovery and logout. `ark-ui` TCP tests prove that only an
account-bound external client sees and reaches the capability. `scripts/protocol.sh` keeps the
schema and generated Swift contract exact.
