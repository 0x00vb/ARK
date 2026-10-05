# 0029. The window opens a runtime elsewhere in place of its own page, and comes back

Status: Accepted
Date: 2026-10-05

## Context
An app's window runs its own runtime in the same process (ADR 0016). `remote::run_window` opens
a window on a runtime elsewhere instead, decided at launch: going back means quitting and
starting again. ARK Desktop's cloud projects (`ark-cloud/docs/CLOUD.md`, D2) need both in one
window: this computer's projects, and the projects on a runtime in the cloud, with a choice
between them that is always in view ("This Mac | Cloud").

The page a runtime serves already works in a Tauri window over HTTP (`transport.js`). What is
missing is a way into that page from a running window, a way back, and keeping the window's
IPC (which drives this computer's runtime) away from a page that some other machine serves.

| Option | Why not chosen |
|---|---|
| Show the projects of both runtimes in one list, the local host forwarding calls to the remote one | Two runtimes behind one `Host`: every method needs to know where a project is. A large change for one consumer |
| A second window per remote runtime | Two windows with the same page and the same menus for one person's work; the choice of place is not in view |
| Give the remote page a narrow IPC (a Tauri capability for its origin) | Any IPC open to a page from elsewhere is a way into this computer's runtime if that machine is compromised |

## Decision
- `ark_ui::remote::visit(url, token)` (feature `desktop`), called from an app's handler,
  checks the runtime (`hello`) and loads its page in the window in place of the app's own.
  The window's own runtime goes on running. On failure the window stays where it is.
- The page elsewhere goes back with `ark.openLocal()`. That loads `https://ark-local.invalid/`,
  which the window catches (a Tauri navigation hook) and replaces with its own page. It is
  defined only in the window on a page elsewhere (`__TAURI__` is there, the page's origin is
  not the app's).
- The window's commands (`ark_call`, `ark_subscribe`, `ark_pick_folder`) answer only the app's
  own page (`tauri:`, or `http://tauri.localhost` on Windows). Event channels opened by the own
  page stop when the window leaves it, so none of this runtime's events reach the next page.
- A page in the window leaves room for the window's controls whatever its transport
  (`data-shell="desktop"`).
- `ark.sidebar({ id, render })`: a row at the top of the projects list, for what the list as a
  whole depends on. ARK Desktop draws "This Mac | Cloud" there, on both runtimes' pages.

## Consequences
- An app can offer runtimes in several places in one window without ARK knowing where they are
  or who provides them.
- While the window shows a page elsewhere, this computer's questions still show system
  notifications (the window counts as in front only on its own page). Questions from the page
  elsewhere show on that page, as in a browser.
- A runtime elsewhere that stops answering leaves its page reconnecting, then "Disconnected"
  with Retry and Back to this computer. A reload while it is down shows the system's error page;
  quitting and opening the app again opens its own page.
- `OWN_PAGE` loaded in a frame of the own page is refused and does nothing.
- If loading the page elsewhere fails, the own page is loaded again (its event channels were
  already stopped).
- Left open: Tauri keeps a channel message over 8 KB until a page fetches it, by a sequential id
  and without a permission check. One queued for the own page just before the window leaves it
  could be fetched by the page elsewhere. The page elsewhere is the person's own runtime;
  revisit if the window ever opens pages nobody vouches for.
- `run_window` (launch-time, no own runtime) is unchanged: no `__TAURI__`, no way back.

## Check
`crates/ark-ui/src/desktop.rs` tests: only the app's own origins count as its page.
`ARK_UI_PAGE_TESTS=1 cargo test -p ark-ui --test page`: `visit` (the window's controls, the way
back, the row above the projects) and `workbench` (a browser has no way back). A native run in
ARK Desktop (2026-10-05, its ADR 0009): the window opened a runtime elsewhere and came back,
and a page elsewhere was refused `ark_call`, `ark_subscribe`, `ark_pick_folder` and the dialog
plugin (Tauri's own permissions refuse remote origins first; the own-page check is the second).
