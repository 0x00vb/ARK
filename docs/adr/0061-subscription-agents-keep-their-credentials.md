# 0061. Subscription agents keep their credentials on the computer that runs them

Status: Accepted
Date: 2026-10-11
Amends: 0012, 0039, 0043 and 0052 only by making their setup and user-facing choice explicit

## Context

ARK already uses Claude Code and Codex subscriptions through their official CLIs. The CLI owns
the login and signs its own requests. ARK does not read its credentials (ADR 0012). A chat on
another computer installs and signs in the CLI there through a temporary terminal shown to the
person (ADR 0052). Claude Code and Codex also work as ACP workers, but the window offers them
only when a person types `agent:claude-code` or `agent:codex` (ADR 0043).

The missing product decision is how to offer those workers, how to add other subscription
agents such as Grok, and how to keep sign-in out of ARK Account and ARK Cloud. Grok Build has a
documented native ACP process, `grok agent stdio`, plus browser and device-code sign-in. It is an
agent program with its own tools, not a model backend for ARK's loop.

## Decision

The official agent program owns its login on the computer that runs it. ARK may start its
documented install and sign-in commands in a visible, temporary setup terminal. ARK never
copies, imports, refreshes, proxies or uploads its tokens or credential files.

| Surface | Decision |
|---|---|
| Chat model | Keep the current Claude and Codex subscription choices. No new sign-in screen |
| Agent | Add `Runs with` below the agent identity without replacing the focused conversation. List ready ordinary models and Grok. `Add another` reveals Grok when it is not ready |
| Place | An agent has no separate computer choice. Its catalog, setup and work belong to the runtime where that project's agents run |
| Setup | A program that is not ready offers one action in place: Install, Install connector, Update or Sign in. The target runtime resolves an opaque action id to a reviewed command and returns to the waiting choice when ready |
| Grok | Add a native ACP worker using `grok agent stdio`. Do not add Grok to `ark-model` until a documented pure-model interface can run with its own tools disabled |
| Remote | When the project runs on My machine, install and sign in there. Its setup terminal is relayed over the existing authenticated link and is not stored |
| Cloud | Consumer subscriptions do not run on ARK-managed Cloud in this change. The Cloud consumer does not register subscription workers or their setup actions. Cloud uses ARK models, a person's API key, or a later vendor-approved route |
| Claude and Codex | Keep existing explicit `agent:*` compatibility and current chat-model behavior unchanged, but do not newly offer either subscription as an agent. Anthropic consumer terms require explicit permission for automated access. OpenAI's supported third-party route is app-specific Sign in with ChatGPT, not reuse of a Codex CLI login |
| Extensibility | Known ACP programs are catalog data. A new program needs a documented protocol, setup probes, a permission test, a checked version and a vendor-supported access path. It does not add a provider branch to orchestration |

The normal path has no extra interaction. If the chosen program is already ready, selecting it
changes what later tasks run on. A new or expired login adds one provider-owned browser or
device-code step. In the current window, the person's draft and choice stay in place. Setup
restores the selector and focus. It never submits the draft or starts work.

The first release supports Grok on macOS and Linux through the pinned npm package and native ACP
process. Windows reports it as unavailable until its npm command shim and permission behavior
have been checked on a Windows machine.

### Interaction contract

`Runs with` is not a custom select. It is a native select followed by one `Add another` button.
The select contains `Same as chat`, ready models and ready offered workers. The button expands an
in-place list of offered workers that are not ready. Each row has the worker's title, state and
exactly one current action. Success collapses the list, refreshes the select, leaves its value
unchanged, focuses the select and announces `<name> is ready` through the polite live region. The
person must open the select and choose it. Failure leaves the row and recovery action visible.

Tab order is the select, `Add another`, then the disclosed actions. The button exposes
`aria-expanded` and the list has a stable labelled region. Escape collapses the list and returns
focus to `Add another`. A redraw keeps focus on the same worker action. If that action no longer
exists, focus returns to `Add another`. `Checking…` belongs only to `Runs with`; it does not delay
the rest of the inspector.

Setup opens immediately as a modal terminal sheet with an accessible title and a visible state
outside terminal output. Focus stays inside it. Escape remains available to the terminal program.
F6, named in the terminal's accessible label, moves focus from xterm to the sheet controls on
every system; normal Tab navigation reaches `Done`, `Retry` and `Close`. The sheet distinguishes
`Opening…`, `Installing…`, `Updating…`, `Signing in…`, `Finished`, `Disconnected`, `That sign-in
expired` and failure. A finished process triggers one catalog recheck. Success offers `Done`;
failure offers `Retry` and `Close`. Closing restores focus to the `Runs with` select on success,
or the same recovery action on failure. It never rebuilds the inspector or changes the message
draft.

An idle agent whose saved worker is unavailable keeps that choice visible but cannot start a new
task. Its recovery action sits beside `Runs with`. Messaging work that is already running remains
available. `Ask` returns only when that runtime reports ready or the person selects another
choice. `Runs with` is disabled while an agent works, so a generic saved mark never has to explain
future-task semantics.

Every ordinary surface resolves a worker spec through the runtime catalog. It shows `Claude
Code`, `Codex` or `Grok`, never `agent:*`. A saved worker missing from the current catalog reads
`Unavailable`. Catalog reads are keyed by runtime and project. Project switch, reconnect and
completed setup invalidate that key. A result from an earlier runtime is discarded and cannot
change state or focus in the current inspector.

## Implementation plan

| # | Layer | Work | Done means |
|---|---|---|---|
| 0 | `ark-acp`, security tests | Run a Grok ACP spike before product code. Record the exact initialize capabilities, every client request, whether it needs ACP `terminal/*` or `fs/*`, its own tool use, model setting, effective permission sources, existing Grok and Claude allow rules, MCP servers, hooks, unknown permission shapes, refusal, restart, usage and failure when ARK rejects an unsupported method. Test a clean profile and the person's ordinary profile | Done 2026-10-11 with Grok 1.0.50 and `grok-4.7`. A separate Grok home, provider-owned login and explicit `ask` rules produced one ACP request per edit and command. Reject changed nothing; allow wrote once; load-session plus reject left it unchanged; config-option model selection worked; no client filesystem, terminal or unknown request was needed |
| 1 | `ark-runtime`, `ark-acp`, `ark-interact` | Add `Workers::offers()` with a default empty implementation. Its neutral `WorkerOffer` reports spec, title, readiness by component, opaque setup action ids and model choices. Add a `worker_offers` read through the project runtime's held/resource route and a shared terminal report. Only the target runtime resolves an action id to argv | Old implementors compile unchanged. A remote project never receives the local runtime's offers. CLI and window receive the same catalog. No command, credential path or vendor branch enters orchestration or the wire |
| 2 | `ark-acp` | Add Grok as catalog data with `grok agent stdio`. Give it a separate Grok home containing ARK's fixed config and the login Grok writes there itself. Set `ask` rules for every Bash and edit call, disable remembered grants, auto-update, workflows and Claude, Cursor and Codex compatibility. Start with an empty home, never grant project trust and pass `mcpServers: []`, so personal extensions and project MCP servers do not run. Use ACP `session/set_config_option` for a named model. Keep `env_clear`; pass no ARK key, token, account variable or normal home. Accept only checked 1.0 versions | Scripted tests cover model choice, permissions, refusal, cancellation, restart, isolated configuration and incompatible versions. A live test covers one approved file change and command |
| 3 | `ark-interact`, shared page, Desktop UI | Add `Runs with` below the agent identity. Keep `Same as chat` first, then ready models, then ready offered agent programs. `Add another` shows Grok when it needs setup; its one action returns to the selector | A person can create an agent, choose Grok without typing a spec, and understand the result without setup prose |
| 4 | `ark-ui`, `ark-reach`, Desktop | Add `worker_setup {spec, action}` locally and its place-bound remote route, using the existing setup terminal transport. Accept only an action the target runtime currently offers for that spec; reject a stale action and recheck. Preserve the in-window draft and choice across close, refusal, timeout, disconnect and success. Persist neither terminal bytes nor one-time codes | Local and SSH tests prove the opaque action ran only on the project's runtime. Credentials never enter stores, traces, logs, crash fixtures or ordinary events. Device codes exist only in authenticated live terminal frames and disappear when it closes |
| 5 | docs and release | Record official installer origin, supported systems, checked CLI version, probe timeout, update failure and rollback. Dogfood local and remote Grok. Run ARK and Desktop checks, then release ARK before updating Desktop | Acceptance checks below pass. A dated review of the current OpenAI, Anthropic and xAI primary terms and integration docs is attached to the release evidence. No claim says ARK includes or resells a vendor subscription |

## States and words

| State | What the person sees | Recovery |
|---|---|---|
| Checking | `Checking…` in the opened selector | It resolves without blocking the rest of the inspector |
| Ready | The program's name under `Runs with` | Select it |
| Missing | `Install <name>` | Visible setup terminal, then automatic recheck |
| Connector missing | `Install connector` | Visible setup terminal, then automatic recheck |
| Signed out or expired | `Sign in to <name>` | Provider browser or device-code flow, then automatic recheck |
| Setup already running | `Connecting…` | Reopen its one live setup terminal. Do not start another |
| Setup cancelled | The selector and draft unchanged | Try again or choose another |
| Install or update failed | The last useful provider error | Retry. The earlier working version stays selected when possible |
| Sign-in code expired | `That sign-in expired` | Start one new provider sign-in |
| No entitlement or quota | The provider's reason and reset time when reported | Choose another or retry after the reported time |
| Probe timed out | `<name> did not answer` | Retry. Do not call it ready |
| Setup ended but probe failed | `Could not confirm <name>` | Retry the check or setup |
| Computer offline | `<computer> is not answering` | Retry after reconnect. Never run setup here instead |
| Unsupported version | `<name> needs an update` | Update in the same setup terminal. Work does not start |
| Unsupported system | `<name> is not available on this computer` | Choose another |
| Sign-in expires during a task | The task stops with its work kept and asks for sign-in | Sign in, then explicitly continue the task |
| Provider refuses access | The provider's reason in plain words | Sign in again or choose another. Work and draft remain |

## Acceptance and review gates

| Gate | Evidence |
|---|---|
| Existing user | Existing Claude and Codex chat-model users see no change. Existing explicit worker specs with a checked connector see no change. Neither subscription is newly presented as an agent |
| First use | Show one applicable setup control at a time. A clean computer may move through install and sign-in states. Success returns to the waiting choice, selects nothing without an explicit choice and starts no work |
| Credential boundary | Tests seed recognizable values and prove ARK passes no vendor token or ARK secret through environment, prompt, setup metadata, protocol payloads, ordinary events, stores, logs or crash fixtures. One-time codes may cross only authenticated live terminal frames and are never kept |
| Authority | With the default `allow_mutate = false`, each Grok file change and command is an ARK question. If the person enables in-folder changes, those changes follow the existing worker grant while commands still ask. Deny leaves the folder unchanged. An unknown or changed permission shape stops the worker |
| Place | Local setup touches only local state. Remote setup touches only the chosen computer. Disconnect never falls back locally |
| Recovery | Expired login, cancelled setup, timeout and link reconnect preserve the in-window draft. Task restart does not repeat an approved change. App restart draft persistence is not claimed in this change |
| Interface | Keyboard, screen reader, light, dark, 360 px and a slow connection cover every state above. The current design score target is 10/10: discoverable, visible state, recoverable, naturally mapped and constrained |
| Runtime switch | An inspector opened on runtime A, followed by a project switch to runtime B and a late setup completion from A, keeps B's catalog, state, enabled actions and focus |
| Compatibility | `Workers::offers()` defaults to empty. The terminal and window use the same labels. Old clients remain unchanged. A new client omits worker choices when an old runtime lacks `worker_offers`, and marks an already selected worker unavailable there |
| Release | `ARK_UI_PAGE_TESTS=1 scripts/ci.sh` passes in ARK. `scripts/check.sh` passes in Desktop. Live subscription runs need the owner's explicit approval because they consume plan usage |
| Cloud cut | The Cloud consumer's configuration test proves it registers no subscription worker and exposes no subscription setup action |

## Cut from this change

- No ARK OAuth client for a vendor subscription.
- No reuse of a Codex CLI login as ARK's identity. A later Codex integration must use OpenAI's
  app-specific Sign in with ChatGPT path and identify ARK as the caller.
- No credential forwarding, account pooling, token import or private endpoint emulation.
- No ARK-managed Cloud execution on a consumer subscription.
- No screen scraping, terminal typing or session-file reading to control work ARK did not start.
- No generic arbitrary command entry. Each supported agent has reviewed catalog data.
- No claim that another product's implementation proves legal permission. Public copy and the
  applicable vendor terms receive a focused review before release.

## Terms review, 2026-10-11

This is a product release gate, not legal advice. It records what the primary vendor sources say
on the review date and why the offered catalog is narrower than the compatible catalog.

| Provider | Primary source reviewed | Release result |
|---|---|---|
| xAI | [Consumer Terms](https://x.ai/legal/terms-of-service) last updated 2026-09-11; [CLI Reference](https://docs.x.ai/build/cli/reference) last updated 2026-07-21; [Headless and Scripting](https://docs.x.ai/build/cli/headless-scripting); [Permissions and Safety](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md) | Offer Grok. The terms describe agentic actions and the official CLI documents device authentication, headless use and `grok agent stdio` for ACP |
| OpenAI | [Terms of Use](https://openai.com/policies/terms-of-use/) effective 2026-01-01; [Sign in with ChatGPT](https://developers.openai.com/siwc); [Codex app-server integration](https://developers.openai.com/siwc/token-sharing-open-source/codex-app-server) | Do not offer the existing Codex worker login. OpenAI permits eligible open-source local apps to use a ChatGPT plan through app-specific authorization, but that route identifies the app and manages its own token. Reusing Codex CLI credentials does not implement it |
| Anthropic | [Consumer Terms](https://www.anthropic.com/legal/consumer-terms) effective 2025-10-08; [Claude Code setup](https://docs.anthropic.com/en/docs/claude-code/getting-started); [Pro or Max plan documentation](https://support.anthropic.com/en/articles/11145838-using-claude-code-with-your-pro-or-max-plan) | Do not offer Claude Code as a subscription worker. Section 3 bars automated or non-human access unless an API key is used or Anthropic explicitly permits it. The reviewed sources do not grant ARK that permission |

The implementation enforces this result with `WorkerOffer::discoverable`: only Grok is revealed
by the chooser. Hidden compatibility entries keep existing explicit Claude Code and Codex specs
usable and named, but expose no setup action through the new surface.

## Consequences

Existing Claude and Codex chat-model behavior and workers with checked connectors remain
unchanged and undisclosed by the new selector. New Grok users see one provider-owned sign-in on
each computer they use. ARK can add later ACP agents without
becoming an identity or token proxy. ARK-managed Cloud cannot use consumer subscriptions through
this path. A worker remains an uncontained process with the rights of its operating-system user,
as recorded in ADR 0043. This decision prevents ARK from handing it secrets. It does not claim
the worker cannot read files, sockets or credential stores that the same operating-system user
can read.

## Check

The plan, design and implementation QA gates passed. The implementation tests the isolated Grok
home and fixed rules, secret-free probes, fail-closed permission shapes, hidden compatibility
workers, local and remote setup, concurrent setup reattachment, runtime switching and the late
terminal-close race. `ARK_UI_PAGE_TESTS=1 RUST_TEST_THREADS=1 scripts/ci.sh` passed on 2026-10-11,
including clippy with warnings denied, the complete Rust and WebKit suites, version 0.20.0, the
release build and the simulated benchmark against the committed baseline.
