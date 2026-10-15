# Completed-work integration

Board: `35c94426`. Base: `f482160852a0d4fd1626d2b4c2d1bd3d815c7d22`.
This is local source integration, not a public candidate or release.

## Source chain

| Step | Source | Applied here |
|---|---|---|
| Existing main | Voice merge `f482160`, including the faster test profile and remote-provider fixes | Unchanged base |
| Efficiency `958effaa` | `11eee32` plus the frozen `efficiency-2026-10-15/final.patch` | Once, with documentation reconciled |
| Startup `c3ed5945` | The same efficiency baseline plus `catalog-startup-2026-10-15/startup.patch` | Once, after efficiency |

The startup-only patch changes two model files. Its full worktree diff already includes
efficiency and must not be applied again. Production, test, baseline and supporting document
hashes match the completed startup tree. `manifest.json` records the exact identities.
Main's voice, provider-routing code, lockfile, test profile and release instructions stay
unchanged. IFC/A5 and its unneeded Core prerequisites are excluded. Original dirty trees
and owner evidence are preserved.

The two evidence folders are byte-for-byte copies of the completed owners' folders.
Their original statements about uncommitted state, open follow-ups and card aliases are
historical records, not the current integration status. The efficiency protocol sentence
about a further regression rejecting the prompt is a rejection contingency. The final
accepted patch includes both the state-update prompt and corrected approval mapping,
supported by the fresh 30/30 condition. It is not the isolation-only patch or the rejected
29/30 candidate. Vendor configuration drift prevents causal savings or latency claims.

Evidence-tree digests sort relative paths with JavaScript `localeCompare(path, 'en')`, then
hash the concatenation of `path`, NUL, each file's SHA-256 and newline. File count and total
bytes are also recorded. No raw user settings or credentials are copied.

## Verification

Independent plan/design QA passed before edits. Both owners' full gates and final QA pass.
Those checks do not certify this combined source. The combined model suite passes 99/99
(`model-tests.log`). Focused approval and request-profile checks pass 18/18. Authenticated
remote dictation, remote sign-in, the remote model-picker browser scenario and split-chat
dictation browser checks all pass (`focused-regressions.log`, tool-output excerpt).
The exact combined page-enabled full gate passes (`full-ci.log`): formatting, protocol,
version, lint, every workspace test, 56 real WebKit scenarios, release build and all 22
simulated workloads. The tested staged diff is identified in `manifest.json`. Final evidence
and state updates add no production changes. Independent final QA passes (`qa.md`) for the
local main update. No new live model matrix or paid call is part of this integration.

## Remaining release and device gates

| Gate | Status |
|---|---|
| Physical microphone and multi-minute transcription quality | Not run on macOS, Windows or Linux; release follow-up |
| Clean-Mac first-install and onboarding observation | Not established by these integration tests |
| Native consumer validation of the combined runtime | A future immutable release and consumer pin are required |
| Fresh-clone release verification, signing, packaging and public download checks | Not run for this untagged source |
| Push, tag, release or publication | Not authorized or performed |

The separate hosted API-key evidence gate and real-VPS subscription check remain open.
The synthetic writing-app checks retained in the owner reports do not close these gates.
