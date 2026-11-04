# 0098. Codex model calls use a qualified host transport

Status: Implementation candidate. Product acceptance remains open.
Date: 2026-11-04.
Amends: 0012, 0049, 0094 (automation sequences).

## Problem

Codex is an agent program. Disabling its shell, apps and web search does not remove
its other tools. A read-only sandbox still permits reads outside an automation's
path grant. A model instruction cannot establish ARK's approval boundary.

Codex 0.166 also derives tools from model metadata. Disabling every feature still
leaves Code Mode and delegation when that metadata selects them. GPT6 requests can
carry these tools in `input[type=additional_tools]`, without top-level `tools`.

## Decision

| Boundary | Contract |
| --- | --- |
| Qualified binary | Only the audited official Codex 0.166.0 native macOS arm64 executable. Pin its full BLAKE3 digest against the npm release whose SHA512 integrity was checked. Other builds remain unsupported for protected work. |
| Launcher | Resolve the official npm layout to its native executable and launch that file directly. Bind both launcher and native file identities. Refuse a changed file. |
| Startup | Clear the environment. Retain only home, executable path, temporary directory, locale and TLS certificate paths. Disable active audited features before startup. Hooks, plugins and native apps cannot start through inherited settings. Retired flags are not applied. |
| Model | Discover through the vendor program. Read only its noncredential model cache. Require the matching client version, a recent receipt and the selected model and effort. Preserve the complete non-tool metadata. |
| Execution metadata | Remove patch, shell, search and experimental native tools. Select standard tool mode and no native delegation. Freeze this catalog in a private directory. Check its content hash before each call. Later vendor cache refreshes do not change it. |
| Configuration | Check effective active features. Utility tool switches are omitted by the typed config response, so verify their raw session flag layer and winning origin receipts. Disable every configured MCP server before a thread starts. Unsupported configuration fails closed. |
| Account | Read account metadata through the vendor protocol. Require a ChatGPT account and stable account identity. Store only its fingerprint. Verify the current account asynchronously before every actual model turn. Never read a credential file. |
| Transport | Qualified calls use app-server only. Stateless calls create an ephemeral thread. A failed start or protocol never falls back to exec or native search. |
| Conversation | Resume only an ARK-owned receipt matching the binary, model metadata, account revision and isolation manifest. A legacy or foreign conversation is refused. Default cleanup removes its receipt. Explicit `ARK_SESSION_KEEP_FILES` retention keeps the receipt with the vendor files. |
| Activation | Protected work requires an explicit model slug and effort. A default alias cannot freeze a later provider default. Ordinary chats retain their existing selection syntax. |

A supported ordinary chat also prefers this transport. Unsupported binaries retain
ordinary chat compatibility. A known qualified binary with unavailable metadata
returns an error, rather than switching back to native tools. Failed qualification
can retry after a bounded interval or an executable change. Once qualified, drift
never downgrades the provider.

Capability checks use the cached contract and cheap file identity checks. They do not
start a vendor process or hash the full native executable. Each actual turn hashes
the executable and frozen catalog once, then checks fresh vendor account metadata.
There is no account cache interval that permits a changed account to make a call.

A completed reply without reported usage remains a completed reply. Its usage is
unmeasured. A failed or unusable qualified reply does not retry automatically when
its spending may be partial or unknown. Known partial counts are diagnostic data,
not a claim that the failed call was free.

Receipts prevent accidental foreign conversation reuse and model tool escape.
Other processes running as the same local user are trusted. Metadata changes can
refuse protected resume and require a new chat or activation approval. They never
select another model automatically.

## Alternatives

| Alternative | Reason |
| --- | --- |
| Advertise ARK as native dynamic tools | This adds tools. It does not remove Codex's native handlers. |
| Rely on the native read-only sandbox | This does not establish ARK's path, account and network bounds. |
| Flip `host_controls_tools` | A capability claim needs the enforced transport and dispatcher evidence. |
| Qualify exec at the same time | Its config layering differs. Ignoring user config plus per-server MCP overrides can create an invalid transport. Qualified providers refuse it. |
| Replace the full model metadata with a small template | This changes context, reasoning, input modalities and provider prompts. |

## Evidence and remaining gates

Evidence is under `../board/evidence/ark-daily-use-20261104/schedules/`.
The official native release integrity receipt, failed probes and corrected nested
wire inventories are retained there. Real binary probes inject native patch,
command, image, delegation, plugin, MCP and unknown calls into fresh and resumed
threads. These establish dispatcher behavior without inference.

The implementation gate also requires process cancellation, drift, foreign
conversation refusal and actual subscription calls. Product acceptance requires
an activated protected run, an actual timer occurrence, a refused action and a
restart with durable results and no repeated effects. Do not infer those outcomes
from the metadata probes.
