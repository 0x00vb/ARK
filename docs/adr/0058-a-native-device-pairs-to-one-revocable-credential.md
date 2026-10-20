# 0058. A native device pairs to one revocable credential
Status: Accepted
Date: 2026-10-10

## Context

Browser pairing ends in an HttpOnly cookie and protects changes with same-origin checks. A native
app has no trusted page origin or cookie boundary. Its credential has the runtime owner's full
authority if the same saved token is reused. A native credential must instead be accepted only by
the typed external-client contract. A lost or restored phone must not keep either authority
accidentally.

## Decision

A signed-in client creates an eight-character, single-use pairing code. The native app sends the
code to an address validated as HTTPS. The runtime returns one random credential, immutable
server-side device id, expiry and mutation epoch. It keeps only the credential hash. A client
installation nonce may correlate diagnostics, but it is never authorization identity. The
server-generated record and credential binding are the only identity used for authorization and
revocation. The editable device name is not identity.

Saved credentials carry `owner` or `external_client`. A missing scope in an older access file is
`owner`. External-client credentials and grants are stored outside the legacy owner arrays, so an
older binary ignores them instead of widening their authority. Browser links remain owner-scoped.
A native QR grant is external-client-scoped and cannot be redeemed by `/session`. An
external-client credential can call only `/client/v1/*`; it cannot authenticate the page, generic
RPC, browser carrier, generic event stream or screen controls.

Account pairing is a second entry to the same credential type. The ARK Cloud API trades the app's
account access token for a 60-second ES256 assertion addressed only to
`urn:ark:runtime:<route-id>`. The account access token never reaches the runtime. The client sends
the assertion with its stable runtime route id, installation id and device name. `ark-account`
fetches and caches the API's separate pairing JWKS and requires the exact API issuer, audience,
`typ=ark-runtime-pair+jwt`, `azp=ark-ios`, `scope=runtime.pair`, route id, owner `sub`, `iat`, `exp`
and `jti`. Signing keys have a bounded cache lifetime. An unknown `kid` refreshes the JWKS at a
bounded cadence. A verified `jti` is consumed once until expiry.
Credential issuance and the persisted `jti` hash share one locked write, so restart does not reopen
the assertion. The registry never returns runtime credentials. Pairing one installation does not
revoke another. Re-pairing the same installation replaces its earlier credential.

The app stores the credential only in Keychain with
`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. It never stores it in UserDefaults, files, URLs,
links, notifications, analytics, crash reports or logs. App Transport Security has no broad
exception. A backup, restored install or second device cannot use the credential. Reinstall pairs
again. The credential lasts 90 days. The app offers renewal while the current credential is valid,
starting 14 days before expiry. An expired credential returns to pairing without losing a draft.

Revocation, expiry, 401 and sign-out cancel event streams, invalidate the mutation epoch, remove
the push route and purge the Keychain item and protected local data. First release persists no
chat or result body. It may persist a bounded, Data Protection protected index of ids, titles and
restoration hints. Drafts stay in memory. A cold offline launch exposes no work.

Before the app becomes inactive it presents an opaque privacy cover, so the app switcher's system
snapshot contains no work. The cover leaves only after foreground authentication state and the
open screen have been revalidated.

Each mutation uses the device epoch and a strictly increasing counter through the durable intent
and receipt protocol in `docs/MOBILE.md`. One device has one mutation in flight.

## Consequences

- Native and browser sessions remain separate security mechanisms with the same revocation list.
- The app stays useful after ordinary launches but never transfers authority through a backup.
- Pairing needs new endpoints and server-side immutable device records.
- The listener boundary, not hidden controls, limits a phone credential's authority.

## Check

- Tests cover one-use and expired codes, rate limits, wrong-code invalidation and HTTPS refusal.
- Tests cover wrong account subject, wrong route, owner-default migration and refusal of every
  non-client HTTP surface.
- Security tests search app containers, logs, URLs and crash fixtures for the credential.
- Device tests cover launch, backup/restore expectation, reinstall, revoke, 401 and sign-out.
- Revocation closes SSE, refuses every mutation and removes the notification route.
