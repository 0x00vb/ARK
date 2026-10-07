# 0044. The Browser panel is a view of its own in the window

Status: Accepted
Date: 2026-10-07

## Context
The Browser panel (`ark-ui/web/js/workbench.js`) showed pages in an `<iframe>`. A page that sends
`X-Frame-Options` or a `frame-ancestors` that does not allow the app's page is blanked by the
web view, and the panel stayed white with no message. Found in ARK Desktop on 2026-10-07: an agent
started a Next.js app (`npm run dev`, ADR 0041) whose security headers say
`X-Frame-Options: SAMEORIGIN` and `frame-ancestors 'self'`. Most real sites (GitHub, Google,
banks) send the same headers, so the panel worked only for pages that allow frames.

Options:

| Option | Why not, or why |
|---|---|
| Ask apps to drop the headers | ARK cannot change other people's pages, and should not ask |
| A proxy in the runtime that strips the headers | Rewrites every response, breaks cookies, websockets (hot reload) and absolute addresses, and takes a page's protection away |
| A native webview over the panel (Tauri multiwebview) | A top-level page: headers about frames do not apply, sign-ins, cookies and hot reload work as in a browser. Window only |
| Say when a page refuses | Needed in a plain browser, where a native view does not exist |

## Decision

| Where | The Browser panel |
|---|---|
| The window (Tauri) | A webview of the window's own, label `ark-browser`, created on the first address and kept over the panel's place. The page tells the window where the panel is (`ark_browser_show {url?, x, y, width, height}`) once the place holds still for a frame, and to `hide`, `close`, go `back`, `forward` or `reload` it (`ark_browser`); loads come back on a channel (`ark_browser_events`). Tauri's `unstable` feature, held to `~2.12` |
| A plain browser (ARK served over HTTP) | Still an iframe. The runtime asks the page's headers (`frame_check`) and the panel says "<host> can't be shown here" with Open in your browser when they refuse frames |

Rules for the window's view:

| Question | Decision |
|---|---|
| IPC | None. It is a page elsewhere: `ark_call` and the others refuse every webview not on the app's own page (`from_own_page`) |
| What it may load | Anything except the app's own page and what reaches the app: `tauri:`, `asset:`, `ipc:`, `file:`, the `tauri`, `ipc` and `asset` `.localhost` hosts, and `OWN_PAGE`. Checked for frames too, so `about:`, `data:` and `blob:` frames inside pages keep working |
| `OWN_PAGE` from the view | Refused. Only the window's own webview (`main`) goes home on it |
| Finding the window | By label (`get_window("main")`, `get_webview("main")`): once a second webview exists the window is no longer a `WebviewWindow` and `get_webview_window` finds nothing |
| Cookies and storage | A data store of its own (macOS 14 and later; before, the system's default store, which the app's own page does not use for cookies) |
| New windows (`target=_blank`, `window.open`) | Open in the same view. Popups that talk back to their opener (some OAuth flows) do not work |
| Downloads | Not saved; the person's browser opens the address (http and https only, at most one every 3 seconds) |
| Coming to the front | Creating the view brings the app to the front on macOS, so the view is created only while the window has focus. The page asks again when it does |
| ARK's own UI over the pane | The view is drawn above the page. It steps aside (hidden) while a menu, the palette, a dialog, a toast or the narrow window's sidebar covers it, or anything an app marks `data-ark-overlay` |
| The page reloads, or the window visits a runtime elsewhere | The view closes (`on_page_load` of `main`, Started). A view still being made for the page before is closed as soon as it exists, and so is one whose tab closed meanwhile |
| The pane's resize grip | Stays uncovered along the pane's edge |

`frame_check` decides as browsers do: an enforced `frame-ancestors` decides alone (refuses unless
it lists `*`); without one, `X-Frame-Options` refuses on `DENY` or only `SAMEORIGIN`, and other
values are ignored. It asks public addresses only (`check_public` for the address, then
`ark_tools::web::public_client`, whose resolver connects to public addresses only and whose
redirects to private ones fail), so a runtime on a server never probes its own machine or
network, and a name cannot pass the check and then resolve to one. It reads headers, never the
body, and answers `framable: true` when it cannot tell.

## Consequences
- In the window any page shows, including the processes agents start and sites that refuse
  frames. The view keeps its page while the tab is hidden.
- While the person types in the view, the app's keys (⌘K, ⌘N, Esc) go to the page, not to ARK.
  Clicking anywhere in ARK gives them back. Menu accelerators for these keys are a later change.
- During a live window resize the view can trail the panel by a frame.
- A toast over the pane hides the view for a moment.
- An address in a single-page app that changes without a load (`history.pushState`) does not
  reach the address bar.
- `frame_check` is a guess from the headers: a page that lists the app's origin, or that the
  runtime cannot reach, loads as before. Pages on this machine or a private network are not
  asked, so in a plain browser a local dev server that refuses frames still shows blank (the
  window has no such limit).
- While the tab is out of sight the page looks at the view's place a few times a second, not
  every frame.
- Windows and Linux have multiwebview too; untested (macOS ships).

## Check
- `desktop.rs` tests: `the_browser_never_loads_the_apps_own_page` (refused and allowed
  addresses), `only_the_apps_own_page_is_its_own`.
- `frame.rs` tests: the header rules as browsers apply them, the Next.js headers that showed
  white, and that private addresses (by name or number) are not asked.
- Page tests in WebKit (`tests/page.rs`, with a stub in place of the window's view):
  `the_browser_view_follows_the_panel_and_steps_aside_for_menus` (its place inside the pane and
  clear of the grip, no calls while nothing moves, the address bar following a load, a refused
  address, the palette, another tab, the pane closed, Back, Reload, the tab closed) and
  `a_page_that_refuses_frames_says_so_in_a_browser`.
- Natively in ARK Desktop (2026-10-07): duckduckgo.com, which refuses frames, showed in the
  panel. Still to check by a person at the screen: a Next.js dev server with those headers and
  github.com with a sign-in, a Desktop dialog over the pane, a window resize, a project switch, a
  reload, This Mac | Cloud, the window hidden to the menu bar and back from the Dock, a
  notification while the window is behind.
