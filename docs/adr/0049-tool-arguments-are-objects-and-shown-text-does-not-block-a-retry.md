# 0049. Tool arguments are objects in every reply schema; text already shown does not block a retry

Status: Accepted
Date: 2026-10-07

## Context
An ARK agent on Codex stopped with `codex sent a malformed reply: arguments for files.edit are
not JSON: expected `,` or `}` at line 1 column 1573`. The Codex session file shows why: the
reply schema (`ark_model::cli::REPLY_SCHEMA`) carried each call's arguments as a JSON string,
because OpenAI's strict schemas forbid an open object. The 6,043-character edit was JavaScript
inside a JSON string inside JSON, and one quote was left unescaped. The grammar could not catch
it, since to the grammar the arguments were any string. Claude Code had the same problem until
its arguments became an object (`CLAUDE_REPLY_SCHEMA`, `bench/RESULTS.md`).

The error is transient (`ProviderError::Malformed`), but the turn still stopped. The harness
retried only when no text of the attempt had been shown, and the Codex app server had already
streamed the reply's note. The rule existed so that a second answer would not appear after half
of a first one. But it tied recovery to display: anything shown made any failure fatal. The same
held for an API stream that breaks after some text (an `overloaded_error` event mid-stream).

## Decision
| Question | Decision |
|---|---|
| Where tool schemas live | Unchanged: `ToolDef.parameters` is plain JSON Schema at ARK's boundary. Each provider adapter turns it into what its vendor takes. |
| Codex's reply schema | Built per call from the tools offered (`ark-model/src/reply.rs`). `tool_calls.items` is an `anyOf` with one closed variant per tool: `name` is a one-value `enum`, `arguments` is the tool's schema in OpenAI's strict form. The grammar checks the arguments, and they arrive as an object. |
| Making a schema strict | The schema is rebuilt from the keywords strict mode allows. Every object is closed and every key required. A key the tool does not require may be null, and that null is dropped when read back unless the tool's own schema accepts null. `oneOf` becomes `anyOf`, `const` a one-value `enum`, and an `enum` always carries its `type`. Descriptions and constraint keywords (`minLength`, `pattern`, ...) are left out: the prompt carries the tool's full schema, and the tool checks its own input. |
| What strict mode cannot express | That part alone travels as a string holding JSON and is parsed back: an open object, `$ref`, a tuple, `not`/`if`, a multi-branch `allOf`, nesting deeper than 10 levels (objects and arrays counted), or `anyOf` branches that cannot be told apart by their keys. When the whole arguments object is open, it is a string as before. A schema over OpenAI's limits (5,000 properties, 1,000 enum values, 120,000 characters) uses `REPLY_SCHEMA` for the whole call. |
| Reading back | Exactly the inverse of the transform (`Decode`). A part due as JSON that does not parse is a `Malformed` reply. Arguments are never repaired or guessed. |
| A schema Codex refuses | `invalid_json_schema` marks that model (`codex:<model>`, process-wide) to use `REPLY_SCHEMA` from then on, and the call fails as `Malformed`, which is retried. A vendor session reopens after any failure, so the retry does not resend a delta to the old thread. The failed session is discarded unless its files are kept. |
| Session resumes | `Delta.tools` carries every tool available now, so a resumed thread's turn gets the schema of the current tools. |
| Claude Code | Unchanged: `--json-schema` takes open objects, so arguments were already objects. A live process fixes its schema when it starts and tools change within a session, so per-tool variants would cost a new process at each change. |
| API providers | Unchanged: native tool calling. |
| ARK's own tools | `files.edit` lists its ten edit shapes as closed variants, and `files.write` gives the type of a row's cells. `crates/ark-sdk/tests/reply_schema.rs` keeps every ARK tool typed, with no part sent as a string. |
| Retry after shown text | Recovery no longer depends on display. A transient failure is retried whether or not text was shown. Before the `Retry` event, the harness sends `HarnessEvent::TextWithdrawn { text }` with what that attempt streamed. The retry then streams its own text. Tools run only after a model call succeeds, so a retry never runs a completed tool again. |
| Front-ends | The window removes the withdrawn text from the element it streamed into. The turn record drops it, so a reopened chat does not show it. Text mode on stdout ends the line and compares the answer with the retry's text only. The terminal ends the line, and the retry's warning row follows: a terminal cannot take printed text back. |

## Consequences
- Codex cannot send arguments that are not JSON for the parts it types. Measured live
  (2026-10-07, Codex 0.153, ChatGPT plan): a `files.edit` call with quotes, `\n`, nested arrays
  and JavaScript came back exactly. With all 44 of ARK's tools offered, the schema was accepted.
- Cost: the typed schema adds input tokens (all 44 tools: 21,390 against 18,365 with
  `REPLY_SCHEMA`, same prompt, uncached `codex exec`). Output fell from 448 tokens (118 of them
  reasoning) to 242, since the model no longer escapes JSON. Optional keys are written as `null`.
  A turn usually offers fewer tools.
- Every provider gets the typed `files.edit` and `files.write` schemas. Simulated bench
  (2026-10-07): `files-edit` 11,592 → 13,730 tokens (+18%), `workbooks` 21,163 → 22,547
  (+6.5%), the other 20 workloads unchanged; `bench/baseline.json` regenerated for it. With the
  earlier Files schemas and ADR 0050's changes, the two read 11,600 and 20,843.
- An optional key whose own schema accepts null cannot be left out by Codex: it is sent as null.
  `properties` without `additionalProperties` is treated as closed.
- A retry after shown text pays for a whole new answer. Before, the turn failed.
- In a terminal, the text of a failed attempt stays above the retry's warning.
- Ruled out: repairing malformed arguments, and a retry rule that depends on what was shown.

## Check
- `crates/ark-model/src/reply.rs`: strict form, null handling, fallbacks, variants told apart by
  keys, depth and size limits, a long edit with quotes and code read back exactly, a part that is
  not JSON rejected.
- `crates/ark-model/src/cli.rs`: the app server and `codex exec` get the tools' schema, a resumed
  thread gets the current tools' schema, long calls and several tools in one reply arrive whole,
  a refused schema switches the model (in every lane) and is retryable.
- `crates/ark-sdk/tests/reply_schema.rs`: every ARK tool is a typed variant, nothing as text;
  `ARK_REPLY_SCHEMA_OUT` writes the schema for a live check with `codex exec --output-schema`.
- `crates/ark-runtime/tests/retry.rs`: text then a failure is withdrawn and retried; a malformed
  reply after its note recovers and its tool runs once; a completed change is not repeated; every
  attempt withdraws its own text; a bad request after text is not retried; retries stay bounded.
- `crates/ark-ui/tests/page.rs` `text_of_a_failed_attempt_is_taken_back_and_the_retry_shows_once`
  (WebKit): the failed attempt's text leaves the page, the retry's shows once, and the turn
  replays the same.
