// The project's board: To do, In progress, Done. The person and the agents change the same items
// (ark-orchestrate's `board`); the page draws them and follows the orchestrator's `work` events.
//
//   view   "board" in the top bar beside Chat, when the app runs agents
//   panel  "board-item" in the side pane: one item, its owner, note, the work done on it
//
// At rest a card is a status circle, a title and its owner. Two marks only: "Needs you" (an agent
// waits for the person on it) and a live dot while its agent works. Everything else is in the
// panel. A drag moves a card; an agent's move glides so the person sees what changed.

import { h, icon, clear, clip } from "./dom.js";
import { markdown } from "./md.js";
import { registry } from "./ext.js";

const COLUMNS = [
  { state: "todo", label: "To do" },
  { state: "doing", label: "In progress" },
  { state: "done", label: "Done" },
];
const PERSON = "user";
const DONE_SHOWN = 5;
const UNDO_MS = 5000;

// ---- the shared state: the view and the panel draw from it ------------------------------------

const store = {
  ark: null,
  items: [],
  tasks: {},
  agents: [],
  project: null,
  removing: new Set(), // deleted, waiting for undo to pass
  listeners: new Set(),
  changed(opts = {}) {
    for (const fn of this.listeners) fn(opts);
  },
};

const item = (id) => store.items.find((w) => w.id === id);
const task = (id) => store.tasks[id];
const openTask = (w) => [...(w.tasks || [])].reverse().map((t) => [t, task(t)]).find(([, t]) => t && !["done", "failed", "cancelled"].includes(t.state));
const running = (w) => (w.tasks || []).some((t) => task(t)?.state === "running");
/// One of its tasks waits for the person's answer: the runtime keeps the question on the task.
const asking = (w) => (w.tasks || []).some((t) => {
  const r = task(t);
  return r && r.question && !r.question.answer && !["done", "failed", "cancelled"].includes(r.state);
});
/// Waiting for the person: an agent asks a question on it, or it is blocked and theirs.
export const needsYou = (w) => w.state !== "done" && (asking(w) || (!!w.blocked && w.owner === PERSON));
const ownerName = (o) => (o === PERSON ? "You" : o || "");
const byName = (b) => (b === PERSON ? "You" : b === "chat" ? store.ark?.summary?.()?.name || "Chat" : b);
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

async function load() {
  try {
    const b = await store.ark.call("board");
    if (!b) return;
    store.items = b.items || [];
    store.tasks = b.tasks || {};
    store.agents = b.agents || [];
    store.changed();
  } catch {}
}

let reloadTimer;
function reloadSoon() {
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(load, 250);
}

/// An item as the runtime has it now; it keeps its place in the board's order.
function upsert(w) {
  const i = store.items.findIndex((x) => x.id === w.id);
  if (i === -1) store.items.push(w);
  else store.items[i] = w;
  store.items = ordered(store.items);
}

/// The runtime's order: by column, then by place; Done latest first.
function ordered(items) {
  const col = (s) => COLUMNS.findIndex((c) => c.state === s);
  return [...items].sort((a, b) => col(a.state) - col(b.state)
    || (a.state === "done" ? (b.done_ms || 0) - (a.done_ms || 0) : (a.rank - b.rank) || (a.created_ms - b.created_ms)));
}

async function change(id, ch) {
  try {
    upsert(await store.ark.call("board_change", { item: id, change: ch }));
    store.changed({ quiet: true });
  } catch (e) {
    store.ark.toast(e.message);
    load();
  }
}

export function registerBoard(ark) {
  store.ark = ark;
  ark.on("ark:orch", (m) => {
    const ev = m.event || {};
    if (ev.type === "work") {
      upsert(ev.item);
      store.changed({ by: ev.by, id: ev.item.id });
      if ((ev.item.tasks || []).some((t) => !task(t))) reloadSoon();
    } else if (ev.type === "work_removed") {
      store.items = store.items.filter((w) => w.id !== ev.item);
      store.changed();
    } else if (ev.task && store.items.some((w) => (w.tasks || []).includes(ev.task))) {
      reloadSoon();
    } else if (ev.type === "agent_spawned" || ev.type === "agent_state") {
      reloadSoon();
    }
  });
  ark.on("ark:orch_lagged", load);
  ark.on("ark:connected", load);
  ark.on("ark:summary", (m) => {
    const p = m.summary?.project?.id;
    if (p && p !== store.project) {
      store.project = p;
      load();
    }
  });
  store.project = ark.summary?.()?.project?.id || null;
  (async () => {
    let first;
    try {
      first = await ark.call("board");
    } catch {
      return;
    }
    // Only an app that runs agents has a board; an app's own "board" view comes first.
    if (!first || registry.views.has("board")) return;
    store.items = first.items || [];
    store.tasks = first.tasks || {};
    store.agents = first.agents || [];
    ark.panel({ id: "board-item", title: "Item", icon: "layers", render: (el, params) => itemPanel(el, params) });
    ark.view({ id: "board", title: "Board", icon: "layers", nav: true, render: (el) => boardView(el) });
  })();
}

// ---- small pieces ---------------------------------------------------------------------------------

function avatar(owner) {
  if (!owner) return null;
  const you = owner === PERSON;
  let hash = 0;
  for (const c of owner) hash = (hash * 31 + c.charCodeAt(0)) >>> 0;
  return h("span.bd-avatar", {
    title: ownerName(owner),
    "data-you": you ? "true" : null,
    style: you ? null : { "--hue": `var(--seg-${hash % 9})` },
  }, you ? "Y" : owner[0].toUpperCase());
}

/// The status circle: empty (to do), half (in progress), filled with a check (done).
function statusMark(state) {
  const svg = {
    todo: '<circle cx="8" cy="8" r="6"/>',
    doing: '<circle cx="8" cy="8" r="6"/><path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" stroke="none"/>',
    done: '<circle cx="8" cy="8" r="6.6" fill="currentColor" stroke="none"/><path d="M5.2 8.3l1.9 1.9 3.7-4.1" stroke="var(--card)"/>',
  }[state];
  const span = h("span.bd-mark", { "data-state": state });
  span.innerHTML = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">${svg}</svg>`;
  return span;
}

function ago(ms) {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/// Grow a textarea with its text.
function autosize(t) {
  const fit = () => {
    t.style.height = "auto";
    t.style.height = `${t.scrollHeight}px`;
  };
  t.addEventListener("input", fit);
  requestAnimationFrame(fit);
  return fit;
}

// ---- the board --------------------------------------------------------------------------------

function boardView(el) {
  const ark = store.ark;
  el.classList.add("bd");
  const v = {
    filter: false, // only what needs you
    allDone: false,
    adding: false,
    drag: null,
    pending: false, // a render waits for the drag to end
    seen: new Set(store.items.map((w) => w.id)),
    focus: null, // the card to focus after a render
  };
  const needsChip = h("button.bd-needs", { type: "button", onclick: () => { v.filter = !v.filter; render({ animate: false }); } });
  const head = h("div.bd-head", needsChip);
  const cols = h("div.bd-cols");
  const undo = h("div.bd-undo", { role: "status" });
  undo.hidden = true;
  el.append(head, cols, undo);
  // Cards are kept by id and redrawn only when what they show changes, so a card being
  // pressed or a title being typed is never swapped out by an agent's change.
  const cards = new Map(); // id -> { node, sig }
  const lists = new Map(); // state -> list element
  const counts = new Map(); // state -> count element
  for (const c of COLUMNS) {
    const list = h("div.bd-list", { "data-state": c.state });
    const count = h("span.bd-count");
    lists.set(c.state, list);
    counts.set(c.state, count);
    cols.append(h("section.bd-col", { "data-state": c.state, "aria-label": c.label },
      h("header.bd-col-head", h("span", c.label), count), list));
  }
  const more = h("button.bd-more", { type: "button", onclick: () => { v.allDone = !v.allDone; render({ animate: false }); } });
  const addBtn = h("button.bd-add", { type: "button", onclick: () => { v.adding = true; render({ animate: false }); input.focus(); } }, icon("plus"), "Add");
  const input = h("textarea.bd-input", { rows: 1, placeholder: "What needs doing?", "aria-label": "New item" });
  const fitInput = autosize(input);

  // ---- drawing ----

  const current = (id) => item(id);
  const sigOf = (w) => [w.title, w.state, w.owner, w.blocked, needsYou(w), running(w), openId() === w.id].join("\u0001");

  function card(w) {
    const id = w.id;
    const needs = needsYou(w);
    const mark = h("button.bd-check", {
      type: "button",
      "aria-label": w.state === "done" ? "Mark as not done" : "Mark as done",
      onclick: (e) => { e.stopPropagation(); const x = current(id); if (x) toggleDone(x); },
    }, statusMark(w.state));
    const title = h("div.bd-title", w.title);
    const tags = [];
    if (needs) tags.push(h("span.bd-tag.you", "Needs you"));
    else if (w.blocked && w.state !== "done") tags.push(h("span.bd-tag.blocked", { title: w.blocked }, "Blocked"));
    const who = avatar(w.owner);
    const node = h("div.bd-card", {
      tabindex: "0",
      role: "button",
      "data-id": id,
      "data-state": w.state,
      "aria-label": w.title,
      "aria-current": openId() === id ? "true" : null,
    },
    mark,
    h("div.bd-body", title, tags.length ? h("div.bd-tags", tags) : null),
    who ? h("span.bd-who", who, running(w) ? h("span.bd-live", { title: `${ownerName(w.owner)} is working on it` }) : null) : null);
    node.addEventListener("pointerdown", (e) => { const x = current(id); if (x) press(e, x, node); });
    node.addEventListener("keydown", (e) => { const x = current(id); if (x) cardKeys(e, x); });
    return node;
  }

  const closeAdder = () => {
    v.adding = false;
    input.value = "";
    render({ animate: false });
  };
  input.addEventListener("keydown", async (e) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeAdder(); }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      const title = input.value.trim();
      if (!title) return closeAdder();
      input.value = "";
      fitInput();
      v.adding = true;
      try {
        upsert(await ark.call("board_add", { title, owner: v.filter ? PERSON : undefined }));
        v.mine = title;
        render();
      } catch (err) {
        input.value = title;
        ark.toast(err.message);
      }
    }
  });
  input.addEventListener("blur", () => setTimeout(() => {
    if (document.activeElement !== input && !input.value.trim() && v.adding) closeAdder();
  }, 0));

  /// Put `nodes` in `list` in this order, moving only when the order differs.
  function sync(list, nodes) {
    const now = [...list.children].filter((c) => !c.classList.contains("bd-line"));
    if (now.length === nodes.length && now.every((c, i) => c === nodes[i])) return;
    list.replaceChildren(...nodes);
  }

  /// Draw the board. `animate`: cards that moved glide to their new place, new ones fade in.
  /// `from`: where a dropped card was let go, so it settles from there.
  function render({ animate = true, from = null } = {}) {
    if (v.drag || v.pressing) { v.pending = true; return; }
    v.pending = false;
    const before = new Map();
    if (animate) for (const [id, c] of cards) before.set(id, c.node.getBoundingClientRect());
    const hadFocus = document.activeElement?.closest?.(".bd-card")?.dataset.id;
    const typing = document.activeElement === input;
    const visible = store.items.filter((w) => !store.removing.has(w.id));
    const mine = visible.filter(needsYou);
    needsChip.hidden = !mine.length;
    if (!mine.length) v.filter = false;
    needsChip.textContent = `Needs you · ${mine.length}`;
    needsChip.setAttribute("aria-pressed", v.filter ? "true" : "false");
    const keep = new Set();
    for (const c of COLUMNS) {
      let col = visible.filter((w) => w.state === c.state && (!v.filter || needsYou(w)));
      const total = col.length;
      if (c.state === "done" && !v.allDone) col = col.slice(0, DONE_SHOWN);
      const nodes = col.map((w) => {
        const sig = sigOf(w);
        let e = cards.get(w.id);
        if (!e || e.sig !== sig) {
          e = { node: card(w), sig };
          cards.set(w.id, e);
        }
        keep.add(w.id);
        return e.node;
      });
      if (c.state === "done" && total > DONE_SHOWN) {
        more.textContent = v.allDone ? "Show less" : `${total - DONE_SHOWN} more`;
        nodes.push(more);
      }
      if (c.state === "todo" && !v.filter) {
        // An empty board offers the input at once.
        nodes.push(v.adding || !visible.length ? input : addBtn);
      }
      sync(lists.get(c.state), nodes);
      counts.get(c.state).textContent = total ? String(total) : "";
    }
    for (const id of [...cards.keys()]) if (!keep.has(id)) cards.delete(id);
    // Moving the box in the list blurs it: give the focus back so typing goes on.
    if (typing && input.isConnected) input.focus({ preventScroll: true });
    const refocus = v.focus || hadFocus;
    v.focus = null;
    if (refocus && cards.has(refocus) && document.activeElement !== input) cards.get(refocus).node.focus({ preventScroll: true });
    if (!animate) {
      for (const id of cards.keys()) v.seen.add(id);
      return;
    }
    const still = reduced();
    for (const [id, { node: c }] of cards) {
      const was = id === from?.id ? from.rect : before.get(id);
      if (!was) {
        if (!v.seen.has(id)) {
          // New: fade in. One from an agent or another window is marked for a moment.
          c.animate([{ opacity: 0, transform: still ? "none" : "translateY(6px)" }, { opacity: 1, transform: "none" }], { duration: 200, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
          if (current(id)?.title !== v.mine) {
            c.classList.add("fresh");
            setTimeout(() => c.classList.remove("fresh"), 1200);
          }
        }
        continue;
      }
      const now = c.getBoundingClientRect();
      const dx = was.left - now.left;
      const dy = was.top - now.top;
      if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
      if (still) continue;
      const dropped = id === from?.id;
      c.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "none" }], {
        duration: dropped ? 150 : 250,
        easing: dropped ? "cubic-bezier(0.23, 1, 0.32, 1)" : "cubic-bezier(0.77, 0, 0.175, 1)",
      });
    }
    for (const id of cards.keys()) v.seen.add(id);
  }

  // ---- actions ----

  const openId = () => (registry.panels.has("board-item") && store.openItem) || null;

  function open(w) {
    store.openItem = w.id;
    ark.openPanel("board-item", { id: w.id });
    for (const [id, c] of cards) c.node.setAttribute("aria-current", id === w.id ? "true" : "false");
    ark.reportFocus();
  }

  function toggleDone(w) {
    const to = w.state === "done" ? "todo" : "done";
    move(w, to, null, { animate: true });
  }

  /// Move `w` to column `state`, before item `before` (or at the end; Done at the top).
  function move(w, state, before, { animate = true, from = null } = {}) {
    const rest = store.items.filter((x) => x.id !== w.id);
    const moved = { ...w, state, done_ms: state === "done" ? (w.state === "done" ? w.done_ms : Date.now()) : null };
    let at = before ? rest.findIndex((x) => x.id === before) : -1;
    if (at === -1) {
      if (state === "done") at = rest.findIndex((x) => x.state === "done");
      else {
        const last = rest.map((x) => x.state).lastIndexOf(state);
        at = last === -1 ? rest.findIndex((x) => COLUMNS.findIndex((c) => c.state === x.state) > COLUMNS.findIndex((c) => c.state === state)) : last + 1;
      }
    }
    if (at === -1) at = rest.length;
    rest.splice(at, 0, moved);
    store.items = rest;
    v.focus = w.id;
    render({ animate, from });
    const ch = { state };
    if (before) ch.before = before;
    change(w.id, ch);
  }

  function remove(w) {
    store.removing.add(w.id);
    if (store.openItem === w.id) ark.closePanel("board-item");
    render({ animate: false });
    clear(undo);
    const label = h("span", `Deleted “${clip(w.title, 40)}”`);
    const back = h("button.bd-undo-btn", { type: "button" }, "Undo");
    undo.append(label, back);
    undo.hidden = false;
    let done = false;
    const finish = (keep) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      undo.hidden = true;
      store.removing.delete(w.id);
      if (keep) return render({ animate: false });
      ark.call("board_remove", { item: w.id }).catch((e) => { ark.toast(e.message); load(); });
      store.items = store.items.filter((x) => x.id !== w.id);
      render({ animate: false });
    };
    const timer = setTimeout(() => finish(false), UNDO_MS);
    back.onclick = () => finish(true);
    store.undoing = () => finish(false);
  }
  store.remove = remove;

  // Keys on a card. Moves made with keys do not animate: they are repeated often.
  function cardKeys(e, w) {
    if (e.target.closest("textarea, input")) return;
    const col = [...(lists.get(w.state)?.querySelectorAll(".bd-card") || [])];
    const i = col.findIndex((c) => c.dataset.id === w.id);
    const ci = COLUMNS.findIndex((c) => c.state === w.state);
    if (e.key === "Enter") { e.preventDefault(); return open(w); }
    if (e.key === " ") { e.preventDefault(); return toggleDone(w); }
    if (e.key === "Backspace" || e.key === "Delete") { e.preventDefault(); return remove(w); }
    if (e.shiftKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
      e.preventDefault();
      const to = COLUMNS[ci + (e.key === "ArrowLeft" ? -1 : 1)];
      if (to) move(w, to.state, null, { animate: false });
      return;
    }
    if (e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown") && w.state !== "done") {
      e.preventDefault();
      const up = e.key === "ArrowUp";
      if (up && i > 0) move(w, w.state, col[i - 1].dataset.id, { animate: false });
      if (!up && i < col.length - 1) move(w, w.state, col[i + 2]?.dataset.id || null, { animate: false });
      return;
    }
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      col[i + (e.key === "ArrowUp" ? -1 : 1)]?.focus();
      return;
    }
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const to = COLUMNS[ci + (e.key === "ArrowLeft" ? -1 : 1)];
      const other = to && [...(lists.get(to.state)?.querySelectorAll(".bd-card") || [])];
      other?.[Math.min(i, other.length - 1)]?.focus();
    }
  }

  // ---- dragging: the card follows the pointer, a line shows where it lands ----

  function press(e, w, el) {
    if (e.button !== 0 || e.target.closest(".bd-check")) return;
    const start = { x: e.clientX, y: e.clientY };
    let drag = null;
    v.pressing = true;
    el.setPointerCapture(e.pointerId);
    const onMove = (ev) => {
      if (!drag) {
        if (Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 4) return;
        drag = lift(w, el, start);
      }
      drag.move(ev);
    };
    const end = (ev, cancelled) => {
      el.removeEventListener("pointermove", onMove);
      el.removeEventListener("pointerup", onUp);
      el.removeEventListener("pointercancel", onCancel);
      removeEventListener("keydown", onKey, true);
      v.pressing = false;
      if (drag) return drag.drop(ev, cancelled);
      if (!cancelled) open(w);
      if (v.pending) render({ animate: false });
    };
    const onUp = (ev) => end(ev, false);
    const onCancel = (ev) => end(ev, true);
    const onKey = (ev) => { if (ev.key === "Escape" && drag) { ev.preventDefault(); ev.stopPropagation(); end(null, true); } };
    el.addEventListener("pointermove", onMove);
    el.addEventListener("pointerup", onUp);
    el.addEventListener("pointercancel", onCancel);
    addEventListener("keydown", onKey, true);
  }

  function lift(w, el, start) {
    const r = el.getBoundingClientRect();
    const ghost = el.cloneNode(true);
    ghost.classList.add("bd-ghost");
    Object.assign(ghost.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px` });
    document.body.append(ghost);
    el.classList.add("lifted");
    const line = h("div.bd-line");
    v.drag = { id: w.id };
    let target = null; // { state, before }
    return {
      move(ev) {
        const dx = ev.clientX - start.x;
        const dy = ev.clientY - start.y;
        ghost.style.transform = `translate(${dx}px, ${dy}px)`;
        const under = document.elementFromPoint(ev.clientX, ev.clientY);
        const list = under?.closest?.(".bd-list") || under?.closest?.(".bd-col")?.querySelector(".bd-list");
        if (!list || !cols.contains(list)) { line.remove(); target = null; return; }
        const state = list.dataset.state;
        // Near the top or bottom of a long column, it scrolls.
        const lr = list.getBoundingClientRect();
        if (ev.clientY < lr.top + 36) list.scrollTop -= 8;
        else if (ev.clientY > lr.bottom - 36) list.scrollTop += 8;
        const others = [...list.querySelectorAll(".bd-card")].filter((c) => c.dataset.id !== w.id);
        // Done keeps the latest first: a card dropped there goes to the top.
        const next = state === "done" ? others[0] : others.find((c) => {
          const cr = c.getBoundingClientRect();
          return ev.clientY < cr.top + cr.height / 2;
        });
        target = { state, before: state === "done" ? null : next?.dataset.id || null };
        if (next) list.insertBefore(line, next);
        else list.insertBefore(line, list.querySelector(".bd-more, .bd-add, .bd-input"));
      },
      drop(ev, cancelled) {
        const rect = ghost.getBoundingClientRect();
        ghost.remove();
        line.remove();
        el.classList.remove("lifted");
        v.drag = null;
        const same = target && target.state === w.state && (target.before === w.id || (() => {
          const ids = store.items.filter((x) => x.state === w.state).map((x) => x.id);
          const i = ids.indexOf(w.id);
          return target.before ? ids[i + 1] === target.before : i === ids.length - 1;
        })());
        if (cancelled || !target || same) {
          if (v.pending) render({ animate: false });
          // Back to where it was, from where it was let go.
          if (!reduced()) {
            const now = el.getBoundingClientRect();
            el.animate([{ transform: `translate(${rect.left - now.left}px, ${rect.top - now.top}px)` }, { transform: "none" }], { duration: 150, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
          }
          return;
        }
        move(w, target.state, target.before, { animate: true, from: { id: w.id, rect } });
      },
    };
  }

  const off = () => render();
  store.listeners.add(off);
  render({ animate: false });

  return {
    refresh: () => load(),
    focus: () => {
      const w = store.openItem && item(store.openItem);
      return { surface: { kind_tag: "board", title: w ? w.title : "Board", locator: w ? w.id : undefined } };
    },
    dispose: () => store.listeners.delete(off),
  };
}

// ---- one item, in the side pane --------------------------------------------------------------

function itemPanel(el, params) {
  const ark = store.ark;
  el.classList.add("bd-panel");
  let id = params?.id;
  let editing = null; // a field being typed in keeps its text while the item changes
  const opened = new Set(); // runs whose whole answer the person opened

  const title = h("textarea.bd-p-title", { rows: 1, "aria-label": "Title" });
  const fitTitle = autosize(title);
  title.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); title.blur(); } });
  title.addEventListener("focus", () => (editing = "title"));
  title.addEventListener("blur", () => {
    editing = null;
    const w = item(id);
    const t = title.value.trim();
    if (w && t && t !== w.title) change(id, { title: t });
    else if (w) title.value = w.title;
  });
  const note = h("textarea.bd-p-note", { rows: 2, placeholder: "Add a note", "aria-label": "Note" });
  const fitNote = autosize(note);
  note.addEventListener("focus", () => (editing = "note"));
  note.addEventListener("blur", () => {
    editing = null;
    const w = item(id);
    if (w && note.value !== (w.note || "")) change(id, { note: note.value });
  });
  const segs = h("div.segs.bd-p-states", { role: "group", "aria-label": "Column" });
  const meta = h("div.bd-p-meta");
  const blocked = h("div.bd-p-blocked");
  const work = h("div.bd-p-work");
  const history = h("details.bd-p-history");
  const del = h("button.btn.ghost.small.bd-p-delete", { type: "button" }, "Delete");
  el.append(title, segs, meta, blocked, note, work, history, del);
  // The pane can change width: the text boxes follow.
  const resized = new ResizeObserver(() => { fitTitle(); fitNote(); });
  resized.observe(el);

  del.onclick = () => {
    const w = item(id);
    if (w && store.remove) store.remove(w);
  };

  function menu(anchor, entries) {
    ark.popover(anchor, entries.map((e) => h("button.menu-item", { type: "button", "aria-current": e.current ? "true" : null, onclick: () => { document.querySelector(".popover")?.close?.(); e.run(); } },
      h("span", e.label), e.sub ? h("span.menu-sub", e.sub) : null)));
  }

  function draw() {
    const w = item(id);
    if (!w) {
      for (const x of [segs, meta, blocked, work, history]) clear(x);
      title.value = "";
      return;
    }
    ark.setPanelTitle("board-item", clip(w.title, 28));
    if (editing !== "title") { title.value = w.title; fitTitle(); }
    if (editing !== "note") { note.value = w.note || ""; fitNote(); }

    clear(segs);
    for (const c of COLUMNS) {
      segs.append(h("button.seg", { type: "button", "aria-pressed": w.state === c.state ? "true" : "false", onclick: () => w.state !== c.state && change(id, { state: c.state }) }, c.label));
    }

    // Who has it, and handing it to an agent: the one action here that starts work.
    clear(meta);
    const busy = openTask(w);
    const ownerBtn = h("button.bd-p-owner", { type: "button", disabled: busy ? true : null, title: busy ? `${busy[1].owner} is working on it` : "Change owner" },
      avatar(w.owner) || h("span.bd-avatar.none"), h("span", w.owner ? ownerName(w.owner) : "No owner"));
    ownerBtn.onclick = () => menu(ownerBtn, [
      { label: "No owner", current: !w.owner, run: () => change(id, { owner: "" }) },
      { label: "You", current: w.owner === PERSON, run: () => change(id, { owner: PERSON }) },
      ...store.agents.map((a) => ({ label: a.name, sub: clip(a.role, 40), current: w.owner === a.name, run: () => change(id, { owner: a.name }) })),
    ]);
    meta.append(ownerBtn);
    if (busy) {
      const [tid, t] = busy;
      meta.append(h("span.bd-p-busy", running(w) ? h("span.bd-live") : null, t.state === "running" ? "Working" : t.state === "waiting" ? "Waiting on its helpers" : "Queued"),
        h("button.btn.ghost.small", { type: "button", onclick: () => ark.call("cancel_task", { task: tid }).catch((e) => ark.toast(e.message)) }, "Stop"));
    } else if (w.state !== "done" && store.agents.length) {
      const give = h("button.btn.small.bd-p-give", { type: "button" }, "Give to…");
      give.onclick = () => menu(give, store.agents.map((a) => ({
        label: a.name,
        sub: clip(a.role, 40),
        run: () => ark.call("board_give", { item: id, agent: a.name }).catch((e) => ark.toast(e.message)),
      })));
      meta.append(give);
    }

    clear(blocked);
    blocked.hidden = !(w.blocked && w.state !== "done") && !asking(w);
    if (asking(w)) blocked.append(h("div.bd-p-ask", icon("alert"), h("span", `${ownerName(w.owner)} is waiting for your answer`)));
    if (w.blocked && w.state !== "done") {
      blocked.append(h("div.bd-p-why", h("span", w.blocked),
        h("button.bd-p-x", { type: "button", title: "Not blocked", "aria-label": "Not blocked", onclick: () => change(id, { blocked: "" }) }, icon("x"))));
    }

    // The work done on it: each task, its answer and what it made.
    clear(work);
    const runs = [...(w.tasks || [])].reverse().map((t) => [t, task(t)]).filter(([, t]) => t);
    work.hidden = !runs.length;
    for (const [tid, t] of runs) {
      const said = t.result || t.error || "";
      // The answer as the conversation shows it; a long one opens on a click.
      const body = h("div.bd-p-said.prose", { title: tid, html: markdown(said) });
      if (said.length > 280 && !opened.has(tid)) {
        body.classList.add("more");
        body.onclick = () => { body.classList.remove("more"); opened.add(tid); };
      }
      work.append(h("div.bd-p-run", { "data-state": t.state },
        h("div.bd-p-run-head", avatar(t.owner), h("span", t.owner), h("span.bd-p-state", t.state), t.finished_ms ? h("span.bd-p-ago", ago(t.finished_ms)) : null),
        said ? body : null,
        (t.outputs || []).length ? h("div.bd-p-files", t.outputs.map((a) =>
          h("button.bd-p-file", { type: "button", title: a.name, onclick: () => ark.openArtifact(a.id) }, icon("file"), clip(a.name, 32)))) : null));
    }

    clear(history);
    const steps = [...(w.log || [])].reverse();
    history.append(h("summary", `History · ${steps.length}`),
      steps.map((s) => h("div.bd-p-step", h("span.bd-p-by", byName(s.by)), h("span", s.text), h("span.bd-p-ago", ago(s.at_ms)))));
  }

  const off = () => draw();
  store.listeners.add(off);
  draw();
  return {
    update(p) {
      if (p?.id && p.id !== id) {
        id = p.id;
        editing = null;
        opened.clear();
        draw();
      }
    },
    refresh: () => draw(),
    focus: () => {
      const w = item(id);
      return w ? { surface: { kind_tag: "board", title: w.title, locator: w.id } } : null;
    },
    dispose() {
      resized.disconnect();
      store.listeners.delete(off);
      if (store.openItem === id) store.openItem = null;
      store.changed({ quiet: true });
    },
  };
}
