# 0058. A native device pairs to one revocable credential
Status: Accepted
Date: 2026-10-10

## Context

Browser pairing ends in an HttpOnly cookie and protects changes with same-origin checks. A native
app has no trusted page origin or cookie boundary. Its credential has the runtime owner's full
authority, including project writes and shell-backed actions. A lost or restored phone must not
keep that authority accidentally.

## Decision

A signed-in client creates an eight-character, single-use pairing code. The native app sends the
code to an address validated as HTTPS. The runtime returns one random credential, immutable
server-side device id, expiry and mutation epoch. It keeps only the credential hash. A client
installation nonce may correlate diagnostics, but it is never authorization identity. The
server-generated record and credential binding are the only identity used for authorization and
revocation. The editable device name is not identity.

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
- Pairing needs a new endpoint and server-side immutable device records.
- Hiding powerful controls on the phone does not reduce the credential's authority.

## Check

- Tests cover one-use and expired codes, rate limits, wrong-code invalidation and HTTPS refusal.
- Security tests search app containers, logs, URLs and crash fixtures for the credential.
- Device tests cover launch, backup/restore expectation, reinstall, revoke, 401 and sign-out.
- Revocation closes SSE, refuses every mutation and removes the notification route.
