# 0059. Native phone notifications use a blind relay
Status: Accepted
Date: 2026-10-10

## Context

An App Store iPhone app receives remote notifications through APNs. Apple's provider credential
belongs to ARK's developer team and cannot ship inside every self-hosted runtime. Requiring ARK
Cloud for foreground use would break ARK's self-hosted boundary. Sending question or project text
through a shared relay would reveal work the relay does not need.

## Decision

Native push is optional and uses an ARK-operated blind relay in a separate `ark-cloud/push`
service. It does not require an ARK Account.

1. The iPhone registers with APNs on each launch, then registers that token directly with the
   relay over TLS using Apple App Attest. The relay fixes the bundle id, APNs environment and
   generic payload template from the attested app. Development uses an explicit local fake; it
   never weakens production attestation.
2. The relay returns an opaque route id and two random 256-bit capabilities: send and manage. It
   stores only their hashes. The app keeps manage in Keychain and hands route id plus send
   capability to its paired runtime over their authenticated connection.
3. The runtime stores send in its existing secret store. It can ask only for the fixed generic
   payload to that route, with expiry, collapse id, opaque attention id and a unique request id. It
   cannot select another APNs token, topic, environment, title or body.
4. The relay accepts a request id once for seven days, limits each route to 12 requests a minute
   and 200 a day, and returns the first receipt to a retry. A route expires after 90 days unless
   the attested app renews it. Rotation overlaps the previous capability for five minutes.
5. APNs-token replacement is accepted only from the attested app using manage. App sign-out and
   runtime device revocation delete the route using manage or send respectively. Expired or
   invalid APNs tokens retire the route.

The relay retains route id, encrypted APNs token, capability hashes, environment, expiry and last
delivery state. It keeps no IP address or work identifier in durable logs. Request ids and delivery
receipts expire after seven days; aggregate abuse counters expire after 30 days. V1 notification
text is generic. The relay stores no chat, project, question or result text.

A notification tap validates its ids, opens the bound runtime, fetches fresh state and only then
enables an action. No notification executes a mutation. Sandbox and production routes are
separate. Revocation removes the route.

The app and self-hosted runtime remain fully usable in foreground without the relay. Relay outage
delays notifications but does not delay work or foreground refresh. ARK does not claim native
push for arbitrary self-hosting until this path passes production APNs checks.

Universal Links use only an ARK-owned HTTPS domain with its associated-domain file. Arbitrary
self-hosted addresses enter through the app's QR scanner or manual address field. No link carries
a credential or mutation.

## Consequences

- Native notifications add a small operated service and Apple credential rotation.
- The service is optional for self-hosting and blind to work content.
- A stolen send capability can send only bounded generic notifications to its one route. It cannot
  address another device or choose content.
- Generic notification wording reveals less on a lock screen and requires a fetch after tap.
- `ark-cloud` ADR 0018 owns the service, issuance, retention and operational boundary.

## Check

- Relay tests cover auth, rate limits, sandbox/production separation, collapse and expiry.
- App tests cover APNs token rotation, tap routing, resolved items, revoke and relay outage.
- Production APNs is proved on a signed physical iPhone before any availability claim.
- Privacy review confirms relay records contain no work content or device credential.
