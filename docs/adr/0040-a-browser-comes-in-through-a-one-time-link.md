# 0040. A browser comes in through a one-time link and keeps a session in a cookie

Status: Accepted
Date: 2026-10-06

## Context
A runtime with saved tokens (ADR 0016) serves its page to any browser, which needs a token:
`?token=` in the address, kept in `sessionStorage`, sent in a header and, for `EventSource`,
in the URL. That fits a person pasting their own token. It does not fit a control plane (ARK
Cloud's console, ark-cloud ADR 0016) sending people into their workspace's runtime from a
button: a token in the address reaches the browser's history (synced across devices), a token
in the page's storage is readable by any script that runs in it, a token in the events URL
reaches proxy logs, and a token never ends on its own. A token here is the runtime's owner,
Terminal included.

| Option | Why not chosen |
|---|---|
| The control plane hands the browser a saved token (`?token=` as today) | In history and the page; lives until revoked |
| A proxy in front of the runtime holds the token and the browser a cookie of the proxy's | Every call and event stream through the proxy; ARK unchanged but the cost moves to the operator |
| Users and logins inside the runtime | A runtime has one owner (ADR 0016); identity belongs to the app or its control plane |

## Decision
- **A link is a one-time code** (`arkl_`, 256 bits) that whoever runs the runtime makes
  (`Tokens::link(name, valid_for, session)`; ARK Desktop's `token link NAME [MINUTES]`). Only
  its hash is kept, in `access.json` beside the tokens. The browser trades it once,
  `POST /session {code}`, for a **session**: a token (`arks_`) with an end
  (`TokenInfo::expires_ms`), under the link's name.
- **The code travels in the fragment** (`#code=`), which no server or proxy sees, and the page
  takes it out of the address before anything else. History keeps at most a used code.
- **The session lives in an HttpOnly cookie**, never in the page. `SameSite=Strict`, `Path=/`;
  on a real host `__Host-ark-session` and `Secure`; on a loopback address (where cookies do not
  keep ports apart) `ark-session-<port>` over plain HTTP. The event stream needs no token in its
  URL.
- **A request the cookie carries must come from this page** unless it only reads: its `Origin`
  names this `Host`, or, without one, `Sec-Fetch-Site: same-origin`. Otherwise 403.
  Header and query tokens (scripts, the window, `remote::Client`) are as before.
- **Revoking a name ends every session of it and its unused links.** `DELETE /session` ends
  this browser's session alone. An event stream checks its token or session again at each
  ping (15 s) and before an event at most once a second, and closes once it is gone.
- **An entry** (`ArkUi::entry(url)`, ARK Desktop's `ARK_ENTRY_URL`): where a browser without a
  session goes. `GET /session` says `{signed_in, entry}` without a token. The page goes there
  on a 401 (once; again only after 30 s, so a broken link shows the error instead of looping),
  when its stream finds the session gone, and from Retry after it stopped reconnecting (4 tries,
  about 15 s, with an entry). The app's entry signs the person in and comes back with a link.
- **Every answer carries a Content-Security-Policy**: scripts only from the runtime (no inline
  script, nothing from elsewhere), no framing, no `<base>`, forms only to itself; frames and
  images may load the web (the workbench's browser, pictures in answers). Also
  `Referrer-Policy: no-referrer` and `nosniff`.

## Consequences
- A browser in a workspace holds nothing a script in the page can take away. A script that
  runs in the page can still act while it runs; the policy is what stops one from running.
- A session ends on its own (the app picks how long; ARK Desktop: 12 hours by default), and
  coming back is a trip through the entry, unseen when the person is still signed in there.
- `access.json` changes take a lock (`access.json.lock`, `File::lock`), so the server trading a
  code and a command making a link do not lose each other's writes.
- Inline scripts and scripts from elsewhere no longer run on a served page. An app that adds
  one (`ArkUi::script`) must serve it as a file.
- A cookie is per host: two runtimes on one loopback address with different ports keep apart
  only by the cookie's name.

## Check
`crates/ark-ui/src/access.rs` (`a_link_becomes_one_session_that_ends_on_its_own`),
`crates/ark-ui/tests/serve.rs` (`a_one_time_link_becomes_a_session_cookie_the_page_never_sees`,
`a_revoked_session_stops_receiving_events_on_a_stream_it_already_had`). Dogfooded through ARK
Cloud's console against ARK Desktop's binary (`ARK_COMPUTE=local`) in Chromium: sign in, Open
ARK, refresh, a session past its end, the runtime killed and woken by Retry, a browser signed
out from another, Sign out with ARK open in another tab.
