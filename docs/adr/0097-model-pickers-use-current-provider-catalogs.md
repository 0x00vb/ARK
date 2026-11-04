# 0097. Model pickers use current provider catalogs

Status: Accepted
Date: 2026-11-04

## Context

Codex discovery was live, but an old installed CLI advertised an old catalog. Claude,
OpenAI and Anthropic picker entries used suggested names. OpenCode Go filtered its public
catalog through a fixed model list. Endpoint listings could remain cached for five minutes.
These paths hid additions and retained removed models until ARK changed.

## Decision

| Data | Authority |
| --- | --- |
| Codex models and effort | The connected CLI's complete model/list response |
| Claude models and effort | The connected CLI's initialization response |
| API models | The connected account's complete provider model listing |
| OpenCode Go models | Advertised chat models whose metadata names the supported chat-completions transport |
| Named endpoint models | The endpoint's complete model listing |

Backend.catalog_state is optional on the wire. Current means a complete valid response,
including an empty list. Unavailable means discovery failed. An omitted state retains old
client behavior or an actual connection step. Discovery does not establish paid inference
entitlement. Effort choices come from explicit provider metadata, never model-name guesses.

The picker queries when opened and every 30 seconds while visible. Requests do not overlap.
The next completed refresh applies additions, removals and changed effort choices. Metadata
requests are bounded. HTTP pagination has one six-second deadline, a one-MiB page limit and
a twenty-page limit. Incomplete responses are errors, never partial catalogs.

No successful or failed endpoint listing is cached across refresh requests. Account-specific
responses are discarded if their account revision changed while discovery ran. The selected
spec stays unchanged when unavailable or removed. There is no silent model fallback.

The existing model controls show discovery failure or an empty catalog with Retry. They do
not ask for a new key or sign-in when the connection is already configured. Refresh preserves
focus, input caret and scroll. Unchanged catalog data does not rebuild the view. Refresh has
no transition. Closing or hiding the view stops periodic requests.

## Alternatives

| Option | Result |
| --- | --- |
| Ship updated static names | Rejected. Every provider change would require another ARK release. |
| Assume a failed listing means disconnected | Rejected. It sends people to replace working credentials. |
| Replace the selected model after removal | Rejected. It changes quality, price and execution identity without a decision. |

## Consequences

Provider discovery may need a newer vendor CLI. Updating ARK's own model names cannot fix
an obsolete CLI catalog. The local Codex installation was updated from 0.153.4 to 0.166.0;
the real ARK catalog then returned eight models including GPT-6.1 Sol and Terra without
adding those identifiers to ARK.

Rust callers constructing Backend add catalog_state, usually None for custom backends.
Old JSON clients ignore the optional field. Provider metadata probes make no inference call.

Verification is recorded in ../board/evidence/ark-daily-use-20261104 from the workspace root.
Live quality, protected execution and release qualification are separate gates.
