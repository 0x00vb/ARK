// Boot and glue. Connects to the host, installs `window.ark` for app scripts, then wires the
// conversation, composer, sidebar and side pane to the host's event stream.

import { connect } from "./transport.js";
import { h, icon, clear, $, hydrateIcons, tokens } from "./dom.js";
import { markdown } from "./md.js";
import { Thread, approvalCard } from "./conversation.js";
import { Composer } from "./composer.js";
import { Sidebar } from "./sidebar.js";
import { Topbar } from "./topbar.js";
import { SETTINGS, settingsView } from "./settings.js";
import { Pane, invocationView, artifactView } from "./pane.js";
import { registry, installApi, dispatchExt, presenterFor } from "./ext.js";
import { registerBuiltins, BUILTINS } from "./workbench.js";
import { registerPlugins } from "./plugins.js";

const transport = connect();
const appEl = $("#app");
const state = { summary: null, ui: {}, sessionId: null, running: false, approvals: new Map(), extChanged: false, view: null };
// Main-column views (`ark.view`), built on first show and kept while hidden.
const views = new Map();

// ---- the api views and app scripts use ------------------------------------------------------

const api = {
  h, icon, markdown,
  call: (method, params) => transport.call(method, params),
  ext: (name, params = {}) => transport.call("ext", { name, params }),
  runAction: (action, input = {}, approved = false) => transport.call("run_action", { action, input, approved }),
  setFocus: (focus) => transport.call("focus", { focus }).catch(() => {}),
  // The system's folder picker in the window: a path, or null when dismissed. Undefined
  // where there is none (a browser), so the caller asks for a typed path instead.
  pickFolder: ({ title } = {}) => transport.pickFolder(title),
  summary: () => state.summary,
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
    const key = `inv:${id}`;
    pane.openTab(key, `inv:${String(id).replace(/^inv:/, "").slice(0, 6)}`, "file", (el) => invocationView(el, api, id));
  },
  openArtifact: (id, viewerId, params) => {
    const v = registry.viewers.find((x) => x.id === viewerId);
    const prm = params === undefined && viewerId ? { viewer: viewerId } : params && { ...params, viewer: viewerId };
    pane.openTab(`art:${id}`, "Artifact", "file", (el, p) => {
      const ctl = artifactView(el, api, id, viewerId, p);
      api.call("artifact", { id, length: 1 }).then((r) => retitle(`art:${id}`, r.artifact.name)).catch(() => {});
      return ctl;
    }, prm, { wide: v ? !!v.wide : registry.viewers.some((x) => x.wide), fill: true });
  },
  openPanel(id, params) {
    const p = registry.panels.get(id);
    if (!p) return toast(`No panel named ${id}`);
    pane.openTab(`panel:${id}`, p.title, p.icon || "box", (el, prm) => p.render(el, prm, window.ark) || {}, params, { wide: !!p.wide, fill: !!p.fill });
  },
  setPanelTitle: (id, title) => retitle(`panel:${id}`, title),
  closePanel: (id) => pane.remove(`panel:${id}`),
  toast,
  popover,
  refreshSidebar: () => sidebar.refresh(state.sessionId),
  // Ask the window to reflect a view's focus again (after its selection changed).
  reportFocus: () => reportFocus(),
  _changed: () => {
    state.extChanged = true;
    queueMicrotask(() => {
      if (!state.extChanged) return;
      state.extChanged = false;
      sidebar?.renderExtensions();
      topbar?.render();
    });
  },
};

// Terminal, Browser and Files first, so an app's panel with the same id replaces one.
registerBuiltins(installApi(api));

let thread, composer, sidebar, topbar, pane;

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
  if (above) el.style.bottom = `${window.innerHeight - r.top + 6}px`;
  else el.style.top = `${r.bottom + 6}px`;
  requestAnimationFrame(() => el.classList.add("open"));
  const onDown = (e) => { if (!el.contains(e.target) && e.target !== anchor) close(); };
  const onKey = (e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } };
  function close() {
    document.removeEventListener("mousedown", onDown, true);
    document.removeEventListener("keydown", onKey, true);
    el.classList.remove("open");
    setTimeout(() => el.remove(), 140);
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

async function change(method, params) {
  if (state.running) return toast("The agent is working. Stop it or wait for the turn to finish.");
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
}

// ---- session display -------------------------------------------------------------------------

function applySummary(s) {
  const changed = s.session.id !== state.sessionId;
  state.summary = s;
  state.sessionId = s.session.id;
  state.title = s.session.title || (s.session.turns ? "Untitled chat" : "");
  showTitle();
  composer.setModel(s.model);
  document.title = s.session.title ? `${s.session.title} · ${s.name}` : s.name;
  return changed;
}

async function loadSession() {
  pane.reset();
  thread.clear();
  const turns = await transport.call("transcript", { limit: 30 }).catch(() => []);
  if (turns.length) thread.renderStored(turns, { hasMore: turns.length === 30 });
  else showWelcome();
  thread.scroll(true);
  sidebar.refresh(state.sessionId);
  refreshRing();
}

function showWelcome() {
  const w = state.ui.welcome || {};
  thread.welcome(
    { title: w.title, subtitle: w.subtitle || state.summary?.tagline, logo: state.ui.logo },
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
  clear(el).append(h("span", "Disconnected from the agent. "), h("button.link", { onclick: () => { el.dataset.show = "false"; delete el.dataset.persist; msg.retry(); } }, "Retry"));
  el.dataset.persist = "";
  el.dataset.show = "true";
}

function onEvent(msg) {
  if (msg.kind === "reconnecting" || msg.kind === "disconnected" || msg.kind === "connected") {
    dispatchExt(`ark:${msg.kind}`, msg);
    return connection(msg);
  }
  // App scripts can follow the same stream: `ark.on("ark:turn_finished", fn)`.
  if (msg.kind !== "ext" && msg.kind !== "text") dispatchExt(`ark:${msg.kind}`, msg);
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
      if (applySummary(msg.summary)) loadSession();
      else sidebar.refresh(state.sessionId);
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
  $("#asks").append(approvalCard(msg, decide));
}

async function decide(id, decision) {
  try {
    await transport.call("approve", { id, decision });
  } catch (e) {
    toast(e.message);
  }
}

function present(d) {
  const by = presenterFor(d.view);
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
  $("#session-title").textContent = !v ? state.title || "" : v.nav ? state.summary?.project?.name || "" : v.title;
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

function palette() {
  const overlay = $("#overlay");
  if (!overlay.hidden) return closePalette();
  const input = h("input.palette-input", { placeholder: "Search commands, views and actions", "aria-label": "Search" });
  const list = h("div.palette-list", { role: "listbox" });
  const items = [
    ...[...registry.views.values()].map((v) => ({ label: v.title, hint: "view", run: () => showView(v.id) })),
    ...(registry.settings.size ? [{ label: "Settings", hint: "⌘,", run: () => api.openSettings() }] : []),
    ...[...registry.panels.values()].map((p) => ({ label: p.title, hint: "panel", run: () => api.openPanel(p.id) })),
    ...registry.actions.filter((a) => !a.where || a.where === "palette").map((a) => ({ label: a.label, hint: "action", run: () => a.run({}, window.ark) })),
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
  requestAnimationFrame(() => { overlay.classList.add("open"); input.focus(); });
}

function closePalette() {
  const overlay = $("#overlay");
  overlay.classList.remove("open");
  setTimeout(() => (overlay.hidden = true), 120);
  composer.focus();
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
    const pending = [...state.approvals.values()].filter((m) => !m.request?.agent).map((m) => m.id).pop();
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

async function bootOnce(again) {
  let hello;
  try {
    hello = await transport.call("hello");
  } catch (e) {
    clear($("#thread")).append(h("div.welcome", h("h1", "Not connected"), h("p", e.message)));
    return;
  }
  feed.seq = hello.seq || 0;
  state.ui = hello.ui || {};
  state.commands = hello.commands || [];
  composer.commands = state.commands;
  if (state.ui.accent) document.documentElement.style.setProperty("--accent", state.ui.accent);
  applySummary(hello.summary);
  composer.placeholder = `Ask ${hello.summary.name} anything`;
  if (!again) {
    sidebar.renderExtensions();
    topbar.render();
    // Settings › Tool servers, when the app has plugins.
    registerPlugins(window.ark);
  }
  await loadSession();
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
}

async function start() {
  hydrateIcons();
  for (const [id, name] of [["toggle-sidebar", "sidebar"], ["reveal-sidebar", "sidebar"], ["toggle-pane", "pane"], ["close-pane", "x"]]) $(`#${id}`).append(icon(name));
  appEl.dataset.tabs = "0";
  try {
    const s = localStorage.getItem("ark-sidebar");
    if (s) appEl.dataset.sidebar = s;
  } catch {}
  appEl.dataset.shell = transport.kind;
  thread = new Thread($("#thread"), $("#scroller"), api);
  composer = new Composer(api);
  sidebar = new Sidebar(api);
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
