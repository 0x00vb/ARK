# 0011. Capabilities people see, connections ARK owns

Status: Accepted
Date: 2026-10-03

## Context
People should think in capabilities (Mail, Calendar, Web, Files, Browser), not in OAuth
apps, MCP servers, transports or tool schemas. Agents need those capabilities as tools.
Some need an account (Google for Mail and Calendar). The person decided (2026-10-03): ARK
owns the connection for core capabilities (its own Google OAuth client, "Connect Google"),
not an arbitrary hosted MCP server; MCP can sit underneath; other sign-in methods and
third-party MCP servers must fit the same model later; nothing Gmail-specific in the
architecture.

ARK already has the pieces a capability needs once it is connected: `Adapter` and
`Registry` for its tools, `RiskTier` and the `ApprovalGate` for reading against acting,
`AgentSpec::tools` for which agent gets it, MCP plugins that join while ARK runs.

## Decision
Three things, in a new crate `ark-connect`, with service details kept as data:

| Thing | What it is | Example |
|---|---|---|
| Capability | what a person sees: a name, one line, the tools it gives (an action namespace), and what it needs | Mail: "send email from your account", `mail.*`, needs a Google account with `gmail.send` |
| Provider | where an account lives and how ARK signs in to it | Google: OAuth 2.0 with PKCE and a loopback redirect, ARK's client id |
| Account | one signed-in account, its granted scopes; tokens in the OS keychain, never in a file | `google:me@example.com` |

A capability's tools have neutral names (`mail.send`, `calendar.events`), so another
provider can back the same capability. Connected capabilities reach turns as a tool source
beside MCP plugins (`ark_tools::ToolSource`); the engine merges every source when a turn
starts, as it does for plugins. Reading is `Observe`, acting is `Mutate`, through the same
gate as every other tool. Disconnecting revokes the token at the provider and deletes it.

Each capability asks only for its own scopes, added to the same account when connected
(incremental consent). The first release needs no Google restricted scope: Mail sends
(`gmail.send`, sensitive) and Calendar reads and adds events (`calendar.events`,
sensitive). Reading mail (`gmail.readonly`, restricted) waits for Google's restricted-scope
verification (`docs/GOOGLE.md`).

## Consequences
- The capability list (`✓ Mail`, `○ Calendar · Connect`) is computed from the catalog, the
  accounts and the engine's tools, once, for both front-ends.
- A new capability on an existing provider is a catalog entry and an adapter; a new
  provider with OAuth is a catalog entry. Neither touches the runtime.
- A third-party MCP server becomes a capability by a catalog entry that names its server;
  its sign-in goes through the same accounts (later step).
- ARK holds refresh tokens on the person's machine. A process that can read the keychain
  entry can act as the person within the granted scopes.
- An app or a test can use another secret store (`Secrets`); CI never touches a keychain.

## Check
`crates/ark-connect` tests (PKCE and state, token exchange and refresh against a local
server, account round trip with a memory secret store, adapters against a local API);
`crates/ark-ui/tests/neutral.rs` keeps service names out of the runtime crates.
