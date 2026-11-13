// Boot and glue. Connects to the host, installs `window.ark` for app scripts, then wires the
// conversation, composer, sidebar and side pane to the host's event stream.

import { contextMenu, controlActions, bindContextMenu } from "./context-menu.js";
import { connect } from "./transport.js";
import { h, icon, clear, $, hydrateIcons, tokens, ago } from "./dom.js";
import { markdown } from "./md.js";
import { Thread, approvalCard } from "./conversation.js";
import { Composer } from "./composer.js";
import { createChatSurface } from "./chat-surface.js";
import { chatView } from "./chat-view.js";
import { acceptDrops, uploadFolder } from "./drop.js";
import { Sidebar, ARK, agentKey, hiddenProjects, sidebarGrip } from "./sidebar.js";
import { openRecent } from "./recent.js";
import { Topbar } from "./topbar.js";
import { SETTINGS, settingsView, registerAppearance } from "./settings.js";
import { Pane, invocationView, artifactView } from "./pane.js";
import { registry, installApi, dispatchExt, presenterFor, onExt, waitForExtension, loadedExtensions, viewForCommand } from "./ext.js";
import { registerBuiltins, BUILTINS } from "./workbench.js";
import { fileView, fileRecovery, kindOf, KIND_ICON, setFileKinds } from "./file.js";
import { registerGrants } from "./grants.js";
import { registerPlugins } from "./plugins.js";
import { registerMarketplace } from "./marketplace.js";
import { registerSkills } from "./skills.js";
import { registerBoard, boardWaiting } from "./board.js";
import { registerCalendar } from "./calendar.js";
import { Waiting } from "./waiting.js";
import { registerDevices, keepInStep, openItems, deviceName } from "./devices.js";
import { os, label, relabel } from "./keys.js";
import { Places } from "./places.js";
import { Identity, DETAILS } from "./identity.js";
import { registerComputers } from "./computers.js";
import { reviewComputerMessage } from "./computer-recovery.js";
import { openLinksOutside, setOpener } from "./links.js";
import { MAX_VISIBLE_CHATS, chatLeaf, visibleChats, splitChat as splitLayoutChat, removeChat as removeLayoutChat, replaceChat, updateRatio, restoreLayout } from "./chat-layout.js";

const transport = connect();
if (transport.openLink) {
  setOpener((url) => transport.openLink(url).catch(() => window.open(url, "_blank", "noopener")));
}
openLinksOutside(document);
const pageParams = new URL(location.href).searchParams;
const detachedSession = pageParams.get("detached") || "";
const windowIdentity = pageParams.get("window") || "main";
const detachedWindow = !!detachedSession;
const tabStorageKey = windowIdentity === "main" ? "ark-open-chats" : `ark-open-chats:${windowIdentity}`;
const layoutStorageKey = windowIdentity === "main" ? "ark-chat-layout" : `ark-chat-layout:${windowIdentity}`;
const TAB_STORAGE_VERSION = 1;
const LAYOUT_STORAGE_VERSION = 1;
document.documentElement.dataset.runtime = transport.local ? "local" : "remote";
const appEl = $("#app");
const state = { summary: null, ui: {}, runtime: null, sessionId: null, running: false, approvals: new Map(), working: [], online: true, connectionEpoch: 0, extChanged: false, restyle: false, view: null, homeView: null, agent: null };
const secondaryChats = new Map();
let activeSession = null;
const resourceOnline = new Map();
const focusWrites = new Map();
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
  fileSnapshots: () => !!state.fileSnapshots,
  on: onExt,
  call: (method, params = {}) => resourceMethod(method, params)
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
  chatView: (el, options) => chatView(el, window.ark || api, options),
  // The questions waiting for an answer, as `ark:approval` brought them (also those asked
  // before the app's code ran).
  approvals: () => [...state.approvals.values()],
  // The system's folder picker in the window: a path, or null when dismissed. Undefined
  // where there is none (a browser), so the caller asks for a typed path instead.
  pickFolder: ({ title } = {}) => transport.pickFolder(title),
  askFolder: (anchor, title) => askFolder(anchor, title),
  // The folders the person removed from the sidebar: their agents are not offered anywhere.
  hiddenFolders: () => [...hiddenProjects().keys()],
  // Whether there is a folder picker (the window's own runtime).
  canPickFolder: transport.kind === "desktop",
  // The window's own view for the Browser panel (`show(rect, url)`, `act(action)`,
  // `listen(fn)`); undefined in a browser, where the panel is a frame.
  browserView: transport.browserView,
  // Signed native-app updates. Undefined in a browser; `status()` is null in development and
  // when the packaged consumer did not opt in.
  updates: transport.updates,
  pageUpdates: transport.pageUpdates,
  // True when the runtime runs on the computer showing this page (the window's own runtime,
  // or a browser on this machine). False for a runtime elsewhere: its files, programs and
  // sign-ins are on that machine, so actions that need this one should not be offered.
  isLocal: () => transport.local,
  // In the window, on a runtime elsewhere it opened: back to this computer's runtime.
  // Undefined anywhere else (the window's own page, a browser).
  openLocal: transport.openLocal,
  summary: () => activeSummary(),
  // App modules load beside this module, while the first `hello` is still in flight. Wait for
  // its resource location before making a project-scoped call. A failed hello can recover; this
  // promise therefore stays pending instead of caching the startup failure.
  ready: async () => {
    if (!isReady) await firstReady;
    return state.summary;
  },
  // This computer as a sentence names it ("this Mac"), once the places are read.
  hereName: () => places?.here,
  pendingPlace: () => places?.pending || null,
  dictationRoute: () => ({ place: places?.pending || state.summary?.place?.name || "", session: state.sessionId || "", runtime: places?.pending ? undefined : state.summary?.location?.runtime }),
  pendingModel: () => composer?.pendingModel || "",
  placeChanged: (place) => composer?.placeChanged(place),
  // Resolves once the page shows chat `id`, rebuilt (a chat the host just opened).
  shown: async (id) => {
    for (let i = 0; i < 200 && (state.sessionId !== id || feed.booting); i++) await new Promise((r) => setTimeout(r, 25));
  },
  project: () => activeSummary()?.project,
  send: (text) => submit(text),
  // Put text in the composer, unsent, with the conversation in view.
  compose: async (text, { append = false, images = [] } = {}) => {
    const draft = append ? composer.input.value : "";
    if (!await showConversation()) return;
    composer.set(draft ? `${draft}\n\n${text}` : text);
    if (images.length) composer.addImages(images);
  },
  submit: (text, options) => submit(text, options),
  steer,
  cancel: () => transport.call("cancel").catch(() => {}),
  command: (line) => (line.endsWith(" ") ? composer.set(line) : runCommand(line)),
  newChatView: el => renderNewChatView(el, currentLocation()),
  /// New chat: in `project`, a new ARK chat there; without one, a new chat with the agent the
  /// window shows (ARK at start).
  newSession: (project) => project ? arkNewChat(project) : newChatWithCurrent(),
  // A code to sign in another device (Settings › Devices, ADR 0047).
  pairCode: () => transport.pairCode(),
  openSession: (id) => {
    leaveStart();
    return openChatTab(id);
  },
  /// An agent's chat in the app's `agentConversation` view. `options` is `{ project,
  /// conversation }` (another folder: the window goes there first), or, as before, a session
  /// that opens as a plain chat when no app draws agents' chats.
  async openAgentChat(name, options) {
    const { project, conversation, session, fallback } = options && typeof options === "object" ? options : { fallback: options };
    const view = [...registry.views.values()].find(view => view.agentConversation);
    if (!view) return fallback ? this.openSession(fallback) : false;
    const params = { name, ...(project ? { project } : {}), ...(conversation ? { conversation } : {}) };
    if (session) {
      // An agent on another computer: its chat there puts the window at its project.
      try { await openChatTab(session); } catch (error) { toast(error.message); return false; }
      await showView(view.id, params);
      return true;
    }
    if (project) {
      try { await arriveAt(project); } catch (error) { toast(error.message); return false; }
      await showView(view.id, params);
      return true;
    }
    const current = currentLocation();
    if (this.location && (this.location.runtime !== current.runtime || this.location.project !== current.project)) {
      toast("Open the original project to continue.");
      return false;
    }
    return this.openView(view.id, params);
  },
  /// `/history [words]`: this agent's chats in the search overlay.
  history: (words = "") => openHistory(currentEntry(), words),
  openView: (id, params) => showView(id, params),
  contextMenu,
  present: directive => present(directive),
  showConversation: () => showConversation(),
  currentView: () => state.view,
  switchProject: (name) => detachedWindow ? switchWindowProject(name) : change("switch_project", { name }),
  openProject: (id) => detachedWindow ? openWindowProject(id) : change("open_project", { id }),
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
    openFileTab(path, params, location, ark);
  },
  openPanel(id, params) {
    const p = registry.panels.get(id);
    if (!p) return toast(`No panel named ${id}`);
    const current = currentLocation();
    const project = params?.project || state.summary?.project;
    if (!panelShown(p, project)) return toast(`${p.title} is not available for this project`);
    const place = params && Object.hasOwn(params, "place") ? params.place : state.summary?.place?.name || "";
    const location = p.projectScoped ? { ...current, session: "" } : current;
    if (p.projectScoped) params = { ...(params || {}), project, place };
    const ark = scopedApi(location);
    const where = place || api.hereName() || "This computer";
    const title = p.projectScoped ? `${p.title} · ${project?.name || "Project"} · ${where}` : p.title;
    pane.openTab(resourceKey("panel", location, id), title, p.icon || "box", (el, prm) => p.render(el, prm, ark) || {}, params, {
      wide: !!p.wide, fill: !!p.fill, expandable: !!p.expandable, api: ark, projectScoped: !!p.projectScoped, closesWithProject: !!p.closesWithProject, project: project?.id,
    });
  },
  panelOpen: (id) => !!pane?.isOpen && pane.active === resourceKey("panel", currentLocation(), id),
  setPanelTitle: (id, title) => {
    const p = registry.panels.get(id);
    const location = p?.projectScoped ? { ...currentLocation(), session: "" } : currentLocation();
    retitle(resourceKey("panel", location, id), title);
  },
  closePanel: (id) => {
    const p = registry.panels.get(id);
    const location = p?.projectScoped ? { ...currentLocation(), session: "" } : currentLocation();
    return pane.remove(resourceKey("panel", location, id));
  },
  toast,
  popover,
  contextMenu,
  refreshSidebar: () => sidebar.refresh(state.sessionId),
  refreshNavigation: () => { topbar?.render(); sidebar?.paintProfile(); },
  refreshModel: () => { composer.setModel(state.summary?.model); showTitle(); },
  setExecutionMode: async (mode) => {
    try {
      const summary = await transport.call("execution_mode", {
        mode,
        revision: state.summary?.execution_mode_revision || 0,
      });
      applySummary(summary);
      dispatchExt("ark:summary", { kind: "summary", summary });
      return summary;
    } catch (error) {
      toast(error.message || String(error));
      const summary = await transport.call("summary");
      applySummary(summary);
      throw error;
    }
  },
  // Ask the window to reflect a view's focus again (after its selection changed).
  reportFocus: () => reportFocus(),
  refreshViewTitle: () => showTitle(),
  _changed: (restyle = false) => {
    state.extChanged = true;
    state.restyle ||= restyle;
    queueMicrotask(() => {
      if (!state.extChanged) return;
      state.extChanged = false;
      sidebar?.renderExtensions();
      if (state.restyle) {
        state.restyle = false;
        sidebar?.restyle();
      }
      topbar?.render();
      composer?.setModel(state.summary?.model);
    });
  },
};

const RESOURCE_METHODS = {
  project_models: ["project", "models"],
  automation_validate: ["project", "automation_validate"],
  automation_preview_inputs: ["project", "automation_preview_inputs"],
  automation_begin_start: ["project", "automation_begin_start"],
  automation_acknowledge_start: ["project", "automation_acknowledge_start"],
  automation_cancel_start: ["project", "automation_cancel_start"],
  term_open: ["terminal", "terminal_open"], term_read: ["terminal", "terminal_read"],
  term_write: ["terminal", "terminal_write"], term_resize: ["terminal", "terminal_resize"], term_close: ["terminal", "terminal_close"],
  procs: ["process", "processes"], proc_output: ["process", "process_output"], proc_stop: ["process", "process_stop"],
  files: ["file", "files"], file_read: ["file", "file_read"], file_write: ["file", "file_write"], file_edit: ["file", "file_edit"],
  file_media: ["file", "file_media"], file_version: ["file", "file_version"], file_restore: ["file", "file_restore"],
  file_snapshot_open: ["file", "file_snapshot_open"], file_snapshot_status: ["file", "file_snapshot_status"],
  file_snapshot_cancel: ["file", "file_snapshot_cancel"], file_snapshot_chunk: ["file", "file_snapshot_chunk"],
  file_snapshot_close: ["file", "file_snapshot_close"], file_probe: ["file", "file_probe"],
  upload_parts: ["file", "upload_parts"], upload_add: ["file", "upload_add"], upload_cancel: ["file", "upload_cancel"], file_open: ["file", "file_open"],
  workspace: ["project", "workspace"], state: ["project", "state"], call: ["project", "call_detail"],
  graph: ["project", "graph"], task_trace: ["project", "task_trace"], task: ["project", "task"], task_call: ["project", "task_call"], orch_events: ["project", "orch_events"],
  agents: ["project", "agents"], create_agent: ["project", "create_agent"], spawn_agent: ["project", "spawn_agent"], submit_task: ["project", "submit_task"],
  agent_conversation_start: ["project", "agent_conversation_start"], agent_conversations: ["project", "agent_conversations"],
  work_admit: ["project", "work_admit"], work_receipt: ["project", "work_receipt"], work_execute: ["project", "work_execute"],
  work_answer: ["project", "work_answer"], work_cancel: ["project", "work_cancel"],
  work_get: ["project", "work_get"],
  work_list: ["project", "work_list"], work_archive: ["project", "work_archive"], work_resolve: ["project", "work_resolve"],
  cancel_task: ["project", "cancel_task"], retry_task: ["project", "retry_task"],
  automations: ["project", "automations"],
  automation_get: ["project", "automation_get"],
  automation_history: ["project", "automation_history"],
  automation_preview: ["project", "automation_preview"],
  automation_start: ["project", "automation_start"],
  webhook_setup: ["project", "webhook_setup"],
  webhook_get: ["project", "webhook_get"],
  webhook_probe: ["project", "webhook_probe"],
  automation_revoke: ["project", "automation_revoke"],

  automation_save: ["project", "automation_save"],
  automation_layout: ["project", "automation_layout"],
  automation_prepare: ["project", "automation_prepare"],
  automation_activate: ["project", "automation_activate"],
  automation_pause: ["project", "automation_pause"],
  automation_run: ["project", "automation_run"],
  automation_execution: ["project", "automation_execution"],
  schedules: ["project", "schedules"], schedule: ["project", "schedule"], unschedule: ["project", "unschedule"],
  schedule_pause: ["project", "schedule_pause"], schedule_resume: ["project", "schedule_resume"], schedule_run: ["project", "schedule_run"],
  board: ["project", "board"], board_add: ["project", "board_add"], board_change: ["project", "board_change"],
  board_remove: ["project", "board_remove"], board_give: ["project", "board_give"],
  board_rename: ["project", "board_rename"], board_delete: ["project", "board_delete"],
  suspend_agent: ["project", "suspend_agent"], resume_agent: ["project", "resume_agent"], terminate_agent: ["project", "terminate_agent"], message_agent: ["project", "message_agent"],
  profiles: ["project", "profiles"], grants: ["project", "grants"], grant_remove: ["project", "grant_remove"],
  skill_packages: ["project", "skill_packages"], skill_package_install: ["project", "skill_package_install"], skill_package_remove: ["project", "skill_package_remove"],
  skills: ["project", "skills"], tools: ["project", "tools"], usage: ["project", "usage"],
  artifacts: ["artifact", "artifacts"], artifact: ["artifact", "artifact"], invocation: ["invocation", "invocation"],
  picture: ["picture", "picture"], computer_picture: ["picture", "computer_picture"],
};

/// Which calls are resource operations of the chat's location. `agents {all}` reads every
/// folder's agents from the runtime, like `projects`.
function resourceMethod(method, params) {
  return method === "agents" && params?.all ? null : RESOURCE_METHODS[method];
}

/// Pictures waiting in a chat's composer while another chat is shown, by chat.
const imageStash = new Map();

function activeSummary() {
  return secondaryChats.get(activeSession)?.summary || state.summary;
}

function currentLocation() {
  const location = activeSummary()?.location;
  if (!location?.runtime || !location?.project || !location?.session) throw new Error("This chat has no resource location");
  return location;
}

function resourceKey(prefix, location, id) {
  return `${prefix}:${location.runtime}:${location.project}:${location.session}:${id}`;
}

function openFileTab(path, params, location, ark) {
  pane.openTab(resourceKey('file', location, path), path.split('/').pop(), KIND_ICON[kindOf(path)],
    (el, p) => fileView(el, path, ark, p), params, { wide: true, fill: true, api: ark,
      onShow: () => fileRecovery(location, path, 'remember'),
      onClose: () => fileRecovery(location, path, 'clear'),
    });
}

function renderNewChatView(el, location) {
  return registry.newChat?.render?.(el, { location: { ...location }, api: scopedApi(location) }) || null;
}

function scopedApi(location) {
  // Consumer capabilities may be installed or replaced after boot (the Desktop Browser view is
  // one). A resource tab snapshots its location, not the API object's early boot values.
  const scoped = Object.create(window.ark || api);
  scoped.location = { ...location };
  const resourceLocation = location.session ? { ...location } : { ...activeSummary()?.location };
  const authorizedLocation = () => {
    if (resourceLocation.runtime !== location.runtime || resourceLocation.project !== location.project || !resourceLocation.session) {
      throw new Error("Return to the original project to use this panel");
    }
    return resourceLocation;
  };
  scoped.chatView = (el, options) => chatView(el, scoped, options);
  scoped.newChatView = el => renderNewChatView(el, location);
  scoped.openView = (id, params) => {
    const current = currentLocation();
    if (registry.views.get(id)?.resourceScoped && (current.runtime !== location.runtime || current.project !== location.project)) {
      toast("Open the original project to continue.");
      return false;
    }
    showView(id, params);
    return true;
  };
  scoped.remoteResource = location.runtime !== state.runtime;
  scoped.destinationBrowser = scoped.remoteResource || transport.kind === "browser";
  scoped.online = () => resourceOnline.get(locationKey(location)) !== false;
  if (scoped.destinationBrowser) scoped.browserView = undefined;
  scoped.call = (method, params = {}) => {
    const spec = resourceMethod(method, params);
    if (!spec) return (window.ark || api).call(method, params);
    const [kind, operation] = spec;
    const id = kind === "terminal" ? (params.session || "")
      : kind === "process" ? (params.id || "")
      : kind === "artifact" || kind === "invocation" ? (params.id || "")
      : kind === "picture" ? (params.invocation || params.app || "")
      : kind === "file" && method !== "files" && !method.startsWith("upload_") && method !== "file_restore" ? (params.path || "") : "";
    return api.resource(api.resourceRef(kind, id, authorizedLocation()), operation, params);
  };
  if (api.on) scoped.on = (name, fn) => api.on(name, (message) => {
    if (["ark:term", "ark:proc", "ark:orch"].includes(name) && !message?.ref) return;
    if (message?.ref && JSON.stringify(message.ref.location) !== JSON.stringify(resourceLocation)) return;
    if (message?.location && JSON.stringify(message.location) !== JSON.stringify(resourceLocation)) return;
    fn(message);
  });
  scoped.present = directive => present(directive, scoped);
  scoped.ext = (name, params = {}) => api.resource(api.resourceRef("extension", name, authorizedLocation()), "extension", { name, params });
  scoped.runAction = (action, input = {}, approved = false) => api.resource(api.resourceRef("project", "", authorizedLocation()), "run_action", { action, input, approved });
  // A project panel has no chat of its own: its focus goes to the chat the window shows, when
  // that chat is in the panel's project.
  scoped.setFocus = (focus) => {
    const chat = activeSummary()?.location;
    const at = location.session ? location : chat?.project === location.project && chat?.runtime === location.runtime ? chat : null;
    if (!at?.session) return Promise.resolve();
    const target = { ...at };
    const snapshot = structuredClone(focus);
    const key = JSON.stringify([target.runtime, target.project, target.session]);
    const prior = focusWrites.get(key) || Promise.resolve();
    const next = prior.then(() => api.resource(api.resourceRef("project", "", target), "focus", { focus: snapshot }))
      .catch(error => { console.error("Could not update focus:", error); });
    focusWrites.set(key, next);
    void next.then(() => { if (focusWrites.get(key) === next) focusWrites.delete(key); });
    return next;
  };
  scoped.openFile = (path, params) => {
    path = String(path).replace(/^\.\//, "");
    openFileTab(path, params, location, scoped);
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
    if (panel.projectScoped) {
      const current = currentLocation();
      if (current.runtime !== location.runtime || current.project !== location.project) return toast("Return to the original project to open this panel");
      return api.openPanel(id, params);
    }
    pane.openTab(resourceKey("panel", location, id), panel.title, panel.icon || "box", (el, prm) => panel.render(el, prm, scoped) || {}, params, {
      wide: !!panel.wide, fill: !!panel.fill, expandable: !!panel.expandable, api: scoped, closesWithProject: !!panel.closesWithProject, project: location.project,
    });
  };
  const panelLocation = id => registry.panels.get(id)?.projectScoped ? { ...location, session: "" } : location;
  scoped.setPanelTitle = (id, title) => retitle(resourceKey("panel", panelLocation(id), id), title);
  scoped.closePanel = (id) => pane.remove(resourceKey("panel", panelLocation(id), id));
  return scoped;
}

// Terminal, Browser and Files first, so an app's panel with the same id replaces one.
registerBuiltins(installApi(api));

let thread, composer, sidebar, topbar, identity, pane, waiting, places;
let primarySurface;
let chatLayout = null;
let pendingPrimary = null;
let draggedSession = null;
const chatTabs = { ids: [], titles: new Map() };

function draftKey(session, runtime = state.runtime) {
  return `ark-draft:${runtime || "runtime"}:${session}`;
}

function scrollKey(session) {
  return `ark-scroll:${state.runtime || "runtime"}:${session}`;
}

function saveDraft(session = state.sessionId, value = composer?.input?.value, runtime = state.runtime) {
  if (!session || value == null) return;
  try { localStorage.setItem(draftKey(session, runtime), value); } catch (error) { console.error(error); }
}

function savedDraft(session, runtime = state.runtime) {
  try { return localStorage.getItem(draftKey(session, runtime)) || ""; } catch { return ""; }
}

function captureDraft(location) {
  const runtime = state.runtime;
  return (text, images = []) => {
    const key = locationKey(location);
    const target = locationKey(state.summary?.location) === key ? composer
      : [...secondaryChats.values()].find(chat => locationKey(chat.summary.location) === key)?.composer;
    if (images.length && target) target.addImages(images);
    else if (images.length) {
      // No window shows this chat: the files are named in its draft instead.
      text = [text, `Added to the project: ${images.map(i => i.path).join(", ")}`].filter(Boolean).join("\n");
      for (const image of images) URL.revokeObjectURL(image.url);
    }
    if (!text) return;
    if (target) {
      target.addLine(text, { focus: false });
      saveDraft(location.session, target.input.value, runtime);
    } else {
      const draft = savedDraft(location.session, runtime).replace(/\s+$/, "");
      saveDraft(location.session, `${draft}\n${text}`, runtime);
    }
  };
}

function saveScroll(session, element) {
  if (!session || !element) return;
  try { localStorage.setItem(scrollKey(session), String(element.scrollTop)); } catch {}
}

function restoreScroll(session, element) {
  if (!session || !element) return;
  let top = 0;
  try { top = Number(localStorage.getItem(scrollKey(session)) || 0); } catch {}
  requestAnimationFrame(() => { element.scrollTop = Number.isFinite(top) ? top : 0; });
}

function saveTabs() {
  // Do not let the early spatial-tabs experiment reopen a stray chat on launch. Tabs written
  // from this release carry an explicit format and restore normally.
  try { localStorage.setItem(tabStorageKey, JSON.stringify({ version: TAB_STORAGE_VERSION, ids: chatTabs.ids.slice(-12) })); } catch {}
}

function saveLayout() {
  // The pre-release tree format could reopen an incomplete split after a chat disappeared.
  // Start this version in one full-height chat. Layouts made from here restore normally.
  try { localStorage.setItem(layoutStorageKey, JSON.stringify({ version: LAYOUT_STORAGE_VERSION, tree: chatLayout })); } catch {}
}

function rememberTab(summary = state.summary) {
  const session = summary?.session;
  if (!session?.id) return;
  if (!chatTabs.ids.includes(session.id)) chatTabs.ids.push(session.id);
  chatTabs.titles.set(session.id, session.title || "Untitled chat");
  saveTabs();
  renderChatTabs();
}

async function openChatTab(id) {
  if (!id) return focusChat(state.sessionId);
  if (visibleChats(chatLayout).includes(id)) {
    if (activeSession === id) {
      if (!await showConversation(false)) return;
      return (id === state.sessionId ? composer : secondaryChats.get(id)?.composer)?.focus();
    }
    return focusChat(id);
  }
  if (detachedWindow) {
    const before = activeSession;
    const chat = await loadSecondary(id);
    if (!chat) return;
    if (!chatLayout || visibleChats(chatLayout).length <= 1) {
      chatLayout = chatLeaf(id);
      if (before && before !== id) removeSecondary(before);
    } else if (before) {
      chatLayout = replaceChat(chatLayout, before, id);
      if (before !== id) removeSecondary(before);
    }
    saveLayout();
    renderWorkspace();
    focusChat(id);
    return;
  }
  saveDraft();
  if (!await showConversation(false)) return;
  if (!chatTabs.ids.includes(id)) chatTabs.ids.push(id);
  saveTabs();
  if (!chatLayout || activeSession === state.sessionId || visibleChats(chatLayout).length === 1) {
    pendingPrimary = { before: state.sessionId, after: id };
    if (!(await change("open_session", { id }))) pendingPrimary = null;
    return;
  }
  const before = activeSession;
  const chat = await loadSecondary(id);
  if (!chat) return;
  removeSecondary(before);
  chatLayout = replaceChat(chatLayout, before, id);
  saveLayout();
  renderWorkspace();
  focusChat(id);
}

async function closeChatTab(id) {
  const at = chatTabs.ids.indexOf(id);
  chatTabs.ids = chatTabs.ids.filter((entry) => entry !== id);
  chatTabs.titles.delete(id);
  saveTabs();
  if (secondaryChats.has(id)) {
    chatLayout = removeLayoutChat(chatLayout, id);
    removeSecondary(id);
    saveLayout();
    renderWorkspace();
  }
  if (detachedWindow) {
    const next = visibleChats(chatLayout)[0] || chatTabs.ids[Math.min(at, chatTabs.ids.length - 1)];
    if (next) await openChatTab(next);
    else await transport.closeWindow?.();
    renderChatTabs();
    return;
  }
  if (id === state.sessionId) {
    chatLayout = removeLayoutChat(chatLayout, id);
    const next = visibleChats(chatLayout)[0] || chatTabs.ids[Math.min(at, chatTabs.ids.length - 1)];
    if (next) {
      if (!chatLayout) chatLayout = chatLeaf(next);
      pendingPrimary = { promote: next };
      await change("open_session", { id: next });
    }
    else await createGeneralSession();
  }
  if (!activeSession || activeSession === id) focusChat(state.sessionId, false);
  renderChatTabs();
}

/// The strip of open chats. A tab keeps its element while it stays open, so the selection
/// moves with a transition, the focus and hover survive and nothing flickers. A conversation
/// view that is not a chat yet (a new chat) takes a tab of its own at the end.
const chatTabItems = new Map();
const DRAFT_TAB = "view:draft";

function chatTabItem(key) {
  const close = h("button.chat-tab-close", { type: "button" }, icon("x"));
  const titleEl = h("span.chat-tab-title");
  const tab = h("button.chat-tab", { type: "button", role: "tab" }, titleEl);
  const item = h("div.chat-tab-item", { "data-key": key, draggable: key === DRAFT_TAB ? "false" : "true" }, tab, close);
  const entry = { key, item, tab, close, titleEl, title: "" };
  const draft = key === DRAFT_TAB;
  const select = (event) => {
    if (draft) return;
    // The highlight moves now; the chat follows when the host has switched.
    for (const other of chatTabItems.values()) other.item.dataset.selected = String(other === entry);
    (event.altKey ? splitInto(key, activeSession || state.sessionId, "right") : openChatTab(key)).finally(renderChatTabs);
  };
  close.addEventListener("click", (event) => {
    event.stopPropagation();
    if (draft) void showConversation();
    else void closeChatTab(key);
  });
  tab.addEventListener("click", select);
  tab.addEventListener("keydown", (event) => {
    if (event.key === "Tab" && !event.shiftKey) {
      event.preventDefault();
      close.focus();
      return;
    }
    if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(event); }
  });
  if (!draft) {
    bindContextMenu(item, () => {
      const active = activeSession || state.sessionId;
      return [
        { label: "Open chat", run: () => void openChatTab(key) },
        { label: "Rename chat", disabled: key !== active || (key === state.sessionId ? state.running : !!secondaryChats.get(key)?.running), run: () => renameChatTabInline(entry) },
        { label: "Split right", disabled: key === active || visibleChats(chatLayout).length >= MAX_VISIBLE_CHATS, run: () => void splitInto(key, active, "right") },
        { label: "Close chat", run: () => void closeChatTab(key) },
      ];
    });
    item.addEventListener("dragstart", (event) => beginTabDrag(event, key, item));
    item.addEventListener("dragend", (event) => void endTabDrag(event, key));
    item.addEventListener("dragover", (event) => {
      if (!draggedSession || draggedSession === key) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "move";
      item.dataset.drop = "true";
    });
    item.addEventListener("dragleave", () => delete item.dataset.drop);
    item.addEventListener("drop", (event) => {
      event.preventDefault();
      const from = draggedSession;
      delete item.dataset.drop;
      if (!from || from === key) return;
      const ordered = chatTabs.ids.filter((entryId) => entryId !== from);
      ordered.splice(ordered.indexOf(key), 0, from);
      chatTabs.ids = ordered;
      saveTabs();
      renderChatTabs();
    });
    tab.addEventListener("dblclick", (event) => {
      event.preventDefault();
      event.stopPropagation();
      renameChatTabInline(entry);
    });
  }
  return entry;
}

function renderChatTabs() {
  const bar = $("#chat-tabs");
  const list = $("#chat-tab-list");
  if (!bar || !list) return;
  const conversationView = !!state.view && !!registry.views.get(state.view)?.agentConversation;
  // An agent's chat names itself in the title, whose menu holds its other chats; ARK's open
  // chats stay as tabs over its new chat.
  const agentChat = conversationView && !!views.get(state.view)?.ctl?.identity?.()?.agent;
  const draftTitle = conversationView ? views.get(state.view)?.ctl?.title?.() || "New chat" : null;
  const keys = agentChat ? [] : [...chatTabs.ids, ...(conversationView ? [DRAFT_TAB] : [])];
  bar.hidden = agentChat || keys.length < 2 && visibleChats(chatLayout).length < 2;
  if (conversationView) appEl.dataset.conversationView = "true";
  else delete appEl.dataset.conversationView;
  const active = state.view ? null : activeSession || state.sessionId;
  let selected = null;
  keys.forEach((key, index) => {
    let entry = chatTabItems.get(key);
    if (!entry) {
      entry = chatTabItem(key);
      chatTabItems.set(key, entry);
    }
    const draft = key === DRAFT_TAB;
    const title = draft ? draftTitle : chatTabs.titles.get(key) || "Untitled chat";
    const isSelected = draft || key === active;
    if (entry.title !== title) {
      entry.title = title;
      entry.titleEl.textContent = title;
      entry.close.title = `Close ${title}`;
      entry.close.setAttribute("aria-label", entry.close.title);
    }
    if (draft && !entry.face) {
      const face = views.get(state.view)?.ctl?.identity?.()?.avatar?.(16);
      if (face) {
        entry.face = h("span.chat-tab-face", { "aria-hidden": "true" }, face);
        entry.tab.prepend(entry.face);
      }
    }
    entry.item.dataset.session = draft ? "" : key;
    entry.item.dataset.selected = String(isSelected);
    entry.tab.setAttribute("aria-selected", String(isSelected));
    entry.tab.title = isSelected && !draft ? `${title}\nDouble-click to rename` : title;
    if (list.children[index] !== entry.item) list.insertBefore(entry.item, list.children[index] || null);
    if (isSelected) selected = entry.item;
  });
  for (const [key, entry] of chatTabItems) {
    if (keys.includes(key)) continue;
    entry.item.remove();
    chatTabItems.delete(key);
  }
  if (selected && selected !== lastSelectedTab) requestAnimationFrame(() => selected.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "smooth" }));
  lastSelectedTab = selected;
  const split = $("#chat-split");
  if (split) {
    const divided = visibleChats(chatLayout).length > 1;
    split.disabled = !divided && !chatTabs.ids.some((id) => !visibleChats(chatLayout).includes(id));
    split.setAttribute("aria-pressed", String(divided));
    split.title = divided ? "Close other panes" : "Split with another open chat";
    split.setAttribute("aria-label", split.title);
  }
}
let lastSelectedTab = null;

function renameChatTabInline(entry) {
  const { item, key: id, title: before } = entry;
  const active = activeSession || state.sessionId;
  const running = id === state.sessionId
    ? state.running
    : !!secondaryChats.get(id)?.running;
  if (item.querySelector("input") || id !== active || running) return;
  const input = h("input.chat-tab-rename", { value: before === "Untitled chat" ? "" : before, "aria-label": "Chat title" });
  item.replaceChildren(input);
  input.focus();
  input.select();
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    const title = input.value.trim();
    if (save && title) {
      const chat = secondaryChats.get(id);
      if (chat) {
        try {
          chat.applySummary(await transport.call("chat_rename", { session: id, title, model: chat.summary.model }));
          chatTabs.titles.set(id, title);
        } catch (error) { toast(error.message || String(error)); }
      } else if (await change("rename_session", { id, title })) chatTabs.titles.set(id, title);
    }
    item.replaceChildren(entry.tab, entry.close);
    renderChatTabs();
    requestAnimationFrame(() => entry.tab.focus());
  };
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") { event.preventDefault(); void finish(true); }
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); void finish(false); }
  });
  input.addEventListener("blur", () => void finish(true));
}

function buildSecondarySurface(session) {
  return createChatSurface({ session });
}

class SecondaryChat {
  constructor(read, elements) {
    this.elements = elements;
    this.summary = read.summary;
    this.session = read.summary.session.id;
    this.running = false;
    const base = scopedApi(read.summary.location);
    const chat = this;
    this.api = Object.create(base);
    this.api.summary = () => chat.summary;
    this.api.dictationRoute = () => ({ place: chat.summary?.place?.name || "", session: chat.session, runtime: chat.summary?.location?.runtime });
    this.api.submit = (text, options) => chat.submit(text, options);
    this.api.send = this.api.submit;
    this.api.steer = async (text) => {
      try {
        await transport.call("chat_steer", { session: chat.session, text });
        chat.composer.steering(text);
      } catch {
        chat.composer.enqueue(text);
      }
    };
    this.api.cancel = () => transport.call("chat_cancel", { session: chat.session }).catch(() => {});
    this.api.loadEarlier = async (before) => {
      const read = await transport.call("chat_read", { session: chat.session, model: chat.summary.model, before, limit: 30 });
      chat.thread.renderStored(read.transcript || [], { prepend: true, hasMore: (read.transcript || []).length === 30 });
    };
    this.api.chooseModel = async (spec) => {
      const summary = await transport.call("chat_model", { session: chat.session, spec, model: chat.summary.model });
      chat.applySummary(summary);
      return summary;
    };
    this.api.setExecutionMode = async (mode) => {
      const summary = await transport.call("chat_execution_mode", {
        session: chat.session,
        mode,
        revision: chat.summary.execution_mode_revision || 0,
        model: chat.summary.model,
      });
      chat.applySummary(summary);
      return summary;
    };
    this.thread = new Thread(elements.threadEl, elements.scroller, this.api);
    this.composer = new Composer(this.api, { ...elements, captureDraft });
    this.composer.commands = state.commands || [];
    this.composer.placeholder = `Ask ${read.summary.name} anything`;
    this.applySummary(read.summary);
    if (read.transcript?.length) this.thread.renderStored(read.transcript, { hasMore: read.transcript.length === 30 });
    else this.thread.greet(this.session);
    if (read.running) {
      this.thread.begin(read.running);
      this.setRunning(true);
      for (const message of read.live || []) this.handle(message);
    }
    for (const approval of read.approvals || []) this.handle({ kind: "approval", ...approval });
    elements.surface.addEventListener("pointerdown", () => this.focus(false), true);
    this.composer.input.value = savedDraft(this.session);
    this.composer.restoreImages(imageStash.get(this.session));
    imageStash.delete(this.session);
    this.composer.changed();
    this.composer.input.addEventListener("input", () => saveDraft(this.session, this.composer.input.value));
    elements.scroller.addEventListener("scroll", () => saveScroll(this.session, elements.scroller), { passive: true });
    restoreScroll(this.session, elements.scroller);
  }

  applySummary(summary) {
    this.summary = summary;
    this.composer.setModel(summary.model);
    this.composer.setExecutionMode(summary.execution_mode || "auto", summary.execution_mode_revision || 0);
    chatTabs.titles.set(this.session, summary.session.title || "Untitled chat");
    if (detachedWindow && activeSession === this.session) {
      state.summary = summary;
      state.sessionId = this.session;
      state.title = summary.session.title || (summary.session.turns ? "Untitled chat" : "");
      showTitle();
      document.title = summary.session.title ? `${summary.session.title} · ${summary.name}` : summary.name;
    }
    renderChatTabs();
  }

  setRunning(running) {
    this.running = running;
    this.composer.setRunning(running);
  }

  async submit(text, { images = [], named = false } = {}) {
    text = String(text || "").trim();
    if (!text) return;
    // The agent began meanwhile: the message waits for it, pictures with it.
    if (this.running) return this.composer.enqueue(text, images);
    this.thread.begin(text, { images }).local = true;
    this.setRunning(true);
    try {
      await transport.call("chat_send", {
        session: this.session,
        text,
        images: images.map(i => i.path),
        model: this.summary.model,
        execution_mode: this.composer.executionMode || "auto",
        execution_mode_revision: this.composer.executionModeRevision || 0,
      });
    } catch (error) {
      this.thread.finish({ ok: false, error: error.message });
      this.setRunning(false);
      // A text made of the file names was never typed: it is not put back.
      this.composer.set(named ? "" : text);
      this.composer.addImages(images.map(({ path, name }) => ({ path, name })));
      toast(error.message);
    }
  }

  handle(message) {
    if (message.session && message.session !== this.session) return false;
    switch (message.kind) {
      case "turn_started":
        if (!this.thread.live) this.thread.begin(message.input);
        this.setRunning(true);
        break;
      case "turn_cleanup": toast(message.message); break;
      case "text": this.thread.event(message); break;
      case "event":
        this.thread.event(message);
        if (message.event?.type === "steered") this.composer.delivered(message.event.text);
        else if (message.event?.type === "present" && activeSession === this.session) present(message.event.directive, this.api);
        break;
      case "approval":
        state.approvals.set(message.id, { ...message, session: this.session });
        sidebar.setAttention([...state.approvals.values()]);
        this.thread.approval(message, (id, decision) => transport.call("chat_approve", { session: this.session, id, decision }).catch((error) => toast(error.message)));
        break;
      case "approval_closed":
        state.approvals.delete(message.id);
        sidebar.setAttention([...state.approvals.values()]);
        this.thread.approvalClosed(message.id);
        break;
      case "post": this.thread.post(message.post); break;
      case "turn_finished": {
        this.thread.finish(message);
        this.setRunning(false);
        if (message.summary) this.applySummary(message.summary);
        this.composer.clearSteering();
        const next = this.composer.takeNext(message.undelivered || []);
        if (next) setTimeout(() => this.submit(next.text, { images: next.images, named: next.named }), 120);
        break;
      }
      default: return false;
    }
    return true;
  }

  focus() {
    focusChat(this.session);
  }

  dispose() {
    this.composer.dispose();
    imageStash.set(this.session, this.composer.stashImages());
    saveDraft(this.session, this.composer.input.value);
    saveScroll(this.session, this.elements.scroller);
    this.elements.surface.remove();
  }
}

async function focusChat(session = state.sessionId, focusComposer = true) {
  if (!await showConversation(false)) return;
  activeSession = session;
  if (detachedWindow && secondaryChats.has(session)) {
    const summary = secondaryChats.get(session).summary;
    state.summary = summary;
    state.sessionId = session;
    state.title = summary.session.title || (summary.session.turns ? "Untitled chat" : "");
    showTitle();
    document.title = summary.session.title ? `${summary.session.title} · ${summary.name}` : summary.name;
  }
  for (const surface of document.querySelectorAll(".chat-surface")) surface.dataset.active = String(surface.dataset.session === session);
  renderChatTabs();
  if (focusComposer) (secondaryChats.get(session)?.composer || (session === state.sessionId ? composer : null))?.focus();
  pane?.syncConversation();
}

async function loadSecondary(id, suppliedRead) {
  if (!id) return null;
  if (secondaryChats.has(id)) return secondaryChats.get(id);
  try {
    const read = suppliedRead || await transport.call("chat_read", { session: id, model: state.summary?.model, limit: 30 });
    const current = state.summary?.location;
    const other = read.summary?.location;
    if (!current || !other || current.project !== other.project || current.runtime !== other.runtime) {
      toast("Open chats from the same project and computer in a split");
      return null;
    }
    const chat = new SecondaryChat(read, buildSecondarySurface(id));
    secondaryChats.set(id, chat);
    rememberTab(read.summary);
    return chat;
  } catch (error) {
    toast(error.message || String(error));
    return null;
  }
}

async function showWindowSession(id) {
  const request = ++viewRequest;
  if (!await leaveCurrentView() || request !== viewRequest) return null;
  const read = await transport.call("chat_read", { session: id, model: activeSummary()?.model, limit: 30 });
  if (request !== viewRequest) return null;
  pane?.restoreConversation();
  for (const chat of secondaryChats.values()) chat.dispose();
  secondaryChats.clear();
  state.summary = read.summary;
  state.sessionId = read.summary.session.id;
  state.title = read.summary.session.title || (read.summary.session.turns ? "Untitled chat" : "");
  const chat = new SecondaryChat(read, buildSecondarySurface(id));
  secondaryChats.set(id, chat);
  chatTabs.ids = [id];
  chatTabs.titles = new Map([[id, read.summary.session.title || "Untitled chat"]]);
  chatLayout = chatLeaf(id);
  saveTabs();
  saveLayout();
  renderWorkspace();
  await focusChat(id, false);
  pane.reset(read.summary.project?.id);
  sidebar.refresh(id);
  showTitle();
  return read.summary;
}

async function newWindowSession(project = activeSummary()?.project?.id) {
  try {
    const summary = await transport.call("window_new_session", { project });
    return await showWindowSession(summary.session.id);
  } catch (error) {
    toast(error.message || String(error));
    return null;
  }
}

async function openWindowProject(project) {
  if (project !== state.summary?.project?.id && !await leaveCurrentView()) return false;
  try {
    const sessions = await transport.call("sessions", { project });
    if (sessions?.length) return await showWindowSession(sessions[0].id);
    return await newWindowSession(project);
  } catch (error) {
    toast(error.message || String(error));
    return false;
  }
}

async function switchWindowProject(name) {
  try {
    const projects = await transport.call("projects");
    const project = projects.find((entry) => entry.name === name || entry.id === name);
    if (!project) throw new Error(`No project named ${name}`);
    return await openWindowProject(project.id);
  } catch (error) {
    toast(error.message || String(error));
    return false;
  }
}

function removeSecondary(id) {
  const chat = secondaryChats.get(id);
  if (!chat) return;
  pane?.restoreConversation();
  chat.dispose();
  secondaryChats.delete(id);
}

async function splitInto(id, target = activeSession || state.sessionId, edge = "right") {
  if (!id || id === target) return;
  const visible = visibleChats(chatLayout);
  if (!visible.includes(id) && visible.length >= MAX_VISIBLE_CHATS) return toast("A window can show up to four chats");
  if (id !== state.sessionId && !(await loadSecondary(id))) return;
  chatLayout = splitLayoutChat(chatLayout, target, id, edge);
  saveLayout();
  renderWorkspace();
  focusChat(id);
}

function closeOtherPanes() {
  for (const id of [...secondaryChats.keys()]) removeSecondary(id);
  chatLayout = chatLeaf(state.sessionId);
  saveLayout();
  renderWorkspace();
  focusChat(state.sessionId, false);
}

function beginTabDrag(event, id, item) {
  if (event.target.closest(".chat-tab-close, input")) return event.preventDefault();
  draggedSession = id;
  event.dataTransfer.effectAllowed = "move";
  event.dataTransfer.setData("text/x-ark-chat", id);
  appEl.dataset.arranging = "true";
  requestAnimationFrame(() => { item.dataset.dragging = "true"; });
}

function clearTabDrag() {
  draggedSession = null;
  delete appEl.dataset.arranging;
  for (const item of document.querySelectorAll("[data-dragging], [data-drop], .chat-drop-target[data-over]")) {
    delete item.dataset.dragging;
    delete item.dataset.drop;
    delete item.dataset.over;
  }
}

function dragEndedOutside(event) {
  if (!transport.detachChat || !event || event.dataTransfer?.dropEffect !== "none") return false;
  const { screenX: x, screenY: y } = event;
  if (!Number.isFinite(x) || !Number.isFinite(y) || (x === 0 && y === 0)) return false;
  const inset = 6;
  return x < window.screenX + inset || x > window.screenX + window.outerWidth - inset
    || y < window.screenY + inset || y > window.screenY + window.outerHeight - inset;
}

async function endTabDrag(event, id) {
  const detach = draggedSession === id && dragEndedOutside(event);
  const point = { x: event?.screenX || 0, y: event?.screenY || 0 };
  clearTabDrag();
  if (!detach) return;
  const item = document.querySelector(`.chat-tab-item[data-session="${CSS.escape(id)}"]`);
  if (item) item.dataset.detaching = "true";
  try {
    await transport.detachChat(id, point);
    await closeChatTab(id);
  } catch (error) {
    if (item) delete item.dataset.detaching;
    toast(error.message || "That chat could not open in a new window");
  }
}

function renderWorkspace() {
  const workspace = $("#chat-workspace");
  if (!workspace || !chatLayout) return;
  chatLayout = pruneLayout(chatLayout) || chatLeaf(state.sessionId);
  clear(workspace).append(renderLayoutNode(chatLayout, []));
  const divided = visibleChats(chatLayout).length > 1;
  workspace.dataset.split = String(divided);
  appEl.dataset.split = String(divided);
  pane?.syncConversation();
}

/// A visible split may only contain live chat surfaces. Missing presentation state collapses
/// instead of reserving an empty bordered region in the workspace.
function pruneLayout(node) {
  if (!node) return null;
  if (node.kind === "chat") return node.session === state.sessionId || secondaryChats.has(node.session) ? node : null;
  const first = pruneLayout(node.first);
  const second = pruneLayout(node.second);
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

function renderLayoutNode(node, path) {
  if (node.kind === "chat") {
    const surface = secondaryChats.get(node.session)?.elements.surface
      || (node.session === state.sessionId ? primarySurface : null);
    const leaf = h("div.chat-layout-leaf", { "data-session": node.session });
    if (surface) leaf.append(surface);
    for (const edge of ["top", "right", "bottom", "left"]) {
      const target = h(`div.chat-drop-target.${edge}`, { "data-edge": edge, "aria-hidden": "true" });
      target.addEventListener("dragenter", (event) => { event.preventDefault(); target.dataset.over = "true"; });
      target.addEventListener("dragleave", () => delete target.dataset.over);
      target.addEventListener("dragover", (event) => {
        if (!draggedSession || draggedSession === node.session) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = "move";
      });
      target.addEventListener("drop", (event) => {
        event.preventDefault();
        const id = draggedSession;
        clearTabDrag();
        if (id) void splitInto(id, node.session, edge);
      });
      leaf.append(target);
    }
    return leaf;
  }
  const group = h(`div.chat-layout-split.${node.direction}`, { "data-direction": node.direction });
  const first = renderLayoutNode(node.first, [...path, "first"]);
  const second = renderLayoutNode(node.second, [...path, "second"]);
  const divider = h("div.chat-divider", {
    role: "separator", tabindex: "0",
    "aria-orientation": node.direction === "row" ? "vertical" : "horizontal",
    "aria-label": "Resize chats", "aria-valuemin": "20", "aria-valuemax": "80", "aria-valuenow": String(Math.round(node.ratio * 100)),
  });
  const paint = (ratio) => {
    const a = `${ratio}fr`;
    const b = `${1 - ratio}fr`;
    if (node.direction === "row") group.style.gridTemplateColumns = `${a} 5px ${b}`;
    else group.style.gridTemplateRows = `${a} 5px ${b}`;
    divider.setAttribute("aria-valuenow", String(Math.round(ratio * 100)));
  };
  paint(node.ratio);
  divider.addEventListener("pointerdown", (event) => {
    event.preventDefault();
    divider.setPointerCapture(event.pointerId);
    const move = (next) => {
      const rect = group.getBoundingClientRect();
      const raw = node.direction === "row" ? (next.clientX - rect.left) / rect.width : (next.clientY - rect.top) / rect.height;
      paint(Math.min(0.8, Math.max(0.2, raw)));
    };
    divider.onpointermove = move;
    divider.onpointerup = (next) => {
      move(next);
      divider.onpointermove = null;
      divider.onpointerup = null;
      const ratio = Number(divider.getAttribute("aria-valuenow")) / 100;
      chatLayout = updateRatio(chatLayout, path, ratio);
      saveLayout();
    };
  });
  divider.addEventListener("keydown", (event) => {
    const lower = node.direction === "row" ? "ArrowLeft" : "ArrowUp";
    const higher = node.direction === "row" ? "ArrowRight" : "ArrowDown";
    if (![lower, higher].includes(event.key)) return;
    event.preventDefault();
    chatLayout = updateRatio(chatLayout, path, node.ratio + (event.key === higher ? 0.05 : -0.05));
    saveLayout();
    renderWorkspace();
  });
  group.append(first, divider, second);
  return group;
}

async function restoreVisibleChats(available) {
  let stored = null;
  try { stored = JSON.parse(localStorage.getItem(layoutStorageKey) || "null"); } catch {}
  // Do not revive a legacy split. A deliberate layout from this version is versioned and will
  // restore, but an older experimental tree cannot surprise someone on launch.
  const tree = stored?.version === LAYOUT_STORAGE_VERSION ? stored.tree : null;
  chatLayout = restoreLayout(tree, available, state.sessionId);
  for (const id of visibleChats(chatLayout)) {
    if ((detachedWindow || id !== state.sessionId) && !(await loadSecondary(id))) chatLayout = removeLayoutChat(chatLayout, id);
  }
  saveLayout();
  renderWorkspace();
  activeSession = state.sessionId;
  for (const surface of document.querySelectorAll(".chat-surface")) surface.dataset.active = String(surface.dataset.session === state.sessionId);
  renderChatTabs();
}

function applyPrimaryChange(before, after) {
  pane?.restoreConversation();
  if (!before) {
    chatLayout = chatLeaf(after);
    activeSession = after;
    renderWorkspace();
    renderChatTabs();
    return;
  }
  if (!chatLayout) chatLayout = chatLeaf(after);
  if (pendingPrimary?.promote === after) {
    removeSecondary(after);
  } else if (pendingPrimary?.before === before && pendingPrimary.after === after) {
    chatLayout = replaceChat(chatLayout, before, after);
  } else if (!visibleChats(chatLayout).includes(after)) {
    chatLayout = replaceChat(chatLayout, before, after);
  }
  pendingPrimary = null;
  activeSession = after;
  saveLayout();
  renderWorkspace();
  renderChatTabs();
}

const expectedExtensions = [...document.querySelectorAll("script[data-ark-extension]")]
  .map((script) => ({ id: script.dataset.arkExtension, src: script.getAttribute("src") }));
const extensionErrors = [];
let rejectExtension;
const extensionFailed = expectedExtensions.length
  ? new Promise((_, reject) => { rejectExtension = reject; })
  : null;
addEventListener("error", (event) => {
  const source = event.filename || event.target?.src;
  if (source && expectedExtensions.some((entry) => source.includes(entry.src))) {
    const failure = { source, message: event.message || "The module did not load" };
    extensionErrors.push(failure);
    rejectExtension?.(new Error(failure.message));
  }
}, true);

async function extensionsReady() {
  if (!expectedExtensions.length) return;
  const loaded = () => new Set(loadedExtensions().map((entry) => entry.id));
  const pending = expectedExtensions.filter(({ id }) => !loaded().has(id));
  if (!pending.length) return;
  let timeout;
  try {
    await Promise.race([
      Promise.all(pending.map(({ id }) => waitForExtension(id))),
      extensionFailed,
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error("The app extension did not finish loading")), 5000);
      }),
    ]);
  } catch (error) {
    const ready = loaded();
    const missing = expectedExtensions.filter(({ id }) => !ready.has(id));
    console.error("ARK extension startup failed", { missing, loaded: [...ready], errors: extensionErrors, cause: error });
    document.body.replaceChildren(h("main.startup-failure", { role: "alert" },
      h("div.startup-failure-card",
        h("h1", "ARK could not finish opening"),
        h("p", "A Desktop component did not load. Quit and open ARK again."),
        h("details", h("summary", "Details"), h("pre", JSON.stringify({ missing, errors: extensionErrors }, null, 2))))));
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

function retitle(key, title) {
  const t = pane.tabs.get(key);
  if (t) {
    t.title = title;
    pane.renderTabs();
  }
}

// ---- small UI services ----------------------------------------------------------------------

let toastTimer;
/// A short message. With `action` (`{ label, run }`), a button that undoes or follows up; the
/// message then stays long enough to reach it.
function toast(msg, { action } = {}) {
  const t = $("#toast");
  delete t.dataset.persist;
  t.textContent = msg;
  const hide = () => {
    t.dataset.show = "false";
    delete t.dataset.persist;
    t.querySelector("button")?.remove();
  };
  if (action) {
    t.append(" ", h("button.link", { type: "button", onclick: () => { hide(); action.run(); } }, action.label));
    t.dataset.persist = "";
  }
  t.dataset.show = "true";
  clearTimeout(toastTimer);
  toastTimer = setTimeout(hide, action ? 6000 : 3200);
}

function popover(anchor, children, { above = false, end = false, onClose } = {}) {
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
  el.style.maxHeight = `${Math.max(0, window.innerHeight - 24)}px`;
  el.style.overflowY = "auto";
  const height = el.offsetHeight;
  // Above is the default for a composer control. Menus fill in after they open, so an anchor
  // in the top half has no room there for one that grows.
  if (above && r.top < Math.max(height + 18, 360) && window.innerHeight - r.bottom > r.top) above = false;
  if (above) {
    el.style.bottom = `${Math.max(12, Math.min(window.innerHeight - r.top + 6, window.innerHeight - height - 12))}px`;
    el.classList.add("above");
    el.style.transformOrigin = end ? "bottom right" : "bottom left";
  } else el.style.top = `${Math.max(12, Math.min(r.bottom + 6, window.innerHeight - height - 12))}px`;
  requestAnimationFrame(() => el.classList.add("open"));
  const onDown = (e) => { if (!el.contains(e.target) && e.target !== anchor) close(); };
  const onKey = (e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } };
  function close() {
    document.removeEventListener("mousedown", onDown, true);
    document.removeEventListener("keydown", onKey, true);
    el.classList.remove("open");
    if (el.contains(document.activeElement) && anchor.isConnected) anchor.focus();
    onClose?.();
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

async function submit(text, { images = [], named = false } = {}) {
  text = String(text || "").trim();
  if (!text) return;
  // A slash word that is not a command in this chat is ordinary message text. In particular,
  // selecting another computer must never make a hidden command mutate the local empty chat.
  if (sharedCommand(text)) return;
  if (text.startsWith("/") && (viewForCommand(text, api.project()) || composer.commandAvailable(text))) return runCommand(text);
  if (state.running) return composer.enqueue(text, images, named);
  // No model chosen yet: the text waits in the composer while the person picks one.
  if (composer.missing) {
    composer.set(named ? "" : text);
    composer.addImages(images);
    return composer.modelMenu();
  }
  // The first message of a chat placed on another computer starts it there.
  if (await places.beforeSend(text, images, named)) return;
  for (const hook of registry.submitHooks) {
    try {
      if (await hook(text)) return composer.addImages(images);
    } catch (e) {
      composer.addImages(images);
      return toast(e.message || String(e));
    }
  }
  thread.begin(text, { images }).local = true;
  setRunning(true);
  try {
    await transport.call("send", {
      text,
      images: images.map(i => i.path),
      execution_mode: composer.executionMode || "auto",
      execution_mode_revision: composer.executionModeRevision || 0,
    });
  } catch (e) {
    toast(e.message);
    thread.finish({ ok: false, error: e.message });
    setRunning(false);
    composer.set(named ? "" : text);
    composer.addImages(images.map(({ path, name }) => ({ path, name })));
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
  if (sharedCommand(line)) return;
  const view = viewForCommand(line, api.project());
  if (view) return showView(view.id);
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

// ---- agents: who the window shows and opening their chats (ADR 0110) -----------------------

/// Resolves once the window shows `project` on the same computer, opening it when it does not.
async function arriveAt(project) {
  const now = currentLocation();
  if (now.project === project) return;
  const runtime = now.runtime;
  let off, timer;
  const observed = new Promise((resolve, reject) => {
    const inspect = () => {
      const at = currentLocation();
      if (at.runtime !== runtime) reject(new Error("The computer changed. Try again."));
      else if (at.project === project) resolve();
    };
    off = onExt("ark:summary", inspect);
    timer = setTimeout(() => reject(new Error("The project did not open. Try again.")), 4000);
    inspect();
  });
  try {
    await Promise.all([observed, api.openProject(project).then(ok => { if (!ok) throw new Error("Could not open the project"); })]);
  } finally { off?.(); clearTimeout(timer); }
}

/// A new chat with ARK: in `project`, else in the folder shown now.
async function arkNewChat(project) {
  await api.ready();
  if (registry.newChat && project && project !== currentLocation().project) {
    try { await arriveAt(project); } catch (error) { toast(error.message); return false; }
  }
  const location = { ...currentLocation() };
  const createChat = () => {
    const current = currentLocation();
    if (current.runtime !== location.runtime || current.project !== location.project) throw new Error("The project changed. Open New chat again.");
    return createGeneralSession(project);
  };
  return registry.newChat ? registry.newChat.open({ project: project || location.project, location, createChat }) : createChat();
}

/// The sidebar's +: the app makes an agent in the folder in view and opens its chat.
async function newAgent(anchor = $("#new-agent")) {
  if (appEl.dataset.compact === "true") appEl.dataset.drawer = "closed";
  try {
    await registry.newAgent.open({ location: { ...currentLocation() }, anchor });
  } catch (error) {
    toast(error.message || String(error));
  }
}

const arkName = () => state.summary?.name || "ARK";

/// Whether the sidebar lists the built-in assistant (an app may show only its agents).
const assistant = () => registry.newAgent?.assistant !== false;

/// With no built-in assistant: no agent yet is one New agent button in the window; at start,
/// the latest agent opens instead of the assistant's chat.
let openedFirst = false;
/// Settings, a search result or any view opened from the start state replaces it.
let startLeft = false;
let hadAgents = false;
function leaveStart() {
  if (appEl.dataset.noAgents !== "true") return;
  startLeft = true;
  appEl.dataset.noAgents = "false";
}
async function agentsSettled(entries) {
  if (assistant()) return;
  const none = entries.length === 0;
  // The last agent gone (or none yet): the start state comes back.
  if (!none) startLeft = false;
  if (none && hadAgents) startLeft = false;
  hadAgents = !none;
  appEl.dataset.noAgents = String(none && !startLeft);
  // A chat with an agent or group that is gone gives way to the start; Settings stays.
  const shown = state.view && registry.views.get(state.view);
  if (none && !startLeft && (shown?.agentConversation || shown?.groupConversation)) await showConversation(false);
  if (none) {
    let start = $("#no-agents");
    if (!start) {
      start = h("div.no-agents", { id: "no-agents" }, h("button.btn.primary.no-agents-new", { type: "button", onclick: (e) => void newAgent(e.currentTarget) }, icon("plus"), "New agent"));
      $(".main").append(start);
    }
    return;
  }
  if (openedFirst || state.agent || (state.view && views.get(state.view)?.ctl?.identity?.()?.agent)) return;
  openedFirst = true;
  await openEntry(entries[0]);
}

function arkFace(size) {
  return state.ui.logo
    ? h("img.identity-logo", { src: state.ui.logo, alt: "", width: size, height: size })
    : h("span.identity-logo.fallback", { style: { width: `${size}px`, height: `${size}px` } }, icon("chat"));
}

/// The agent in view, or the last one shown; ARK at start.
function currentEntry() {
  const key = state.agent ? agentKey(state.agent.project, state.agent.name) : ARK;
  return sidebar?.find(key) || (state.agent
    ? { key, name: state.agent.name, label: state.agent.name, project: state.agent.project }
    : { key: ARK, ark: true, name: arkName(), label: arkName() });
}

function setAgent(agent) {
  state.agent = agent;
  sidebar?.markAgent(agent ? agentKey(agent.project, agent.name) : ARK);
}

const hasAgentView = () => [...registry.views.values()].some(view => view.agentConversation);
/// The app's view of a group's chat (`groupConversation: true`), if it draws them.
const groupView = () => [...registry.views.values()].find(view => view.groupConversation)?.id || null;

/// ARK's chats that have a turn or are running their first, newest first, without hidden folders.
async function arkChats() {
  const hidden = hiddenProjects();
  const asking = new Set([...state.approvals.values()].filter(item => !item.request?.agent).map(item => item.session));
  const running = (chat) => chat.working || state.working.some(w => w.session === chat.id);
  return (await transport.call("sessions", { general: true }) || [])
    .filter(chat => (chat.turns > 0 || running(chat)) && !hidden.has(chat.project))
    .map(chat => ({ id: chat.id, title: chat.title || "Untitled chat", updated_at: chat.updated_at, project: chat.project, folder: chat.project_name,
      mark: asking.has(chat.id) ? "asking" : running(chat) ? "working" : null }));
}

async function agentChats(entry) {
  const rows = await api.call("agent_conversations", { agent: entry.name, project: entry.project });
  return (Array.isArray(rows) ? rows : []).map(row => ({ id: row.id, title: row.title || "Untitled chat", updated_at: row.updated_at, default: !!row.default,
    mark: row.asking ? "asking" : row.running ? "working" : null }));
}

/// A tile or row: the agent's latest chat in its own folder. ARK: its newest chat of any folder,
/// in a chat tab of its own with `newTab` (⌘-click). An agent with no chat yet opens its first
/// one, which becomes its default chat.
async function openEntry(entry, { newTab = false } = {}) {
  if (entry.group) {
    if (!groupView()) return false;
    await showView(groupView(), { id: entry.id });
    return true;
  }
  if (entry.ark) {
    const newest = (await arkChats())[0];
    if (!newest) return arkNewChat();
    if (newTab) {
      await openChatTab(newest.id);
      return true;
    }
    if (newest.id === state.sessionId && !detachedWindow) return showConversation();
    await api.openSession(newest.id);
    return true;
  }
  if (entry.session) return api.openAgentChat(entry.name, { project: entry.project, session: entry.session });
  const latest = (await agentChats(entry))[0];
  // The default chat opens without an id: the view knows it before it lists the agent's chats.
  return api.openAgentChat(entry.name, { project: entry.project, conversation: latest && !latest.default ? latest.id : undefined });
}

/// New chat with `entry`. An agent on an agent program has one chat: that one opens. Until an
/// agent has its default chat, a new chat is that one (the phone shows only the default chat).
async function newChatWith(entry) {
  if ((!entry || entry.ark) && !assistant()) return newAgent();
  if (!entry || entry.ark) return arkNewChat();
  if (entry.program) return api.openAgentChat(entry.name, { project: entry.project });
  if (entry.session) return api.openAgentChat(entry.name, { project: entry.project, session: entry.session });
  let chats;
  try { chats = await agentChats(entry); } catch (error) { toast(error.message); return false; }
  return api.openAgentChat(entry.name, { project: entry.project, ...(chats.some(chat => chat.default) ? { conversation: "new" } : {}) });
}

function newChatWithCurrent() {
  return newChatWith(currentEntry());
}

function openHistory(entry, words = "") {
  return searchChats({ scope: entry, words });
}

/// The title's menu: New chat and the last chats with the agent in view.
function recentMenu(anchor, { keyboard, onClose }) {
  const entry = currentEntry();
  const agent = chatIdentity()?.agent;
  const program = entry.program || agent?.program;
  const conversation = agent?.conversation || null;
  return openRecent(anchor, {
    keyboard, onClose, popover, online: state.online, key: entry.key,
    load: () => entry.ark ? arkChats() : agentChats(entry),
    current: row => entry.ark ? !state.view && row.id === state.sessionId : row.id === conversation || (!conversation && row.default),
    newChat: program ? null : () => newChatWith(entry),
    open: row => entry.ark ? api.openSession(row.id) : api.openAgentChat(entry.name, { project: entry.project, conversation: row.default ? undefined : row.id }),
    more: () => openHistory(entry),
  });
}

/// `/new` and `/history [words]` act on the agent in view, in every conversation.
function sharedCommand(text) {
  const line = String(text || "").trim();
  if (line === "/new") { void newChatWithCurrent(); return true; }
  const history = /^\/history(?:\s+([\s\S]*))?$/.exec(line);
  if (history) { void openHistory(currentEntry(), (history[1] || "").trim()); return true; }
  return false;
}

/// A folder from the window's picker; where there is none (a browser), a typed path.
async function askFolder(anchor, title) {
  const picked = await api.pickFolder({ title });
  if (picked !== undefined) return picked;
  return new Promise((done) => {
    const input = h("input.menu-input", { placeholder: "/full/path/to/folder", "aria-label": title, spellcheck: "false" });
    const menu = popover(anchor, [h("div.menu-head", title), input], { onClose: () => done(null) });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && input.value.trim()) {
        const path = input.value.trim();
        done(path);
        menu.close();
      }
    });
    input.focus();
  });
}

/// A new ARK chat in a folder the person picks (it becomes a project if it was not one).
async function openFolder(anchor) {
  const path = await askFolder(anchor || $("#session-title"), "Open a folder");
  if (!path || !await api.addProject({ path })) return;
  const opened = (await transport.call("projects").catch(() => [])).find(project => project.current);
  await arkNewChat(opened?.id);
}

function projectActions(project) {
  return registry.actions.filter((a) => {
    if (a.where !== "project") return false;
    try { return !a.when || a.when({ project }); } catch { return false; }
  });
}

/// The details pane's Folder menu. An agent's chat offers the app's actions only.
function folderMenu(anchor, { agent = false } = {}) {
  const summary = activeSummary();
  const project = summary?.project;
  if (!project) return;
  const item = (text, run, sub) => h("button.menu-item", { type: "button", role: "menuitem", onclick: () => { menu.close(); run(); } },
    sub ? h("span.menu-item-main", h("span", text), h("span.menu-item-sub", sub)) : h("span", text));
  const own = agent ? [] : [
    !summary.session?.turns && (api.canPickFolder || api.isLocal()) && item("Choose folder…", () => void openFolder(anchor)),
    item("Change folder…", async () => {
      const path = await askFolder(anchor, `Choose the folder for all chats in ${project.name}`);
      if (!path) return;
      try { await api.call("project_folder", { id: project.id, path }); } catch (e) { toast(e.message); }
    }, `All chats in ${project.name}`),
    item("Rename", () => renameFolder(anchor, project)),
    project.workspace && item("Copy path", () => navigator.clipboard?.writeText(project.workspace).then(() => toast("Copied"), e => toast(e.message))),
  ].filter(Boolean);
  const actions = projectActions(project).map((a) => item(a.label, async () => {
    try { await a.run({ project }, window.ark); } catch (e) { toast(e.message || String(e)); }
  }));
  if (!own.length && !actions.length) return;
  const menu = popover(anchor, [...own, own.length && actions.length ? h("div.menu-sep") : null, ...actions].filter(Boolean));
  menu.querySelector("button")?.focus({ preventScroll: true });
}

function renameFolder(anchor, project) {
  const input = h("input.menu-input", { value: project.name, "aria-label": "Folder name", maxlength: "120" });
  const menu = popover(anchor, [input]);
  input.addEventListener("keydown", async (e) => {
    if (e.key !== "Enter" || !input.value.trim()) return;
    menu.close();
    if (input.value.trim() === project.name) return;
    try { await api.call("rename_project", { id: project.id, name: input.value.trim() }); } catch (error) { toast(error.message); }
  });
  input.focus();
  input.select();
}

async function createGeneralSession(project) {
  if (!await showConversation()) return;
  if (detachedWindow) return newWindowSession(project);
  return change("new_session", project ? { project } : undefined);
}

// Moving to another chat never waits: a chat that is working goes on in the background.
async function change(method, params) {
  if (["open_session", "new_session", "open_project", "switch_project", "add_project"].includes(method)) {
    const request = ++viewRequest;
    if (!await leaveCurrentView() || request !== viewRequest) return false;
  }
  try {
    await transport.call(method, params);
    return true;
  } catch (e) {
    toast(e.message);
    return false;
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
  showTitle();
  appEl.dataset.running = String(on);
  places?.show(state.summary, on);
}

// ---- session display -------------------------------------------------------------------------

function applySummary(s) {
  const before = state.sessionId;
  if (state.sessionId && s.session.id !== state.sessionId) saveDraft();
  const changed = s.session.id !== state.sessionId;
  const projectChanged = s.project?.id !== state.summary?.project?.id;
  const runtimeChanged = !!state.summary && s.location?.runtime !== state.summary?.location?.runtime;
  if (projectChanged || runtimeChanged) { viewRequest++; pane?.setWorkspace(null); pane?.setExpanded(false); rememberedConversation = null; }
  if (state.summary && projectChanged) {
    if (visibleChats(chatLayout).length > 1) closeOtherPanes();
    chatTabs.ids = [];
    chatTabs.titles.clear();
  }
  state.summary = s;
  if (!isReady && validLocation(s.location)) {
    isReady = true;
    readyResolve();
  }
  state.sessionId = s.session.id;
  primarySurface?.setAttribute("data-session", state.sessionId);
  state.title = s.session.title || (s.session.turns ? "Untitled chat" : "");
  if (s.location) resourceOnline.set(locationKey(s.location), !s.place || s.place.state === "ok");
  showTitle();
  if (projectChanged || runtimeChanged) {
    for (const [id, entry] of views) {
      if (!registry.views.get(id)?.resourceScoped) continue;
      retireResourceView(id, entry);
      views.delete(id);
    }
  }
  if ((projectChanged || runtimeChanged) && state.view) {
    const view = registry.views.get(state.view);
    if (view?.resourceScoped) showConversation(false);
    else if (view?.projectScoped) views.get(state.view)?.ctl?.update?.({ project: s.project });
  }
  composer.setModel(s.model);
  composer.setExecutionMode(s.execution_mode || "auto", s.execution_mode_revision || 0);
  showTitle();
  rememberTab(s);
  if (changed) {
    // The pictures wait with the chat they were put in. Promotion disposes the secondary
    // composer and saves its latest draft and pictures first.
    if (before) imageStash.set(before, composer.stashImages());
    applyPrimaryChange(before, state.sessionId);
    composer.restoreImages(imageStash.get(state.sessionId));
    imageStash.delete(state.sessionId);
    const draft = savedDraft(state.sessionId);
    composer.input.value = draft;
    composer.caret = draft.length;
    composer.input.setSelectionRange(draft.length, draft.length);
    composer.changed();
  }
  places?.show(s, state.running);
  composer.dictation.routeChanged();
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
  restoreScroll(state.sessionId, $("#scroller"));
}

function showWelcome() {
  thread.greet(state.sessionId);
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
  state.online = msg.kind === "connected";
  if (msg.kind === "connected") return toast("Reconnected");
  state.connectionEpoch++;
  sidebar.setOnline(false);
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
  const hazard = state.computerHazard;
  const message = hazard ? hazard.error || `Check your message in ${hazard.app}` : activity?.line;
  el.hidden = !user && !hazard;
  if (el.hidden) return;
  const who = user ? h("button.computer-who", { type: "button", title: user.session ? "Open the chat" : "" },
    // On a runtime elsewhere it is not the person's own computer.
    h("span.working-dot"), message || `${user.who} is using ${(window.ark?.isLocal?.() ?? true) ? "your computer" : "the computer ARK runs on"}`)
    : h("span.computer-who", { role: "status", "aria-live": "polite" }, message || "Computer needs attention");
  if (user?.session) who.onclick = () => api.openSession(user.session);
  const stop = h("button.computer-stop", { type: "button", title: "Take the computer back" }, "Stop");
  stop.onclick = () => transport.call("computer_stop").catch((e) => toast(e.message));
  const check = hazard ? h("button.computer-stop", { type: "button", "aria-label": "Check the message app again" }, "Check again") : null;
  if (check) check.onclick = () => transport.call("computer_check").catch((e) => toast(e.message));
  const review = hazard ? h("button.computer-stop", { type: "button" }, "Review…") : null;
  if (review) review.onclick = () => reviewComputerMessage((method, params) => transport.call(method, params), () => {}, review).catch(e => toast(e.message));
  el.append(...[who, check, review, user ? stop : null].filter(Boolean));
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
  if (msg.kind === "approval") {
    state.approvals.set(msg.id, msg);
    sidebar.setAttention([...state.approvals.values()]);
  } else if (msg.kind === "approval_closed") {
    state.approvals.delete(msg.id);
    sidebar.setAttention([...state.approvals.values()]);
  }
  // App scripts can follow the same stream: `ark.on("ark:turn_finished", fn)`.
  // A summary listener must see the new summary through `ark.summary()`, not the previous
  // chat. Summary is dispatched in its case below, after `applySummary`.
  if (msg.kind !== "ext" && msg.kind !== "text" && msg.kind !== "summary") dispatchExt(`ark:${msg.kind}`, msg);
  // A detached window owns its foreground in the page. Runtime-wide summary events belong to
  // another native window; addressed events still update this window's live chat.
  if (detachedWindow && msg.session && secondaryChats.get(msg.session)?.handle(msg)) return;
  if (detachedWindow && msg.kind === "summary") return;
  // A question an orchestrated agent asks belongs to no chat (`session` is empty).
  if (msg.session && msg.session !== state.sessionId) {
    if (secondaryChats.get(msg.session)?.handle(msg)) return;
    return elsewhere(msg);
  }
  switch (msg.kind) {
    case "turn_started":
      if (!thread.live) {
        thread.begin(msg.input);
      }
      setRunning(true);
      break;
    case "turn_cleanup":
      toast(msg.message);
      break;
    case "text":
      thread.event(msg);
      break;
    case "event": {
      thread.event(msg);
      const ev = msg.event;
      if (ev.type === "context" && !["plan", "critic"].includes(ev.report.role)) setRing(ev.report.total, ev.report.budget);
      else if (ev.type === "steered") composer.delivered(ev.text);
      else if (ev.type === "present" && (!activeSession || activeSession === state.sessionId)) present(ev.directive);
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
    case "orch":
      if (/^(task_|agent)/.test(msg.event?.type || "")) refreshAgentsSoon();
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
    case "computer_hazard":
      state.computerHazard = msg.hazard;
      showComputer();
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
      if (next) setTimeout(() => submit(next.text, { images: next.images, named: next.named }), 120);
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
      if (msg.name === "ark:group") refreshAgentsSoon();
      break;
    case "resync":
      boot(true);
      break;
  }
}

// Agents' marks follow their tasks; a burst of events reads them once.
let agentsTimer = 0;
function refreshAgentsSoon() {
  clearTimeout(agentsTimer);
  agentsTimer = setTimeout(() => sidebar.refresh(), 250);
}

function ask(msg) {
  // An app's conversation may show the same question in place; the tray keeps its own card.
  if (document.querySelector(`#asks .approval[data-id="${msg.id}"]`)) return;
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

function present(d, target = api) {
  if (document.hidden || !document.hasFocus()) return;
  const entry = state.view && views.get(state.view);
  const view = state.view && registry.views.get(state.view);
  if (entry && !view?.workspace && !entry.ctl?.conversation?.()) return;
  if (view?.workspace && entry.ctl?.acceptsPresentation && !entry.ctl.acceptsPresentation(d)) return;
  const by = presenterFor(d.view);
  // A file opens in a tab of its own, unless the app shows files itself.
  if (d.view === "file" && d.path && (!by?.panel || by.panel.builtin)) return target.openFile(d.path, d);
  if (by?.view) return target.openView(by.view.id, d);
  if (by?.panel) return target.openPanel(by.panel.id, d);
  if (by?.viewer && d.artifact) return target.openArtifact(d.artifact, by.viewer.id, d);
  if (d.artifact) return target.openArtifact(d.artifact, undefined, d);
  if (d.view === "evidence" && d.locator) return target.openInvocation(d.locator);
  toast(`The agent asked to show ${d.view === "split" ? "a split view" : `a ${d.view}`}, which this window cannot show.`);
}

// ---- views in the main column ---------------------------------------------------------------

/// Show an app's view in place of the conversation. A view is built once; showing it again
/// passes new params to `update`, or calls `refresh`.
let rememberedConversation = null, viewRequest = 0;
const retainedResourceViews = new Map();
const retainedViewKey = (id, location) => JSON.stringify([id, location.runtime, location.project]);
function retireResourceView(id, entry) {
  const key = retainedViewKey(id, entry.location);
  entry.el.hidden = true; entry.el.inert = true;
  retainedResourceViews.set(key, entry);
  entry.retiring = Promise.resolve().then(() => entry.ctl?.beforeRemove?.() ?? true).then(allowed => {
    if (allowed !== true) return false;
    entry.ctl?.dispose?.(); entry.el.remove(); entry.retired = true;
    if (retainedResourceViews.get(key) === entry) retainedResourceViews.delete(key);
    return true;
  }).catch(error => { toast(`Unfinished work in the previous project: ${error.message}. Return to that project to continue.`); return false; });
  return entry.retiring;
}
async function showView(id, params) {
  const v = id === SETTINGS ? settingsView : registry.views.get(id);
  if (!v) return toast(`No view named ${id}`);
  leaveStart();
  const request = ++viewRequest;
  const previous = state.view && views.get(state.view);
  if (previous && state.view !== id && previous.ctl?.beforeRemove) {
    try { if (await previous.ctl.beforeRemove() !== true || request !== viewRequest) return; }
    catch (error) { toast(error.message); return; }
  }
  if (v.workspace && !registry.views.get(state.view)?.workspace) rememberedConversation = previous?.ctl?.conversation?.() || null;
  if (!v.workspace) rememberedConversation = null;
  pane?.restoreConversation();
  // A project view owns the full main column. End the chat layout before showing it so the
  // top selector always remains a visible route back to Chat.
  if (visibleChats(chatLayout).length > 1) closeOtherPanes();
  if ((v.projectScoped || v.resourceScoped) && params === undefined) params = { project: api.project() };
  const location = { ...currentLocation() };
  let entry = views.get(id);
  if (entry && v.resourceScoped && (entry.location.runtime !== location.runtime || entry.location.project !== location.project)) {
    retireResourceView(id, entry);
    views.delete(id);
    entry = null;
  }
  let restored = false;
  if (!entry && v.resourceScoped) {
    const key = retainedViewKey(id, location), retained = retainedResourceViews.get(key);
    if (retained) {
      await retained.retiring;
      if (request !== viewRequest) return false;
      if (!retained.retired) {
        retainedResourceViews.delete(key); entry = retained; entry.el.inert = false; views.set(id, entry); restored = true;
      }
    }
  }
  if (!entry) {
    const el = h("div.view", { "data-view": id });
    $("#stage").append(el);
    entry = { el, ctl: null, location };
    views.set(id, entry);
    try {
      entry.ctl = v.render(el, params, v.resourceScoped ? scopedApi(location) : window.ark) || {};
    } catch (e) {
      console.error(e);
      el.append(h("div.insp-empty", String(e)));
      entry.ctl = {};
    }
  } else if (!restored && params !== undefined && entry.ctl.update) {
    entry.ctl.update(params);
  } else if (!restored) {
    entry.ctl.refresh?.();
  }
  if (state.view && state.view !== id) views.get(state.view)?.ctl?.hide?.();
  for (const [k, x] of views) x.el.hidden = k !== id;
  state.view = id;
  entry.ctl.show?.();
  if (v.agentConversation && params?.name) setAgent({ name: params.name, project: params.project || location.project });
  else if (v.agentConversation && params?.draft) setAgent(null);
  // A group's chat: its tile is the current one.
  if (v.groupConversation && params?.id) { state.agent = null; sidebar?.markAgent(`group:${params.id}`); }
  if (id !== SETTINGS) state.homeView = id;
  appEl.dataset.view = id;
  $("#stage").hidden = false;
  showTitle();
  sidebar.markView(id);
  topbar.mark(id);
  reportFocus();
  entry.ctl.focusInput?.();
  if (pane?.isOpen) pane.reportFocus();
  pane?.syncConversation();
}

async function leaveCurrentView() {
  const entry = state.view && views.get(state.view);
  if (!entry?.ctl?.beforeRemove) return true;
  try { return await entry.ctl.beforeRemove() === true; }
  catch (error) { toast(error.message); return false; }
}

async function showConversation(focusComposer = true) {
  const request = ++viewRequest;
  if (state.view && views.get(state.view)?.ctl?.beforeRemove && (!await leaveCurrentView() || request !== viewRequest)) return false;
  state.homeView = null;
  setAgent(null);
  if (!state.view) {
    pane?.syncConversation();
    return true;
  }
  pane?.restoreConversation();
  views.get(state.view)?.ctl?.hide?.();
  state.view = null;
  delete appEl.dataset.view;
  $("#stage").hidden = true;
  showTitle();
  sidebar.markView(null);
  topbar.mark(null);
  reportFocus();
  if (focusComposer) composer.focus();
  pane?.syncConversation();
  return true;
}

/// Home is the current project's workspace. It remembers whether the person last used Chat or
/// one of the project's app views. A view that is not offered in the new project falls back to
/// Chat instead of restoring an invalid mode.
function showHome(focusComposer = true) {
  const id = state.homeView;
  const view = id && registry.views.get(id);
  let offered = false;
  try { offered = !!view && (!view.when || view.when({ project: api.project() })); } catch {}
  if (offered) showView(id);
  else showConversation(focusComposer);
}

/// The chat's title over the conversation; over a view of the project (whose tab says which),
/// the project's name; else the view's title (Settings).
/// Who this chat is with. An app view supplies its own; a plain chat is with ARK itself.
function chatIdentity() {
  if (state.view) {
    const identity = views.get(state.view)?.ctl?.identity?.() || null;
    // An agent's chat works in the agent's folder: the app's folder actions only.
    if (identity?.agent && !identity.folder) {
      const project = activeSummary()?.project;
      if (project) identity.folder = { name: project.name, path: project.workspace || "", open: projectActions(project).length ? (anchor) => folderMenu(anchor, { agent: true }) : null };
    }
    return identity;
  }
  const summary = activeSummary();
  if (!summary?.session) return null;
  const active = secondaryChats.get(activeSession)?.composer || composer;
  const picker = registry.modelPicker;
  const spec = summary.model;
  const sessionId = summary.session.id;
  return {
    name: summary.name || "ARK",
    title: secondaryChats.get(activeSession)?.summary?.session?.title || state.title || "New chat",
    description: summary.tagline || "",
    avatar: arkFace,
    model: {
      label: active.model.textContent,
      mark: () => picker?.mark?.(spec) || null,
      disabled: secondaryChats.get(activeSession)?.running ?? state.running,
      open: (anchor) => active.modelMenu(anchor),
    },
    rename: async (title) => {
      await transport.call("rename_session", { id: sessionId, title });
      if (sessionId === state.sessionId) state.title = title;
      chatTabs.titles.set(sessionId, title);
      showTitle();
    },
    folder: summary.project ? { name: summary.project.name, path: summary.project.workspace || "", open: (anchor) => folderMenu(anchor) } : null,
  };
}

function showTitle() {
  const v = state.view && (state.view === SETTINGS ? settingsView : registry.views.get(state.view));
  const el = $("#session-title");
  const controller = v && views.get(state.view)?.ctl;
  el.textContent = !v ? state.title || "" : controller?.title?.() || (v.nav === true ? state.summary?.project?.name || "" : v.title);
  let options = $("#view-options");
  if (!options) {
    options = h("button.icon-btn", { id: "view-options", type: "button", "aria-label": "Conversation options", "aria-haspopup": "menu", onclick: () => {
      const entries = views.get(state.view)?.ctl?.menu?.();
      if (entries?.length) popover(options, entries);
    } }, icon("more"));
    $(".top-actions").prepend(options);
  }
  options.hidden = !controller?.menu;
  // The face and name of who the chat is with replace the plain title and open its details.
  const chip = identity?.draw(chatIdentity());
  if (chip) el.replaceChildren(chip);
  renderChatTabs();
  // A chat that runs on another computer says where.
  const place = !v && state.summary?.place;
  if (place) el.append(h("span.place-mark", { title: `Runs on ${place.name}` }, icon("server"), place.name));
  const workspace = state.view && registry.views.get(state.view)?.workspace && views.get(state.view);
  pane?.setWorkspace(workspace ? workspace.el : null, workspace ? () => workspace.ctl?.focus?.() || null : null);
  pane?.syncConversation();
}

/// What the person looks at: the view's focus, else the side pane's.
function reportFocus() {
  if (pane?.workspace || pane?.expanded || (pane?.isOpen && document.activeElement?.closest("#pane"))) return pane.reportFocus();
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

function panelShown(panel, project = api.project()) {
  try {
    return !panel.when || !!panel.when({ project });
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
    { label: "New chat", hint: label("⌘N"), run: () => api.newSession() },
    ...((api.canPickFolder || api.isLocal()) ? [{ label: "Open folder…", hint: "folder", run: () => void openFolder() }] : []),
    ...(!api.canPickFolder ? [{ label: "Upload a folder…", hint: "folder", run: () => uploadFolder({
      ...api,
      openProject: async (id) => { if (await api.openProject(id)) await arkNewChat(id); },
    }) }] : []),
    ...sidebar.hiddenList().map(([id, name]) => ({ label: `Show ${name} again`, hint: "folder", run: () => sidebar.restore(id) })),
    ...[...registry.views.values()]
      .filter((v) => !v.when || v.when({ project: api.project() }))
      .map((v) => ({ label: v.title, hint: "view", run: () => showView(v.id) })),
    ...(registry.settings.size ? [{ label: "Settings", hint: label("⌘,"), run: () => api.openSettings() }] : []),
    ...[...registry.panels.values()]
      .filter((p) => panelShown(p))
      .map((p) => ({ label: p.title, hint: "panel", run: () => api.openPanel(p.id) })),
    ...registry.actions.filter((a) => (!a.where || a.where === "palette") && shown(a)).map((a) => ({ label: a.label, hint: "action", run: () => a.run({}, window.ark) })),
    ...(state.commands || [])
      .filter((c) => composer.commandAvailable(`/${c.name}`))
      .map((c) => ({ label: `/${c.name}${c.usage ? " " + c.usage : ""}`, hint: c.summary, run: () => (c.usage && !c.usage.startsWith("[") ? composer.set(`/${c.name} `) : runCommand(`/${c.name}`)) })),
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

let searchGeneration = 0;
/// ⇧⌘F: chats and the app's results across folders. With `scope` (an agent, or ARK), only that
/// agent's chats (`/history`, All chats), filtered by `words`.
async function searchChats({ scope = null, words = "" } = {}) {
  const overlay = $("#overlay");
  if (!overlay.hidden && !scope) return closePalette();
  const restore = document.activeElement;
  const generation = ++searchGeneration;
  const location = { ...currentLocation() };
  const searchRuntime = state.runtime;
  const placeholder = scope ? `Search chats with ${scope.label || scope.name}` : "Search chats and agents";
  const input = h("input.palette-input", { placeholder, "aria-label": placeholder, role: "combobox", "aria-autocomplete": "list", "aria-controls": "chat-search-results", "aria-expanded": "true" });
  input.value = words;
  const list = h("div.palette-list", { id: "chat-search-results", role: "listbox", "aria-busy": "true" }, h("div.palette-empty", "Searching…"));
  let items = [];
  let failedProjects = 0;
  let loadState = "loading";
  let loadError = "";
  let selected = 0;
  const close = () => {
    searchGeneration++;
    overlay.hidden = true;
    if (restore?.isConnected) restore.focus();
    else composer.focus();
  };
  const open = async (item) => {
    close();
    if (item.open) {
      try { await item.open(); } catch (error) { api.toast(error.message || "Could not open the chat"); }
      return;
    }
    try {
      if (state.runtime !== searchRuntime || currentLocation()?.runtime !== location.runtime) throw new Error("The computer changed. Search again.");
      if (currentLocation()?.project !== item.project.id || currentLocation()?.runtime !== searchRuntime) {
        let off, timer;
        const observed = new Promise((resolve, reject) => {
          const inspect = () => {
            const now = currentLocation();
            if (now?.runtime !== location.runtime && now?.runtime !== searchRuntime) reject(new Error("The computer changed. Search again."));
            else if (now?.project === item.project.id && now?.runtime === searchRuntime) resolve();
          };
          off = onExt("ark:summary", inspect);
          timer = setTimeout(() => reject(new Error("The project did not open. Try again.")), 4000);
          inspect();
        });
        try {
          await Promise.all([observed, api.openProject(item.project.id).then(ok => {
            if (!ok) throw new Error("Could not open the project");
          })]);
        } finally { off?.(); clearTimeout(timer); }
      }
      const now = currentLocation();
      if (now?.runtime !== searchRuntime || now?.project !== item.project.id) throw new Error("The project changed. Search again.");
      if (item.source) await item.source.open(item.entry, api.at(now));
      else await api.openSession(item.session.id);
    } catch (error) {
      api.toast(error.message || "Could not open the result");
    }
  };
  const paint = () => {
    const query = input.value.trim().toLocaleLowerCase();
    const shown = items.filter((item) => !query || [item.title, item.description, item.project?.name].some(text => text?.toLocaleLowerCase().includes(query))).slice(0, 100);
    selected = Math.min(selected, Math.max(0, shown.length - 1));
    clear(list);
    list.setAttribute("aria-busy", String(loadState === "loading"));
    if (loadState === "loading") {
      list.append(h("div.palette-empty", "Searching…"));
      input.removeAttribute("aria-activedescendant");
      return [];
    }
    if (loadState === "error") {
      list.append(h("div.palette-empty", loadError || "Search is not available", h("button.dk-btn", { type: "button", onclick: () => void loadResults() }, "Try again")));
      input.removeAttribute("aria-activedescendant");
      return [];
    }
    if (failedProjects) list.append(h("div.palette-note", "Some results are not available", h("button.dk-btn.ghost", { type: "button", onclick: () => void loadResults() }, "Try again")));
    if (!shown.length) {
      const message = failedProjects
        ? (items.length ? "No matching available results" : "Some results could not be searched")
        : scope ? (items.length ? "No matching chats" : "No chats yet")
        : (items.length ? "No matching chats or agents" : "No chats or agents yet");
      list.append(h("div.palette-empty", message));
    }
    shown.forEach((item, index) => {
      const row = h("div.palette-item", { id: `chat-search-${index}`, role: "option", "aria-selected": String(index === selected) },
        h("span.palette-item-main", h("span", item.title || "Untitled"), h("span.palette-hint", [item.project?.name, item.description].filter(Boolean).join(" · "))),
        item.mark === "asking" ? h("span.palette-hint.palette-mark", "Needs you")
          : item.mark === "working" ? h("span.palette-hint.palette-mark", h("span.working-dot", { "aria-hidden": "true" }), "Working")
            : h("span.palette-hint", item.source ? (item.source.label || "") : item.updated_at ? ago(item.updated_at) : ""));
      row.addEventListener("mousedown", (event) => { event.preventDefault(); void open(item); });
      list.append(row);
    });
    const active = shown.length ? `chat-search-${selected}` : "";
    if (active) {
      input.setAttribute("aria-activedescendant", active);
      list.querySelector(`#${active}`)?.scrollIntoView({ block: "nearest" });
    } else input.removeAttribute("aria-activedescendant");
    return shown;
  };
  input.addEventListener("input", () => { selected = 0; paint(); });
  input.addEventListener("keydown", (event) => {
    const shown = paint();
    if (event.key === "ArrowDown" && shown.length) { selected = (selected + 1) % shown.length; paint(); event.preventDefault(); }
    if (event.key === "ArrowUp" && shown.length) { selected = (selected + shown.length - 1) % shown.length; paint(); event.preventDefault(); }
    if (event.key === "Enter" && shown[selected]) { event.preventDefault(); void open(shown[selected]); }
    if (event.key === "Escape") { event.stopPropagation(); close(); }
  });
  clear(overlay).append(h("div.palette-box", input, list));
  overlay.hidden = false;
  overlay.onmousedown = (event) => event.target === overlay && close();
  input.focus();
  const loadResults = async () => {
    loadState = "loading";
    loadError = "";
    failedProjects = 0;
    paint();
    try {
      if (scope) {
        const rows = scope.ark ? await arkChats() : await agentChats(scope);
        if (generation !== searchGeneration) return;
        items = rows.map(row => ({
          title: row.title, updated_at: row.updated_at, mark: row.mark,
          project: scope.ark ? { id: row.project, name: row.folder } : null,
          open: () => scope.ark ? api.openSession(row.id) : api.openAgentChat(scope.name, { project: scope.project, conversation: row.id }),
        }));
        loadState = "ready";
        paint();
        return;
      }
      const projects = await api.call("projects");
      const sources = [...registry.searchSources.values()];
      const groups = await Promise.all(projects.map(async (project) => {
        const results = await Promise.allSettled([
          api.call("sessions", { project: project.id }),
          ...sources.map(source => Promise.resolve().then(() => source.load(project, api))),
        ]);
        const rows = [];
        results.forEach((result, index) => {
          if (result.status === "rejected") { failedProjects++; return; }
          if (!Array.isArray(result.value)) { failedProjects++; return; }
          for (const entry of result.value) {
            if (index === 0) {
              if (entry.turns > 0) rows.push({ project, session: entry, title: entry.title, updated_at: entry.updated_at });
            } else if (typeof entry?.id === "string" && typeof entry.title === "string") {
              rows.push({ project, entry, source: sources[index - 1], title: entry.title, description: entry.description, updated_at: entry.updated_at });
            }
          }
        });
        return rows;
      }));
      if (generation !== searchGeneration) return;
      items = groups.flat().sort((a, b) => new Date(b.updated_at || 0) - new Date(a.updated_at || 0));
      loadState = "ready";
      paint();
    } catch (error) {
      if (generation !== searchGeneration) return;
      loadState = "error";
      loadError = error.message || "Search is not available";
      paint();
    }
  };
  await loadResults();
}

// The desktop window is an app, not a browser: no reload or print keys, and no browser menu
// except on text, a link or a selection, where it copies and pastes.
function appLike() {
  addEventListener("keydown", (e) => {
    const k = e.key.toLowerCase();
    if (e.key === "F5" || ((e.metaKey || e.ctrlKey) && (k === "r" || k === "p"))) e.preventDefault();
  }, true);
  const contextual = e => {
    if (e.defaultPrevented) return;
    const file = e.target.closest(".file-item");
    const turn = e.target.closest(".turn .user,.turn .agent");
    const viewer = e.target.closest(".fv");
    const scope = file || viewer;
    const actions = scope ? controlActions(scope.querySelectorAll(file ? "button" : ".fv-tools button:not([hidden]),.wb-bar button:not([hidden])")) : turn ? [{ label: "Copy message", run: () => navigator.clipboard.writeText(turn.innerText).catch(error => toast(error.message)) }] : [];
    if (actions.length) contextMenu(e, scope?.querySelector("button") || turn, actions);
  };
  document.addEventListener("keydown", e => { if (e.key === "ContextMenu" || e.shiftKey && e.key === "F10") contextual(e); });
  document.addEventListener("contextmenu", (e) => {
    contextual(e);
    const onText = e.target.closest("input, textarea, [contenteditable], a, .xterm");
    if (!onText && !String(window.getSelection() || "")) e.preventDefault();
  });
}

// ⌘K and ⌘N are also items of the window's menu (`desktop.rs`), so they work while the Browser
// panel's own view has the keyboard (ADR 0044). When the page had the keyboard it acted on the
// key already: a menu key right after the page's own press of it, with no key between, is not
// acted on twice.
const MENU_KEYS = { k: () => palette(), n: () => api.newSession() };
const pressed = { key: "", at: 0 };
addEventListener("keydown", (e) => {
  Object.assign(pressed, e.metaKey || e.ctrlKey ? { key: e.key.toLowerCase(), at: performance.now() } : { key: "", at: 0 });
}, true);

function menuKey(e) {
  const run = Object.hasOwn(MENU_KEYS, e.detail) && MENU_KEYS[e.detail];
  if (!run || (pressed.key === e.detail && performance.now() - pressed.at < 500)) return;
  run();
}

function keys(e) {
  if (e.defaultPrevented) return;
  const mod = e.metaKey || e.ctrlKey;
  const key = e.key.toLowerCase();
  if (mod && Object.hasOwn(MENU_KEYS, key)) {
    e.preventDefault();
    return MENU_KEYS[key]();
  }
  // ⌘1 to ⌘9: the nth agent of the sidebar. In a browser these keys are the browser's tabs.
  if (mod && !e.shiftKey && !e.altKey && /^[1-9]$/.test(e.key) && transport.shell === "desktop") {
    e.preventDefault();
    return sidebar.openNth(Number(e.key) - 1);
  }
  if (mod && e.shiftKey && e.key.toLowerCase() === "f") { e.preventDefault(); return searchChats(); }
  if (mod && e.key === "," && registry.settings.size) { e.preventDefault(); return api.openSettings(); }
  // The built-in panels' keys, unless an app replaced the panel.
  for (const b of BUILTINS) {
    if (b.key(e) && registry.panels.get(b.id)?.builtin) { e.preventDefault(); return api.openPanel(b.id); }
  }
  if (state.view) {
    if (mod && key === "\\") { e.preventDefault(); return pane.toggle(); }
    if (e.key === "Escape") {
      if (pane.isOpen && !document.activeElement?.closest("#pane")) return pane.close();
      return views.get(state.view)?.ctl?.cancel?.();
    }
    if (!mod && e.key.length === 1 && !e.target.closest("input, textarea, [contenteditable], .pane, .popover")) views.get(state.view)?.ctl?.focusInput?.();
    if (mod && key === "b") { e.preventDefault(); toggleSidebar(); }
    return;
  }
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
  if (state.view) return views.get(state.view)?.ctl?.rename?.();
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
  const compact = w <= 760 || (w <= 1050 && appEl.dataset.pane === "open" && !pane?.expanded);
  appEl.dataset.compact = String(compact);
  const styles = getComputedStyle(appEl);
  const px = (name) => Number.parseFloat(styles.getPropertyValue(name)) || 0;
  const chrome = compact ? 0 : px("--sb");
  const room = Math.max(320, w - chrome - px("--chat-min"));
  appEl.style.setProperty("--pw-room", `${room}px`);
  let paneWidth = 0;
  if (appEl.dataset.pane === "open" && !pane?.expanded) {
    if (w <= 760) paneWidth = w;
    else {
      const preferred = appEl.hasAttribute("data-sized") ? px("--pw-user") : appEl.dataset.wide === "true" ? 760 : 470;
      paneWidth = Math.min(Math.max(320, preferred), room);
    }
  }
  appEl.style.setProperty("--pw", `${paneWidth}px`);
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
    if (s?.accent) window.arkTheme.accent(s.accent);
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
  const connectionEpoch = state.connectionEpoch;
  let hello;
  let detachedRead = null;
  try {
    // The person's time zone: what they schedule "at 9" runs at their 9, wherever ARK runs.
    let tz;
    try { tz = Intl.DateTimeFormat().resolvedOptions().timeZone; } catch {}
    hello = await transport.call("hello", tz ? { tz } : undefined);
    if (detachedWindow) {
      detachedRead = await transport.call("chat_read", { session: detachedSession, model: hello.summary?.model, limit: 30 });
    }
  } catch (e) {
    if (e.signIn) return signInScreen();
    clear($("#thread")).append(h("div.welcome", h("h1", "Not connected"), h("p", e.message)));
    return;
  }
  feed.seq = Math.max(hello.seq || 0, detachedRead?.seq || 0);
  state.ui = hello.ui || {};
  state.runtime = hello.runtime;
  state.fileSnapshots = !!hello.file_snapshots;
  setFileKinds(hello.file_kinds, hello.kind_nouns);
  state.commands = hello.commands || [];
  composer.commands = state.commands;
  if (state.ui.accent) window.arkTheme.accent(state.ui.accent);
  else window.arkTheme.fitAccent();
  applySummary(detachedRead?.summary || hello.summary);
  places.load();
  composer.placeholder = `Ask ${hello.summary.name} anything`;
  if (!again) {
    sidebar.renderExtensions();
    topbar.render();
    // ARK's own Settings sections.
    registerAppearance(window.ark);
    registerGrants(window.ark);
    registerMarketplace(window.ark);
    window.ark.panel({ id: DETAILS, title: "Details", icon: "chat", render: (el) => identity.panel(el) });
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
    boardWaiting((count) => sidebar.setBoardWaiting(count));
    registerCalendar(window.ark);
  }
  if (detachedWindow) {
    secondaryChats.get(detachedSession)?.dispose();
    secondaryChats.delete(detachedSession);
    await loadSecondary(detachedSession, detachedRead);
    chatLayout = chatLeaf(detachedSession);
    renderWorkspace();
    focusChat(detachedSession, false);
  } else await loadSession();
  state.working = hello.working || [];
  sidebar.setWorking(state.working);
  sidebar.setOnline(state.online && connectionEpoch === state.connectionEpoch);
  state.computerHazard = hello.computer_hazard || null;
  showComputer(hello.computer);
  // Questions are placed again below: the current chat's in the thread, others' in the tray.
  for (const el of document.querySelectorAll("#asks .approval[data-chat]")) el.remove();
  state.approvals.clear();
  if (detachedWindow) for (const approval of hello.approvals || []) {
    if (approval.session !== detachedSession) state.approvals.set(approval.id, approval);
  }
  if (detachedWindow) for (const approval of detachedRead?.approvals || []) {
    state.approvals.set(approval.id, { ...approval, session: detachedSession });
  }
  sidebar.setAttention([...state.approvals.values()]);
  if (!detachedWindow && hello.running) {
    thread.begin(hello.running);
    setRunning(true);
    // Rebuild the running turn from its events so far. Open questions come from
    // `approvals` below, so the replay skips them.
    for (const m of hello.live || []) {
      if (m.kind === "text" || m.kind === "event") onEvent(m);
    }
  } else if (!detachedWindow) setRunning(false);
  if (!detachedWindow) for (const a of hello.approvals || []) onEvent({ kind: "approval", ...a });
  waiting.refresh();
}

async function start() {
  await extensionsReady();
  hydrateIcons();
  for (const [id, name] of [["reveal-sidebar", "sidebar"], ["toggle-pane", "pane"], ["close-pane", "x"]]) $(`#${id}`).append(icon(name));
  appEl.dataset.tabs = "0";
  try {
    const s = localStorage.getItem("ark-sidebar");
    if (s) appEl.dataset.sidebar = s;
  } catch {}
  appEl.dataset.shell = transport.shell;
  appEl.dataset.os = os;
  appEl.dataset.window = detachedWindow ? "detached" : "main";
  relabel(document.body);
  if (transport.shell === "desktop") appLike();
  try {
    const restored = JSON.parse(localStorage.getItem(tabStorageKey) || "null");
    if (restored?.version === TAB_STORAGE_VERSION && Array.isArray(restored.ids)) {
      chatTabs.ids = restored.ids.filter((id) => typeof id === "string").slice(-12);
    }
  } catch {}
  if (detachedSession && !chatTabs.ids.includes(detachedSession)) chatTabs.ids.push(detachedSession);
  const primaryElements = createChatSurface({ kind: "primary" });
  primarySurface = primaryElements.surface;
  const workspace = h("div.chat-workspace", { id: "chat-workspace", "data-split": "false" }, primarySurface);
  $("#stage").before(workspace);
  primarySurface.addEventListener("pointerdown", () => {
    if (visibleChats(chatLayout).length < 2) return;
    focusChat(state.sessionId, false);
  }, true);
  thread = new Thread($("#thread"), $("#scroller"), api);
  composer = new Composer(api, { ...primaryElements, captureDraft });
  places = new Places(api);
  acceptDrops(api, captureDraft, $(".main"), {
    accepts: () => !state.view || !!views.get(state.view)?.ctl?.captureUploadOrigin,
    captureOrigin: () => {
      if (state.view) return views.get(state.view)?.ctl?.captureUploadOrigin?.();
      const target = api.at(currentLocation());
      return { uploadApi: target, appendDraft: captureDraft(target.location), attachImages: true };
    },
  });
  sidebar = new Sidebar(api, {
    runtime: () => state.runtime,
    hasAgents: hasAgentView,
    loadAgents: () => transport.call("agents", { all: true }),
    loadGroups: () => groupView() ? transport.call("groups") : Promise.resolve([]),
    assistant: () => registry.newAgent?.assistant !== false,
    arkName,
    arkFace,
    arkFaceKey: () => state.ui.logo || "",
    open: (entry, options) => openEntry(entry, options),
    newChat: () => api.newSession(),
    newAgent: () => newAgent(),
    settled: (entries) => void agentsSettled(entries),
    newChatWith,
    history: (entry) => openHistory(entry),
    openView: (id) => {
      if (appEl.dataset.compact === "true") appEl.dataset.drawer = "closed";
      void showView(id);
    },
  });
  sidebarGrip(appEl, $("#sidebar-grip"), toggleSidebar, toast);
  waiting = new Waiting(api, transport, () => state.sessionId);
  topbar = new Topbar(api, sidebar);
  identity = new Identity(api, chatIdentity, recentMenu);
  pane = new Pane(appEl, api, { conversation: () => {
    const entry = state.view && views.get(state.view);
    const liveConversation = entry?.ctl?.conversation?.();
    const location = currentLocation();
    if (rememberedConversation && (rememberedConversation.location?.runtime !== location.runtime || rememberedConversation.location?.project !== location.project || !rememberedConversation.surface?.isConnected)) rememberedConversation = null;
    const supplied = liveConversation || (registry.views.get(state.view)?.workspace ? rememberedConversation : null);
    const surface = supplied?.surface || secondaryChats.get(activeSession)?.elements.surface || primarySurface;
    return {
      surface, title: supplied?.title || activeSummary()?.session?.title || "Chat",
      location: supplied?.location || activeSummary()?.location, focus: supplied?.focus, attention: supplied?.attention || document.getElementById("asks"),
      mount: root => acceptDrops(api, captureDraft, root, {
        accepts: () => !state.view || !!views.get(state.view)?.ctl?.captureUploadOrigin,
        captureOrigin: () => state.view ? views.get(state.view)?.ctl?.captureUploadOrigin?.()
          : { uploadApi: api.at(currentLocation()), appendDraft: captureDraft(currentLocation()), attachImages: true },
      }),
    };
  } });
  fit();
  addEventListener("resize", fit);
  new MutationObserver(fit).observe(appEl, { attributes: true, attributeFilter: ["data-pane"] });
  // The drawer closes once something in it is picked, or on a click outside it.
  document.addEventListener("click", (e) => {
    if (appEl.dataset.drawer !== "open") return;
    const inside = e.target.closest(".sidebar");
    if (e.target.closest("#reveal-sidebar")) return;
    if (e.target.closest(".agents-retry")) return;
    if (!inside || e.target.closest("button, a")) appEl.dataset.drawer = "closed";
  });
  $("#search-chats").addEventListener("click", () => searchChats());
  $("#chat-split").addEventListener("click", () => {
    if (visibleChats(chatLayout).length > 1) return closeOtherPanes();
    const other = [...chatTabs.ids].reverse().find((id) => !visibleChats(chatLayout).includes(id));
    if (other) void splitInto(other, activeSession || state.sessionId, "right");
  });
  $("#reveal-sidebar").addEventListener("click", toggleSidebar);
  $("#toggle-pane").addEventListener("click", () => pane.toggle());
  $("#ctx-ring").addEventListener("click", () => runCommand("/context"));
  $("#session-title").addEventListener("dblclick", renameInline);
  document.addEventListener("keydown", keys);
  addEventListener("ark:menu-key", menuKey);
  transport.subscribe(receive);
  // Prove the page graph and first usable shell before waiting on runtime or store data. A slow
  // runtime is not evidence that a downloaded interface is broken.
  requestAnimationFrame(() => {
    void transport.pageUpdates?.ready(document.documentElement.dataset.arkAssets || "embedded").then((outcome) => {
      if (["applied", "rolled_back"].includes(outcome?.state)) {
        const key = `ark-page-update-outcome:${outcome.state}:${outcome.failed || outcome.current}`;
        if (sessionStorage.getItem(key) !== "shown") {
          sessionStorage.setItem(key, "shown");
          toast(outcome.state === "applied" ? "Interface update applied" : "The previous interface was restored");
        }
      }
    }).catch(() => {});
  });
  await boot();
  try {
    const sessions = await transport.call("sessions", { project: state.summary?.project?.id });
    const available = new Set((sessions || []).map((session) => session.id));
    available.add(state.sessionId);
    chatTabs.ids = chatTabs.ids.filter((id) => available.has(id));
    for (const session of sessions || []) {
      if (chatTabs.ids.includes(session.id)) chatTabs.titles.set(session.id, session.title || "Untitled chat");
    }
    await restoreVisibleChats([...available]);
    renderChatTabs();
  } catch {}
  try {
    const path = fileRecovery(currentLocation());
    if (path) api.openFile(path, { recover: true });
  } catch {}
  composer.input.addEventListener("input", () => saveDraft());
  const primaryScroller = primarySurface.querySelector("#scroller");
  primaryScroller.addEventListener("scroll", () => saveScroll(state.sessionId, primaryScroller), { passive: true });
  (secondaryChats.get(activeSession)?.composer || composer).focus();
  requestAnimationFrame(() => appEl.classList.add("ready"));
}

// App scripts are modules too and run after this one; start once they have registered.
if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
else start();
