import { h, icon } from "./dom.js";

export function ticketShell(el) {
  el.classList.add("ticket");
  const head = h("header.ticket-head");
  const status = h("p.ticket-status", { role: "status" });
  const content = h("div.ticket-content");
  const properties = h("aside.ticket-properties", { "aria-label": "Properties" });
  el.append(head, status, h("div.ticket-body", content, properties));
  return { head, status, content, properties };
}

export function ticketMenu(api, anchor, choices) {
  const menu = api.popover(anchor, choices, { end: true });
  const controls = () => [...menu.querySelectorAll("button:not(:disabled)")];
  (menu.querySelector('[aria-checked="true"]:not(:disabled)') || controls()[0])?.focus();
  menu.addEventListener("click", event => { if (event.target.closest("button:not(:disabled)")) menu.close(); });
  menu.addEventListener("keydown", event => {
    const items = controls(), index = items.indexOf(document.activeElement);
    const next = { ArrowDown: (index + 1) % items.length, ArrowUp: (index + items.length - 1) % items.length, Home: 0, End: items.length - 1 }[event.key];
    if (next != null) { event.preventDefault(); items[next]?.focus(); }
  });
  return menu;
}

export function ticketProperty(api, { label, value, mark, entries, disabled = false, title }) {
  const control = h(entries ? "button.ticket-property" : "div.ticket-property", {
    ...(entries ? { type: "button", disabled, "aria-haspopup": "menu", "aria-label": `${label}: ${value}` } : {}),
    title,
  }, mark, h("span.ticket-property-value", value), entries ? icon("chevron-down") : null);
  if (entries) control.onclick = () => ticketMenu(api, control, entries.map(entry => h("button.menu-item", {
    type: "button", role: "menuitemradio", "aria-checked": String(!!entry.current), "aria-current": entry.current ? "true" : null,
    onclick: () => entry.run(),
  }, h("span", entry.label), entry.sub ? h("span.menu-sub", entry.sub) : null)));
  return h("div.ticket-property-row", h("span.ticket-property-label", label), control);
}
