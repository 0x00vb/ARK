# Running ARK

What an ARK application does when things go wrong, how to see what it is doing, and the
limits that keep a long-running session bounded. Everything here is generic: it applies to
any application on ARK.

## Settings

| Variable | Default | Meaning |
|---|---:|---|
| `ARK_PROVIDER`, `ARK_MODEL` | `ollama`, per provider | the model; two tiers with `ARK_PROVIDER_FAST` / `ARK_MODEL_FAST` and `ARK_PROVIDER_STRONG` / `ARK_MODEL_STRONG` |
| `ARK_INFERENCE_EXECUTABLE`, `ARK_INFERENCE_*` | none | with `ARK_PROVIDER=inference`: the local engine executable, backend, limits and deadlines ([`LOCAL_INFERENCE.md`](LOCAL_INFERENCE.md)) |
| `ARK_CTX_BUDGET` | by model | prompt-token budget per request (tool schemas not counted). Unset: a quarter of the reasoning model's input window, 6,000 to 48,000 and at most half the window; 6,000 when the window is unknown (ADR 0050) |
| `ARK_MODEL_RETRIES` | 4 | retries of a model call after a temporary failure |
| `ARK_MODEL_OFFLINE_S` | 60 | how long a model that cannot be reached (no connection, a timeout) is waited for, past the retries |
| `ARK_HTTP_CONNECT_TIMEOUT_S` | 10 | connecting to a provider |
| `ARK_HTTP_IDLE_TIMEOUT_S` | 120 | silence while waiting for the next bytes of a reply |
| `ARK_HTTP_TIMEOUT_S` | 600 | a whole non-streaming request |
| `ARK_MAX_OUTPUT_TOKENS` | Anthropic 8192, Ollama 4096, OpenAI none | output cap per request |
| `ARK_LOG` | `warn` | log level and filter (`info`, `debug`, `ark_runtime=debug`, ...) |
| `ARK_TRACE` | `full` | stored turn traces: `full`, `summary` (no text, no tool inputs) or `off` |
| `ARK_PRELOAD_ACTIONS` | 2 | actions whose schemas are offered before the model asks for them |
| `ARK_READY_LIST` | on | name the loaded actions in the first message; `0` turns it off |
| `ARK_INLINE_ROWS` | on | show small arrays in a tool result whole; `0` shows only their count |
| `ARK_WHOLE_RESULT_TOKENS` | 0 | give a result whole when it is under this many tokens (0: always the summary) |
| `ARK_PRICE` | none | `in,out[,cached]` US dollars per million tokens, for `/usage` |
| `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL` | none, `https://api.anthropic.com/v1` | Anthropic provider |
| `OPENAI_API_KEY`, `OPENAI_BASE_URL` | none, `https://api.openai.com/v1` | OpenAI, or one compatible endpoint in its place |
| `ARK_ENDPOINTS`, `<NAME>_API_KEY` | none | named OpenAI-compatible endpoints, `name=url,name=url`, each with its key ([below](#named-endpoints)) |
| `OLLAMA_HOST` | `http://localhost:11434` | Ollama provider |
| `ARK_CLAUDE_BIN`, `ARK_CODEX_BIN` | found on `PATH` and the usual install places | the CLI behind the `claude` / `codex` providers |
| `NO_COLOR` | unset | no colour in the terminal shell |

Development only: `ARK_PROVIDER=mock` with `ARK_MOCK_SCRIPT` and `ARK_MOCK_DELAY_MS`
([`CLI.md`](CLI.md#testing-an-app-without-a-model)); `ARK_UI_WEB`, `ARK_UI_ASSETS`
([`UI.md`](UI.md#developing-the-page)); `ARK_UI_PAGE_TESTS`, `ARK_STRESS_TURNS`,
`ARK_STRESS_CONNECTS` for tests ([`CONTRIBUTING.md`](../CONTRIBUTING.md)).

## Subscriptions

`claude` and `codex` use the person's own subscription through the vendor's official CLI.
ARK never reads their credentials.

| Provider | CLI | Sign in | Default model |
|---|---|---|---|
| `claude` | Claude Code (`claude`) | `claude auth login` | `sonnet` (also `opus`, `fable`) |
| `codex` | Codex (`codex`) | `codex login` | Codex's own default (`-m` when a model is named) |

How a call works: the CLI runs with its own tools off (`claude --tools ""`; Codex read-only,
told it has none), in an empty directory. ARK's tools and the app prompt go in the system
prompt, and the reply is JSON checked against a schema (`reply` with text, or `call_tools`).
Codex's schema is built from the tools of the call, so each call's arguments arrive as an object
the grammar checked ([ADR 0049](adr/0049-tool-arguments-are-objects-and-shown-text-does-not-block-a-retry.md));
Claude Code takes arguments as an open object. ARK's loop runs the calls, so approvals, traces
and budgets behave as with an API.

A conversation or agent on a subscription keeps its CLI running between calls
([ADR 0012](adr/0012-vendor-cli-processes-live-with-their-session.md)): Claude Code with
streaming JSON in and out, Codex as `codex app-server` holding the session's thread. The
answer streams while the model writes it. A cancelled turn stops the process, and the next
turn opens a new vendor session from ARK's own request. Stateless calls (planner, critic,
search) start the CLI once. If `codex app-server` cannot start, Codex falls back to
`codex exec` (a warning in the log).

Costs, measured with `minimal-agent` on 2026-10-01 (one question, two tool calls):

| Provider | Time | Tokens |
|---|---:|---:|
| `claude:sonnet` | 15 s | 8.8k |
| `codex:default` | 34 s | 38.9k |

Codex adds about 15k to 18k tokens of its own instructions and skills to every call. On a
running process a call skips the CLI's start-up (Claude Code 2.7 s to 0.7 s, Codex 23 s to
1.7 s for a short reply, 2026-10-04). A subscription's usage limit fails the turn at once with
the time it resets; an overloaded service is retried. What each CLI last reported about the
account's limits (a five-hour and a weekly window: share used, reset time) is in
`ark_model::subscription_usage()` and in the orchestration graph's `accounts`. For Claude Code, ARK's tools are listed as bullets that say they
are used only inside the structured reply: as Markdown headings, Haiku tried to call them
natively and gave up the task (`bench/protocol/`).

### Managed sessions and capacity

A conversation (`Session`) on a subscription, and each orchestrated agent, keeps one vendor
session of its own (`ark_model::session`); the state is in the store, so the next process
resumes it. A resumed call sends only what the vendor has not seen; the session
rotates before its context passes the limit and rehydrates from ARK's own request. Measured:
`bench/RESULTS.md` (orchestration). Details: [`ORCHESTRATION.md`](ORCHESTRATION.md).

| Variable | Default | Meaning |
|---|---|---|
| `ARK_SESSION_REUSE` | `1` | `0`: every call opens a fresh vendor session |
| `ARK_SESSION_MAX_CONTEXT` | `64000` | rotate before the vendor context passes this many tokens |
| `ARK_SESSION_MAX_TURNS` | `100` | rotate after this many calls in one vendor session |
| `ARK_SESSION_IDLE_S` | `1800` | `sweep_idle` releases a session unused this long |
| `ARK_SESSION_KEEP_FILES` | `0` | `1`: keep the vendor's file of a session ARK let go |
| `ARK_<ACCOUNT>_CONCURRENCY` | `2` for `claude`, `codex`; `4` otherwise | requests in flight per account (`ARK_CLAUDE_CONCURRENCY=3`) |

A usage or rate limit reported by a CLI is a temporary error: the account cools down and the
harness retries. When ARK lets a vendor session go (rotation, release, eviction) it deletes
the vendor's file for that session id only: Claude Code's under
`~/.claude/projects/<ARK's work dir>/`, Codex's `rollout-*-<id>.jsonl` under
`~/.codex/sessions/`. Sessions still open when the app exits keep their files and are resumed
next time.

## Named endpoints

Any service that speaks the OpenAI chat API (a router such as OpenRouter or LiteLLM, a vLLM
server, a gateway an app provides) can be a provider of its own, beside the built-in ones
(ADR 0027):

```sh
ARK_ENDPOINTS=router=https://openrouter.ai/api/v1 ROUTER_API_KEY=sk-... \
ARK_PROVIDER=router ARK_MODEL=anthropic/claude-sonnet-4.5 app
```

| | |
|---|---|
| In a spec | `router:anthropic/claude-sonnet-4.5`, `/model router <model>`; a bare model keeps the endpoint |
| From an app | `ark_model::endpoints::register(Endpoint::new("router", url).label("OpenRouter").key(k))`. Without `.key`, the key is `<NAME>_API_KEY` from the environment or the app's key store |
| Names | lowercase letters, digits, `-`, `_`; `claude`, `openai` and the other built-in names are refused |
| Models | the endpoint's `GET /models`, in the model picker under its label; reused 5 minutes. One that cannot be reached shows as not ready |

## When a model call fails

A provider error is classified as temporary or not (`ProviderError::is_transient`):

| Temporary (retried) | Not retried |
|---|---|
| connection failures and timeouts, 408, 425, 429, 5xx, 529, an overload event inside a stream, a reply schema the vendor refused (`Refused`) | 400, 401, 403, 404, other client errors, missing credentials, a malformed protocol message; 402 is out of credit (below) |

The harness retries a temporary failure with exponential backoff and jitter (500 ms doubling,
at most 30 s per wait, never shorter than the server's `retry-after`), up to
`ARK_MODEL_RETRIES` times. When the model cannot be reached at all (no connection, a timeout:
a laptop's network gone), it keeps trying until `ARK_MODEL_OFFLINE_S` have passed since the
first failure. A turn that still fails, or that the app's stop or a crash cut short, is
continued by the person's next message (ADR 0048). Two rules keep retries safe:

- Text a failed attempt had shown is withdrawn first (`HarnessEvent::TextWithdrawn`), and the
  retry streams its own: the window takes it back, text mode ends the line, the terminal keeps
  it above the retry's warning (ADR 0049).
- A retry repeats the model request, never a tool. Tools run only after a model call
  succeeds, so a state-changing action cannot run twice because of a retry.

A reply the model wrote that cannot be used (`ProviderError::Malformed`: tool-call arguments
that are not valid JSON, a reply that is not structured or was cut off) is not sent again
unchanged. Its text is withdrawn and the model is told what was wrong, in a new round, and
sends it again (`Retry { repair: true }`; the window shows "Redoing its last step"). At most 2
in a row and 4 in a turn; then the turn fails with `AgentError::UnusableReply`, shown as "The
model's reply could not be used." with Continue (ADR 0050). On a vendor session the failed
reply ends the session, so a repair is a full, uncached request.

A 402 means the account behind the key has no credit left: `ProviderError::OutOfCredit`. The
turn stops at once with the provider's own message, and both front-ends add what to do ("Add
credit to your OpenRouter account, or pick another model.", `TurnEnd::out_of_credit`).

Every retry is an event (`HarnessEvent::Retry`), a count on the model call
(`ModelCall.retries`), in the metrics (`provider_retries`) and a `warn` log line.

A provider that goes silent fails after the idle timeout instead of hanging. A reply cut off
inside a tool call is an error, not a call with empty arguments.

## Loops

The stall guard ends a turn that stops making progress. Progress is new information: a tool
result the turn has not seen, the first use of a retrieval or a state edit, a newly loaded
action. Three steps in a row without it get a nudge to change course; five end the loop and
force a final answer. Repeating a search, a hypothesis or a failing call does not count. The
step limit (16 by default) still applies to work that keeps making progress.

An identical failed read or observation runs once and gets one retry. A third attempt returns
immediately and tells the model to change the input, use another tool or answer. A successful
call clears its own failure count. A successful state-changing call clears all read failure
counts because it may have repaired their dependency. The limit is per turn. It does not reuse
a failed result or create an invocation for a call that did not run.

## Reuse of earlier results

An identical successful call may be served from its earlier result instead of running again.
Failed, cancelled and partial results are never reused.

| `Reuse` | Within a turn | Across turns |
|---|---|---|
| `Auto` (default) | until an action that changes state runs | only a `Read` of a stored artifact (content-addressed bytes) |
| `Stable` (`.reusable()`) | always | always |
| `Volatile` (`.volatile()`) | never | never |

State-changing actions are never reused. A short artifact id and the full id are the same call.

## Parallel tool calls

When the model asks for several `Read` or `Observe` calls in one reply, they run together, at
most four at a time (`MAX_PARALLEL_TOOLS`). Results, events and messages keep call order. A
state-changing call, an approval, a runtime tool or a reused result first finishes the pending
batch, so a write never overlaps the reads around it.

## Interrupted work

| What stopped | What the next open shows |
|---|---|
| the process, during a tool call | the call, failed: "interrupted: the process stopped before this call finished" |
| the process, during a turn (streaming, a tool, waiting for approval) | the turn, kept and marked incomplete with `session::INTERRUPTED`; no answer is shown; the next turn's carry-over says it did not finish |
| a turn, with an error | the turn, marked incomplete with the error |
| a stored turn that cannot be parsed | that turn, marked unreadable; the rest of the session loads |

A process that is still running keeps its in-progress turn and calls: recovery checks the
recorded process id first. Blobs are written to a temporary file and renamed, and the
database is SQLite in WAL mode, so a kill never leaves a half-written record.

## Logs and traces

Logs are structured and contain no conversation content: counts, names and durations. A
prompt, an answer, a tool input or a tool output never reaches the log.

| Level | Adds |
|---|---|
| `warn` (default) | retries, failed model calls, plugins that failed to load |
| `info` | stalls, failed tool calls (action and a clipped error), one `turn finished` line per turn: steps, model calls, tokens, cached tokens, tool calls, duplicates, retries, time |
| `debug` | every model call: role, model, tokens, time, time to first token, retries |

The shell and the workspace log to `<store>/ark.log` (moved to `ark.log.1` when it is over
5 MB at start); a headless `-p` run logs to stderr. An application can install its own
logger first (`ark_interact::logging::init`).

Each turn's trace is stored for replay (`ARK_TRACE`). Traces are kept for the last 1,000
turns of a session; the transcript keeps every turn.

## Long-running sessions

`crates/ark-ui/tests/resource_bounds.rs` runs turns through the workspace host, each with a
real subprocess, then opens and drops browser connections. Measured with
`ARK_STRESS_TURNS=2000` on an M4 Pro (debug build):

| turn | RSS (MB) | open fds | child processes | store (KB) | turn (ms) |
|---:|---:|---:|---:|---:|---:|
| 50 | 19.5 | 15 | 0 | 4340 | 3 |
| 500 | 21.5 | 15 | 0 | 7769 | 4 |
| 1000 | 21.6 | 15 | 0 | 11597 | 5 |
| 1500 | 21.6 | 15 | 0 | 15249 | 5 |
| 2000 | 21.6 | 15 | 0 | 18901 | 5 |

After 300 browser connections opened and dropped: 15 fds, no child processes. The store grows
about 7 KB per turn (turn, trace, invocation, evidence card, note). Bounds that keep memory
flat: the session's carry-over window, 16 turn records in memory, a 20,000-event replay buffer
for the running turn, an 8,192-event broadcast channel, and counts computed in the database.

## The workspace event stream

Events carry a sequence number. A window that reconnects calls `hello`, which says how far
its answer reaches and includes the running turn's events so far; the page drops what it has
already shown, applies what arrived while it was loading, and reloads on a gap. A browser
reconnects with backoff (1 s doubling to 30 s) and stops after ten failures with a Retry
button. The HTTP server answers only the loopback host names and requires the per-run token.
