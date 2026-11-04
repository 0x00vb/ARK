# 0095. Native Computer uses agent-desktop

Status: Accepted
Date: 2026-11-01

## Context

The owner chose to replace ARK's native automation implementation after reviewing Vercel
Labs' agent-desktop. ARK already uses accessibility observations and numbered controls.
The change reuses upstream target resolution, action validation and delivery reporting.
It does not change who authorizes an action or what proves an external effect.

## Decision

| Boundary | Contract |
|---|---|
| Dependency | Pin the Rust core and macOS adapter to agent-desktop 1.0.2, commit `a47ed664e0db00d1d4df364eb3fe8a5599882f3c` |
| Host | Link into the existing executable. No npm launcher, separate CLI host or snapshot disk store |
| Native identity | One dedicated thread owns the macOS adapter and retained accessibility objects. Other threads carry immutable reference entries, never native handles |
| Observation | Build upstream snapshots in memory. Map native identifiers, values, descriptions, focus and logical geometry separately. Reject incomplete observations and projection overflow |
| Output budget | Public observations show at most 150 elements and 8000 text characters, prioritize focused fields and controls, and disclose truncation. Private evidence remains complete up to 4096 elements and 1 MiB of text |
| Window | Use the exact upstream window identity. A matching title never establishes identity |
| Controls | Keep ARK's per-owner numbered controls. Execute the corresponding immutable upstream entry through its strict validation pipeline |
| Replacement | Focus the bound field, select its contents, then replace the selection without refocusing. Revalidate between steps and prove the whole resulting value |
| Delivery | Stop a sequence on any failure. Do not retry a native press through a second mechanism when delivery is uncertain |
| Submission | Bind Return to the exact window proved by message evidence. Refuse when the approved input target differs |
| Revocation | Scope queued work to the service generation and caller lifetime. Check again at native mutation entry after waits. Work already dispatched cannot be recalled |
| Lifetime | Prune retained objects against live control entries. A queued operation keeps its entry alive. Ending the backend thread releases its native objects |
| ARK policy | Keep approvals, human quiet waiting, one-owner leases, Stop/off and draft/send/confirmation unchanged |
| Platform scope | Qualify macOS first. Upstream platform coverage alone does not enable Windows, Linux or remote Computer access |

The `Screen` seam remains available to applications and tests. `Control` adds default click
and whole-field replacement methods so a backend can perform target validation without
the service falling back to global pointer input.

## Consequences

- ARK maintains the product contract and the upstream integration rather than its own AX
  reader and action implementation.
- The host executable retains its macOS permission identity. Signed-app onboarding must
  still be qualified before distribution.
- Existing message safety records remain authoritative. Check again first takes screen
  ownership back and revokes queued input, including when recovery later refuses. It reads
  all native app windows and requires readable, empty matching composers with the same conversation,
  composer and recipient MAC. A recreated window can recover without reusing its old number.
  Missing evidence or any matching nonempty composer keeps the block. Recovery never confirms
  an external effect or retries a send. In-session submission still binds the exact window.
- Recovery audits the full AX window list against all layer-zero graphics windows and the
  upstream inventory before and after reading. Missing or contradictory coverage refuses
  recovery, including omitted hidden windows. Recovery observations do not replace the
  lease holder's pointer or keyboard target.
- macOS can report auxiliary graphics windows that have no accessible window identity.
  These cannot be safely distinguished from a hidden conversation, so automatic recovery
  can remain unproved even after the visible draft is cleared. The owner's Review
  control offers an explicit acknowledgement. It uses a fresh nonce bound to the exact
  safety record and generation, invalidated on every record change or newer review.
  Acknowledgement revokes the old owner and durably clears only the block. It never confirms
  an external effect or permits an automatic retry. No agent action exposes this operation.
  The control uses the same owner-authenticated channel as Stop; it does not attest physical
  presence at the machine. Saved block status is independent from transient activity and is
  restored when Computer is enabled. Status reads while off never open the safety key.
- Names retain the old direction-mark, space and phone-number normalization. Field values
  remain exact. Recovery accepts old role/name identities and resolves old AX identifiers
  through upstream's complete typed identifier query when DOM identifiers are preferred.
  A legacy alias must resolve uniquely to the same retained object in the observation.
- No task-success or performance improvement is assumed. Qualification records native
  behavior and compares task outcomes before promoting the candidate.
- The original upstream license is retained in `third-party/agent-desktop/`. Desktop includes
  it in its native bundle resources.
- Upstream documents macOS13 or later. This host qualification does not establish support
  for older macOS versions that the Desktop shell can run on.
- The disposable native fixture establishes the integration contract. Real WhatsApp and
  other app-specific event handling, signed-host permission onboarding, and distribution
  remain separate qualification work.
- Future dependency updates require targeting, revocation and message-evidence regression
  checks. The Rust crates are unpublished, so ARK owns compatibility with the pinned source.

## Verification

Evidence and remaining gates are recorded in
`../board/evidence/agent-desktop-migration-20261101/` at the workspace root.

## Native bridge build target

Core0.33.1 retains the pinned macOS crate under third-party/agent-desktop/macos. Only its
standalone manifest and build target input differ. VENDOR.json records the original bytes.
The core types still come from the original exact upstream Git revision.

The bridge uses AGENT_DESKTOP_MACOS_DEPLOYMENT_TARGET, default10.15 as upstream, while the
Desktop application retains its macOS11 target. This isolates library source availability
from the consumer's target and keeps all warning flags enabled. It intentionally restores
the earlier deprecation diagnostic range. The deprecated bare-name resolver remains for
behavior compatibility. Replacing that resolver is a separate change with native launch
and cancellation requirements.
