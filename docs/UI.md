# ark-ui: the graphical workspace

`ark-ui` opens an ARK app in a window: the conversation, what the agent is doing, approvals,
sessions, and inline views of context, usage, state and traces. It opens the same
`ark_interact::App` the terminal shell opens, so commands, sessions, labels and traces mean
the same thing in both. It holds no agent logic: one `Interaction` drives the one runtime.

```text
 ark-runtime          ark-interact            ark-ui
 ┌────────────┐      ┌───────────────┐      ┌──────────────────────────┐     ┌──────────────┐
 │ Harness    │ ◄──► │ Interaction   │ ◄──► │ Host  (call / events)    │ ◄──►│ web/ page    │
 │ Approval…  │      │ App, Report   │      │  ├ Tauri window (desktop)│     │ + app scripts│
 │ Cancel,    │      │ activity,     │      │  └ HTTP + SSE (serve)    │     │   (window.ark)│
 │ Steering   │      │ trace, inspect│      └──────────────────────────┘     └──────────────┘
 └────────────┘      └───────────────┘
```

## Three ways to run

| Mode | How | Needs |
|---|---|---|
| Native window | `ArkUi::new(app).run_desktop(tauri::generate_context!())` | feature `desktop` (default), Tauri |
| Local browser | `ArkUi::new(app).serve("127.0.0.1:0").await` prints a URL with a token | nothing extra (`default-features = false`) |
| Your own front-end | `ArkUi::new(app).open().await` gives a `Host`; call it and subscribe to its events | nothing extra |

A headless consumer depends on `ark-sdk` alone and never compiles Tauri. The writer example
serves in a browser without the desktop feature: `writer --serve`.

## What the workspace shows

| Area | Contents |
|---|---|
| Conversation | Your messages, streamed answers, and the agent's work as short activity lines ("Reading chapter-04.md"). A work block folds to one line ("Used 3 tools · Reading notes") when the turn ends. A tool row opens on click: arguments, the result the model saw, duration, approval, and what it added to the context. |
| Composer | Model, and a quiet "24% context" meter that runs `/context` when clicked. |
| Sidebar | New session, the app's navigation, projects and sessions with times. |
| Side pane | Only domain content the person or the agent asked for: an artifact, an evidence record, an app panel. Tabs, resizable. Closed, the conversation gets the width. |
| Inline reports | `/context`, `/usage`, `/state`, `/trace` and every other command render in the conversation. There is no permanent inspector. |

Narrow windows (up to 760 px, or up to 1000 px with the pane open) fold the sidebar into a
drawer opened from the top-left button. Below 760 px an open pane covers the window.

## Keys

| Key | Action |
|---|---|
| ↩ | Send. While the agent works: queue for the next turn |
| ⌘↩ | While the agent works: steer (the message reaches the agent at its next step) |
| ⇧↩ | New line |
| esc | Close the palette, else deny a pending approval, else stop the turn, else close the pane |
| ⌘K | Command palette |
| ⌘N | New session |
| ⌘B | Sidebar |
| ⌘\ | Side pane |

A steered message waits above the composer as "Next step" and joins the conversation where
the agent read it. A steer that arrives after the agent's last step becomes the next prompt.

## Approvals, cancel, sessions

- Approvals come from the runtime's `ApprovalGate` through `ChannelApprover`, the same path
  the shell uses. The card offers Allow once, Allow for session (the approval class is shown)
  and Deny. The window only shows the question.
- Stop uses the turn's `CancelToken`. A cancelled tool call kills its process group.
- Opening a session loads its stored turns for display (paged, "Show earlier turns") and
  restores ARK working state. The model still receives only the bounded carry-over: stored
  transcript is not model context.

## Building a workspace

From SecRes (`workbench/apps/secres-desktop/src/main.rs`), trimmed:

```rust
let ui = ArkUi::new(secres_app::app(root)?)       // the same App the `secres` shell opens
    .accent("#16a34a")
    .asset("logo.svg", include_bytes!("../ui/logo.svg").as_slice())
    .logo("logo.svg")
    .asset("secres.js", include_bytes!("../ui/secres.js").as_slice())
    .script("secres.js")
    .stylesheet("secres.css")
    .welcome("What are we looking into?", "Binaries, a live device, a web target or code. …")
    .suggestion("Is a device attached?", "Is a device attached? Tell me its model, OS version and architecture.")
    .handler("notes", |ctx: HandlerCtx, _| async move {
        Ok(json!(ctx.engine.store.list_notes(&ctx.project.id)?))
    });
ui.run_desktop(tauri::generate_context!())
```

| `ArkUi` method | Purpose |
|---|---|
| `new(app)`, `.start(Start)` | The app, and model/project/session choices as the shell's flags give them |
| `.accent(css)`, `.logo(asset)` | Theme colour and logo |
| `.welcome(title, line)`, `.suggestion(label, prompt)` | The empty-session screen |
| `.asset(path, bytes)` | A file the page can load, served at `ext/<path>` |
| `.script(asset)`, `.stylesheet(asset)` | Load an asset as a module or stylesheet after ARK's own |
| `.handler(name, f)` | A backend method the app's scripts call with `ark.ext(name, params)` |
| `.open()`, `.serve(addr)`, `.serve_background(addr)`, `.run_desktop(ctx)` | Start |

A handler gets `HandlerCtx { engine, project, emitter }`. `emitter.emit(name, payload)`
pushes an event the page receives with `ark.on(name, fn)`; SecRes streams its terminal
output this way.

## The page extension API

App scripts register on `window.ark`. ARK does not know what any of it means.

| Call | Registers |
|---|---|
| `ark.panel({ id, title, icon, nav, presents, wide, fill, render })` | A side-pane panel; `nav: true` adds it to the sidebar |
| `ark.artifactViewer({ id, label, match, primary, presents, wide, render })` | A view of an artifact; when several match, the artifact header switches between them (plus Raw) |
| `ark.toolRenderer({ match, render })` | How a tool call's full output renders when its row is opened |
| `ark.action({ id, label, icon, where, when, run })` | A button on an artifact, or an entry in the ⌘K palette |
| `ark.status({ id, render })` | A line at the bottom of the sidebar |
| `ark.on(name, fn)` | App events, and ARK's own as `ark:<kind>` (`ark:turn_finished`, `ark:event`, ...) |

`render(el, params, api)` may return `{ update(params), refresh(), focus(), dispose() }`.
`presents` lists the `PresentDirective` views it handles, so an agent's "show this" opens
it. `focus()` returns `{ surface, selection }`: while the panel is visible the agent is told
what the person is looking at.

Helpers on the same object: `ark.h` and `ark.icon` (DOM), `ark.markdown`, `ark.call` (a Host
method), `ark.ext` (an app handler), `ark.runAction(action, input)`, `ark.command(line)`,
`ark.openArtifact(id, viewer, params)`, `ark.openPanel(id, params)`, `ark.setPanelTitle`,
`ark.openInvocation(id)`, `ark.project()`, `ark.setFocus`, `ark.toast`.

A small real panel, SecRes's notebook (`ui/notes.js`, trimmed):

```js
ark.panel({
  id: "notes", title: "Notes", icon: "file", nav: true, presents: ["notebook"],
  render(el) {
    const list = h("div.sr-notes");
    el.append(list);
    const refresh = async () => {
      const notes = await ark.ext("notes");
      list.replaceChildren(...notes.slice().reverse().map((n) => h("article", { html: ark.markdown(n.body) })));
    };
    refresh();
    return { refresh, focus: () => ({ surface: { kind_tag: "notebook", title: "Notes" } }) };
  },
});
```

SecRes adds 14 surfaces this way (artifacts, notes, device, terminal, browser, editor and
diff panels; functions, hex, events and tree viewers; recipe actions; two tool renderers;
tool health), one module each under `ui/`.

## Protocol

Both transports carry `call(method, params) -> Value` and one event stream. A custom
front-end uses the same.

| Methods | |
|---|---|
| Session | `hello`, `summary`, `sessions`, `projects`, `new_session`, `open_session`, `rename_session`, `switch_project`, `model` |
| Turn | `send`, `steer`, `cancel`, `approve`, `command` |
| Views | `transcript`, `trace`, `call`, `context`, `usage`, `state` |
| Store | `artifacts`, `artifact`, `invocation`, `run_action` |
| Page | `focus`, `ext` (an app handler) |

| Events (`kind`) | |
|---|---|
| `turn_started`, `text`, `event`, `turn_finished` | A turn. `event` carries the `HarnessEvent` and its described `Activity` |
| `approval`, `approval_closed` | A pending approval, and its end (answered, or the turn ended) |
| `summary` | Session, model, project or context changed |
| `ext` | An app handler's `emitter.emit` |

Every event carries `seq`, one higher than the last. `hello` returns `seq` (everything up to
it is in its answer) and `live`, the running turn's events so far, so a window that opens or
reconnects mid-turn rebuilds the turn and then follows it; the page drops events it has
already shown and reloads on a gap. In a browser the stream reconnects with backoff (1 s
doubling to 30 s) and stops after ten failures with a Retry button.

The HTTP server binds 127.0.0.1, requires the per-run token on every request except static
files, and rejects a `Host` header that is not the loopback address, so another local
process or a web page cannot drive the agent.

## Developing the page

- `ARK_UI_WEB=<dir>` serves ARK's page files from disk instead of the embedded copy;
  `ARK_UI_ASSETS=<dir>` does the same for the app's. Reload the browser to see a change.
- `ARK_PROVIDER=mock ARK_MOCK_SCRIPT=<file>` gives a scripted model (see `CLI.md`).
- `cargo test -p ark-ui` runs the protocol tests (`tests/host.rs`) and the neutrality test.
- The page tests drive the real page in WebKit, the engine behind the macOS window. They
  need Node and a browser, so they run only when asked:

  ```text
  cd crates/ark-ui/tests/web && npm ci && npx playwright-core install webkit
  ARK_UI_PAGE_TESTS=1 cargo test -p ark-ui --test page
  ```

## Tokens

The window sends the model what the shell sends. `tests/host.rs` runs 500 turns through a CLI
`Interaction` and 500 through the `Host` and compares every request: the system prompt and
messages byte for byte (invocation ids aside), and the tool list, which differs only by
`present` (a window can show things; a terminal cannot). Output from a run of that test:

| path | requests | turn 50 answer request | turn 500 answer request |
|---|---:|---:|---:|
| CLI (Interaction) | 1000 | 1231 | 1234 |
| GUI (Host) | 1000 | 1231 | 1234 |

## Limits

- The page is plain ES modules with no build step and no CDN. App scripts may load their own
  libraries (SecRes loads CodeMirror and xterm.js from cdnjs on first use).
- One `Host` serves one live session. Two browser tabs on one server share it.
- Approval grants are not persisted across processes, as in the shell.
- There is no macOS WebDriver for the native window; its page is tested in WebKit instead,
  and an app's bundle can be checked through its browser transport (SecRes:
  `scripts/verify-desktop.sh`).
