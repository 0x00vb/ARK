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
| A runtime elsewhere | `ArkUi::new(app).access(Access::Tokens(..)).serve(addr)` on a server; clients use `remote::Client` or `remote::run_window(url, token, ctx)` | saved access tokens, a TLS proxy ([`REMOTE.md`](REMOTE.md)) |

A headless consumer depends on `ark-sdk` alone and never compiles Tauri. The writer example
serves in a browser without the desktop feature: `writer --serve`.

## What the workspace shows

| Area | Contents |
|---|---|
| Conversation | Your messages, streamed answers, and the agent's work as short activity lines ("Reading chapter-04.md"). A work block folds to one line ("Reading notes and 2 more steps") when the turn ends. A tool row opens on click: arguments, the result the model saw, duration, approval, and what it added to the context. |
| Composer | Model, and a quiet "24% context" meter that runs `/context` when clicked. The model menu lists models by backend (subscriptions, Ollama, APIs with a key); a backend that is not ready shows its sign-in or install command, and an API without a key a field to paste one (checked, then kept in the system keychain). An app can draw this menu itself (`ark.modelPicker`); then, while the app says no model is chosen, the button asks for one and sending opens the menu with the text kept. |
| Sidebar | New chat, then projects with their chats beneath (times, "Show more" after 8). The current project is unfolded, the others fold; a chevron unfolds one without opening it, and a fold choice lasts until the current project changes. A project's menu renames it and changes its folder; "+" adds a folder as a project, or an empty one. Settings at the bottom. |
| Top bar | The title (the chat's, or the project's over one of its views). With app views marked `nav`: tabs, "Chat" first. On the right, a + menu: Terminal, Browser, Files, then app panels marked `nav`. |
| Side pane | What the person or the agent opened, beside the conversation: Terminal, Browser, Files, an artifact, an evidence record, an app panel. Tabs, resizable. Closed, the conversation gets the width. |
| Terminal | The project's shell, in its folder (one per project; closing the tab keeps it, reopening re-attaches with its scrollback). The agent's `present` may type a command at the prompt; the person presses Enter. |
| Browser | A page with an address bar; words become a search, and `localhost:3000` (or `127.0.0.1`, `[::1]`, `*.localhost`) opens over http. In the window the page is a view of its own over the pane (ADR 0044), so any site shows; it steps aside while ARK's menus, palette or dialogs cover it. An address that does not answer says "can't be reached" with Try again; a line under the bar shows a load. In a browser it is a frame: a page that refuses frames says so, with Open in your browser (`frame_check`; pages on this machine or a private network are not asked and may show blank). |
| Processes | When the app offers processes (ADR 0041): what agents started (command, folder, state, address), the selected one's live output, Stop, and Open in the Browser. Opens when a process starts in the project the window shows; a process started with `open` opens that page in the Browser once ready. |
| Files | The project's folder. A file opens in a tab of its own (see [Files](#files)). A path outside the folder is refused. |
| A file | A spreadsheet as a grid, a document as a page, text in an editor: the file the agent works on, edited in place. A turn that wrote files ends with a card per file that opens it. |
| Inline reports | `/context`, `/usage`, `/state`, `/trace` and every other command render in the conversation. There is no permanent inspector. |
| App views | An app's `ark.view` takes the main column in place of the conversation (ARK Desktop's graph, agents and tasks). The "Chat" tab, or choosing a chat, brings the conversation back. |
| Settings | One page in the main column for the app's and the account's configuration, with the sections apps register (`ark.setting`), then ARK's own (Appearance, Capabilities, Permissions). Sections marked `advanced` (the app's, then Skills and Tool servers) are listed under Advanced. A project's own settings are in its menu. |
| Settings › Appearance | ARK's own section: Light, Dark or System. The choice is kept in the page's storage and set on `<html data-theme>` before the first paint; until the person chooses, the app's `.theme(...)` applies. Pages that draw their own colours read `data-theme` and listen for `ark:theme` on `window` |
| Settings › Skills | ARK's own section: the skills of the project folder and the store (name, "This project" or "All projects", description). Add skills opens the Terminal in the project folder with `npx skills add <package>` typed; Enter is the person's |
| Settings › Tool servers | ARK's own section, when the app has plugins: the MCP servers (running or why not, their tools, the command), Add (a name and a command, or a pasted `mcpServers` snippet), Restart, Remove (a second click confirms). The same as `/plugin`. An app section with the id `plugins` replaces it |
| Board | In an app that runs agents, a tab beside Chat: the project's work as To do, In progress and Done ([`ORCHESTRATION.md`](ORCHESTRATION.md#the-board)). A card is a status circle, a title and its owner; "Needs you" and a dot while its agent works are the only marks. Drag moves a card (a line shows where it lands), the circle completes it, + adds one (Enter keeps the box open for the next). A click opens it in the side pane: title and note in place, owner, "Give to…" an agent (starts a task linked to it), what blocks it, each run's answer and files, its history, Delete (with Undo). Keys on a card: Enter opens, Space completes, Backspace deletes, arrows move the focus, Shift with arrows moves the card. Changes by agents and other windows glide into place |
| Agents' questions | An orchestrated agent's approval waits in a tray at the bottom right, whatever the main column shows. In the native window, a question that arrives while the window is not in front (hidden, minimized, another app in use) also shows a system notification. |
| What waits for you | In the same tray, above the questions: work you started that failed and results posted into a chat while you were elsewhere, in any project (ADR 0043). A row opens its chat, or shows why the work failed when it has no chat; x or Delete clears it. The terminal's `/waiting` lists the same items |

Narrow windows (up to 760 px, or up to 1000 px with the pane open) fold the sidebar into a
drawer opened from the top-left button. Below 760 px an open pane covers the window.

## Files

What the window does with a project's files ([ADR 0019](adr/0019-files-are-shared-in-place.md)).
The file in the folder is the only copy: the agent's `files.*` tools and the window read and
write it, and each sees the other's changes.

| | |
|---|---|
| Open | A file card in the conversation, the Files panel, the agent's `present {view: "file", path}`, or a stored copy of the file (an artifact with a `path`). Each file is a tab; the view follows the kind the host reports (`text`, `markdown`, `sheet`, `document`, `pdf`, `image`; [ADR 0021](adr/0021-an-output-is-a-file-with-a-kind.md)) |
| Spreadsheet | Click selects, typing replaces, Enter or a double click edits, the bar above shows the cell's formula. Arrows, Tab, Shift for a range, Delete clears, copy and paste as tab-separated text, sheets at the bottom. Each change saves at once as `{cell, value}`; formulas are computed again |
| Document | Type to change a block; Enter starts the next; `# `, `## `, `- `, `1. ` at the start of a line make a heading or a list; ⌘B and ⌘I; Tab indents a list item. Saves itself a moment after typing stops, as block edits |
| Text | The editor, saved with ⌘S |
| Markdown | A page, with the project's images it links to (`![chart](chart.png)`); Edit shows the text, saved with ⌘S |
| PDF, image | The file itself (`file_media`): a PDF in the browser's own viewer. Read only |
| Someone else's change | The tab reads the file again when the agent (or another agent) writes it, or the window comes back to the front, unless the person has unsaved typing. A document edit based on an old version is refused and the tab reloads |
| What the agent is told | While the tab is visible: the file, the cell or block selected, and what the person changed since their last message (`edited`) |
| Open in its app | When the runtime is local (`ark.isLocal()`), for any file ARK has a format for |

## Keys

| Key | Action |
|---|---|
| ↩ | Send. While the agent works: queue for the next turn |
| ⌘↩ | While the agent works: steer (the message reaches the agent at its next step) |
| ⇧↩ | New line |
| esc | Close the palette, else deny a pending approval, else stop the turn, else close the pane |
| ⌘K | Command palette |
| ⌘N | New chat |
| ⌘, | Settings (when it has a section) |
| ⌘B | Sidebar |
| ⌘\ | Side pane |
| ⌃` | Terminal |
| ⌘T | Browser (in the window; a browser keeps ⌘T for itself) |
| ⌘P | Files |

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
- Several chats can work at once (ADR 0017). Moving to another chat while a turn runs leaves
  it running in the background: the sidebar marks it with a dot, its questions wait in the
  tray under "In ‹chat›", and opening it again shows its turn as it goes. A chat in the
  background ends with its turn; its session is stored as usual.
- While an agent uses the person's computer (ADR 0018), the top bar shows "‹who› is using your
  computer · Stop". Stop takes the screen back; nothing shows otherwise.
- Opening a session loads its stored turns for display (paged, "Show earlier turns") and
  restores ARK working state. The model still receives only the bounded carry-over: stored
  transcript is not model context.

## Building a workspace

From ARK Desktop (`ark-desktop/src/main.rs`, `setup.rs`), trimmed; its `ui_files!` macro is
one `.asset` per file:

```rust
let ui = ArkUi::new(app(root, secrets)?)
    .asset("logo.svg", include_bytes!("../ui/logo.svg").as_slice())
    .asset("desktop.js", include_bytes!("../ui/desktop.js").as_slice())
    .asset("desktop.css", include_bytes!("../ui/desktop.css").as_slice())
    .theme(Theme::Light)
    .keep_running(true)
    .accent("#335eb7")
    .logo("logo.svg")
    .script("desktop.js")
    .stylesheet("desktop.css")
    .welcome("What should we work on?", "")
    .suggestion("Make a checklist", "Make a packing checklist for a three-day work trip and save it as packing.md.")
    .handler("desktop.folder", |ctx, _v| async move {
        let dir = ctx.engine.workspace_dir(&ctx.project.id)?;   // in the app: ~/Documents/ARK/<name> on a Mac
        std::fs::create_dir_all(&dir)?;
        Ok(json!({ "path": dir }))
    });
```

| `ArkUi` method | Purpose |
|---|---|
| `new(app)`, `.start(Start)` | The app, and model/project/session choices as the shell's flags give them |
| `.accent(css)`, `.logo(asset)` | Theme colour and logo |
| `.theme(Theme::Light)` | The theme until the person picks one in Settings › Appearance (default `Theme::System`) |
| `.welcome(title, line)`, `.suggestion(label, prompt)` | The empty-session screen; an empty `line` shows no line (without `.welcome`, the tagline) |
| `.asset(path, bytes)` | A file the page can load, served at `ext/<path>` |
| `.script(asset)`, `.stylesheet(asset)` | Load an asset as a module or stylesheet after ARK's own |
| `.handler(name, f)` | A backend method the app's scripts call with `ark.ext(name, params)` |
| `.keep_running(true)` | The native window: closing it hides it and the app keeps running (schedules, agents' work), with Open and Quit in the menu bar (macOS) or the tray (Windows, Linux); the Dock icon opens it again. Default: closing quits |
| `.open()`, `.serve(addr)`, `.serve_background(addr)`, `.run_desktop(ctx)` | Start |

A handler gets `HandlerCtx { engine, project, emitter }`. `emitter.emit(name, payload)`
pushes an event the page receives with `ark.on(name, fn)`; ARK Desktop sends the progress
of a Copy to Cloud this way (`desktop:cloud_copy`).

## The page extension API

App scripts register on `window.ark`. ARK does not know what any of it means.

| Call | Registers |
|---|---|
| `ark.panel({ id, title, icon, nav, presents, wide, fill, render })` | A side-pane panel; `nav: true` lists it in the top bar's + menu. An app panel with the id `terminal`, `browser` or `files` replaces ARK's, and an app panel's `presents` wins over a built-in one for the same view |
| `ark.view({ id, title, icon, nav, render })` | A view of the current project in the main column, in place of the conversation; `nav: true` makes it a top-bar tab (a string names the tab). Built once, kept while hidden |
| `ark.setting({ id, title, icon, advanced, render(el, api) })` | A section of the Settings page; `advanced: true` lists it under Advanced |
| `ark.modelPicker({ label(model), render(el, { close }) })` | The app draws the composer's model menu. `label` gets the host's model ("claude sonnet") and returns the button's text, or `{ text, missing: true }` while nothing is chosen. `ark.refreshModel()` asks again after the app's state changed |
| `ark.onSubmit(fn)` | `fn(text)` sees each message before it is sent (typed, queued or a suggestion; not `/commands`). Returning true (or a promise of true) means the app took it and nothing is sent |
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
`undefined` in a browser, which cannot see the runtime's disk; ask for a typed path then). `ark.isLocal()`: true when the runtime runs on the computer
showing the page (the window's own runtime, or a browser on this machine's loopback address),
false for a runtime elsewhere; `<html data-runtime="local|remote">` says the same for styles.
Offer actions that need the person's own computer (open a local app, a native picker, "on
this Mac") only when it is true.

In the window the Browser panel's page is drawn above the page (ADR 0044). An app's element that
can cover the pane (a dialog, a menu, a sheet) carries `data-ark-overlay`, and the Browser steps
aside while it is shown. `ark.browserView` is the window's carrier for the Browser panel
(`undefined` in a browser: `show`, `act`, `reach`, `listen`); apps do not need it.

A small real viewer, ARK Desktop's diagrams (`ui/diagram.js`, trimmed):

```js
ark.artifactViewer({
  id: "diagram", label: "Diagram", wide: true, presents: ["diagram"],
  match: (a) => a.kind === "diagram" || /\.(mmd|mermaid)$/i.test(a.name),
  render(el, artifact) {
    const stage = h("div.dk-diagram");
    el.append(stage);
    ark.call("artifact", { id: artifact.id }).then((r) => renderMermaid(stage, r.text || ""));
    return { focus: () => ({ surface: { kind_tag: "diagram", title: artifact.name, artifact: artifact.id } }) };
  },
});
```

ARK Desktop adds its agents view, the inspector, its model menu, the account and Cloud settings,
the This Mac | Cloud switch and the diagram viewer this way, one module each under `ui/`.

## Protocol

Both transports carry `call(method, params) -> Value` and one event stream. A custom
front-end uses the same.

| Methods | |
|---|---|
| Skills | `skills` (each: `name`, `description`, `scope` `project` or `store`, `chat`: the conversation may read it). Does not wait for a running turn |
| Plugins | `plugins` (each: `name`, `command`, `args`, `env_keys`, `running`, `tools`, `error`; null when the app has none), `plugin_add {text}` or `{spec}` (answers the plugins added), `plugin_remove {name}`, `plugin_restart {name}`. None waits for a running turn |
| Workbench | `frame_check {url}` (whether a frame may show the page, from its headers: `{framable, reason?}`; public addresses only), `term_open {cols, rows}` (the project's shell: `session`, `cwd`, `history`), `term_write {session, data}`, `term_resize`, `term_close`, `files {path?}` (entries with `kind`) |
| Files | `file_read {path}` (`kind`, `version` and `text`, `sheets` or `blocks`; or `why` it cannot be shown), `file_edit {path, edits, base?}` (the file as saved), `file_write {path, text, base?}` (text files), `file_version {path}`, `file_media {path}` (a PDF or an image: `media_type`, `version`, base64 `data`), `file_open {path}` (in its app), `file_restore {artifact}` (puts a file back as a version kept before a call changed it, ADR 0034: `{path, undo}`, `undo` puts back what it replaced). All inside the project folder; none waits for a running turn |
| Uploads | `upload_parts {upload, parts}`, then `upload_project {upload, name}` (a new project, ADR 0030) or `upload_add {upload}` (into the open project's folder: `{added}`, names in use get a number, ADR 0035), or `upload_cancel {upload}`. None waits for a running turn |
| Session | `hello`, `summary`, `sessions {project?}`, `projects`, `new_session`, `open_session`, `rename_session`, `switch_project`, `open_project {id}` (back to its last chat), `add_project {path?, name?}` (a folder becomes a project, or opens the one it is), `rename_project {id, name}`, `project_folder {id, path}`, `workspace {path?}` (the project's folder; `/folder` in the terminal), `model`, `models`, `subscription_usage` (each subscription's limits as its CLI last reported them: `[{account, plan?, windows: [{minutes, used_percent, resets_at}], limited, reported_ms}]`) |
| Keys | `api_keys` (`can_save`, `saved`: providers with a saved key), `api_key_save {provider, key}` (checked with the provider, then kept in the system keychain), `api_key_forget {provider}`. Need an app with connections; a key in the environment still comes first |
| Turn | `send` (`ask: false` refuses the turn's state-changing actions instead of asking: a client nobody answers), `steer`, `cancel` (both take `session` for a chat in the background), `approve {id, decision}` (a live question's number, or a kept question's string id: a task's question after a restart, ADR 0037), `command`, `working` (the chats running a turn: `[{session, title, project, input}]`) |
| Views | `transcript {session?}`, `trace`, `call`, `context`, `usage`, `state` |
| Computer | `computer_stop` (take the screen back from the agent using it) |
| Store | `artifacts`, `artifact` (the project's, or one an orchestrated agent produced), `invocation`, `run_action` |
| Agents | `graph` (each agent has `doing`: what it does now in words, as `/agents` shows it), `agents`, `tools`, `profiles` (what `AgentSpec.profile_name` may name), `spawn_agent {spec}`, `suspend_agent`, `resume_agent`, `terminate_agent`, `message_agent {agent, text}` |
| Tasks | `submit_task {agent, task}`, `task {task}` (record and timeline lines), `task_trace`, `cancel_task`, `retry_task`, `unschedule {schedule}`, `orch_events {after, limit}` |
| Board | `board` (`items`, the `tasks` run for them, the live `agents`; null without an orchestrator), `board_add {title, note?, owner?, state?, blocked?, before?}`, `board_change {item, change}` (a `WorkChange`), `board_remove {item}`, `board_give {item, agent}` (answers the `task`). Follow it with `orch` events of type `work` and `work_removed` |
| Waiting for you | `attention` (what waits in any project: questions tasks keep, failures, results posted into a chat; `ark_interact::attention::Item`), `attention_open {id}` (clears a failure or a result and returns it with `text`), `attention_clear {id}` (not a question: it leaves when answered), `attention_seen {session}` (the window shows this chat to the person: what was posted there is seen; a window sends it on a post into the chat it shows while in view, and when it comes back into view) |
| Notifications to devices | `push_key` (the runtime's public key, for `PushManager.subscribe`), `push_add {subscription, name}` (the server adds who asked and the page's address), `push_remove {id \| endpoint}`, `push_devices` (names and times), `push_here {endpoint, visible}` (this device's page is in view: it is not notified of what it shows), `devices_signed_in` (devices signed in with a code), `device_sign_out {name}`. `POST /pair` makes a code for another device. ADR 0047 |
| Page | `focus`, `ext` (an app handler) |

| Events (`kind`) | |
|---|---|
| `turn_started`, `text`, `event`, `turn_finished` | A turn, with its chat's `session`. `event` carries the `HarnessEvent` and its described `Activity`. `turn_finished` carries `summary` only for the chat windows show |
| `working` | The chats running a turn changed (`working`, as the method answers) |
| `computer` | Who uses the screen changed (`user`: `{owner, who, session}` or null) |
| `term` | A terminal's output (`session`, `data`), or `exit: true`. Not replayed to a window that rejoins a turn; `term_open` returns the scrollback |
| `approval`, `approval_closed` | A pending approval (`request`, `line`, `title`; for a chat's question `session` and `chat`, its title; for an agent's question `project`, `{id, name}`), and its end (answered, or the turn ended) |
| `attention` | What waits for the person may have changed in some project (a question asked or answered, a task queued or finished, a post, a clear): read `attention` again |
| `post` | Work a chat set up posted its result there (`session`, `post`: `{from, title, text, ok, at, after}`, ADR 0025). Stored turns from `transcript` carry theirs in `posts` |
| `summary` | Session, model, project or context changed |
| `files_changed` | Files of a project changed without a tool call: a restore or files added (`project`, `paths`) |
| `ext` | An app handler's `emitter.emit` |
| `orch` | An orchestration event: `event` (an `OrchEvent`), `line` and `tone` (its timeline line, from `ark_interact::orchestration::event_line`), `activity` (for an agent's runtime event, what the conversation would show; none for steps and context reports) |

App scripts following these with `ark.on("ark:…")` see every chat's turn events, the ones in
the background too: compare `msg.session` with `ark.summary().session.id` to keep to the open chat.

The agent and task methods need an app with an orchestrator and do not wait for the
conversation: they answer while a turn runs. The ones that change something build an
`ark_interact::orchestration::Control`, the same one the terminal's `/agent` and `/task`
build, and only shape the answer. `spawn_agent` takes an `AgentSpec` as JSON and
`submit_task` a `TaskSpec`; both are what the orchestrator stores, so the same definitions
work wherever the runtime runs. Tests: `tests/orchestration.rs`.

Every event carries `seq`, one higher than the last. `hello` returns `seq` (everything up to
it is in its answer), `approvals` (live questions and those tasks kept across a restart), `working`, `computer`, and `live`, the open chat's running turn's events so far, so a window that opens or
reconnects mid-turn rebuilds the turn and then follows it; the page drops events it has
already shown and reloads on a gap. In a browser the stream reconnects with backoff (1 s
doubling to 30 s) and stops after ten failures with a Retry button.

Locally, the HTTP server requires the per-run token on every request except static files
and `GET /health`, and rejects a `Host` header that is not the loopback address, so another
local process or a web page cannot drive the agent. With saved access tokens
(`Access::Tokens`) it accepts any `Host` and checks the token instead: a runtime other machines
connect to ([`REMOTE.md`](REMOTE.md), ADR 0016). A token is read from `Authorization: Bearer`,
`X-Ark-Token` or `?token=` (the page's `EventSource`).

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

- The page is plain ES modules with no build step and no CDN. xterm.js 5.3 (MIT) ships in
  `web/vendor/xterm/` for the terminal. App scripts may ship their own libraries (ARK Desktop
  ships Mermaid in its `ui/vendor/`).
- In the window, while the person types in the Browser's page, ARK's keys (⌘K, ⌘N, Esc) go to
  the page; a click in ARK gives them back. In a browser the Browser is a frame, and a page on
  this machine that refuses frames shows blank. The editor is a plain text area (no
  highlighting).
- A spreadsheet tab shows values, not cell styles (colours, fonts, merged cells); a document
  shows headings, lists, tables, bold and italic, not fonts or images (an image is a marker).
  Both survive edits. No undo beyond the browser's own while typing.
- A `present` directive for a view nothing shows (no panel or viewer lists it) is a toast in
  the window; the model is still told it was presented.
- Windows on one `Host` show one chat, the same one: two browser tabs on one server, or a
  window and a browser on a remote runtime, move together. Other chats can be working in the
  background.
- "Always in this project" grants last until removed in Settings › Permissions (or
  `/grants`); "for this session" grants end with the process, as in the shell.
- There is no macOS WebDriver for the native window; its page is tested in WebKit instead,
  and an app's bundle can be checked through its browser transport.
