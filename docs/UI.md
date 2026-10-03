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
| Composer | Model, and a quiet "24% context" meter that runs `/context` when clicked. The model menu lists models by backend (subscriptions, Ollama, APIs with a key); a backend that is not ready shows its sign-in or install command. |
| Sidebar | New chat, then projects with their chats beneath (times, "Show more" after 8). The current project is unfolded, the others fold; a chevron unfolds one without opening it, and a fold choice lasts until the current project changes. A project's menu renames it and changes its folder; "+" adds a folder as a project, or an empty one. Settings at the bottom. |
| Top bar | The title (the chat's, or the project's over one of its views). With app views marked `nav`: tabs, "Chat" first. Buttons on the right for app panels marked `nav`. |
| Side pane | Only domain content the person or the agent asked for: an artifact, an evidence record, an app panel. Tabs, resizable. Closed, the conversation gets the width. |
| Inline reports | `/context`, `/usage`, `/state`, `/trace` and every other command render in the conversation. There is no permanent inspector. |
| App views | An app's `ark.view` takes the main column in place of the conversation (ARK Desktop's graph, agents and tasks). The "Chat" tab, or choosing a chat, brings the conversation back. |
| Settings | One page in the main column for the app's and the account's configuration, with the sections apps register (`ark.setting`). ARK has none of its own, so it shows only once one exists. A project's own settings are in its menu. |
| Agents' questions | An orchestrated agent's approval waits in a tray at the bottom right, whatever the main column shows. |

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
| ⌘N | New chat |
| ⌘, | Settings (when an app registered a section) |
| ⌘B | Sidebar |
| ⌘\ | Side pane |

A steered message waits above the composer as "Next step" and joins the conversation where
the agent read it. A steer that arrives after the agent's last step becomes the next prompt.

## Approvals, cancel, sessions

- Approvals come from the runtime's `ApprovalGate` through `ChannelApprover`, the same path
  the shell uses. The card offers Allow once, Allow for session (the approval class is shown)
  and Deny. The window only shows the question.
- With `App::orchestrate`, the orchestrator's agents ask through the same approver. Their
  request carries `agent` and `task`; it is answered from the tray and outlives the
  conversation's turn. esc denies only the conversation's own question.
- The headline is `ark_interact::approval_title`, the same in the terminal and the window:
  "The agent wants to run:" for the conversation's own question, "reader-a (task t-…) wants
  to run:" for an agent's.
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
| `ark.panel({ id, title, icon, nav, presents, wide, fill, render })` | A side-pane panel; `nav: true` adds a button for it to the top bar |
| `ark.view({ id, title, icon, nav, render })` | A view of the current project in the main column, in place of the conversation; `nav: true` makes it a top-bar tab (a string names the tab). Built once, kept while hidden |
| `ark.setting({ id, title, icon, render(el, api) })` | A section of the Settings page |
| `ark.artifactViewer({ id, label, match, primary, presents, wide, render })` | A view of an artifact; when several match, the artifact header switches between them (plus Raw) |
| `ark.toolRenderer({ match, render })` | How a tool call's full output renders when its row is opened |
| `ark.action({ id, label, icon, where, when, run })` | A button on an artifact, or an entry in the ⌘K palette |
| `ark.status({ id, render })` | A line at the bottom of the sidebar |
| `ark.on(name, fn)` | App events, and ARK's own as `ark:<kind>` (`ark:turn_finished`, `ark:event`, ...; in a browser also `ark:reconnecting`, `ark:disconnected` with `retry()`, `ark:connected`) |

`render(el, params, api)` may return `{ update(params), refresh(), focus(), dispose() }`.
`presents` lists the `PresentDirective` views it handles, so an agent's "show this" opens
it. `focus()` returns `{ surface, selection }`: while the panel is visible the agent is told
what the person is looking at.

Helpers on the same object: `ark.h` and `ark.icon` (DOM), `ark.markdown`, `ark.call` (a Host
method), `ark.ext` (an app handler), `ark.runAction(action, input)`, `ark.command(line)`,
`ark.compose(text)` (puts text in the composer, unsent, so an empty state can suggest a request),
`ark.openArtifact(id, viewer, params)`, `ark.openPanel(id, params)`, `ark.closePanel(id)`,
`ark.openView(id, params)`, `ark.showConversation()`, `ark.currentView()`, `ark.setPanelTitle`,
`ark.openInvocation(id)`, `ark.project()`, `ark.setFocus`, `ark.toast`, and
`ark.pickFolder({ title })`: the window's own folder picker (a path, `null` if dismissed,
`undefined` in a browser, which cannot see the runtime's disk; ask for a typed path then).

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
| Session | `hello`, `summary`, `sessions {project?}`, `projects`, `new_session`, `open_session`, `rename_session`, `switch_project`, `open_project {id}` (back to its last chat), `add_project {path?, name?}` (a folder becomes a project, or opens the one it is), `rename_project {id, name}`, `project_folder {id, path}`, `workspace {path?}` (the project's folder; `/folder` in the terminal), `model`, `models` |
| Turn | `send`, `steer`, `cancel`, `approve`, `command` |
| Views | `transcript`, `trace`, `call`, `context`, `usage`, `state` |
| Store | `artifacts`, `artifact` (the project's, or one an orchestrated agent produced), `invocation`, `run_action` |
| Agents | `graph`, `agents`, `tools`, `profiles` (what `AgentSpec.profile_name` may name), `spawn_agent {spec}`, `suspend_agent`, `resume_agent`, `terminate_agent`, `message_agent {agent, text}` |
| Tasks | `submit_task {agent, task}`, `task {task}` (record and timeline lines), `task_trace`, `cancel_task`, `retry_task`, `orch_events {after, limit}` |
| Page | `focus`, `ext` (an app handler) |

| Events (`kind`) | |
|---|---|
| `turn_started`, `text`, `event`, `turn_finished` | A turn. `event` carries the `HarnessEvent` and its described `Activity` |
| `approval`, `approval_closed` | A pending approval (`request`, `line`, `title`), and its end (answered, or the turn ended) |
| `summary` | Session, model, project or context changed |
| `ext` | An app handler's `emitter.emit` |
| `orch` | An orchestration event: `event` (an `OrchEvent`), `line` and `tone` (its timeline line, from `ark_interact::orchestration::event_line`), `activity` (for an agent's runtime event, what the conversation would show; none for steps and context reports) |

The agent and task methods need an app with an orchestrator and do not wait for the
conversation: they answer while a turn runs. The ones that change something build an
`ark_interact::orchestration::Control`, the same one the terminal's `/agent` and `/task`
build, and only shape the answer. `spawn_agent` takes an `AgentSpec` as JSON and
`submit_task` a `TaskSpec`; both are what the orchestrator stores, so the same definitions
work wherever the runtime runs. Tests: `tests/orchestration.rs`.

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
