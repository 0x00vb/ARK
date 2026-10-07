# 0046. The agent's browser opens its project's own pages

Status: Accepted
Date: 2026-10-07
Amends: 0015 (addresses on this machine refused), 0041 (the later, narrow exception)

## Context
An agent that starts a dev server (`process.start`, ADR 0041) could not look at what it built:
`browser.open` refuses every address on this computer or a private network (ADR 0015), so a
prompt in a page cannot send the agent to the person's router, database or other apps. In the
preview of ARK Desktop (2026-10-07) a turn showed the refusal as a tool error before the agent
presented the page to the person instead. The owner: reading the pages of the servers an agent
started is the fix developers need.

A process is Ready when its address answers, whoever answers. That is enough to show the person
a page, not to let an agent read one: a command that prints `http://localhost:8080`, or names
`port: 8080`, and serves nothing would lend the agent another app's page, and "Always" on a
command does not cover the port.

ADR 0015 also recorded a gap: only `open` checked the address; a link, a redirect or Back could
land on a private address and the agent read it.

## Decision
| What | Rule |
|---|---|
| Which pages | Public pages, as before, and pages on this computer (`localhost`, its subdomains, loopback, `0.0.0.0` opened as `localhost`) on a port a running process of the agent's project serves itself |
| "Serves itself" | The socket listening on the port of the process's address belongs to the process's own group (each process starts in a group of its own). macOS asks `lsof` once for every listening port; Linux reads `/proc/net/tcp{,6}` and the group's `/proc/<pid>/fd`. When this cannot be told, nothing counts |
| "The agent's project" | The turn's workspace (`RunCtx::workspace`), equal to the folder the process was started in, as given; no workspace, no local pages. Another project's processes do not count |
| Which browser | The fresh one only. The signed-in browser keeps the person's cookies, which `localhost` shares across ports: it opens public pages only |
| Where an action lands | After every action (`open`, `read`, `click`, `type`, `back`) the page's address is checked by its host alone: this computer or a private network (private, loopback, link-local, CGNAT addresses, names without a dot, `.local`) that the project does not serve leaves the page blank (`about:blank`) and the action refused |
| What the agent is told | `browser.open` says it opens public pages and pages a process of this project serves; a refusal says the same and that any address can be shown to the person in their Browser view |
| Seam | `ark-browser` names no other crate: `BrowserService::serve_local_pages(LocalPages)`, a function from a workspace to the ports it serves, set once (the first wins). `ark-sdk` sets it from `Processes::serving` through a weak handle, so processes still stop with the runtime |

Ready keeps its meaning: the Browser view in the window may still show what answers on a port
(a container's published port included).

## Consequences
- An agent reads, clicks through and checks the dev server it started, in its own browser,
  without a question per page. Starting the process is what the person approves (ADR 0041).
- An approved command that is itself a proxy (`socat`, or a `dev` script that forwards to another
  service) counts: that is what "Always `npm run dev`" means. A port published by a container
  (Docker) does not count, since its socket belongs to the container's daemon.
- The check after an action runs once the page has loaded: a GET to a private address has
  already gone out, the agent does not get the page. A name that resolves to a private address
  (`*.internal`, `*.lan`, DNS rebinding) is not caught after a redirect; `open` still resolves
  names first. Blocking before the request needs the DevTools `Fetch` domain, not used yet.
  Requests a public page makes for its parts (images, `fetch`) are Chrome's to limit.
- Only the port of the process's address counts, one per process.
- The port is matched, not the address it is bound to: if another app binds `127.0.0.1:3000`
  while the project's process holds `*:3000`, `localhost:3000` reaches the other app. Rare;
  accepted.
- `lsof` gets two seconds; if it does not answer, nothing counts.
- An address typed without a scheme opens over http on this computer (`localhost:3000`), https
  elsewhere.
- The tool's description and its refusal name `process.start` only when the app wired local
  pages; otherwise they say "public pages only" as before.
- A service shared by two runtimes serves the first one's processes.

## Check
- `crates/ark-proc/tests/processes.rs`: `only_a_port_the_process_itself_listens_on_is_served`
  (a process that names a port another program listens on is Ready but not serving; a server
  it runs is; another project's and a stopped process's are not). Run on macOS and on Linux
  (Docker, Debian bookworm, Rust 1.90, 2026-10-07: all 11 process tests pass).
- `crates/ark-browser/src/lib.rs` unit tests: `an_address_is_private_by_its_host_alone`
  (public IPv6 and IPv4 by number, `about:`, `data:` and error pages are not; loopback, private,
  `.local`, single-label and `localhost.` are), `this_computer_is_localhost_loopback_and_unspecified`.
- `crates/ark-browser/tests/browser.rs`:
  `a_project_opens_the_pages_its_processes_serve_and_nothing_else_here` (real Chrome: the
  project's page by `localhost`, `0.0.0.0`, `127.0.0.1` and `localhost:N` without a scheme;
  another port, another project and the signed-in browser refused; a 302, a link and Back into
  another port refused and the page left blank). It fails without the check after an action.
- `crates/ark-sdk/tests/local_pages.rs`: `an_agent_reads_the_page_of_the_server_it_started`
  (a scripted model starts `python3 -m http.server` with `process.start` and reads its listing
  with `browser.open`; another program's port is refused). It fails without the SDK wiring.
- Simulated bench before and after: the browser is in no workload (ADR 0015), so the numbers do
  not move; a live run with a model is the person's dogfood in ARK Desktop.
