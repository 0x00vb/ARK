// Boot and glue. Connects to the host, installs `window.ark` for app scripts, then wires the
// conversation, composer, sidebar and side pane to the host's event stream.

import { connect } from "./transport.js";
import { h, icon, clear, $, hydrateIcons, tokens, ago } from "./dom.js";
import { markdown } from "./md.js";
import { Thread, approvalCard } from "./conversation.js";
import { Composer } from "./composer.js";
import { createChatSurface } from "./chat-surface.js";
import { chatView } from "./chat-view.js";
import { acceptDrops } from "./drop.js";
import { Sidebar } from "./sidebar.js";
import { Destinations } from "./destinations.js";
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
import { registerBoard } from "./board.js";
import { registerSchedules, schedulesFollow } from "./schedules.js";
import { registerCalendar } from "./calendar.js";
import { Waiting } from "./waiting.js";
import { registerDevices, keepInStep, openItems, deviceName } from "./devices.js";
import { os, label, relabel } from "./keys.js";
import { Places } from "./places.js";
import { registerComputers } from "./computers.js";
import { MAX_VISIBLE_CHATS, chatLeaf, visibleChats, splitChat as splitLayoutChat, removeChat as removeLayoutChat, replaceChat, updateRatio, restoreLayout } from "./chat-layout.js";

const transport = connect();
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
const state = { summary: null, ui: {}, runtime: null, sessionId: null, running: false, approvals: new Map(), working: [], online: true, connectionEpoch: 0, extChanged: false, projectSectionsChanged: false, view: null, homeView: null };
const secondaryChats = new Map();
let activeSession = null;
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
  fileSnapshots: () => !!state.fileSnapshots,
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
  chatView: (el, options) => chatView(el, window.ark || api, options),
  // The questions waiting for an answer, as `ark:approval` brought them (also those asked
  // before the app's code ran).
  approvals: () => [...state.approvals.values()],
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
  compose: (text, { append = false } = {}) => {
    const draft = append ? composer.input.value : "";
    showConversation();
    composer.set(draft ? `${draft}\n\n${text}` : text);
  },
  submit: (text) => submit(text),
  steer,
  cancel: () => transport.call("cancel").catch(() => {}),
  command: (line) => (line.endsWith(" ") ? composer.set(line) : runCommand(line)),
  newSession: (project) => {
    showConversation();
    if (detachedWindow) return newWindowSession(project);
    return change("new_session", project ? { project } : undefined);
  },
  // A code to sign in another device (Settings › Devices, ADR 0047).
  pairCode: () => transport.pairCode(),
  openSession: (id) => {
    return openChatTab(id);
  },
  openView: (id, params) => showView(id, params),
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
      wide: !!p.wide, fill: !!p.fill, api: ark, projectScoped: !!p.projectScoped, closesWithProject: !!p.closesWithProject, project: project?.id,
    });
  },
  setPanelTitle: (id, title) => {
    const p = registry.panels.get(id);
    const location = p?.projectScoped ? { ...currentLocation(), session: "" } : currentLocation();
    retitle(resourceKey("panel", location, id), title);
  },
  closePanel: (id) => {
    const p = registry.panels.get(id);
    const location = p?.projectScoped ? { ...currentLocation(), session: "" } : currentLocation();
    pane.remove(resourceKey("panel", location, id));
  },
  toast,
  popover,
  refreshSidebar: () => sidebar.refresh(state.sessionId),
  refreshNavigation: () => topbar?.render(),
  refreshModel: () => composer.setModel(state.summary?.model),
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
  _changed: (projectSections = false) => {
    state.extChanged = true;
    state.projectSectionsChanged ||= projectSections;
    queueMicrotask(() => {
      if (!state.extChanged) return;
      state.extChanged = false;
      sidebar?.renderExtensions();
      if (state.projectSectionsChanged) {
        state.projectSectionsChanged = false;
        sidebar?.refresh();
      }
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
  file_snapshot_open: ["file", "file_snapshot_open"], file_snapshot_status: ["file", "file_snapshot_status"],
  file_snapshot_cancel: ["file", "file_snapshot_cancel"], file_snapshot_chunk: ["file", "file_snapshot_chunk"],
  file_snapshot_close: ["file", "file_snapshot_close"], file_probe: ["file", "file_probe"],
  upload_parts: ["file", "upload_parts"], upload_add: ["file", "upload_add"], upload_cancel: ["file", "upload_cancel"], file_open: ["file", "file_open"],
  workspace: ["project", "workspace"], state: ["project", "state"],
  graph: ["project", "graph"], task_trace: ["project", "task_trace"], task: ["project", "task"], task_call: ["project", "task_call"], orch_events: ["project", "orch_events"],
  agents: ["project", "agents"], spawn_agent: ["project", "spawn_agent"], submit_task: ["project", "submit_task"],
  cancel_task: ["project", "cancel_task"], retry_task: ["project", "retry_task"],
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

function scopedApi(location) {
  // Consumer capabilities may be installed or replaced after boot (the Desktop Browser view is
  // one). A resource tab snapshots its location, not the API object's early boot values.
  const scoped = Object.create(window.ark || api);
  scoped.location = { ...location };
  scoped.remoteResource = location.runtime !== state.runtime;
  scoped.destinationBrowser = scoped.remoteResource || transport.kind === "browser";
  scoped.online = () => resourceOnline.get(locationKey(location)) !== false;
  if (scoped.destinationBrowser) scoped.browserView = undefined;
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
  // A project panel has no chat of its own: its focus goes to the chat the window shows, when
  // that chat is in the panel's project.
  scoped.setFocus = (focus) => {
    const chat = activeSummary()?.location;
    const at = location.session ? location : chat?.project === location.project ? chat : null;
    if (!at?.session) return Promise.resolve();
    return api.resource(api.resourceRef("project", "", at), "focus", { focus }).catch(() => {});
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
    if (panel.projectScoped) return api.openPanel(id, params);
    pane.openTab(resourceKey("panel", location, id), panel.title, panel.icon || "box", (el, prm) => panel.render(el, prm, scoped) || {}, params, {
      wide: !!panel.wide, fill: !!panel.fill, api: scoped, closesWithProject: !!panel.closesWithProject, project: location.project,
    });
  };
  scoped.setPanelTitle = (id, title) => retitle(resourceKey("panel", location, id), title);
  scoped.closePanel = (id) => pane.remove(resourceKey("panel", location, id));
  return scoped;
}

// Terminal, Browser and Files first, so an app's panel with the same id replaces one.
registerBuiltins(installApi(api));

let thread, composer, sidebar, destinations, topbar, pane, waiting, places;
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
  return text => {
    const key = locationKey(location);
    const target = locationKey(state.summary?.location) === key ? composer
      : [...secondaryChats.values()].find(chat => locationKey(chat.summary.location) === key)?.composer;
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
      showConversation(false);
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
  showConversation(false);
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
    else await api.newSession();
  }
  if (!activeSession || activeSession === id) focusChat(state.sessionId, false);
  renderChatTabs();
}

function renderChatTabs() {
  const bar = $("#chat-tabs");
  const list = $("#chat-tab-list");
  if (!bar || !list) return;
  bar.hidden = chatTabs.ids.length < 2 && visibleChats(chatLayout).length < 2;
  clear(list);
  const active = activeSession || state.sessionId;
  let selectedItem = null;
  for (const id of chatTabs.ids) {
    const title = chatTabs.titles.get(id) || "Untitled chat";
    const selected = id === active;
    const close = h("button.chat-tab-close", { type: "button", title: `Close ${title}`, "aria-label": `Close ${title}` }, icon("x"));
    close.addEventListener("click", (event) => { event.stopPropagation(); void closeChatTab(id); });
    const open = (event) => event.altKey ? void splitInto(id, active, "right") : void openChatTab(id);
    const tab = h("button.chat-tab", {
      type: "button", role: "tab", "aria-selected": String(selected),
      title: selected ? `${title}\nDouble-click to rename` : title,
      onclick: open,
      onkeydown: (event) => {
        if (event.key === "Tab" && !event.shiftKey) {
          event.preventDefault();
          close.focus();
          return;
        }
        if (event.key === "Enter" || event.key === " ") { event.preventDefault(); open(event); }
      },
    }, h("span.chat-tab-title", title));
    const item = h("div.chat-tab-item", { "data-session": id, "data-selected": String(selected), draggable: "true" }, tab, close);
    item.addEventListener("dragstart", (event) => beginTabDrag(event, id, item));
    item.addEventListener("dragend", (event) => void endTabDrag(event, id));
    item.addEventListener("dragover", (event) => {
      if (!draggedSession || draggedSession === id) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "move";
      item.dataset.drop = "true";
    });
    item.addEventListener("dragleave", () => delete item.dataset.drop);
    item.addEventListener("drop", (event) => {
      event.preventDefault();
      const from = draggedSession;
      delete item.dataset.drop;
      if (!from || from === id) return;
      const ordered = chatTabs.ids.filter((entry) => entry !== from);
      ordered.splice(ordered.indexOf(id), 0, from);
      chatTabs.ids = ordered;
      saveTabs();
      renderChatTabs();
    });
    tab.addEventListener("dblclick", (event) => {
      event.preventDefault();
      event.stopPropagation();
      renameChatTabInline(item, id, title);
    });
    list.append(item);
    if (selected) selectedItem = item;
  }
  requestAnimationFrame(() => selectedItem?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  const split = $("#chat-split");
  if (split) {
    const divided = visibleChats(chatLayout).length > 1;
    split.disabled = !divided && !chatTabs.ids.some((id) => !visibleChats(chatLayout).includes(id));
    split.setAttribute("aria-pressed", String(divided));
    split.title = divided ? "Close other panes" : "Split with another open chat";
    split.setAttribute("aria-label", split.title);
  }
}

function renameChatTabInline(item, id, before) {
  const active = activeSession || state.sessionId;
  const running = id === state.sessionId
    ? state.running
    : !!secondaryChats.get(id)?.running;
  if (item.querySelector("input") || id !== active || running) return;
  const input = h("input.chat-tab-rename", { value: before === "Untitled chat" ? "" : before, "aria-label": "Chat title" });
  clear(item).append(input);
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
    renderChatTabs();
    requestAnimationFrame(() => document.querySelector(`.chat-tab-item[data-session="${CSS.escape(id)}"] .chat-tab`)?.focus());
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
    this.api.submit = (text) => chat.submit(text);
    this.api.send = this.api.submit;
    this.api.steer = (text) => transport.call("chat_steer", { session: chat.session, text });
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
    else this.thread.welcome({ title: "New chat", subtitle: "" }, [], () => {});
    if (read.running) {
      this.thread.begin(read.running);
      this.setRunning(true);
      for (const message of read.live || []) this.handle(message);
    }
    for (const approval of read.approvals || []) this.handle({ kind: "approval", ...approval });
    elements.surface.addEventListener("pointerdown", () => this.focus(false), true);
    this.composer.input.value = savedDraft(this.session);
    this.composer.changed();
    this.composer.input.addEventListener("input", () => saveDraft(this.session, this.composer.input.value));
    elements.scroller.addEventListener("scroll", () => saveScroll(this.session, elements.scroller), { passive: true });
    restoreScroll(this.session, elements.scroller);
  }

  applySummary(summary) {
    this.summary = summary;
    this.composer.setModel(summary.model);
    this.composer.setExecutionMode(summary.execution_mode || "manual", summary.execution_mode_revision || 0);
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

  async submit(text) {
    text = String(text || "").trim();
    if (!text || this.running) return;
    this.thread.begin(text).local = true;
    this.setRunning(true);
    try {
      await transport.call("chat_send", {
        session: this.session,
        text,
        model: this.summary.model,
        execution_mode: this.composer.executionMode || "manual",
        execution_mode_revision: this.composer.executionModeRevision || 0,
      });
    } catch (error) {
      this.thread.finish({ ok: false, error: error.message });
      this.setRunning(false);
      this.composer.set(text);
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
      case "text": this.thread.event(message); break;
      case "event":
        this.thread.event(message);
        if (message.event?.type === "steered") this.composer.delivered(message.event.text);
        else if (message.event?.type === "present") present(message.event.directive, this.api);
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
        if (next) setTimeout(() => this.submit(next), 120);
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
    saveDraft(this.session, this.composer.input.value);
    saveScroll(this.session, this.elements.scroller);
    this.elements.surface.remove();
  }
}

function focusChat(session = state.sessionId, focusComposer = true) {
  showConversation(false);
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
  const read = await transport.call("chat_read", { session: id, model: activeSummary()?.model, limit: 30 });
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
  focusChat(id, false);
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
  // A slash word that is not a command in this chat is ordinary message text. In particular,
  // selecting another computer must never make a hidden command mutate the local empty chat.
  if (text.startsWith("/") && (viewForCommand(text, api.project()) || composer.commandAvailable(text))) return runCommand(text);
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
    await transport.call("send", {
      text,
      execution_mode: composer.executionMode || "manual",
      execution_mode_revision: composer.executionModeRevision || 0,
    });
  } catch (e) {
    toast(e.message);
    thread.finish({ ok: false, error: e.message });
    setRunning(false);
    composer.set(text);
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

// Moving to another chat never waits: a chat that is working goes on in the background.
async function change(method, params) {
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
  appEl.dataset.running = String(on);
  places?.show(state.summary, on);
}

// ---- session display -------------------------------------------------------------------------

function applySummary(s) {
  const before = state.sessionId;
  if (state.sessionId && s.session.id !== state.sessionId) saveDraft();
  const changed = s.session.id !== state.sessionId;
  const projectChanged = s.project?.id !== state.summary?.project?.id;
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
  if (projectChanged && state.view) {
    const view = registry.views.get(state.view);
    if (view?.projectScoped) views.get(state.view)?.ctl?.update?.({ project: s.project });
  }
  composer.setModel(s.model);
  composer.setExecutionMode(s.execution_mode || "manual", s.execution_mode_revision || 0);
  rememberTab(s);
  if (changed) {
    // Promotion disposes the secondary composer and saves its latest draft first.
    applyPrimaryChange(before, state.sessionId);
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
  schedulesFollow();
  restoreScroll(state.sessionId, $("#scroller"));
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
    case "orch":
      if (msg.event?.type === "task_created") sidebar.refresh(state.sessionId);
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
  const by = presenterFor(d.view);
  // A file opens in a tab of its own, unless the app shows files itself.
  if (d.view === "file" && d.path && (!by?.panel || by.panel.builtin)) return target.openFile(d.path, d);
  if (by?.panel) return target.openPanel(by.panel.id, d);
  if (by?.viewer && d.artifact) return target.openArtifact(d.artifact, by.viewer.id, d);
  if (d.artifact) return target.openArtifact(d.artifact, undefined, d);
  if (d.view === "evidence" && d.locator) return target.openInvocation(d.locator);
  toast(`The agent asked to show ${d.view === "split" ? "a split view" : `a ${d.view}`}, which this window cannot show.`);
}

// ---- views in the main column ---------------------------------------------------------------

/// Show an app's view in place of the conversation. A view is built once; showing it again
/// passes new params to `update`, or calls `refresh`.
function showView(id, params) {
  const v = id === SETTINGS ? settingsView : registry.views.get(id);
  if (!v) return toast(`No view named ${id}`);
  // A project view owns the full main column. End the chat layout before showing it so the
  // top selector always remains a visible route back to Chat.
  if (visibleChats(chatLayout).length > 1) closeOtherPanes();
  if (v.projectScoped && params === undefined) params = { project: api.project() };
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
  if (id !== "schedules" && id !== SETTINGS) state.homeView = id;
  appEl.dataset.view = id;
  $("#stage").hidden = false;
  showTitle();
  sidebar.markView(id);
  topbar.mark(id);
  destinations?.mark(id === "schedules" ? "schedules" : null);
  reportFocus();
}

function showConversation(focusComposer = true) {
  state.homeView = null;
  if (!state.view) {
    destinations?.mark("home");
    return;
  }
  state.view = null;
  delete appEl.dataset.view;
  $("#stage").hidden = true;
  showTitle();
  sidebar.markView(null);
  topbar.mark(null);
  destinations?.mark("home");
  reportFocus();
  if (focusComposer) composer.focus();
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

function destinationTarget(destination) {
  if (destination === "schedules") return views.get("schedules")?.el.querySelector("[data-destination-heading]") || $("#session-title");
  const entry = state.view && views.get(state.view);
  const visibleChatTitle = state.view || $("#chat-tabs").hidden
    ? $("#session-title")
    : document.querySelector('.chat-tab[aria-selected="true"]');
  return entry?.el.querySelector("[data-destination-heading], h1, [role='heading']") || visibleChatTitle;
}

/// A keyboard jump is instant and moves focus to the destination. A pointer jump leaves focus on
/// its button and gives the new surface one quiet, compositor-only arrival.
function openDestination(destination, { keyboard = false } = {}) {
  if (appEl.dataset.compact === "true") appEl.dataset.drawer = "closed";
  if (destination === "schedules") showView("schedules");
  else showConversation(false);
  const target = destinationTarget(destination);
  if (keyboard) {
    target?.focus({ preventScroll: true });
    return;
  }
  if (!matchMedia("(prefers-reduced-motion: reduce)").matches) {
    target?.closest(".view, .scroller")?.animate(
      [{ opacity: 0.72, transform: "translateY(6px)" }, { opacity: 1, transform: "translateY(0)" }],
      { duration: 160, easing: "cubic-bezier(0.23, 1, 0.32, 1)" },
    );
  }
}

/// The chat's title over the conversation; over a view of the project (whose tab says which),
/// the project's name; else the view's title (Settings).
function showTitle() {
  const v = state.view && (state.view === SETTINGS ? settingsView : registry.views.get(state.view));
  const el = $("#session-title");
  el.textContent = !v ? state.title || "" : v.nav === true ? state.summary?.project?.name || "" : v.title;
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
async function searchChats() {
  const overlay = $("#overlay");
  if (!overlay.hidden) return closePalette();
  const restore = document.activeElement;
  const generation = ++searchGeneration;
  const location = { ...currentLocation() };
  const searchRuntime = state.runtime;
  const input = h("input.palette-input", { placeholder: "Search chats and agents", "aria-label": "Search chats and agents", role: "combobox", "aria-autocomplete": "list", "aria-controls": "chat-search-results", "aria-expanded": "true" });
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
    const shown = items.filter((item) => !query || [item.title, item.description, item.project.name].some(text => text?.toLocaleLowerCase().includes(query))).slice(0, 100);
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
        : (items.length ? "No matching chats or agents" : "No chats or agents yet");
      list.append(h("div.palette-empty", message));
    }
    shown.forEach((item, index) => {
      const row = h("div.palette-item", { id: `chat-search-${index}`, role: "option", "aria-selected": String(index === selected) },
        h("span.palette-item-main", h("span", item.title || "Untitled"), h("span.palette-hint", [item.project.name, item.description].filter(Boolean).join(" · "))),
        h("span.palette-hint", item.source ? (item.source.label || "") : ago(item.session.updated_at)));
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
  document.addEventListener("contextmenu", (e) => {
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
  const mod = e.metaKey || e.ctrlKey;
  const key = e.key.toLowerCase();
  if (mod && Object.hasOwn(MENU_KEYS, key)) {
    e.preventDefault();
    return MENU_KEYS[key]();
  }
  if (mod && e.shiftKey && e.key.toLowerCase() === "f") { e.preventDefault(); return searchChats(); }
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
  const compact = w <= 760 || (w <= 1050 && appEl.dataset.pane === "open");
  appEl.dataset.compact = String(compact);
  const styles = getComputedStyle(appEl);
  const px = (name) => Number.parseFloat(styles.getPropertyValue(name)) || 0;
  const chrome = compact ? 0 : px("--rail") + px("--sb");
  const room = Math.max(320, w - chrome - px("--chat-min"));
  appEl.style.setProperty("--pw-room", `${room}px`);
  let paneWidth = 0;
  if (appEl.dataset.pane === "open") {
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
  for (const [id, name] of [["toggle-sidebar", "sidebar"], ["reveal-sidebar", "sidebar"], ["toggle-pane", "pane"], ["close-pane", "x"]]) $(`#${id}`).append(icon(name));
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
  acceptDrops(api, captureDraft, $(".main"));
  sidebar = new Sidebar(api);
  destinations = new Destinations(openDestination, (id) => showView(id));
  waiting = new Waiting(api, transport, () => state.sessionId);
  topbar = new Topbar(api, destinations);
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
  $("#search-chats").addEventListener("click", searchChats);
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
