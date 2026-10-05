# 0026. An optional ARK Account, signed in through the browser

Status: Accepted
Date: 2026-10-05

## Context
ARK Cloud (ROADMAP step 6) needs to know who it serves: cloud agents, plans, usage. The owner
decided (2026-10-05):
- ARK stays usable with no account.
- Signing in is optional and happens in the browser (Google, GitHub, or a code sent by email).
- The client is an optional ARK crate, so the CLI and products built on ARK can use ARK Cloud
  too.
- The account service lives in its own repo (`ark-cloud`), on Better Auth and Postgres.

ADR 0016 keeps accounts, billing and provisioning out of the runtime: a runtime has an owner,
not users. ADR 0011 already has what a desktop sign-in needs: OAuth 2.0 with PKCE and a
loopback redirect (RFC 8252), and the OS keychain behind `Secrets`.

## Decision
A new crate, `ark-account`, is the client of an ARK Account on one computer. No runtime crate
depends on it; an app turns it on by using it.

| Question | Decision |
|---|---|
| Protocol | OpenID Connect discovery, then OAuth 2.1 authorization code with PKCE (S256) and a loopback redirect, through `ark_connect::oauth`. No custom protocol. `prompt=login`, so a browser that is already signed in still asks which account |
| Which service | `Service::ark(client_id)`: `https://account.aiark.dev`. `ARK_ACCOUNT_URL` points at another one (local, self-hosted). The crate names no auth library |
| Tokens | The refresh token and the last profile in the OS keychain, one entry per service. Access tokens (15 minutes, audience `https://api.aiark.dev`) in memory only. Neither reaches a page, a log or an error |
| Staying signed in | Each refresh rotates the refresh token, and the new one is saved first. Refreshes are serialized. A 4xx from the token endpoint (`ConnectError::Refused`) signs this computer out; a network failure keeps the last known status |
| Several computers | Each computer is its own sign-in (its own refresh token). Signing out here revokes this one only |
| What a feature asks | `Account::can("cloud.agents")` → `Allowed`, `SignIn` or `NotIncluded`. Entitlements come from the service's `/v1/me`; the client never decides them |
| Local work | Never asks. No `if signed_in` outside Cloud features |

## Consequences
- The CLI can sign in the same way. A device-code flow for a computer without a browser comes
  later, from the same service.
- Cloud agents are runtimes ARK Cloud provisions (ADR 0016). A signed-in app gets a runtime's
  URL and token from the control plane with `Account::access_token`. The runtime still has no
  users.
- A process that can read the keychain entry can act as the person at ARK Cloud until the
  token is revoked. This is the same as connected accounts (ADR 0011).
- A Linux server's kernel keyring does not outlast a restart, so the account is for the
  computer a person uses, not a served runtime.
- `ConnectError` has a new variant, `Refused`.

## Check
`crates/ark-account/tests/account.rs`, against a fake service on loopback:
- PKCE and the authorization request's parameters;
- the browser landing on the service's page;
- the account known after a restart with no network, then refreshed with rotation;
- one refresh at a time;
- a sign-in the service ended;
- sign-out with revoke (`client_id` sent), and with no network;
- a discovery document for another issuer, refused.

The service's own tests are in `ark-cloud/account/test/flow.test.ts`.
