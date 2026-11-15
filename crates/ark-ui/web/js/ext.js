import { setAgentIdentity } from "./dom.js";
// The extension points an app's scripts use, available as `window.ark`:
//
//   ark.panel({ id, title, icon, nav, presents, wide, fill, expandable, render(el, params, api) })
//                      expandable offers a full workspace with the active conversation's
//                      existing composer and approvals in a floating dock. It does not
//                      create a conversation or bind the resource to an agent.
//   ark.view({ id, title, icon, nav, command, render(el, params, api) })
//                      a view of the current project in the main column, in place of the
//                      conversation; `nav` makes it a button in the sidebar's foot.
//                      `command` optionally opens the view from an exact slash command.
//                      resourceScoped views receive a captured opening API and are disposed on
//                      project or runtime changes. projectScoped views keep the live API and update. Controllers may provide title(), menu(), rename(),
//                      focusInput(), cancel(), show(), hide(), and captureUploadOrigin() for a primary conversation.
//                      Choosing Chat brings the conversation back. A panel's `nav` lists it in
//                      the title bar's + menu, after ARK's Terminal, Browser and Files.
//                      A view with its own conversation can return conversation():
//                      { surface, title, location, focus(resourceFocus) }. The surface is its
//                      existing chatView element; location names the actual runtime/project.
//                      focus receives a frozen snapshot to include in that recipient's work.
//                      An `agentConversation` view is an agent's chat: the sidebar lists agents
//                      only when one is registered. Its params are `{ name, project?, conversation? }`
//                      (`conversation`: a chat of that agent, "new" for one started on the first
//                      message, none for its first chat). Its identity() may add
//                      `agent: { name, project, conversation, program }` and `rename(title)`.
//   ark.setting({ id, title, icon, advanced, render(el, api) })
//                      a section of the Settings page: the app's or the account's
//                      configuration, not one project's. `advanced` lists it under Advanced.
//   ark.plugin({ id, name, summary, category, icon, render(el, api) })
//                      a trusted app integration in Plugins; render returns optional dispose().
//                      icon is a bundled same-origin image URL. Listing does not enable it.
//   ark.modelPicker({ label(model), mark?(model), render(el, { close }) })
//                      the app draws the composer's model menu and names the model on its
//                      button. `label` gets the host's model ("claude sonnet") and returns
//                      the text, or `{ text, missing: true }` while nothing is chosen: then
//                      sending opens the menu instead. `ark.refreshModel()` asks again.
//                      `mark(model)` returns the provider's own icon, shown in the chat's details.
//   ark.artifactViewer({ id, label, match(artifact), primary, presents, wide, render(el, artifact, api, params) })
//   ark.fileViewer({ id, kinds, render(el, file, api) })
//                      trusted shipped code over immutable file bytes. `api.read(offset)`
//                      returns one bounded Uint8Array; parse untrusted bytes in a worker.
//                      `api.confirm(label)`: one local opening choice beside Cancel,
//                      abort-safe, no saved grant.
//   ark.toolRenderer({ match(action), render(el, call, api) })
//   ark.action({ id, label, icon, where: "artifact" | "palette" | "project" | "agent", when(ctx), run(ctx, api) })
//                      `agent`: an item in an agent tile's menu (not ARK's); `ctx` is
//                      `{ agent: { name, project, title } }`
//                      `group`: an item in a group tile's menu; `ctx` is `{ group: { id, name } }`
//                      `project`: an item in the chat's Folder menu (its details); `ctx` is
//                      `{ project }` (`id`, `name`, `workspace`, `current`)
//   ark.newChat({ open({ project, location, createChat }) })
//     One consumer handler for explicit New chat actions. createChat keeps the captured origin.
//   ark.view({ ..., groupConversation: true }) draws a group's chat (`{ id }`): the sidebar then
//     lists the runtime's groups (`groups`) as tiles that open it.
//   ark.newAgent({ open({ location, anchor }), assistant })
//     What the sidebar's + does: make an agent and open its chat. Without it, + starts a chat.
//     `assistant: false` leaves the built-in assistant out of the sidebar: the window opens
//     the latest agent, or offers New agent when there is none.
//
//   ark.chatView(el, { submit(text), stop(), call(key, id), placeholder, model, files })
//                      a conversation the app draws from its own records with the chat's
//                      own view, so it looks and reads and behaves as a chat (ADR 0076).
//                      Returns `{ render(turns, { empty }), reset(), input, actions, focus(),
//                      set(text), addLine(text, { focus }), addImages(items), stashImages(),
//                      restoreImages(kept), uploads,
//                      setComposer({ placeholder, disabled, running, modelLabel, modelDisabled, submitDisabled }), dispose() }`.
//                      Optional `model.open(anchor)` opens the consumer's real model picker.
//                      Optional `files.captureOrigin()` runs before the chooser opens and returns
//                      `{ uploadApi, appendDraft(line, images?), progressTarget, started?, finished?,
//                      attachImages? }`, all bound to that origin. With `attachImages`, a dropped image
//                      comes to `appendDraft` as `images` (`{ path, name, url }`, for `addImages`) and
//                      leaves with the message: `submit(text, { images, named })`, `named` meaning the
//                      text is only their file names. Answer `attachImages` from `ark.at(location).taskImages()`, with
//                      the chat's own location.
//                      `uploads` is this view's upload container.
//                      Optional `exchanges` (ADR 0119): `{ project, agent, session?, name?, look? }`, or
//                      a function returning it or null, read at each use. Turns keyed `task:<id>`
//                      get a quiet line when that task reached other agents; it opens their messages
//                      over this view (read only; Back or Esc returns). A view that switches agent or
//                      chat calls `view.refreshExchanges()` after; null means no lines and no calls.
//                      Names take `--pet-ink`; the app sets it per `data-variant`.
//                      `turns` take `chat_read`'s transcript shape plus `key`, `note`,
//                      `working: { label, since }`, `status`, `after` (an app's node under
//                      the reply), `streaming` (text being written now) and `approvals`
//                      (questions from `ark.approvals()` the turn waits on, answered in
//                      place). `submit` gets each message the person sends; `actions` holds
//                      the app's controls beside Send. `call(key, id)` returns a tool call's
//                      detail (`task_call`'s shape) so its row opens; with `stop` and
//                      `setComposer({ running: true })` the send button is Stop while
//                      nothing is typed.
//   ark.approvals()    the questions waiting for an answer (`ark:approval` messages), also
//                      those asked before the app's code ran
//   ark.onSubmit(fn)  `fn(text)` sees each message before it is sent (typed, queued or a
//                      suggestion; not commands). Return true, or a promise of true, when the
//                      app took it; the conversation then sends nothing.
//   ark.status({ id, render(el, api) })
//   ark.refreshNavigation() redraws project-dependent app tabs and panel launchers.
//   ark.sidebar({ id, render(el, api) })
//                      a row above the agents, for what the list as a whole depends on (an app
//                      with runtimes in several places says which here)
//   ark.rail, ark.projectSection
//                      removed (ADR 0110): agents are the sidebar, pinned from a tile's menu.
//                      Calling either throws a `RemovedExtensionPoint` error.
//   ark.profile(() => ({ name }) | null)
//                      who the person is: the first line of an empty chat and the sidebar's
//                      profile button. Called each time.
//   ark.agentIdentity((name, look) => styleKey)
//                      one stable style key per assigned participant, shared by single and group
//                      chats. `look` is the agent's `spec.look` when the caller knows it.
//   ark.searchSource({ id, load(project, api), open(entry, scopedApi) })
//                      project search rows: { id, title, description?, updated_at? }.
//                      Loading never navigates. Opening uses the selected project's API.
//   ark.askFolder(anchor, title) the picker, or a typed path under `anchor` in a browser; null if dismissed
//   ark.hiddenFolders()  ids of the folders the person removed from the sidebar; leave their agents out
//   ark.pickFolder({ title })   the window's folder picker: a path, null if dismissed,
//                      undefined where there is none (a browser)
//   ark.updates        signed native-app updates: `status()`, `check()`, `restart()`, `listen(fn)`. The
//                      object is undefined in a browser; status is null in development or
//                      when the packaged app did not opt in. A packaged but unsupported OS
//                      returns `{ state: "unavailable", reason: "platform" }`. No release input
//                      comes from JS.
//   ark.pageUpdates    signed page-only updates: `status()`, `check()`, `apply()`, `listen(fn)`.
//                      The native app fixes the endpoint, key and compatibility contract. The
//                      object is undefined outside its own native page.
//   ark.isLocal()      true when the runtime runs on this computer; false for a runtime
//                      elsewhere. Also `<html data-runtime="local|remote">`, for styles
//   ark.openLocal()    only in the window on a runtime elsewhere it opened (Rust:
//                      `ark_ui::remote::visit`): the window goes back to its own runtime.
//                      Undefined everywhere else
//   ark.ready()        waits until the first host summary has a resource location, then returns
//                      the current summary. App modules use it before their first project call.
//                      It stays pending through a recoverable startup connection failure.
//   ark.on(name, fn)   an app handler's events, and ARK's own as `ark:<kind>` (in a browser
//                      also `ark:reconnecting`, `ark:disconnected` with `retry()`, `ark:connected`)
//                      (`ark:turn_finished`, `ark:event`, `ark:summary`, ...)
//
// A panel's or viewer's `render` may return `{ update(params), refresh(), focus(), dispose() }`.
//
// In the window, the Browser panel's page is a view of its own over the pane (ADR 0044) and is
// drawn above the page. An app's element that can cover the pane (a dialog, a menu, a sheet)
// carries `data-ark-overlay`: the Browser view steps aside while it is shown.
// `focus()` says what it shows (`{ surface, selection }`); the agent sees it while visible.
// `wide` asks for a wider side pane; `fill` drops the pane's padding (editors, terminals).
//
// Nothing here knows any domain; the registry only holds what apps register.

export const registry = {
  panels: new Map(),
  views: new Map(),
  settings: new Map(),
  plugins: new Map(),
  viewers: [],
  fileViewers: new Map(),
  toolRenderers: [],
  actions: [],
  statuses: [],
  sidebars: [],
  profile: null,
  searchSources: new Map(),
  newChat: null,
  newAgent: null,
  modelPicker: null,
  submitHooks: [],
};

// A consumer entry module registers only after its complete static import graph evaluated.
// Script order cannot provide that guarantee for separate ES module graphs.
const extensions = new Map();
const extensionWaiters = new Map();

function extensionReady(id, details = {}) {
  need({ id }, ["id"], "extensionReady");
  if (extensions.has(id)) throw new Error(`ark.extensionReady registered \`${id}\` twice`);
  const record = { id, ...details };
  extensions.set(id, record);
  extensionWaiters.get(id)?.resolve(record);
  window.dispatchEvent(new CustomEvent("ark:extension-ready", { detail: record }));
}

export function waitForExtension(id) {
  if (extensions.has(id)) return Promise.resolve(extensions.get(id));
  if (!extensionWaiters.has(id)) {
    let resolve;
    const promise = new Promise((done) => (resolve = done));
    extensionWaiters.set(id, { promise, resolve });
  }
  return extensionWaiters.get(id).promise;
}

export function loadedExtensions() {
  return [...extensions.values()];
}

const listeners = new Map();

export function onExt(name, fn) {
  if (!listeners.has(name)) listeners.set(name, new Set());
  listeners.get(name).add(fn);
  return () => listeners.get(name).delete(fn);
}

export function dispatchExt(name, payload) {
  for (const fn of listeners.get(name) || []) {
    try {
      fn(payload);
    } catch (e) {
      console.error(`ark: listener for ${name} failed`, e);
    }
  }
}

/// A page extension point that no longer exists. The message says what replaced it.
export class RemovedExtensionPoint extends Error {
  constructor(point) {
    super(`ark.${point} was removed (ADR 0110): agents are the sidebar and are pinned from a tile's menu`);
    this.name = "RemovedExtensionPoint";
  }
}

function need(obj, keys, what) {
  for (const k of keys) if (!obj || obj[k] == null) throw new Error(`ark.${what} needs \`${k}\``);
}

export function installApi(api) {
  const ark = {
    ...api,
    agentIdentity(resolve) {
      setAgentIdentity(resolve);
      api._changed(true);
    },
    panel(p) {
      need(p, ["id", "title", "render"], "panel");
      registry.panels.set(p.id, p);
      api._changed();
    },
    view(v) {
      need(v, ["id", "title", "render"], "view");
      registry.views.set(v.id, v);
      api._changed();
    },
    setting(s) {
      need(s, ["id", "title", "render"], "setting");
      // An app's section keeps its id: ARK's own, registered later, does not replace it.
      if (s.builtin && registry.settings.has(s.id) && !registry.settings.get(s.id).builtin) return;
      registry.settings.set(s.id, s);
      api._changed();
    },
    plugin(entry) {
      need(entry, ["id", "name", "summary", "category", "icon", "render"], "plugin");
      if (["id", "name", "summary", "category", "icon"].some(key => typeof entry[key] !== "string" || !entry[key].trim())
          || typeof entry.render !== "function" || registry.plugins.has(entry.id)) throw new Error("Invalid or duplicate plugin");
      const image = new URL(entry.icon, document.baseURI);
      if (image.origin !== location.origin || !["http:", "https:", "tauri:"].includes(image.protocol)) throw new Error("Plugin artwork must be bundled with the app");
      registry.plugins.set(entry.id, entry);
      api._changed();
    },
    artifactViewer(v) {
      need(v, ["id", "label", "match", "render"], "artifactViewer");
      registry.viewers.push(v);
    },
    fileViewer(v) {
      need(v, ["id", "kinds", "render"], "fileViewer");
      if (!Array.isArray(v.kinds) || !v.kinds.length || v.kinds.some(k => !["map", "model"].includes(k))
        || typeof v.render !== "function" || registry.fileViewers.has(v.id)) throw new Error("Invalid or duplicate file viewer");
      registry.fileViewers.set(v.id, v);
    },
    toolRenderer(r) {
      need(r, ["match", "render"], "toolRenderer");
      registry.toolRenderers.push(r);
    },
    action(a) {
      need(a, ["id", "label", "run"], "action");
      registry.actions.push(a);
    },
    newChat(handler) {
      if (!handler || typeof handler.open !== "function" || handler.render != null && typeof handler.render !== "function" || registry.newChat) throw new Error("Invalid or duplicate New chat handler");
      registry.newChat = handler;
    },
    newAgent(handler) {
      if (!handler || typeof handler.open !== "function" || registry.newAgent) throw new Error("Invalid or duplicate New agent handler");
      registry.newAgent = handler;
      api._changed();
    },
    modelPicker(p) {
      need(p, ["label", "render"], "modelPicker");
      registry.modelPicker = p;
      api._changed();
    },
    onSubmit(fn) {
      registry.submitHooks.push(fn);
    },
    status(s) {
      need(s, ["id", "render"], "status");
      registry.statuses.push(s);
      api._changed();
    },
    sidebar(s) {
      need(s, ["id", "render"], "sidebar");
      registry.sidebars.push(s);
      api._changed();
    },
    rail() {
      throw new RemovedExtensionPoint("rail");
    },
    profile(fn) {
      if (typeof fn !== "function" || registry.profile) throw new Error("Invalid or duplicate profile");
      registry.profile = fn;
    },
    searchSource(source) {
      need(source, ["id", "load", "open"], "searchSource");
      if (typeof source.id !== "string" || !source.id.trim() || typeof source.load !== "function"
        || typeof source.open !== "function" || registry.searchSources.has(source.id)) {
        throw new Error("Invalid or duplicate search source");
      }
      registry.searchSources.set(source.id, source);
    },
    projectSection() {
      throw new RemovedExtensionPoint("projectSection");
    },
    extensionReady,
    on: onExt,
  };
  delete ark._changed;
  window.ark = ark;
  window.dispatchEvent(new CustomEvent("ark:api-ready", { detail: ark }));
  return ark;
}

/// Who handles a `present` directive for `view`: a panel or an artifact viewer that lists it.
/// An app's panel comes before ARK's built-in one for the same view.
export function presenterFor(view) {
  for (const v of registry.views.values()) if ((v.presents || []).includes(view)) return { view: v };
  const panels = [...registry.panels.values()].sort((a, b) => !!a.builtin - !!b.builtin);
  for (const p of panels) if ((p.presents || []).includes(view)) return { panel: p };
  for (const v of registry.viewers) if ((v.presents || []).includes(view)) return { viewer: v };
  return null;
}

export function viewForCommand(line, project) {
  return [...registry.views.values()].find(v => v.command === line.trim() && (!v.when || v.when({ project })));
}
