# Running ARK elsewhere

An ARK app can run on a server and be used from a window or a browser on another machine. It
is the same app and the same protocol as on a laptop; only the carrier and the access check
change (ADR 0016). Nothing here depends on a hosted service: this is how you self-host.

```text
window or browser ──HTTPS + SSE──► proxy (TLS) ──HTTP──► app binary: ArkUi::serve + Access::Tokens
                                                          └ Host ─► Interaction, orchestrator, store
```

## In an app

```rust
use ark_ui::{access::Tokens, Access, ArkUi};

let tokens = Tokens::in_store(&store_root);              // <store>/access.json
ArkUi::new(app)
    .access(Access::Tokens(tokens))
    .serve("127.0.0.1:8420")                              // behind a proxy on the same host
    .await?;
```

| Piece | What it does |
|---|---|
| `Tokens::create(name)` | a new token, returned once; only its SHA-256 is stored (file mode 0600) |
| `Tokens::list()`, `Tokens::revoke(name)` | names and creation times; a revoke works at once, no restart |
| `Access::Tokens` | every request but the page's static files and `GET /health` needs a saved token; any `Host` is accepted |
| `Access::OneTime` (default) | the local mode: a token printed at start, loopback `Host` only |
| `remote::Client::new(url, token)` | `call(method, params)` and `hello()` from Rust: scripts, checks, other services |
| `remote::run_window(url, token, ctx)` | a native window (feature `desktop`) on the runtime's own page |

Give your binary a way to manage tokens (ARK Desktop: `ark-desktop token create NAME`). A token
sent by a client may be in `Authorization: Bearer`, `X-Ark-Token` or `?token=`.

## What a token allows

Everything the window can do, on the runtime's machine, as the OS user the app runs as: run
agents, read and write the project folders, and open a shell in the Terminal panel. Give one
token per device so you can revoke one; run the app as an unprivileged user or in a container;
put one owner's runtime on its own machine or container. There are no users or roles inside a
runtime.

## TLS and the network

ARK serves plain HTTP. Put it behind something that adds TLS:

| Setup | How |
|---|---|
| A public host name | Caddy on the same machine: `ark.example.com { reverse_proxy 127.0.0.1:8420 }` gets and renews a certificate by itself. Keep its access log off (the page's event stream has the token in its address) |
| A private network | Tailscale or WireGuard; serve on the private address only |

Server-sent events pass through Caddy without settings (tested with ARK Desktop's
`deploy/compose.yaml`: a turn's events arrived live through Caddy 2). Other proxies must not
buffer `/events`.

## Running it

Keep the process under a supervisor (systemd, a container's restart policy). On restart,
sessions, agents and tasks come back from the store; a task that was running when the process
stopped is marked interrupted, and can be retried. Agents and tasks run whether or not a client
is connected; a window that connects later reads their results.

`GET /health` answers `{"ok":true,"version":"…"}` without a token, for the supervisor or an
uptime check.

## Limits

| Limit | Why |
|---|---|
| Clients that connect see the same open chat (other chats keep working in the background, ADR 0017) | one `Host` per runtime |
| The Computer (ADR 0018) is not offered by an app served with saved tokens; it would drive the screen of the machine ARK runs on | nobody sits at a server |
| Connecting accounts (Mail, Calendar) needs a browser on the runtime's machine | the OAuth sign-in returns to the runtime's loopback address |
| The signed-in browser opens on the runtime's machine | it is a visible browser there |
| The folder picker is the window's own; a remote page asks for a typed path | the runtime's disk is not the client's |
| Vendor CLIs (Claude Code, Codex) need signing in on the runtime's machine, and their terms may not allow running them as a service for someone else | use API keys or a local model on a shared server |
