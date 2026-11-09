// Recent chats with the agent in view, from the title in the title bar (ADR 0110): New chat, the
// last eight, and All chats when there are more. The last list shows at once and is refreshed in
// place; the first time, three placeholder rows hold its place.

import { h, icon, ago } from "./dom.js";

const SHOWN = 8;
const lists = new Map();

/// `load()` returns rows `{ id, title, updated_at, folder?, mark? }` newest first (`mark`:
/// "asking" or "working"). Returns the menu element.
export function openRecent(anchor, { keyboard, key, load, current, newChat, open, more, online, popover, onClose }) {
  const list = h("div.recent-list", { role: "group", "aria-label": "Recent chats" });
  const choose = (run) => () => { menu.close(); run(); };
  const head = newChat ? [h("button.menu-item.recent-new", { type: "button", role: "menuitem", onclick: choose(newChat) }, icon("new-chat"), h("span", "New chat"), h("kbd", "⌘N"))] : [];
  const menu = popover(anchor, [...head, list], { onClose });
  menu.classList.add("recent-menu");
  if (keyboard) menu.classList.add("instant");
  menu.setAttribute("aria-label", "Recent chats");
  let generation = 0;
  // Until the person moves, the focus follows the current chat once the list arrives.
  let placed = true;

  const enabled = () => [...menu.querySelectorAll("button.menu-item:not(:disabled)")];
  menu.addEventListener("keydown", (event) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    placed = false;
    const buttons = enabled();
    const index = buttons.indexOf(document.activeElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
      : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next]?.focus();
  });

  const draw = (state) => {
    const focused = menu.contains(document.activeElement) ? document.activeElement.dataset.id : null;
    list.setAttribute("aria-busy", String(state.loading));
    if (state.error) {
      list.replaceChildren(h("div.recent-error", h("span", "Could not load"),
        h("button.menu-item.recent-retry", { type: "button", role: "menuitem", onclick: () => void refresh() }, "Try again")));
      return;
    }
    if (!state.rows) {
      list.replaceChildren(...Array.from({ length: 3 }, () => h("div.recent-ghost", { "aria-hidden": "true" }, h("span"), h("span"))));
      return;
    }
    const rows = state.rows.slice(0, SHOWN).map((row) => {
      const mine = current(row);
      const end = row.mark === "asking" ? h("span.recent-end", { "data-mark": "asking" }, "Needs you")
        : row.mark === "working" ? h("span.recent-end", { "data-mark": "working" }, h("span.working-dot", { "aria-label": "Working" }))
          : h("span.recent-end", row.updated_at ? ago(row.updated_at) : "");
      return h("button.menu-item.recent-row", {
        type: "button", role: "menuitemradio", "aria-checked": String(mine), "data-id": row.id, disabled: !online,
        title: row.folder ? `${row.title}\n${row.folder}` : row.title, onclick: choose(() => open(row)),
      }, h("span.recent-check", mine ? icon("check") : null), h("span.recent-text", h("span.recent-title", row.title || "Untitled chat"), row.folder ? h("span.recent-folder", row.folder) : null), end);
    });
    if (state.rows.length > SHOWN && more) rows.push(h("button.menu-item.recent-all", { type: "button", role: "menuitem", disabled: !online, onclick: choose(more) }, h("span", "All chats")));
    list.replaceChildren(...rows);
    const back = placed ? list.querySelector('[aria-checked="true"]:not(:disabled)') : focused && list.querySelector(`[data-id="${CSS.escape(focused)}"]`);
    if (back && menu.contains(document.activeElement)) back.focus({ preventScroll: true });
  };

  const refresh = async () => {
    const mine = ++generation;
    draw({ rows: lists.get(key) || null, loading: true });
    let rows;
    try {
      rows = await load();
    } catch {
      if (mine === generation && menu.isConnected) draw({ error: true });
      return;
    }
    lists.set(key, rows);
    if (mine === generation && menu.isConnected) draw({ rows, loading: false });
  };

  if (online) void refresh();
  else draw({ rows: lists.get(key) || [], loading: false });
  const start = list.querySelector('[aria-checked="true"]:not(:disabled)') || enabled()[0];
  start?.focus({ preventScroll: true });
  return menu;
}
