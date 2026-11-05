// Work: say what should get done, then follow it. The page is the composer and, under it, the
// work that is open, grouped by what it waits on. "View all" moves the composer up and shows the
// project's board. Opening a piece of work opens its conversation as a normal chat.

import { h, icon } from "./dom.js";
import { bindContextMenu } from "./context-menu.js";
import { boardView, statusMark, avatar } from "./board.js";
import { createChatSurface } from "./chat-surface.js";
import { Composer } from "./composer.js";

const PERSON = "user";
const modes = new Map();
const placeKey = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
const GROUPS = [
  { id: "attention", label: "Needs you" },
  { id: "working", label: "Working" },
  { id: "waiting", label: "Waiting" },
  { id: "todo", label: "To do" },
];

/// Open a work item's conversation in the chat.
export async function openWork(api, work) {
  const data = await api.call("work_get", { work, receipt: null });
  const conversation = data.item.conversation_missing ? null : data.item.conversation;
  if (!conversation) throw new Error("This work has not started a conversation yet.");
  return api.openSession(conversation);
}

function group(board, derived) {
  if (["attention", "failed", "interrupted"].includes(derived?.state) || board?.blocked && board.owner === PERSON && board.state !== "done") return "attention";
  if (derived?.state === "in_progress" || !derived && board?.state === "doing") return "working";
  if (derived?.state === "waiting") return "waiting";
  if (board?.state === "todo" && !derived) return "todo";
  return null;
}

function workHome(el, _params, api) {
  let alive = true, generation = 0, archived = false, sending = false, attempt = null;
  const key = placeKey(api);
  const mode = modes.get(key) || { all: false, target: null };
  modes.set(key, mode);
  el.classList.add("work-home");
  const status = h("p.work-status", { role: "status" });
  const list = h("div.work-list", { "aria-label": "Open work" });
  const boardEl = h("div.work-real-board", { "aria-label": "Tasks" });
  const toggle = h("button.work-view-all", { type: "button", onclick: () => show(!mode.all) });
  const context = { ark: api, boards: [], items: [], tasks: {}, agents: [], derived: new Map(), removing: new Set(), listeners: new Set(), changed(options = {}) { for (const listener of this.listeners) listener(options); }, load };

  const surface = createChatSurface({ kind: "work", capabilities: { files: false, model: false, mode: false } });
  const composerApi = Object.create(api);
  composerApi.submit = text => void start(text);
  composerApi.send = composerApi.submit;
  composerApi.steer = composerApi.submit;
  composerApi.cancel = () => {};
  const composer = new Composer(composerApi, surface);
  composer.placeholder = "What should get done?";
  surface.input.placeholder = composer.placeholder;
  surface.input.setAttribute("aria-label", "New work");
  const who = h("button.chip.work-who", { type: "button", "aria-haspopup": "menu", onclick: () => pick() });
  surface.controls.append(who);
  const dock = surface.surface.querySelector(".dock");
  const top = h("div.work-top", dock, h("div.work-bar", toggle));
  const archive = h("button.btn.ghost.work-archive", { type: "button", "aria-pressed": "false", onclick: () => { archived = !archived; archive.setAttribute("aria-pressed", String(archived)); void load(); } }, "Archived");
  el.append(top, status, list, boardEl);
  const controller = boardView(boardEl, { aggregate: true }, context);
  boardEl.querySelector(".bd-head").append(archive);

  function paintWho() {
    const agent = mode.target && context.agents.find(agent => agent.name === mode.target);
    if (mode.target && !agent && context.agents.length) mode.target = null;
    who.hidden = !context.agents.length;
    who.replaceChildren(h("span", agent ? agent.title || agent.name : "General"), icon("chevron-down"));
  }
  function pick() {
    const choose = name => { mode.target = name; menu.close(); paintWho(); composer.focus(); };
    const entry = (name, text) => h("button.menu-item", { type: "button", "aria-checked": String(mode.target === name), role: "menuitemradio", onclick: () => choose(name) }, text);
    const menu = api.popover(who, [entry(null, "General"), ...context.agents.filter(agent => agent.state !== "terminated").map(agent => entry(agent.name, agent.title || agent.name))], { above: false });
  }

  /// Admit and start the work. A retry of the same text reuses its request and receipt, so a lost
  /// reply never admits the work twice.
  async function start(text) {
    if (sending) return composer.set(text);
    const target = mode.target ? { kind: "agent", name: mode.target } : { kind: "general", model: null };
    if (attempt?.text !== text || JSON.stringify(attempt.target) !== JSON.stringify(target)) attempt = { request_id: crypto.randomUUID(), text, target, receipt: null };
    const own = attempt;
    sending = true; surface.send.disabled = true;
    try {
      own.receipt ||= await api.call("work_admit", { request_id: own.request_id, target, task: { objective: text } });
      await api.call("work_execute", { receipt: own.receipt.id });
      if (attempt === own) attempt = null;
      mode.fresh = own.receipt.work;
      await load();
    } catch (error) {
      if (alive) { composer.set(text); api.toast(error.message); }
    } finally {
      sending = false;
      if (alive) surface.send.disabled = false;
    }
  }

  async function perform(method, input) {
    try { await api.call(method, input); await load(); }
    catch (error) { api.toast(error.message); }
  }

  /// The less frequent things a person does to a row, on its context menu.
  function actions(row) {
    const derived = row.derived;
    const list = [];
    if (row.group === "working") list.push({ label: "Stop", run: () => perform("work_cancel", { work: row.id, receipt: null }) });
    if (derived?.start_receipt) list.push({ label: "Start", run: () => perform("work_execute", { receipt: derived.start_receipt }) });
    else if (derived?.run && derived.state === "waiting") list.push({ label: "Start", run: () => perform("automation_start", { run: derived.run }) });
    if (derived?.retry_task) list.push({ label: "Retry", run: () => perform("retry_task", { task: derived.retry_task }) });
    const open = (derived?.failures || []).filter(failure => !failure.resolved);
    if (open.length) list.push({ label: "Mark resolved", run: async () => { for (const failure of open) await perform("work_resolve", { work: row.id, run: failure.run, digest: failure.digest, resolved: true }); } });
    if (derived) list.push({ label: archived ? "Unarchive" : "Archive", run: () => perform("work_archive", { work: row.id, archived: !archived }) });
    return list;
  }

  function open(row) {
    if (row.conversation) return void api.openSession(row.conversation);
    if (row.board) { show(true); controller.openItem(row.id); }
  }

  function rows() {
    const out = [];
    const seen = new Set();
    for (const board of context.items) {
      const derived = context.derived.get(board.id);
      seen.add(board.id);
      const at = group(board, derived);
      if (at) out.push({ id: board.id, group: at, title: board.title, owner: board.owner, board, derived, conversation: derived?.conversation_missing ? null : derived?.conversation || board.conversation || null, status: derived?.status || "" });
    }
    for (const [id, derived] of context.derived) {
      if (seen.has(id)) continue;
      const at = group(null, derived);
      if (at) out.push({ id, group: at, derived, title: derived.title, owner: derived.target?.kind === "agent" ? derived.target.name : null, conversation: derived.conversation_missing ? null : derived.conversation, status: derived.status });
    }
    return out;
  }

  function mark(row) {
    if (row.group === "working") return h("span.work-mark.working");
    if (row.group === "attention") return h("span.work-mark.attention");
    if (row.group === "waiting") return h("span.work-mark.waiting");
    return statusMark("todo");
  }

  function drawList() {
    const all = rows();
    const before = new Set([...list.querySelectorAll("[data-row]")].map(node => node.dataset.row));
    list.replaceChildren(...GROUPS.flatMap(({ id, label }) => {
      const members = all.filter(row => row.group === id);
      if (!members.length) return [];
      return [h("section.work-group", { "aria-label": label },
        h("h2.work-group-title", label, h("span.work-group-count", String(members.length))),
        h("ul.work-rows", members.map(row => {
          const button = h("button.work-row", { type: "button", dataset: { row: row.id, group: row.group, state: row.derived?.state || "" }, onclick: () => open(row) },
            mark(row), h("span.work-row-title", row.title),
            row.status && row.group !== "todo" ? h("span.work-row-status", row.status) : null,
            avatar(row.owner));
          bindContextMenu(button, () => actions(row));
          return h("li", button);
        })))];
    }));
    if (!reduced()) for (const node of list.querySelectorAll("[data-row]")) if (before.size && !before.has(node.dataset.row) || node.dataset.row === mode.fresh) {
      node.animate([{ opacity: 0, transform: "translateY(6px)" }, { opacity: 1, transform: "none" }], { duration: 200, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
    }
    mode.fresh = null;
  }

  /// Move between the list and the board. The composer slides to its new place, so the person sees
  /// one page change shape instead of a new page.
  function show(all, animate = true) {
    const from = top.getBoundingClientRect();
    mode.all = all;
    el.dataset.all = String(all);
    toggle.textContent = all ? "Show less" : "View all";
    toggle.setAttribute("aria-expanded", String(all));
    list.hidden = all; boardEl.hidden = !all;
    if (!animate || reduced()) return;
    const to = top.getBoundingClientRect();
    const dy = from.top - to.top;
    if (Math.abs(dy) > 1) top.animate([{ transform: `translateY(${dy}px)` }, { transform: "none" }], { duration: 280, easing: "cubic-bezier(0.77, 0, 0.175, 1)" });
    (all ? boardEl : list).animate([{ opacity: 0, transform: "translateY(10px)" }, { opacity: 1, transform: "none" }], { duration: 220, delay: 80, easing: "cubic-bezier(0.23, 1, 0.32, 1)", fill: "backwards" });
  }

  async function load() {
    const own = ++generation;
    try {
      const [board, first] = await Promise.all([api.call("board"), api.call("work_list", { archived, limit: 50 })]);
      const items = [...first.items];
      let next = first.next;
      while (next) { const page = await api.call("work_list", { archived, limit: 50, cursor: next }); items.push(...page.items); next = page.next; }
      if (!alive || own !== generation) return;
      context.archived = archived;
      context.boards = board?.boards || [];
      context.tasks = board?.tasks || {};
      context.agents = board?.agents || [];
      context.derived = new Map(items.map(item => [item.id, item]));
      context.items = (board?.items || []).filter(item => (item.archived_by || []).includes("user") === archived);
      context.changed(); status.textContent = "";
    } catch (error) { if (alive && own === generation) status.textContent = error.message; }
  }

  context.listeners.add(() => { drawList(); paintWho(); });
  const keydown = event => {
    if (event.key !== "Escape" || !mode.all || event.defaultPrevented || event.target.closest("input,textarea,[contenteditable],dialog")) return;
    if (boardEl.querySelector(".bd-detail:not([hidden])")) return;
    event.preventDefault(); show(false); toggle.focus({ preventScroll: true });
  };
  el.addEventListener("keydown", keydown);
  show(mode.all, false); paintWho();
  const off = api.on("ark:orch", () => void load());
  void load();
  return {
    refresh: load,
    focusInput: () => composer.focus(),
    dispose() { alive = false; generation++; off?.(); el.removeEventListener("keydown", keydown); controller.dispose(); composer.dispose(); },
  };
}

export function registerWork(api) {
  api.view({ id: "work", title: "Work", icon: "check", nav: "Work", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => workHome(el, params, scoped || api) });
}
