# Running ARK

What an ARK application does when things go wrong, how to see what it is doing, and the
limits that keep a long-running session bounded. Everything here is generic: it applies to
any application on ARK.

## Settings

| Variable | Default | Meaning |
|---|---:|---|
| `ARK_PROVIDER`, `ARK_MODEL` | `ollama`, per provider | the model (also `ARK_PROVIDER_FAST` / `ARK_MODEL_FAST`, `..._STRONG`) |
| `ARK_CTX_BUDGET` | 6000 | prompt-token budget per request (tool schemas not counted) |
| `ARK_MODEL_RETRIES` | 2 | retries of a model call after a temporary failure |
| `ARK_HTTP_CONNECT_TIMEOUT_S` | 10 | connecting to a provider |
| `ARK_HTTP_IDLE_TIMEOUT_S` | 120 | silence while waiting for the next bytes of a reply |
| `ARK_HTTP_TIMEOUT_S` | 600 | a whole non-streaming request |
| `ARK_MAX_OUTPUT_TOKENS` | Anthropic 8192, Ollama 4096, OpenAI none | output cap per request |
| `ARK_LOG` | `warn` | log level and filter (`info`, `debug`, `ark_runtime=debug`, ...) |
| `ARK_TRACE` | `full` | stored turn traces: `full`, `summary` (no text, no tool inputs) or `off` |

## When a model call fails

A provider error is classified as temporary or not (`ProviderError::is_transient`):

| Temporary (retried) | Not retried |
|---|---|
| connection failures and timeouts, 408, 425, 429, 5xx, 529, an overload event inside a stream, a reply whose tool-call arguments are not valid JSON | 400, 401, 403, 404, other client errors, missing credentials, a malformed protocol message |

The harness retries a temporary failure with exponential backoff and jitter (500 ms doubling,
at most 30 s per wait, never shorter than the server's `retry-after`), up to
`ARK_MODEL_RETRIES` times. Two rules keep retries safe:

- A call is retried only if none of its text reached the user. Otherwise the turn fails: a
  second answer after half of a first would duplicate output.
- A retry repeats the model request, never a tool. Tools run only after a model call
  succeeds, so a state-changing action cannot run twice because of a retry.

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
