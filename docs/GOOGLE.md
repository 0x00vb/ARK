# ARK's Google sign-in

ARK signs in to Google with its own OAuth client (ADR 0011), so a person sees "Connect Google"
and nothing else. This page is what has to exist outside the code for that to work for anyone,
in the order it has to happen. Google's review can take weeks, so it gates a release that
ships Mail or Calendar.

## What ARK asks for

| Capability | Scope | Google's class | What it needs from Google |
|---|---|---|---|
| (every sign-in) | `openid`, `email` | non-sensitive | nothing beyond the consent screen |
| Mail: send | `gmail.send` | sensitive | app verification (no security assessment) |
| Calendar: see and add events | `calendar.events` | sensitive | app verification (no security assessment) |
| Mail: read and search (not built yet) | `gmail.readonly` | restricted | restricted-scope verification, and a yearly third-party security assessment (CASA) when the data can reach a server other than the person's device |

Classes as listed by Google on 2026-10-03 (Gmail API scopes page; the Calendar scope is
reported as sensitive by Google's sensitive-scope guide). Check them again in the console's
Data Access page, which shows the class of each scope you add.

Reading mail is left out of the first release on purpose: ARK sends what a tool returns to
the model provider the person chose, which Google may count as data leaving the device, so
`gmail.readonly` likely means a paid assessment every year. Send-only Mail and Calendar need
the ordinary review.

## Steps

| # | Step | Who | Needs |
|---|---|---|---|
| 1 | A Google Cloud project for ARK, owned by an account of the business (not a personal one) | the owner | a Google account |
| 2 | Google Auth Platform › Branding: name "ARK", support email, the logo from `ark-site/brand`, home page `https://aiark.dev`, privacy policy `https://aiark.dev/privacy`, authorized domain `aiark.dev` | the owner | step 1 |
| 3 | Audience: External, publishing status Testing; add the people who dogfood as test users (up to 100). In Testing, refresh tokens expire after 7 days | the owner | step 2 |
| 4 | Data Access: add `openid`, `email`, `gmail.send`, `calendar.events` | the owner | step 2 |
| 5 | Clients › Create: type "Desktop app". Put its client id and secret in `crates/ark-connect/src/catalog.json` (`providers[google].oauth.client_id`, `client_secret`). A desktop client's secret is not confidential (RFC 8252 §8.5); Google still asks for it. Until then: `ARK_GOOGLE_CLIENT_ID`, `ARK_GOOGLE_CLIENT_SECRET` | the owner, then a commit | step 1 |
| 6 | aiark.dev live, with the home page and `/privacy` (the draft is `ark-site/privacy.html`). The DNS is on Cloudflare already | the owner | a host for `ark-site` |
| 7 | Verify `aiark.dev` in Google Search Console (a DNS TXT record in Cloudflare) with the same Google account | the owner | step 6 |
| 8 | Submit for verification: a line per scope on why ARK needs it, and a demo video (unlisted on YouTube) showing the consent screen with the client id, then each scope in use: an agent sending a mail, reading and adding an event | the owner | steps 2 to 7, a build with Mail and Calendar |
| 9 | Publish the app (In production) once approved | the owner | step 8 |

Google states brand verification takes 2 to 3 business days; sensitive-scope review takes
longer and asks questions by email. Steps 1 to 5 can be done today and are enough to dogfood
with test users. Steps 6 to 9 gate the public release.

## What the privacy policy must say

Google reviews the policy against its User Data Policy. It has to say, in words a person can
find:

- which Google data ARK reads and writes (the scopes above) and that it does so only to do
  what the person asked an agent to do;
- that ARK keeps the sign-in on the person's device (the OS keychain) and runs on that
  device; ARK's makers receive none of it;
- that a tool's result, which can include an email or an event, is sent to the AI model
  provider the person chose, to carry out the request; that Google user data is not used to
  develop, improve or train generalized or non-personalized AI or ML models; and that ARK
  does not transfer it for that purpose;
- the Limited Use statement, as Google words it: "ARK's use and transfer of information
  received from Google APIs will adhere to the Google API Services User Data Policy,
  including the Limited Use requirements";
- how to disconnect (Settings › Capabilities or `/disconnect`), which revokes ARK's access
  at Google.

Open question for the owner: a person can run ARK on a consumer subscription (Claude or
ChatGPT through their CLIs), whose own terms decide whether that provider may train on what
it receives. The policy should say the person's provider terms apply, and the review may ask
about it.

## Where it is in the code

| What | Where |
|---|---|
| Provider, scopes and client id | `crates/ark-connect/src/catalog.json` |
| Sign-in (PKCE, loopback redirect) | `crates/ark-connect/src/oauth.rs` |
| Tokens (OS keychain) | `crates/ark-connect/src/secrets.rs` |
| Mail and Calendar tools | `crates/ark-connect/src/services/google.rs` |
