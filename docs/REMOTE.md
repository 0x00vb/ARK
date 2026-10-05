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
| `remote::Client::new(url, token)` | `call(method, params)`, `hello()` and `events()` from Rust: scripts, checks, other services (`ark_interact::remote`, re-exported by `ark-ui`) |
| `app --connect URL` (`ark-cli`) | the app's terminal on the runtime, with `--token` or `ARK_TOKEN` ([`CLI.md`](CLI.md#on-another-machine), ADR 0024) |
| `remote::run_window(url, token, ctx)` | a native window (feature `desktop`) on the runtime's own page |
| `remote::send_folder(client, folder, name, go_on)` | copy a folder from this computer to the runtime, where it becomes a new project (ADR 0030). `Folder::read(dir)` says how many files and bytes first |
| `remote::visit(url, token)` | from an app's handler: the running window opens the runtime's page in place of its own; `ark.openLocal()` there comes back (ADR 0029) |

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

## Saved keys and connections

On a person's computer, API keys and account tokens go in the OS keychain. A server's keyring
(the Linux kernel keyring) is gone after a restart, so a served app keeps them in one encrypted
file in the store instead (ADR 0028):

```rust
use ark_sdk::prelude::ark_connect::FileSecrets;

let ark = Ark::builder()
    .store_root(&root)
    .connections(true)
    .secrets(Arc::new(FileSecrets::in_store(&root)?))   // <store>/secrets.enc
    .build()?;
```

| File or variable | What it is |
|---|---|
| `<store>/secrets.enc` | every saved secret, AES-256-GCM, mode 0600 |
| `<store>/secrets.key` | the key, made on first use, mode 0600; unused when `ARK_SECRETS_KEY` is set |
| `ARK_SECRETS_KEY` | the key from the host (`openssl rand -base64 32`), to keep it off the volume |

Back up the key with the store, or keep `ARK_SECRETS_KEY` in the host's secret store. Without the
key the app refuses to start and says how to recover; the secrets can be entered again.
`examples/writing-agent` does this when run with `--serve ADDR --tokens`.

## Limits

| Limit | Why |
|---|---|
| Clients that connect, windows and terminals, see the same open chat (other chats keep working in the background, ADR 0017) | one `Host` per runtime |
| The Computer (ADR 0018) is not offered by an app served with saved tokens; it would drive the screen of the machine ARK runs on. `serve` refuses to start such an app (`ArkBuilder::computer(false)` when serving with tokens) | nobody sits at a server |
| Connecting accounts (Mail, Calendar) needs a browser on the runtime's machine | the OAuth sign-in returns to the runtime's loopback address |
| The signed-in browser opens on the runtime's machine | it is a visible browser there |
| The folder picker is the window's own; a remote page asks for a typed path | the runtime's disk is not the client's |
| Vendor CLIs (Claude Code, Codex) need signing in on the runtime's machine, and their terms may not allow running them as a service for someone else | use API keys or a local model on a shared server |
