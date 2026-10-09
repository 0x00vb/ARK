// Boot and glue. Connects to the host, installs `window.ark` for app scripts, then wires the
// conversation, composer, sidebar and side pane to the host's event stream.

import { connect } from "./transport.js";
import { h, icon, clear, $, hydrateIcons, tokens } from "./dom.js";
import { markdown } from "./md.js";
import { Thread, approvalCard } from "./conversation.js";
import { Composer } from "./composer.js";
import { acceptDrops } from "./drop.js";
import { Sidebar } from "./sidebar.js";
import { Topbar } from "./topbar.js";
import { SETTINGS, settingsView, registerAppearance } from "./settings.js";
import { Pane, invocationView, artifactView } from "./pane.js";
import { registry, installApi, dispatchExt, presenterFor, onExt } from "./ext.js";
import { registerBuiltins, BUILTINS } from "./workbench.js";
import { fileView, kindOf, KIND_ICON, setFileKinds } from "./file.js";
import { registerCapabilities } from "./capabilities.js";
import { registerGrants } from "./grants.js";
import { registerPlugins } from "./plugins.js";
import { registerSkills } from "./skills.js";
import { registerBoard } from "./board.js";
import { registerSchedules, schedulesFollow } from "./schedules.js";
import { Waiting } from "./waiting.js";
import { registerDevices, keepInStep, openItems, deviceName } from "./devices.js";
import { os, label, relabel } from "./keys.js";
import { Places } from "./places.js";
import { registerComputers } from "./computers.js";

const transport = connect();
document.documentElement.dataset.runtime = transport.local ? "local" : "remote";
const appEl = $("#app");
const state = { summary: null, ui: {}, runtime: null, sessionId: null, running: false, approvals: new Map(), working: [], extChanged: false, view: null };
const resourceOnline = new Map();
const locationKey = (location) => `${location?.runtime || ""}:${location?.project || ""}:${location?.session || ""}`;
let readyResolve;
let isReady = false;
const firstReady = new Promise((resolve) => (readyResolve = resolve));
const validLocation = (location) => !!(location?.runtime && location?.project && location?.session);
// Main-column views (`ark.view`), built on first show and kept while hidden.
const views = new Map();

// ---- the api views and app scripts use ------------------------------------------------------

const api = {
  h, icon, markdown,
  on: onExt,
  call: (method, params = {}) => RESOURCE_METHODS[method]
    ? scopedApi(currentLocation()).call(method, params)
    : transport.call(method, params),
  windowCall: (method, params = {}) => transport.call(method, params),
  ext: (name, params = {}) => transport.call("ext", { name, params }),
  runAction: (action, input = {}, approved = false) => scopedApi(currentLocation()).runAction(action, input, approved),
  setFocus: (focus) => scopedApi(currentLocation()).setFocus(focus),
  resourceRef: (kind, id = "", location = currentLocation()) => ({ location: { ...location }, kind, id: String(id || "") }),
  resource: (ref, kind, input = {}) => resourceOnline.get(locationKey(ref?.location)) === false
    ? Promise.reject(new Error("That computer is not answering"))
    : transport.call("resource", { ref, operation: { kind, input } }),
  at: (location) => scopedApi(location),
  // The system's folder picker in the window: a path, or null when dismissed. Undefined
  // where there is none (a browser), so the caller asks for a typed path instead.
  pickFolder: ({ title } = {}) => transport.pickFolder(title),
  // Whether there is a folder picker (the window's own runtime).
  canPickFolder: transport.kind === "desktop",
  // The window's own view for the Browser panel (`show(rect, url)`, `act(action)`,
  // `listen(fn)`); undefined in a browser, where the panel is a frame.
  browserView: transport.browserView,
  // Signed native-app updates. Undefined in a browser; `status()` is null in development and
  // when the packaged consumer did not opt in.
  updates: transport.updates,
  // True when the runtime runs on the computer showing this page (the window's own runtime,
  // or a browser on this machine). False for a runtime elsewhere: its files, programs and
  // sign-ins are on that machine, so actions that need this one should not be offered.
  isLocal: () => transport.local,
  // In the window, on a runtime elsewhere it opened: back to this computer's runtime.
  // Undefined anywhere else (the window's own page, a browser).
  openLocal: transport.openLocal,
  summary: () => state.summary,
  // App modules load beside this module, while the first `hello` is still in flight. Wait for
  // its resource location before making a project-scoped call. A failed hello can recover; this
  // promise therefore stays pending instead of caching the startup failure.
  ready: async () => {
    if (!isReady) await firstReady;
    return state.summary;
  },
  // This computer as a sentence names it ("this Mac"), once the places are read.
  hereName: () => places?.here,
  // Resolves once the page shows chat `id`, rebuilt (a chat the host just opened).
  shown: async (id) => {
    for (let i = 0; i < 200 && (state.sessionId !== id || feed.booting); i++) await new Promise((r) => setTimeout(r, 25));
  },
  project: () => state.summary?.project,
  send: (text) => submit(text),
  // Put text in the composer, unsent, with the conversation in view.
  compose: (text) => {
    showConversation();
    composer.set(text);
  },
  submit: (text) => submit(text),
  steer,
  cancel: () => transport.call("cancel").catch(() => {}),
  command: (line) => (line.endsWith(" ") ? composer.set(line) : runCommand(line)),
  newSession: () => {
    showConversation();
    return change("new_session");
  },
  // A code to sign in another device (Settings › Devices, ADR 0047).
  pairCode: () => transport.pairCode(),
  openSession: (id) => {
    showConversation();
    if (id !== state.sessionId) return change("open_session", { id });
  },
  openView: (id, params) => showView(id, params),
  showConversation: () => showConversation(),
  currentView: () => state.view,
  switchProject: (name) => change("switch_project", { name }),
  openProject: (id) => change("open_project", { id }),
  // `{ path }` makes a folder a project (or opens the one it is); `{ name }` an empty one.
  addProject: (params) => change("add_project", params),
  openSettings: (section) => showView(SETTINGS, section ? { section } : undefined),
  loadEarlier,
  openInvocation: (id) => {
    const location = currentLocation();
    const ark = scopedApi(location);
    const key = resourceKey("inv", location, id);
    pane.openTab(key, `inv:${String(id).replace(/^inv:/, "").slice(0, 6)}`, "file", (el) => invocationView(el, ark, id), undefined, { api: ark });
  },
  // A stored copy of a project file opens as the file, as it is now (one view per kind), unless
  // an app's viewer is asked for or matches it.
  openArtifact: (id, viewerId, params) => {
    const location = currentLocation();
    const ark = scopedApi(location);
    if (viewerId) return openArtifactTab(id, viewerId, params, location);
    ark.call("artifact", { id, length: 1 }).then((r) => {
      const a = r.artifact;
      const lens = registry.viewers.some((v) => { try { return v.match(a); } catch { return false; } });
      if (a.path && !lens) ark.openFile(a.path, params);
      else openArtifactTab(id, undefined, params, location);
    }).catch(() => openArtifactTab(id, undefined, params, location));
  },
  // A file of the project in a tab of its own: a spreadsheet, a document or text.
  openFile: (path, params) => {
    path = String(path).replace(/^\.\//, "");
    const location = currentLocation();
    const ark = scopedApi(location);
    pane.openTab(resourceKey("file", location, path), path.split("/").pop(), KIND_ICON[kindOf(path)], (el, p) => fileView(el, path, ark, p), params, { wide: true, fill: true, api: ark });
  },
  openPanel(id, params) {
    const p = registry.panels.get(id);
    if (!p) return toast(`No panel named ${id}`);
    const location = currentLocation();
    const ark = scopedApi(location);
    pane.openTab(resourceKey("panel", location, id), p.title, p.icon || "box", (el, prm) => p.render(el, prm, ark) || {}, params, { wide: !!p.wide, fill: !!p.fill, api: ark });
  },
  setPanelTitle: (id, title) => retitle(resourceKey("panel", currentLocation(), id), title),
  closePanel: (id) => pane.remove(resourceKey("panel", currentLocation(), id)),
  toast,
  popover,
  refreshSidebar: () => sidebar.refresh(state.sessionId),
  refreshModel: () => composer.setModel(state.summary?.model),
  // Ask the window to reflect a view's focus again (after its selection changed).
  reportFocus: () => reportFocus(),
  _changed: () => {
    state.extChanged = true;
    queueMicrotask(() => {
      if (!state.extChanged) return;
      state.extChanged = false;
      sidebar?.renderExtensions();
      topbar?.render();
      composer?.setModel(state.summary?.model);
    });
  },
};

const RESOURCE_METHODS = {
  term_open: ["terminal", "terminal_open"], term_read: ["terminal", "terminal_read"],
  term_write: ["terminal", "terminal_write"], term_resize: ["terminal", "terminal_resize"], term_close: ["terminal", "terminal_close"],
  procs: ["process", "processes"], proc_output: ["process", "process_output"], proc_stop: ["process", "process_stop"],
  files: ["file", "files"], file_read: ["file", "file_read"], file_write: ["file", "file_write"], file_edit: ["file", "file_edit"],
  file_media: ["file", "file_media"], file_version: ["file", "file_version"], file_restore: ["file", "file_restore"],
  upload_parts: ["file", "upload_parts"], upload_add: ["file", "upload_add"], upload_cancel: ["file", "upload_cancel"], file_open: ["file", "file_open"],
  workspace: ["project", "workspace"], state: ["project", "state"],
  graph: ["project", "graph"], task_trace: ["project", "task_trace"], task: ["project", "task"], orch_events: ["project", "orch_events"],
  agents: ["project", "agents"], spawn_agent: ["project", "spawn_agent"], submit_task: ["project", "submit_task"],
  cancel_task: ["project", "cancel_task"], retry_task: ["project", "retry_task"],
  schedules: ["project", "schedules"], schedule: ["project", "schedule"], unschedule: ["project", "unschedule"],
  schedule_pause: ["project", "schedule_pause"], schedule_resume: ["project", "schedule_resume"], schedule_run: ["project", "schedule_run"],
  board: ["project", "board"], board_add: ["project", "board_add"], board_change: ["project", "board_change"],
  board_remove: ["project", "board_remove"], board_give: ["project", "board_give"],
  suspend_agent: ["project", "suspend_agent"], resume_agent: ["project", "resume_agent"], terminate_agent: ["project", "terminate_agent"], message_agent: ["project", "message_agent"],
  profiles: ["project", "profiles"], grants: ["project", "grants"], grant_remove: ["project", "grant_remove"],
  skills: ["project", "skills"], tools: ["project", "tools"], usage: ["project", "usage"],
  artifacts: ["artifact", "artifacts"], artifact: ["artifact", "artifact"], invocation: ["invocation", "invocation"],
  picture: ["picture", "picture"], computer_picture: ["picture", "computer_picture"],
};

function currentLocation() {
  const location = state.summary?.location;
  if (!location?.runtime || !location?.project || !location?.session) throw new Error("This chat has no resource location");
  return location;
}

function resourceKey(prefix, location, id) {
  return `${prefix}:${location.runtime}:${location.project}:${location.session}:${id}`;
}

function scopedApi(location) {
  // Consumer capabilities may be installed or replaced after boot (the Desktop Browser view is
  // one). A resource tab snapshots its location, not the API object's early boot values.
  const scoped = Object.create(window.ark || api);
  scoped.location = { ...location };
  scoped.remoteResource = location.runtime !== state.runtime;
  scoped.online = () => resourceOnline.get(locationKey(location)) !== false;
  if (scoped.remoteResource) scoped.browserView = undefined;
  scoped.call = (method, params = {}) => {
    const spec = RESOURCE_METHODS[method];
    if (!spec) return (window.ark || api).call(method, params);
    const [kind, operation] = spec;
    const id = kind === "terminal" ? (params.session || "")
      : kind === "process" ? (params.id || "")
      : kind === "artifact" || kind === "invocation" ? (params.id || "")
      : kind === "picture" ? (params.invocation || params.app || "")
      : kind === "file" && method !== "files" && !method.startsWith("upload_") && method !== "file_restore" ? (params.path || "") : "";
    return api.resource(api.resourceRef(kind, id, location), operation, params);
  };
  if (api.on) scoped.on = (name, fn) => api.on(name, (message) => {
    if (["ark:term", "ark:proc", "ark:orch"].includes(name) && !message?.ref) return;
    if (message?.ref && JSON.stringify(message.ref.location) !== JSON.stringify(location)) return;
    if (message?.location && JSON.stringify(message.location) !== JSON.stringify(location)) return;
    fn(message);
  });
  scoped.ext = (name, params = {}) => api.resource(api.resourceRef("extension", name, location), "extension", { name, params });
  scoped.runAction = (action, input = {}, approved = false) => api.resource(api.resourceRef("project", "", location), "run_action", { action, input, approved });
  scoped.setFocus = (focus) => api.resource(api.resourceRef("project", "", location), "focus", { focus }).catch(() => {});
  scoped.openFile = (path, params) => {
    path = String(path).replace(/^\.\//, "");
    pane.openTab(resourceKey("file", location, path), path.split("/").pop(), KIND_ICON[kindOf(path)], (el, p) => fileView(el, path, scoped, p), params, { wide: true, fill: true, api: scoped });
  };
  scoped.openInvocation = (id) => {
    const key = resourceKey("inv", location, id);
    pane.openTab(key, `inv:${String(id).replace(/^inv:/, "").slice(0, 6)}`, "file", (el) => invocationView(el, scoped, id), undefined, { api: scoped });
  };
  scoped.openArtifact = (id, viewerId, params) => {
    if (viewerId) return openArtifactTab(id, viewerId, params, location);
    scoped.call("artifact", { id, length: 1 }).then((r) => {
      const artifact = r.artifact;
      const lens = registry.viewers.some((v) => { try { return v.match(artifact); } catch { return false; } });
      if (artifact.path && !lens) scoped.openFile(artifact.path, params);
      else openArtifactTab(id, undefined, params, location);
    }).catch(() => openArtifactTab(id, undefined, params, location));
  };
  scoped.openPanel = (id, params) => {
    const panel = registry.panels.get(id);
    if (!panel) return toast(`No panel named ${id}`);
    pane.openTab(resourceKey("panel", location, id), panel.title, panel.icon || "box", (el, prm) => panel.render(el, prm, scoped) || {}, params, { wide: !!panel.wide, fill: !!panel.fill, api: scoped });
  };
  scoped.setPanelTitle = (id, title) => retitle(resourceKey("panel", location, id), title);
  scoped.closePanel = (id) => pane.remove(resourceKey("panel", location, id));
  return scoped;
}

// Terminal, Browser and Files first, so an app's panel with the same id replaces one.
registerBuiltins(installApi(api));

let thread, composer, sidebar, topbar, pane, waiting, places;

function retitle(key, title) {
  const t = pane.tabs.get(key);
  if (t) {
    t.title = title;
    pane.renderTabs();
  }
}

// ---- small UI services ----------------------------------------------------------------------

let toastTimer;
function toast(msg) {
  const t = $("#toast");
  delete t.dataset.persist;
  t.textContent = msg;
  t.dataset.show = "true";
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.dataset.show = "false"), 3200);
}

function popover(anchor, children, { above = false, end = false } = {}) {
  document.querySelector(".popover")?.close?.();
  const el = h("div.popover", { role: "menu" }, children);
  document.body.append(el);
  const r = anchor.getBoundingClientRect();
  const w = Math.max(240, r.width);
  el.style.minWidth = `${w}px`;
  if (end) {
    el.style.right = `${Math.max(12, window.innerWidth - r.right)}px`;
    el.style.transformOrigin = "top right";
  } else el.style.left = `${Math.min(r.left, window.innerWidth - w - 12)}px`;
  if (above) {
    el.style.bottom = `${window.innerHeight - r.top + 6}px`;
    el.classList.add("above");
    el.style.transformOrigin = end ? "bottom right" : "bottom left";
  } else el.style.top = `${r.bottom + 6}px`;
  requestAnimationFrame(() => el.classList.add("open"));
  const onDown = (e) => { if (!el.contains(e.target) && e.target !== anchor) close(); };
  const onKey = (e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } };
  function close() {
    document.removeEventListener("mousedown", onDown, true);
    document.removeEventListener("keydown", onKey, true);
    el.classList.remove("open");
    setTimeout(() => el.remove(), 120);
  }
  el.close = close;
  setTimeout(() => {
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey, true);
  });
  return el;
}

// ---- actions --------------------------------------------------------------------------------

async function submit(text) {
  text = String(text || "").trim();
  if (!text) return;
  if (text.startsWith("/")) return runCommand(text);
  if (state.running) return composer.enqueue(text);
  // No model chosen yet: the text waits in the composer while the person picks one.
  if (composer.missing) {
    composer.set(text);
    return composer.modelMenu();
  }
  // The first message of a chat placed on another computer starts it there.
  if (await places.beforeSend(text)) return;
  for (const hook of registry.submitHooks) {
    try {
      if (await hook(text)) return;
    } catch (e) {
      return toast(e.message || String(e));
    }
  }
  thread.begin(text).local = true;
  setRunning(true);
  try {
    await transport.call("send", { text });
  } catch (e) {
    toast(e.message);
    thread.finish({ ok: false, error: e.message });
    setRunning(false);
  }
}

async function steer(text) {
  try {
    await transport.call("steer", { text });
    // It waits above the composer until the runtime delivers it at a step boundary; then it
    // joins the thread where the agent actually read it.
    composer.steering(text);
  } catch {
    composer.enqueue(text);
  }
}

async function runCommand(line) {
  if (state.running) return composer.enqueue(line);
  let reply;
  try {
    reply = await transport.call("command", { line });
  } catch (e) {
    return toast(e.message);
  }
  const effect = reply.effect;
  if (effect?.type === "clear_view") thread.clear();
  else if (effect?.type === "exit") transport.call("quit").catch(() => toast("Close the window to leave. The session is saved."));
  else if (effect?.type === "submit") {
    // The host started the turn; show it like any other.
    thread.begin(effect.text).local = true;
    setRunning(true);
  }
  thread.report(line, reply);
}

// Moving to another chat never waits: a chat that is working goes on in the background.
async function change(method, params) {
  try {
    await transport.call(method, params);
  } catch (e) {
    toast(e.message);
  }
}

async function loadEarlier(before) {
  try {
    const turns = await transport.call("transcript", { before, limit: 30 });
    thread.renderStored(turns, { prepend: true, hasMore: turns.length === 30 });
  } catch (e) {
    toast(e.message);
  }
}

function setRunning(on) {
  state.running = on;
  composer.setRunning(on);
  appEl.dataset.running = String(on);
  places?.show(state.summary, on);
}

// ---- session display -------------------------------------------------------------------------

function applySummary(s) {
  const changed = s.session.id !== state.sessionId;
  state.summary = s;
  if (!isReady && validLocation(s.location)) {
    isReady = true;
    readyResolve();
  }
  state.sessionId = s.session.id;
  state.title = s.session.title || (s.session.turns ? "Untitled chat" : "");
  if (s.location) resourceOnline.set(locationKey(s.location), !s.place || s.place.state === "ok");
  showTitle();
  composer.setModel(s.model);
  places?.show(s, state.running);
  // A chat on a computer that does not answer reads only.
  const away = !!s.place && s.place.state !== "ok";
  $("#composer").dataset.away = String(away);
  // Not by keyboard either.
  $("#input").readOnly = away;
  $("#send").disabled = away;
  document.title = s.session.title ? `${s.session.title} · ${s.name}` : s.name;
  return changed;
}

async function loadSession() {
  pane.reset(state.summary?.project?.id);
  placeAsks();
  thread.clear();
  const turns = await transport.call("transcript", { limit: 30 }).catch(() => []);
  if (turns.length) thread.renderStored(turns, { hasMore: turns.length === 30 });
  // A chat on a computer that does not answer is not empty: its turns are there. The line
  // above the composer says so.
  else if (!(state.summary?.place && state.summary.place.state !== "ok")) showWelcome();
  thread.scroll(true);
  sidebar.refresh(state.sessionId);
  refreshRing();
  schedulesFollow();
}

function showWelcome() {
  const w = state.ui.welcome || {};
  thread.welcome(
    // An empty subtitle from the app means none; no subtitle at all shows the tagline.
    { title: w.title, subtitle: w.subtitle ?? state.summary?.tagline, logo: state.ui.logo },
    w.suggestions || [],
    (prompt) => submit(prompt),
  );
}

function setRing(used, budget) {
  const ring = $("#ctx-ring");
  const pct = budget ? Math.min(1, used / budget) : 0;
  ring.textContent = used ? `${Math.round(pct * 100)}% context` : "";
  ring.dataset.level = pct > 0.9 ? "high" : pct > 0.7 ? "mid" : "low";
  ring.title = `${tokens(used)} of ${tokens(budget)} tokens · /context for the breakdown`;
}

async function refreshRing() {
  try {
    const v = await transport.call("context");
    setRing(v.total, v.budget);
  } catch {}
}

// ---- host events ------------------------------------------------------------------------------

// Every server event carries a sequence number. `hello` says how far its answer reaches, so
// events at or below it are already shown; events that arrive while the page boots wait in
// `early` and are applied after. A gap means events were lost: reload.
const feed = { seq: 0, booting: false, early: [] };

function receive(msg) {
  if (typeof msg.seq !== "number") return onEvent(msg);
  if (feed.booting) return void feed.early.push(msg);
  if (msg.seq <= feed.seq) return;
  if (feed.seq && msg.seq > feed.seq + 1) {
    feed.seq = msg.seq;
    return void boot(true);
  }
  feed.seq = msg.seq;
  onEvent(msg);
}

function connection(msg) {
  const el = $("#toast");
  if (msg.kind === "connected") return toast("Reconnected");
  if (msg.kind === "reconnecting") return toast(`Connection lost; retrying in ${Math.round(msg.delay / 1000)} s`);
  // Disconnected for good: say so until the person retries.
  clearTimeout(toastTimer);
  clear(el).append(...[h("span", "Disconnected from the agent. "), h("button.link", { onclick: () => { el.dataset.show = "false"; delete el.dataset.persist; msg.retry(); } }, "Retry"),
    // In the window on a runtime elsewhere: this computer's runtime is one click away.
    transport.openLocal ? h("span", " · ") : null,
    transport.openLocal ? h("button.link", { onclick: () => transport.openLocal() }, "Back to this computer") : null].filter(Boolean));
  el.dataset.persist = "";
  el.dataset.show = "true";
}

/// While an agent uses the person's computer: who, and a way to take it back. Nothing shows
/// otherwise.
function showComputer(user = state.computerUser, activity = state.computerActivity) {
  state.computerUser = user;
  state.computerActivity = activity;
  const el = $("#computer-use");
  clear(el);
  const hazard = ["hazardous", "submitted_unconfirmed"].includes(activity?.phase);
  el.hidden = !user && !hazard;
  if (el.hidden) return;
  const who = user ? h("button.computer-who", { type: "button", title: user.session ? "Open the chat" : "" },
    // On a runtime elsewhere it is not the person's own computer.
    h("span.working-dot"), activity?.line || `${user.who} is using ${(window.ark?.isLocal?.() ?? true) ? "your computer" : "the computer ARK runs on"}`)
    : h("span.computer-who", { role: "status", "aria-live": "polite" }, activity?.line || "Computer needs attention");
  if (user?.session) who.onclick = () => api.openSession(user.session);
  const stop = h("button.computer-stop", { type: "button", title: "Take the computer back" }, "Stop");
  stop.onclick = () => transport.call("computer_stop").catch((e) => toast(e.message));
  const check = hazard ? h("button.computer-stop", { type: "button", "aria-label": "Check the message app again" }, "Check again") : null;
  if (check) check.onclick = () => transport.call("computer_check").then(() => { state.computerActivity = null; showComputer(); }).catch((e) => toast(e.message));
  el.append(...[who, check, user ? stop : null].filter(Boolean));
}

/// Another chat's turn: it works in the background. Its question waits in the tray; the
/// sidebar marks it while it works.
function elsewhere(msg) {
  if (msg.kind === "approval") {
    state.approvals.set(msg.id, msg);
    ask(msg);
  } else if (msg.kind === "approval_closed") {
    state.approvals.delete(msg.id);
  } else if (msg.kind === "turn_finished" || msg.kind === "post") {
    sidebar.refresh(state.sessionId);
  }
}

function onEvent(msg) {
  // The session ended (signed out from another device, or its time is up): the code field.
  if (msg.kind === "signed_out") return signInScreen();
  if (msg.kind === "reconnecting" || msg.kind === "disconnected" || msg.kind === "connected") {
    dispatchExt(`ark:${msg.kind}`, msg);
    return connection(msg);
  }
  if (msg.kind === "resource_connection" && msg.location) {
    resourceOnline.set(locationKey(msg.location), !!msg.online);
  }
  // App scripts can follow the same stream: `ark.on("ark:turn_finished", fn)`.
  // A summary listener must see the new summary through `ark.summary()`, not the previous
  // chat. Summary is dispatched in its case below, after `applySummary`.
  if (msg.kind !== "ext" && msg.kind !== "text" && msg.kind !== "summary") dispatchExt(`ark:${msg.kind}`, msg);
  // A question an orchestrated agent asks belongs to no chat (`session` is empty).
  if (msg.session && msg.session !== state.sessionId) return elsewhere(msg);
  switch (msg.kind) {
    case "turn_started":
      if (!thread.live) {
        thread.begin(msg.input);
      }
      setRunning(true);
      break;
    case "text":
      thread.event(msg);
      break;
    case "event": {
      thread.event(msg);
      const ev = msg.event;
      if (ev.type === "context" && !["plan", "critic"].includes(ev.report.role)) setRing(ev.report.total, ev.report.budget);
      else if (ev.type === "steered") composer.delivered(ev.text);
      else if (ev.type === "present") present(ev.directive);
      break;
    }
    case "approval":
      state.approvals.set(msg.id, msg);
      // An orchestrated agent's question is not part of the conversation: it waits in the
      // tray, whatever the main column shows.
      if (msg.request?.agent) ask(msg);
      else thread.approval(msg, decide);
      break;
    case "approval_closed":
      state.approvals.delete(msg.id);
      thread.approvalClosed(msg.id);
      document.querySelector(`#asks .approval[data-id="${msg.id}"]`)?.remove();
      break;
    case "working":
      state.working = msg.working || [];
      sidebar.setWorking(state.working);
      break;
    case "computer":
      showComputer(msg.user, state.computerActivity);
      break;
    case "computer_action":
      if (msg.activity) msg.activity.line = msg.line;
      showComputer(state.computerUser, msg.activity);
      break;
    case "post":
      thread.post(msg.post);
      if (msg.session === state.sessionId) waiting.seen(state.sessionId);
      break;
    case "turn_finished": {
      thread.finish(msg);
      setRunning(false);
      if (msg.summary) applySummary(msg.summary);
      sidebar.refresh(state.sessionId);
      refreshRing();
      composer.clearSteering();
      const next = composer.takeNext(msg.undelivered || []);
      if (next) setTimeout(() => submit(next), 120);
      break;
    }
    case "summary":
      // Another chat: rebuild from the host, which knows whether it is working.
      {
        const changed = applySummary(msg.summary);
        dispatchExt("ark:summary", msg);
        if (changed) boot(true);
        else sidebar.refresh(state.sessionId);
      }
      break;
    case "attention":
      waiting.refresh();
      // A link came, went, or changed state: the places offered follow.
      if (msg.linked) places.load();
      break;
    case "place_changes":
      places.changed(msg);
      break;
    case "ext":
      dispatchExt(msg.name, msg.payload);
      break;
    case "resync":
      boot(true);
      break;
  }
}

function ask(msg) {
  if (document.querySelector(`.approval[data-id="${msg.id}"]`)) return;
  const card = approvalCard(msg, decide);
  // Another chat's question says which chat, and opens it.
  if (msg.session && !msg.request?.agent) {
    card.dataset.chat = msg.session;
    card.prepend(h("button.approval-chat", { type: "button", onclick: () => api.openSession(msg.session) },
      `In ${msg.chat || "another chat"}`));
  } else if (msg.project?.id) {
    // An agent's question from another project says which, and opens it: "Always in this
    // project" means that project.
    card.dataset.project = msg.project.id;
    card.prepend(h("button.approval-chat", { type: "button", onclick: () => api.openProject(msg.project.id) },
      `In ${msg.project.name}`));
  }
  $("#asks").append(card);
  placeAsks();
}

/// A question's project shows only while the window is in another one.
function placeAsks() {
  const here = state.summary?.project?.id;
  for (const card of document.querySelectorAll("#asks .approval[data-project]")) {
    card.querySelector(".approval-chat").hidden = card.dataset.project === here;
  }
}

async function decide(id, decision) {
  try {
    await transport.call("approve", { id, decision });
  } catch (e) {
    toast(e.message);
  }
}

function openArtifactTab(id, viewerId, params, location = currentLocation()) {
  const v = registry.viewers.find((x) => x.id === viewerId);
  const prm = params === undefined && viewerId ? { viewer: viewerId } : params && { ...params, viewer: viewerId };
  const ark = scopedApi(location);
  const key = resourceKey("art", location, id);
  pane.openTab(key, "Artifact", "file", (el, p) => {
    const ctl = artifactView(el, ark, id, viewerId, p);
    ark.call("artifact", { id, length: 1 }).then((r) => retitle(key, r.artifact.name)).catch(() => {});
    return ctl;
  }, prm, { wide: v ? !!v.wide : registry.viewers.some((x) => x.wide), fill: true, api: ark });
}

function present(d) {
  const by = presenterFor(d.view);
  // A file opens in a tab of its own, unless the app shows files itself.
  if (d.view === "file" && d.path && (!by?.panel || by.panel.builtin)) return api.openFile(d.path, d);
  if (by?.panel) return api.openPanel(by.panel.id, d);
  if (by?.viewer && d.artifact) return api.openArtifact(d.artifact, by.viewer.id, d);
  if (d.artifact) return api.openArtifact(d.artifact, undefined, d);
  if (d.view === "evidence" && d.locator) return api.openInvocation(d.locator);
  toast(`The agent asked to show ${d.view === "split" ? "a split view" : `a ${d.view}`}, which this window cannot show.`);
}

// ---- views in the main column ---------------------------------------------------------------

/// Show an app's view in place of the conversation. A view is built once; showing it again
/// passes new params to `update`, or calls `refresh`.
function showView(id, params) {
  const v = id === SETTINGS ? settingsView : registry.views.get(id);
  if (!v) return toast(`No view named ${id}`);
  let entry = views.get(id);
  if (!entry) {
    const el = h("div.view", { "data-view": id });
    $("#stage").append(el);
    entry = { el, ctl: null };
    views.set(id, entry);
    try {
      entry.ctl = v.render(el, params, window.ark) || {};
    } catch (e) {
      console.error(e);
      el.append(h("div.insp-empty", String(e)));
      entry.ctl = {};
    }
  } else if (params !== undefined) {
    entry.ctl.update?.(params);
  } else {
    entry.ctl.refresh?.();
  }
  for (const [k, x] of views) x.el.hidden = k !== id;
  state.view = id;
  appEl.dataset.view = id;
  $("#stage").hidden = false;
  showTitle();
  sidebar.markView(id);
  topbar.mark(id);
  reportFocus();
}

function showConversation() {
  if (!state.view) return;
  state.view = null;
  delete appEl.dataset.view;
  $("#stage").hidden = true;
  showTitle();
  sidebar.markView(null);
  topbar.mark(null);
  reportFocus();
  composer.focus();
}

/// The chat's title over the conversation; over a view of the project (whose tab says which),
/// the project's name; else the view's title (Settings).
function showTitle() {
  const v = state.view && (state.view === SETTINGS ? settingsView : registry.views.get(state.view));
  const el = $("#session-title");
  el.textContent = !v ? state.title || "" : v.nav ? state.summary?.project?.name || "" : v.title;
  // A chat that runs on another computer says where.
  const place = !v && state.summary?.place;
  if (place) el.append(h("span.place-mark", { title: `Runs on ${place.name}` }, icon("server"), place.name));
}

/// What the person looks at: the view's focus, else the side pane's.
function reportFocus() {
  const entry = state.view && views.get(state.view);
  if (entry) {
    let focus = null;
    try { focus = entry.ctl?.focus?.() || null; } catch {}
    if (focus) return api.setFocus(focus);
  }
  pane.reportFocus();
}

// ---- command palette (⌘K) ------------------------------------------------------------------

/// An action's `when` decides whether it is offered now (a throwing `when` hides it).
function shown(a) {
  try {
    return !a.when || !!a.when({});
  } catch {
    return false;
  }
}

function palette() {
  const overlay = $("#overlay");
  if (!overlay.hidden) return closePalette();
  const input = h("input.palette-input", { placeholder: "Search commands, views and actions", "aria-label": "Search" });
  const list = h("div.palette-list", { role: "listbox" });
  const items = [
    ...[...registry.views.values()].map((v) => ({ label: v.title, hint: "view", run: () => showView(v.id) })),
    ...(registry.settings.size ? [{ label: "Settings", hint: label("⌘,"), run: () => api.openSettings() }] : []),
    ...[...registry.panels.values()].map((p) => ({ label: p.title, hint: "panel", run: () => api.openPanel(p.id) })),
    ...registry.actions.filter((a) => (!a.where || a.where === "palette") && shown(a)).map((a) => ({ label: a.label, hint: "action", run: () => a.run({}, window.ark) })),
    ...(state.commands || []).map((c) => ({ label: `/${c.name}${c.usage ? " " + c.usage : ""}`, hint: c.summary, run: () => (c.usage && !c.usage.startsWith("[") ? composer.set(`/${c.name} `) : runCommand(`/${c.name}`)) })),
  ];
  let sel = 0;
  const paint = () => {
    const q = input.value.toLowerCase();
    const shown = items.filter((i) => !q || i.label.toLowerCase().includes(q) || (i.hint || "").toLowerCase().includes(q));
    sel = Math.min(sel, Math.max(0, shown.length - 1));
    clear(list);
    shown.slice(0, 40).forEach((it, i) => {
      const row = h("div.palette-item", { role: "option", "aria-selected": String(i === sel) }, h("span", it.label), h("span.palette-hint", it.hint || ""));
      row.addEventListener("mousedown", (e) => { e.preventDefault(); closePalette(); it.run(); });
      list.append(row);
    });
    return shown;
  };
  input.addEventListener("input", () => { sel = 0; paint(); });
  input.addEventListener("keydown", (e) => {
    const shown = paint();
    if (e.key === "ArrowDown") { sel = (sel + 1) % shown.length; paint(); e.preventDefault(); }
    if (e.key === "ArrowUp") { sel = (sel + shown.length - 1) % shown.length; paint(); e.preventDefault(); }
    // preventDefault: closing moves focus to the composer, which would get the line break.
    if (e.key === "Enter" && shown[sel]) { e.preventDefault(); closePalette(); shown[sel].run(); }
    if (e.key === "Escape") { e.stopPropagation(); closePalette(); }
  });
  clear(overlay).append(h("div.palette-box", input, list));
  overlay.hidden = false;
  overlay.onmousedown = (e) => e.target === overlay && closePalette();
  paint();
  input.focus();
}

function closePalette() {
  const overlay = $("#overlay");
  overlay.hidden = true;
  composer.focus();
}

// The desktop window is an app, not a browser: no reload or print keys, and no browser menu
// except on text, a link or a selection, where it copies and pastes.
function appLike() {
  addEventListener("keydown", (e) => {
    const k = e.key.toLowerCase();
    if (e.key === "F5" || ((e.metaKey || e.ctrlKey) && (k === "r" || k === "p"))) e.preventDefault();
  }, true);
  document.addEventListener("contextmenu", (e) => {
    const onText = e.target.closest("input, textarea, [contenteditable], a, .xterm");
    if (!onText && !String(window.getSelection() || "")) e.preventDefault();
  });
}

function keys(e) {
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.key.toLowerCase() === "k") { e.preventDefault(); return palette(); }
  if (mod && e.key.toLowerCase() === "n") { e.preventDefault(); return api.newSession(); }
  if (mod && e.key === "," && registry.settings.size) { e.preventDefault(); return api.openSettings(); }
  // The built-in panels' keys, unless an app replaced the panel.
  for (const b of BUILTINS) {
    if (b.key(e) && registry.panels.get(b.id)?.builtin) { e.preventDefault(); return api.openPanel(b.id); }
  }
  if (state.view) return;
  if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); return toggleSidebar(); }
  if (mod && e.key.toLowerCase() === "\\") { e.preventDefault(); return pane.toggle(); }
  if (e.key === "Escape") {
    if (!$("#overlay").hidden) return closePalette();
    const pending = [...state.approvals.values()].filter((m) => !m.request?.agent && (!m.session || m.session === state.sessionId)).map((m) => m.id).pop();
    if (pending != null) {
      e.preventDefault();
      document.querySelector(`.approval[data-id="${pending}"]`)?.decide?.("deny");
      return;
    }
    if (state.running) { e.preventDefault(); return api.cancel(); }
    if (pane.isOpen && !document.activeElement?.closest("#pane")) return pane.close();
  }
  // Typing anywhere goes to the composer.
  if (!mod && e.key.length === 1 && !e.target.closest("input, textarea, [contenteditable], .pane, .popover")) composer.focus();
}

function renameInline() {
  const el = $("#session-title");
  if (el.querySelector("input") || state.running) return;
  const before = el.textContent;
  const input = h("input.title-input", { value: state.summary?.session.title || "", "aria-label": "Session title" });
  clear(el).append(input);
  input.focus();
  input.select();
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    const title = input.value.trim();
    el.textContent = before;
    if (save && title) await change("rename_session", { title });
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") finish(true);
    if (e.key === "Escape") { e.stopPropagation(); finish(false); }
  });
  input.addEventListener("blur", () => finish(true));
}

function toggleSidebar() {
  if (appEl.dataset.compact === "true") {
    appEl.dataset.drawer = appEl.dataset.drawer === "open" ? "closed" : "open";
    return;
  }
  appEl.dataset.sidebar = appEl.dataset.sidebar === "open" ? "closed" : "open";
  try { localStorage.setItem("ark-sidebar", appEl.dataset.sidebar); } catch {}
}

/// Too narrow for the sidebar beside the conversation (or, with the pane open, beside both):
/// the sidebar folds into a drawer. The person's own open/closed choice is kept for wider sizes.
function fit() {
  const w = innerWidth;
  const compact = w <= 760 || (w <= 1000 && appEl.dataset.pane === "open");
  appEl.dataset.compact = String(compact);
  if (!compact) delete appEl.dataset.drawer;
}

// ---- boot -------------------------------------------------------------------------------------

async function boot(again = false) {
  feed.booting = true;
  feed.early = [];
  try {
    await bootOnce(again);
  } finally {
    feed.booting = false;
    const early = feed.early;
    feed.early = [];
    for (const m of early) receive(m);
  }
}

/// A device with no session, on a runtime with saved tokens: the code another, signed-in device
/// shows in Settings › Devices signs it in (a phone's Home Screen app cannot open a link).
function signInScreen() {
  appEl.dataset.signedOut = "true";
  fetch("/session", { cache: "no-store" }).then((r) => r.json()).then((s) => {
    if (s?.accent) document.documentElement.style.setProperty("--accent", s.accent);
  }, () => {});
  const input = h("input.pair-input", {
    type: "text", inputmode: "text", autocomplete: "one-time-code", autocapitalize: "characters",
    spellcheck: "false", placeholder: "XXXX-XXXX", "aria-label": "Code", maxlength: "12",
  });
  const why = h("p.pair-why", { role: "status" });
  const go = h("button.btn.primary", { type: "submit" }, "Sign in");
  const form = h("form.pair-form", { onsubmit: async (e) => {
    e.preventDefault();
    go.disabled = true;
    why.textContent = "";
    if (await transport.signIn(input.value, deviceName())) return location.reload();
    why.textContent = "That code did not work. Make a new one on the other device.";
    go.disabled = false;
    input.select();
  } }, input, go);
  clear($("#thread")).append(h("div.welcome.pair",
    h("h1", "Sign in"),
    h("p", "Type the code from Settings › Devices on a device that is signed in."),
    form, why));
  input.focus();
}

async function bootOnce(again) {
  let hello;
  try {
    // The person's time zone: what they schedule "at 9" runs at their 9, wherever ARK runs.
    let tz;
    try { tz = Intl.DateTimeFormat().resolvedOptions().timeZone; } catch {}
    hello = await transport.call("hello", tz ? { tz } : undefined);
  } catch (e) {
    if (e.signIn) return signInScreen();
    clear($("#thread")).append(h("div.welcome", h("h1", "Not connected"), h("p", e.message)));
    return;
  }
  feed.seq = hello.seq || 0;
  state.ui = hello.ui || {};
  state.runtime = hello.runtime;
  setFileKinds(hello.file_kinds, hello.kind_nouns);
  state.commands = hello.commands || [];
  composer.commands = state.commands;
  if (state.ui.accent) document.documentElement.style.setProperty("--accent", state.ui.accent);
  applySummary(hello.summary);
  places.load();
  composer.placeholder = `Ask ${hello.summary.name} anything`;
  if (!again) {
    sidebar.renderExtensions();
    topbar.render();
    // ARK's own Settings sections.
    registerAppearance(window.ark);
    registerCapabilities(window.ark);
    registerGrants(window.ark);
    registerPlugins(window.ark);
    registerSkills(window.ark);
    registerDevices(window.ark);
    registerComputers(window.ark);
    keepInStep(window.ark);
    // A notification tapped: the item it is about (ADR 0047).
    openItems((id) => {
      const chat = id.startsWith("chat:") ? id.split(":")[1] : null;
      if (chat) return api.openSession(chat);
      waiting.openById(id);
    });
    // The project's board, when the app runs agents.
    registerBoard(window.ark);
    // What this chat runs on its own, above the composer.
    registerSchedules(window.ark);
  }
  await loadSession();
  state.working = hello.working || [];
  sidebar.setWorking(state.working);
  showComputer(hello.computer);
  // Questions are placed again below: the current chat's in the thread, others' in the tray.
  for (const el of document.querySelectorAll("#asks .approval[data-chat]")) el.remove();
  state.approvals.clear();
  if (hello.running) {
    thread.begin(hello.running);
    setRunning(true);
    // Rebuild the running turn from its events so far. Open questions come from
    // `approvals` below, so the replay skips them.
    for (const m of hello.live || []) {
      if (m.kind === "text" || m.kind === "event") onEvent(m);
    }
  } else setRunning(false);
  for (const a of hello.approvals || []) onEvent({ kind: "approval", ...a });
  waiting.refresh();
}

async function start() {
  hydrateIcons();
  for (const [id, name] of [["toggle-sidebar", "sidebar"], ["reveal-sidebar", "sidebar"], ["toggle-pane", "pane"], ["close-pane", "x"]]) $(`#${id}`).append(icon(name));
  appEl.dataset.tabs = "0";
  try {
    const s = localStorage.getItem("ark-sidebar");
    if (s) appEl.dataset.sidebar = s;
  } catch {}
  appEl.dataset.shell = transport.shell;
  appEl.dataset.os = os;
  relabel(document.body);
  if (transport.shell === "desktop") appLike();
  thread = new Thread($("#thread"), $("#scroller"), api);
  composer = new Composer(api);
  places = new Places(api);
  acceptDrops(api, composer, $(".main"));
  sidebar = new Sidebar(api);
  waiting = new Waiting(api, transport, () => state.sessionId);
  topbar = new Topbar(api);
  pane = new Pane(appEl, api);
  fit();
  addEventListener("resize", fit);
  new MutationObserver(fit).observe(appEl, { attributes: true, attributeFilter: ["data-pane"] });
  // The drawer closes once something in it is picked, or on a click outside it.
  document.addEventListener("click", (e) => {
    if (appEl.dataset.drawer !== "open") return;
    const inside = e.target.closest(".sidebar");
    if (e.target.closest("#reveal-sidebar, #toggle-sidebar")) return;
    if (e.target.closest(".project-chev, .project-more, #add-project, .chats-more")) return;
    if (!inside || e.target.closest("button, a, .session")) appEl.dataset.drawer = "closed";
  });
  $("#toggle-sidebar").addEventListener("click", toggleSidebar);
  $("#reveal-sidebar").addEventListener("click", toggleSidebar);
  $("#toggle-pane").addEventListener("click", () => pane.toggle());
  $("#ctx-ring").addEventListener("click", () => runCommand("/context"));
  $("#session-title").addEventListener("dblclick", renameInline);
  document.addEventListener("keydown", keys);
  transport.subscribe(receive);
  await boot();
  composer.focus();
  requestAnimationFrame(() => appEl.classList.add("ready"));
}

// App scripts are modules too and run after this one; start once they have registered.
if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
else start();
