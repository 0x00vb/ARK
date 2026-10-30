# External clients can opt into a private notification relay

Status: accepted for the owner's private pilot, 2026-10-24. Public anonymous registration is not approved by this decision.

## Context

A phone can be suspended while work continues on its computer. Its foreground event stream cannot provide background notifications. A consumer already registers its phone with an account-authenticated relay. The runtime must not gain Apple credentials or a dependency on that consumer.

## Decision

| Concern | Rule |
|---|---|
| Activation | Optional operator HTTPS origin, exact runtime route and finite pilot expiry. No caller-provided URL or platform token |
| Identity | Exact external-client credential authentication on a dedicated listener or account-bound headless listener. Owner credentials cannot register push. Persisted registrations belong to the credential hash and stable runtime route, not a listener process |
| Secret | Only the relay send capability reaches the runtime. Existing encrypted `FileSecrets` owns it. The phone retains manage authority |
| Payload | Five fixed fields: opaque request, attention and collapse ids, expiry and completed or needs_attention kind. Never titles, prompts, answers or files |
| Timing | Existing attention delay, no notices predating registration, own foreground presence suppresses for 45 seconds |
| Retry | Persist the exact payload before sending. Crash or ambiguous transport means unknown, not retry. Only a relay's explicit retry response permits at most three identical attempts |
| Revocation | Read the credential store before dispatch. Expiry, credential revocation and local removal stop new requests |
| Tap | Resolve an opaque locator for that authenticated device. Read fresh local chat and attention state. A tap never mutates work |

Two designs were considered. Sending APNs directly would distribute a consumer's signing key to generic runtimes. A fixed generic relay permits a bounded capability for one registration and keeps platform credentials in the consumer. The latter is used.

## Limits

Relay acceptance is not proof the phone displayed a notification. Unknown and sending are not reported as completed delivery. In-flight requests can race revocation; no new dispatch is admitted once the revoked credential is observed. No native background event-stream claim is made. Anonymous attestation and arbitrary public self-hosting remain a separate proposed consumer boundary.

## Verification

Focused tests cover endpoint constraints, encrypted and redacted capability storage, owner isolation, no historical spam, foreground suppression, revoked credentials, exact retry bodies and notifier restart. Live sandbox APNs, native tap handling and a signed physical-device build remain consumer acceptance work.
