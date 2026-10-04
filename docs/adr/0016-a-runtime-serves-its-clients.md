# 0016. A runtime serves its clients; a window is one of them

Status: Accepted
Date: 2026-10-04

## Context
ROADMAP step 5: ARK runs without a window, and Desktop and the CLI connect to it. ARK
already had one protocol for this: `Host::call(method, params)` plus one numbered event
stream, carried by Tauri IPC (the window) or HTTP and server-sent events (`ArkUi::serve`).
The HTTP server was local only: 127.0.0.1, a token printed at each start, loopback `Host`
headers only. An app (its tools, profiles, handlers and page scripts) is defined by its own
binary, not by ARK, so "ARK running somewhere" always means an app's binary running there.
Later, ARK Cloud runs the same runtime for paying customers (step 6). Cloud must not become a
requirement of ARK (`../WORKSPACE.md`).

## Decision
No second server or protocol. A runtime is an app's binary serving the existing protocol;
every client (the native window, a browser, scripts, later the CLI and Cloud's control plane)
speaks it.

| Question | Decision |
|---|---|
| Headless | `ArkUi::serve` with `Access::Tokens`: the app's binary on a server, no window. Agents and tasks run in that process whether or not a client is connected |
| Auth | Named access tokens, created once, kept as SHA-256 hashes in `<store>/access.json` (mode 0600), revocable by name (`ark_ui::access`). Every request but the page's static files carries one. The one-time token stays for local `serve` |
| Who a token is | The runtime's owner. A token can do everything the window can, including the Terminal: a shell as the runtime's OS user. There are no users or roles inside a runtime |
| Several people | Several runtimes, one per owner (a person, a team, a Cloud workspace). Isolation is the OS or container boundary, not ARK code |
| TLS | Outside ARK: a reverse proxy (Caddy) or a private network (Tailscale). With tokens the server accepts any `Host` and does not need to bind publicly; with the one-time token it stays loopback only |
| A window on a remote runtime | Loads the page that runtime serves (browser transport), so page and runtime are always the same version. `ark_ui::remote` checks the runtime (`hello`) and opens the window on it |
| Sessions | Kept in the store as before. Clients that connect see the same open chat; other chats and agents keep working beside it (ADR 0017) |
| Restart | A supervisor (systemd, launchd, a container) restarts the process; tasks that were running are marked interrupted (existing orchestrator behavior) |
| Cloud | Accounts, billing, provisioning and model gateways stay out of ARK. Cloud provisions runtimes and holds their tokens through the same `access` file |

## Consequences
- Easier: self-hosting is "run the app's binary with tokens behind Caddy"; Cloud is
  "provision that, plus accounts and billing"; any client works with any runtime version.
- A remote window has no native folder picker (it asks for a typed path on the runtime's
  disk), and the page's local storage belongs to the runtime's address.
- Not solved here: connecting accounts (OAuth sign-in redirects to the runtime's loopback)
  and the signed-in browser need a person at the runtime's machine; the CLI as a client.
  Several live conversations and scheduled tasks: ADR 0017.
- A token in an `EventSource` URL can reach proxy logs; tokens are revocable and the proxy
  should not log query strings.

## Check
`crates/ark-ui/tests/serve.rs`: requests without a token or with a revoked one are refused;
a token store accepts any `Host`; the one-time token keeps loopback only; `remote::Client`
reaches a served runtime; a task keeps running with no client connected.
