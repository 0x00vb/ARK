// Who the person works with: ARK first, then the app's agents of every folder (ADR 0110). The
// first few are tiles, the rest rows. Everything comes from the host (`agents {all}`); nothing is
// kept here that could drift from the store, except the person's pins and the folders they hid.
// The foot holds the person (profile, which opens Settings) and the app's views as buttons.

import { h, icon, clear, ago, $, agentFace } from "./dom.js";
import { bindContextMenu, contextMenu } from "./context-menu.js";
import { registry } from "./ext.js";
import { SETTINGS } from "./settings.js";
import { label as keyLabel } from "./keys.js";

export const ARK = "ark";
const HIDDEN_PROJECTS = "ark-hidden-projects";
// ARK, then pins, then the most recent, until this many tiles; pins may take it up to MAX_TILES.
const TILES = 6;
const MAX_TILES = 9;
const COLUMNS = 3;
const LONG_PRESS = 500;
const WIDTH_KEY = "ark-sidebar-width";
export const WIDTH = { min: 200, max: 400, close: 150 };

export const agentKey = (project, name) => `${project}/${name}`;
export const pinStorageKey = (runtime) => `ark-pins:${runtime}`;

/// The folders the person removed from the sidebar earlier, id to name. Their agents get no
/// tile and their chats stay out of ARK's lists.
export function hiddenProjects() {
  try {
    const entries = JSON.parse(localStorage.getItem(HIDDEN_PROJECTS) || "[]");
    return new Map(Array.isArray(entries)
      ? entries.filter((entry) => entry && typeof entry.id === "string").map((entry) => [entry.id, entry.name || "Project"])
      : []);
  } catch {
    return new Map();
  }
}

function saveHiddenProjects(hidden) {
  try {
    localStorage.setItem(HIDDEN_PROJECTS, JSON.stringify([...hidden].map(([id, name]) => ({ id, name }))));
    return true;
  } catch {
    return false;
  }
}

/// Pinned agents of one runtime, in the order they were pinned: `[project, name]` pairs.
export function readPins(runtime) {
  if (!runtime) return [];
  try {
    const pins = JSON.parse(localStorage.getItem(pinStorageKey(runtime)) || "[]");
    return Array.isArray(pins) ? pins.filter((pin) => Array.isArray(pin) && pin.length === 2 && pin.every((part) => typeof part === "string" && part)) : [];
  } catch {
    return [];
  }
}

function writePins(runtime, pins) {
  try {
    if (pins.length) localStorage.setItem(pinStorageKey(runtime), JSON.stringify(pins));
    else localStorage.removeItem(pinStorageKey(runtime));
    return true;
  } catch {
    return false;
  }
}

const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

export class Sidebar {
  /// `nav` is the window's side of it: `runtime()`, `hasAgents()`, `loadAgents()`, `arkName()`,
  /// `arkFace(size)`, `open(entry, { newTab })` (false when it could not), `newChat()`,
  /// `newAgent()`, `newChatWith(entry)`, `history(entry)` and `openView(id, options)`.
  constructor(api, nav) {
    this.api = api;
    this.nav = nav;
    this.list = $("#agents");
    this.note = $("#agents-note");
    this.status = $("#status-ext");
    this.top = $("#sidebar-ext");
    this.settings = $("#open-settings");
    this.views = $("#sidebar-views");
    this.newButton = $("#new-agent");
    this.agents = null;
    this.failed = false;
    this.order = null;
    this.current = ARK;
    this.pending = null;
    this.working = new Set();
    this.attention = [];
    this.board = 0;
    this.online = true;
    this.hidden = hiddenProjects();
    this.items = new Map();
    this.gen = 0;
    this.clickGen = 0;
    this.drawn = false;
    this.placeholders = false;
    this.agentView = false;
    this.typed = { text: "", at: 0 };
    this.newButton.addEventListener("click", () => registry.newAgent ? nav.newAgent() : nav.newChat());
    this.settings.addEventListener("click", () => api.openSettings());
    this.list.addEventListener("keydown", (event) => this.keys(event));
    this.list.addEventListener("focusin", (event) => this.rove(event.target.closest(".agent-btn")));
    addEventListener("focus", () => {
      this.paintProfile();
      void this.refresh({ reorder: true });
    });
  }

  setWorking(list) {
    const ids = new Set(list.map((w) => w.session));
    const same = ids.size === this.working.size && [...ids].every((id) => this.working.has(id));
    this.working = ids;
    if (!same) this.draw();
  }

  setAttention(list) {
    this.attention = [...new Map(list.map((item) => [item.id, item])).values()];
    this.draw();
  }

  /// Board items of the shown folder that wait for the person: ARK's tile says so.
  setBoardWaiting(count) {
    if (count === this.board) return;
    this.board = count;
    this.draw();
  }

  setOnline(online) {
    this.online = online;
    this.list.dataset.offline = String(!online);
    this.draw();
  }

  /// The agent the window shows (`ARK` or `agentKey(project, name)`).
  markAgent(key) {
    this.current = key || ARK;
    if (this.pending === this.current) this.pending = null;
    this.paintCurrent();
  }

  markView(id) {
    this.view = id;
    this.settings.setAttribute("aria-current", String(id === SETTINGS));
    for (const button of this.views.children) {
      if (button.dataset.view === id) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
  }

  /// The app's views (`nav` views of the shown folder), as buttons beside the profile.
  setViews(views) {
    clear(this.views);
    for (const view of views) {
      const label = view.nav === true ? view.title : view.nav;
      this.views.append(h("button.icon-btn.sidebar-view", {
        type: "button", "data-view": view.id, title: label, "aria-label": label,
        onclick: () => this.nav.openView(view.id),
      }, icon(view.icon || "box")));
    }
    this.markView(this.view);
  }

  /// The person, from the app's `ark.profile`: their initial and name; else Settings.
  paintProfile() {
    let person = null;
    try { person = registry.profile?.() || null; } catch (e) { console.error(e); }
    const name = typeof person?.name === "string" && person.name.trim() ? person.name.trim() : "";
    const avatar = this.settings.querySelector(".account-avatar");
    avatar.dataset.empty = String(!name);
    avatar.replaceChildren(name ? [...name][0].toLocaleUpperCase() : icon("person"));
    this.settings.querySelector(".profile-name").textContent = name || "Settings";
    this.settings.setAttribute("aria-label", name ? `${name}, account and settings` : "Settings");
  }

  /// Reads the agents again. `reorder` puts them in order again (load, focus, the person's
  /// action); otherwise only marks change and nobody moves.
  async refresh(options) {
    const reorder = !!options?.reorder;
    const gen = ++this.gen;
    this.agentView = !!this.nav.hasAgents();
    if (!this.agentView) {
      this.failed = false;
      this.draw();
      return;
    }
    if (this.agents === null && !this.loading) {
      this.loading = setTimeout(() => {
        this.placeholders = this.agents === null && !this.failed;
        this.draw();
      }, 300);
      this.draw();
    }
    let rows;
    try {
      rows = await this.nav.loadAgents();
    } catch {
      if (gen !== this.gen) return;
      clearTimeout(this.loading);
      this.failed = true;
      this.placeholders = false;
      this.draw();
      return;
    }
    // Groups: several agents in one chat. A runtime without them lists none.
    const groups = await this.nav.loadGroups?.().catch(() => []);
    if (gen !== this.gen) return;
    clearTimeout(this.loading);
    this.placeholders = false;
    this.groups = Array.isArray(groups) ? groups.filter((g) => g && typeof g.id === "string") : [];
    const first = this.agents === null;
    this.failed = false;
    this.agents = Array.isArray(rows) ? rows.filter((row) => row && typeof row.name === "string" && typeof row.project === "string") : [];
    if (first || reorder) this.order = null;
    this.draw();
    this.nav.settled?.(this.arrange(this.entries()));
  }

  entries() {
    const name = this.nav.arkName() || "ARK";
    const ark = { key: ARK, ark: true, name, label: name };
    const shown = (this.agents || []).filter((agent) => !this.hidden.has(agent.project));
    const label = (agent) => agent.title || agent.name;
    const seen = new Map();
    for (const agent of shown) seen.set(label(agent).toLocaleLowerCase(), (seen.get(label(agent).toLocaleLowerCase()) || 0) + 1);
    const agents = shown.map((agent) => ({
      ...agent, look: typeof agent.look === "string" ? agent.look : "", key: agentKey(agent.project, agent.name), label: label(agent), twin: seen.get(label(agent).toLocaleLowerCase()) > 1,
    }));
    const looks = new Map(shown.map((agent) => [agentKey(agent.project, agent.name), agent.look || ""]));
    const groups = (this.groups || []).map((group) => ({
      key: `group:${group.id}`, group: true, id: group.id, name: group.name, label: group.name, updated_ms: group.updated_ms,
      members: (group.members || []).map((m) => ({ ...m, look: looks.get(agentKey(m.project, m.agent)) || "" })),
    }));
    // An app whose + makes agents may leave the built-in assistant out: only its agents show.
    return this.nav.assistant() ? [ark, ...agents, ...groups] : [...agents, ...groups];
  }

  pins() {
    return readPins(this.nav.runtime());
  }

  /// ARK, pins in pin order, then latest activity. Kept between reorders: an agent that
  /// appears meanwhile goes last.
  arrange(entries) {
    const byKey = new Map(entries.map((entry) => [entry.key, entry]));
    const pinned = this.pins().map(([project, name]) => agentKey(project, name)).filter((key) => byKey.has(key));
    if (!this.order) {
      const rest = entries.filter((entry) => !entry.ark && !pinned.includes(entry.key))
        .sort((a, b) => (b.updated_ms || 0) - (a.updated_ms || 0) || a.label.localeCompare(b.label));
      this.order = [...(byKey.has(ARK) ? [ARK] : []), ...pinned, ...rest.map((entry) => entry.key)];
    } else {
      this.order = this.order.filter((key) => byKey.has(key));
      for (const entry of entries) if (!this.order.includes(entry.key)) this.order.push(entry.key);
    }
    this.tiles = Math.min(this.order.length, Math.max(TILES, Math.min(MAX_TILES, (byKey.has(ARK) ? 1 : 0) + pinned.length)));
    return this.order.map((key) => byKey.get(key));
  }

  find(key) {
    return this.entries().find((entry) => entry.key === key) || null;
  }

  mark(entry) {
    if (!this.online) return null;
    if (entry.ark) {
      if (this.board > 0 || this.attention.some((item) => !item.request?.agent)) return "asking";
      return this.working.size ? "working" : null;
    }
    if (entry.asking || this.attention.some((item) => item.request?.agent === entry.name && (!item.project?.id || item.project.id === entry.project))) return "asking";
    return ["running", "waiting"].includes(entry.state) || entry.running?.length ? "working" : null;
  }

  draw() {
    const entries = this.arrange(this.entries());
    const focused = this.list.contains(document.activeElement) ? document.activeElement.closest(".agent-btn")?.dataset.key : null;
    const seen = new Set();
    let previous = null;
    entries.forEach((entry, index) => {
      let item = this.items.get(entry.key);
      const fresh = !item;
      if (fresh) {
        item = this.item(entry.key);
        this.items.set(entry.key, item);
      }
      this.paint(item, entry, index < this.tiles ? "tile" : "row");
      const next = previous ? previous.nextElementSibling : this.list.firstElementChild;
      if (next !== item) this.list.insertBefore(item, next);
      previous = item;
      if (fresh && this.drawn) this.enter(item);
      seen.add(entry.key);
    });
    for (const [key, item] of this.items) {
      if (seen.has(key)) continue;
      this.items.delete(key);
      this.leave(item);
    }
    for (const old of this.list.querySelectorAll(".agent-item.placeholder")) old.remove();
    if (this.placeholders && this.agents === null) {
      for (let i = 0; i < 3; i++) this.list.append(h("div.agent-item.placeholder", { "aria-hidden": "true", "data-kind": "tile" }, h("span.agent-face-box", h("span.agent-face-ghost")), h("span.agent-name-ghost")));
    }
    clear(this.note);
    this.note.hidden = !this.failed;
    if (this.failed) this.note.append(h("span", "Agents unavailable"), h("button.agents-retry", { type: "button", onclick: () => void this.refresh({ reorder: true }) }, "Try again"));
    this.drawn = entries.length > 1 || !this.agentView || this.agents !== null;
    this.paintCurrent();
    this.rove(focused ? this.button(focused) : null);
    if (focused) this.button(focused)?.focus({ preventScroll: true });
    $("#reveal-sidebar")?.toggleAttribute("data-attention", entries.some((entry) => this.mark(entry) === "asking"));
  }

  item(key) {
    const button = h("button.agent-btn", { type: "button", tabindex: "-1", "data-key": key });
    button.addEventListener("click", (event) => {
      if (button.dataset.pressed === "long") { delete button.dataset.pressed; return; }
      void this.open(key, event);
    });
    bindContextMenu(button, () => this.menu(key));
    // A long press on a touch screen opens the same menu.
    let timer = null;
    const cancel = () => clearTimeout(timer);
    button.addEventListener("pointerdown", (event) => {
      if (event.pointerType !== "touch") return;
      const { clientX, clientY } = event;
      timer = setTimeout(() => {
        button.dataset.pressed = "long";
        contextMenu({ type: "longpress", target: button, defaultPrevented: false, clientX, clientY, preventDefault() {}, stopPropagation() {} }, button, this.menu(key));
      }, LONG_PRESS);
    });
    for (const type of ["pointerup", "pointercancel", "pointerleave"]) button.addEventListener(type, cancel);
    return h("div.agent-item", { role: "listitem", "data-key": key }, button);
  }

  face(entry, size) {
    if (entry.group) {
      const small = Math.round(size * 0.7);
      return h("span.group-face", { style: { width: `${size}px`, height: `${size}px` } },
        ...entry.members.slice(0, 2).map((m) => agentFace(m.agent, small, m.look)));
    }
    return entry.ark ? this.nav.arkFace(size) : agentFace(entry.name, size, entry.look);
  }

  paint(item, entry, kind) {
    const button = item.firstElementChild;
    const mark = this.mark(entry);
    const sub = entry.twin ? entry.project_name : kind === "row" ? entry.preview : null;
    const end = kind === "row" ? (mark === "asking" ? "Needs you" : entry.updated_ms ? ago(entry.updated_ms) : "") : "";
    const signature = JSON.stringify([kind, entry.label, sub, end, mark, entry.ark ? this.nav.arkFaceKey?.() ?? "" : entry.group ? entry.members.map((m) => m.look) : entry.look]);
    item.dataset.kind = kind;
    button.dataset.name = entry.label.toLocaleLowerCase();
    if (button.dataset.signature !== signature) {
      button.dataset.signature = signature;
      button.replaceChildren(
        h("span.agent-face-box", this.face(entry, kind === "tile" ? 40 : 22),
          mark ? h("span.agent-mark", { "data-mark": mark }, mark === "asking" ? icon("alert") : null) : null),
        h("span.agent-text", h("span.agent-name", entry.label), sub ? h("span.agent-sub", sub) : null),
        end ? h("span.agent-end", { "data-mark": mark || null }, end) : "");
    }
    const words = [entry.label, entry.twin ? entry.project_name : null, mark === "asking" ? "needs you" : mark === "working" ? "working" : null];
    button.setAttribute("aria-label", words.filter(Boolean).join(", "));
    button.title = entry.twin ? `${entry.label}\n${entry.project_name}` : entry.label;
    button.setAttribute("aria-disabled", String(!this.online));
  }

  /// Faces drawn again (an app gave agents their own look).
  restyle() {
    for (const item of this.items.values()) delete item.firstElementChild.dataset.signature;
    this.draw();
  }

  button(key) {
    return this.items.get(key)?.firstElementChild || null;
  }

  buttons() {
    return [...this.items.values()].filter((item) => item.isConnected).sort((a, b) => a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1).map((item) => item.firstElementChild);
  }

  paintCurrent() {
    const current = this.pending || this.current;
    for (const [key, item] of this.items) {
      if (key === current) item.firstElementChild.setAttribute("aria-current", "true");
      else item.firstElementChild.removeAttribute("aria-current");
    }
  }

  /// One tab stop for the whole list: the focused agent, else the current one, else ARK.
  rove(target) {
    const buttons = this.buttons();
    const stop = target && buttons.includes(target) ? target : this.button(this.pending || this.current) || buttons[0];
    for (const button of buttons) button.tabIndex = button === stop ? 0 : -1;
  }

  enter(item) {
    if (reducedMotion()) return void item.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
    item.animate([{ opacity: 0, transform: "translateY(4px)" }, { opacity: 1, transform: "none" }], { duration: 200, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
  }

  leave(item) {
    item.dataset.leaving = "";
    item.inert = true;
    const done = () => item.remove();
    const animation = item.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 120, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
    animation.onfinish = done;
    animation.oncancel = done;
  }

  /// Opens an agent's latest chat. The tile is current at once; the latest click wins.
  async open(key, event) {
    if (!this.online) return;
    const entry = this.find(key);
    if (!entry) return;
    const gen = ++this.clickGen;
    this.pending = key;
    this.paintCurrent();
    let ok;
    try {
      ok = await this.nav.open(entry, { newTab: !!(event?.metaKey || event?.ctrlKey) });
    } catch {
      ok = false;
    }
    if (gen !== this.clickGen) return;
    this.pending = null;
    if (ok === false) this.api.toast(`Could not open ${entry.label}. Try again.`);
    this.paintCurrent();
  }

  /// The nth agent (⌘1 to ⌘9).
  openNth(index) {
    const key = this.buttons()[index]?.dataset.key;
    if (key) void this.open(key);
  }

  menu(key) {
    const entry = this.find(key);
    if (!entry || !this.online) return [];
    if (entry.group) {
      const ctx = { group: { id: entry.id, name: entry.label } };
      return registry.actions.filter((action) => action.where === "group" && (!action.when || action.when(ctx)))
        .map((action) => ({ label: action.label, run: () => action.run(ctx, window.ark) }));
    }
    const pinned = !entry.ark && this.pins().some(([project, name]) => project === entry.project && name === entry.name);
    return [
      !entry.program && { label: "New chat", run: () => this.nav.newChatWith(entry) },
      { label: "Chats", run: () => this.nav.history(entry) },
      !entry.ark && { label: pinned ? "Unpin" : "Pin", run: () => this.pin(entry, !pinned) },
      ...(entry.ark ? [] : this.agentActions(entry)),
    ].filter(Boolean);
  }

  /// The app's `where: "agent"` actions for one agent's tile.
  agentActions(entry) {
    const ctx = { agent: { name: entry.name, project: entry.project, title: entry.label } };
    return registry.actions
      .filter((action) => action.where === "agent" && (!action.when || action.when(ctx)))
      .map((action) => ({ label: action.label, run: () => action.run(ctx, window.ark) }));
  }

  pin(entry, on) {
    const runtime = this.nav.runtime();
    const pins = this.pins().filter(([project, name]) => project !== entry.project || name !== entry.name);
    if (on) pins.push([entry.project, entry.name]);
    if (!runtime || !writePins(runtime, pins)) return this.api.toast("Could not save the sidebar preference");
    this.order = null;
    this.draw();
  }

  keys(event) {
    if (event.altKey || event.metaKey || event.ctrlKey) return;
    const buttons = this.buttons();
    const index = buttons.indexOf(document.activeElement);
    if (index < 0) return;
    const tiles = this.tiles;
    const last = buttons.length - 1;
    let next = null;
    if (event.key === "ArrowRight") next = Math.min(last, index + 1);
    else if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowDown") {
      if (index >= tiles) next = Math.min(last, index + 1);
      else if (index + COLUMNS < tiles) next = index + COLUMNS;
      else next = tiles <= last ? tiles : Math.floor(index / COLUMNS) < Math.floor((tiles - 1) / COLUMNS) ? tiles - 1 : index;
    } else if (event.key === "ArrowUp") {
      if (index > tiles) next = index - 1;
      else if (index === tiles) next = tiles - 1;
      else next = index >= COLUMNS ? index - COLUMNS : index;
    } else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else if (event.key.length === 1 && event.key !== " ") {
      const now = performance.now();
      this.typed.text = now - this.typed.at > 700 ? event.key.toLocaleLowerCase() : this.typed.text + event.key.toLocaleLowerCase();
      this.typed.at = now;
      const text = this.typed.text;
      const repeat = [...text].every((c) => c === text[0]);
      const order = [...buttons.slice(index + 1), ...buttons.slice(0, index + 1)];
      const match = order.find((button) => button.dataset.name?.startsWith(text)) || (repeat ? order.find((button) => button.dataset.name?.startsWith(text[0])) : null);
      if (match) next = buttons.indexOf(match);
      event.stopPropagation();
    }
    if (next === null) return;
    event.preventDefault();
    this.rove(buttons[next]);
    buttons[next].focus();
  }

  /// Show a hidden folder's agents and chats again.
  restore(id) {
    const name = this.hidden.get(id) || "Project";
    this.hidden.delete(id);
    if (!saveHiddenProjects(this.hidden)) {
      this.hidden.set(id, name);
      this.api.toast("Could not save the sidebar preference");
      return;
    }
    this.order = null;
    this.draw();
    this.api.toast(`${name} is shown again`);
  }

  hiddenList() {
    return [...this.hidden];
  }

  /// The app's rows above the agents, its status items, the profile and what + makes. The
  /// profile appears once an app has registered a settings section.
  renderExtensions() {
    this.settings.hidden = !registry.settings.size;
    this.paintProfile();
    const words = registry.newAgent ? "New agent" : "New chat";
    this.newButton.setAttribute("aria-label", words);
    this.newButton.title = registry.newAgent ? words : `${words} (${keyLabel("⌘N")})`;
    clear(this.top);
    this.top.hidden = !registry.sidebars.length;
    for (const s of registry.sidebars) {
      const el = h("div.sidebar-item", { "data-id": s.id });
      this.top.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
    clear(this.status);
    for (const s of registry.statuses) {
      const el = h("div.status-item", { "data-id": s.id });
      this.status.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
    if (!!this.nav.hasAgents() !== this.agentView) void this.refresh({ reorder: true });
  }
}

/// The width the person dragged the sidebar to; 0 when none is saved or storage is blocked.
function readWidth() {
  try {
    return Number(localStorage.getItem(WIDTH_KEY)) || 0;
  } catch {
    return 0;
  }
}

/// The sidebar's trailing edge: drag to resize, drag past the minimum to hide, double click for
/// the default width, arrow keys by 16 px, Home and End to the ends. The width is remembered; `toggle()` shows and hides.
export function sidebarGrip(app, grip, toggle, toast) {
  const set = (px) => {
    const width = Math.round(Math.min(Math.max(px, WIDTH.min), WIDTH.max));
    app.style.setProperty("--project-width", `${width}px`);
    grip.setAttribute("aria-valuenow", String(width));
    return width;
  };
  const save = (width) => {
    try {
      if (width) localStorage.setItem(WIDTH_KEY, String(width));
      else localStorage.removeItem(WIDTH_KEY);
    } catch {
      toast("Could not save the sidebar width");
    }
  };
  grip.setAttribute("aria-valuemin", String(WIDTH.min));
  grip.setAttribute("aria-valuemax", String(WIDTH.max));
  const current = () => String(Math.round(app.querySelector(".sidebar").getBoundingClientRect().width));
  const stored = readWidth();
  if (stored > 0) set(stored);
  else grip.setAttribute("aria-valuenow", current());
  grip.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    grip.setPointerCapture(event.pointerId);
    app.classList.add("resizing");
    const before = app.querySelector(".sidebar").getBoundingClientRect().width;
    const from = before - event.clientX;
    let width = null;
    let closing = false;
    const gesture = new AbortController();
    const end = (released) => {
      gesture.abort();
      app.classList.remove("resizing");
      app.removeAttribute("data-sidebar-closing");
      if (!released || closing) set(before);
      if (released && closing) toggle();
      else if (released && width) save(width);
      dispatchEvent(new Event("resize"));
    };
    grip.addEventListener("pointermove", (m) => {
      const want = from + m.clientX;
      closing = want < WIDTH.close;
      app.toggleAttribute("data-sidebar-closing", closing);
      if (!closing) width = set(want);
    }, { signal: gesture.signal });
    grip.addEventListener("pointerup", () => end(true), { signal: gesture.signal });
    grip.addEventListener("pointercancel", () => end(false), { signal: gesture.signal });
  });
  grip.addEventListener("dblclick", () => {
    app.style.removeProperty("--project-width");
    save(null);
    dispatchEvent(new Event("resize"));
    requestAnimationFrame(() => grip.setAttribute("aria-valuenow", current()));
  });
  grip.addEventListener("keydown", (event) => {
    const now = Number(grip.getAttribute("aria-valuenow")) || app.querySelector(".sidebar").getBoundingClientRect().width;
    const want = { ArrowRight: now + 16, ArrowLeft: now - 16, Home: WIDTH.min, End: WIDTH.max }[event.key];
    if (want === undefined) return;
    event.preventDefault();
    save(set(want));
    dispatchEvent(new Event("resize"));
  });
}
