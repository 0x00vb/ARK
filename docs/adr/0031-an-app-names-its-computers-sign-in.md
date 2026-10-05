# 0031. An app can name the computer's sign-in to its ARK Account

Status: Accepted
Date: 2026-10-05

## Context
Each computer is its own sign-in to an ARK Account (ADR 0026). The person's account page at the
service lists them, so they can sign out one they lost or sold. Without names, two computers
look the same there. The app knows the computer's name; the service does not, and nothing in an
access token tells two computers apart.

## Decision
- `Account::set_computer_name(name)`, optional. The crate sends it once per run, after signing
  in or after the first `refresh`: `PUT /v1/me/computer {name, refresh_token}` with the access
  token. The refresh token this computer holds is what tells the service which sign-in it is.
  It goes only to the issuer, like a refresh or a revoke.
- Best effort. A failure never fails a sign-in or a refresh. A 404 (a service without names, or
  a sign-in it no longer knows) is not asked again in that run.
- Not part of OAuth: the sign-in itself (`ark_connect::oauth`, also used for Google) is
  unchanged.

## Consequences
- An app that sets no name sends nothing; its sign-ins look as before.
- Another OpenID Connect provider (`ARK_ACCOUNT_URL`) need not serve the route.
- The service's side is ark-cloud ADR 0010.

## Check
`crates/ark-account/tests/account.rs`: the name sent once per run with the token held then,
sent again with the rotated token on the next run, nothing sent without a name, a service
without names asked once while signing in still works.
