// The top bar's views of the current project: "Chat" (the conversation), then the app's views
// marked `nav` as tabs. On the right, a + menu opens panels marked `nav` (Terminal, Browser,
// Files, then the app's) in the side pane.

import { h, icon, clear, $ } from "./dom.js";
import { label as keyLabel } from "./keys.js";
import { registry } from "./ext.js";

export class Topbar {
  constructor(api) {
    this.api = api;
    this.tabs = $("#view-tabs");
    this.launchers = $("#panel-launchers");
    this.view = null;
  }

  render() {
    const project = this.api.project();
    const views = [...registry.views.values()].filter((v) => v.nav && (!v.when || v.when({ project })));
    clear(this.tabs);
    // With no views there is only the conversation: no tabs.
    this.tabs.hidden = !views.length;
    if (views.length) {
      const tab = (id, label) => h("button.seg.view-tab", {
        type: "button", role: "tab", "data-view": id || "",
        onclick: () => (id ? this.api.openView(id) : this.api.showConversation()),
      }, label);
      this.tabs.append(tab(null, "Chat"), ...views.map((v) => tab(v.id, v.nav === true ? v.title : v.nav)));
    }
    clear(this.launchers);
    if ([...registry.panels.values()].some((p) => p.nav)) {
      this.launchers.append(h("button.icon-btn", {
        type: "button", id: "open-menu", title: "Open", "aria-label": "Open a panel", "aria-haspopup": "menu",
        onclick: (e) => this.menu(e.currentTarget),
      }, icon("plus")));
    }
    this.mark(this.view);
  }

  /// The + menu: every `nav` panel, with its key when it has one.
  menu(anchor) {
    const panels = [...registry.panels.values()].filter((p) => p.nav);
    const pop = this.api.popover(anchor, panels.map((p) => {
      const label = p.nav === true ? p.title : p.nav;
      return h("button.menu-item.open-item", { type: "button", role: "menuitem", "data-panel": p.id, onclick: () => { pop.close(); this.api.openPanel(p.id); } },
        h("span.open-label", icon(p.icon || "box"), label),
        p.keys ? h("kbd.menu-key", keyLabel(p.keys)) : null);
    }), { end: true });
    const items = [...pop.querySelectorAll("button")];
    pop.addEventListener("keydown", (e) => {
      const i = items.indexOf(document.activeElement);
      if (e.key === "ArrowDown") { e.preventDefault(); items[(i + 1) % items.length].focus(); }
      if (e.key === "ArrowUp") { e.preventDefault(); items[(i - 1 + items.length) % items.length].focus(); }
    });
    items[0]?.focus();
  }

  /// Mark the view the main column shows (null: the conversation).
  mark(id) {
    this.view = id;
    for (const t of this.tabs.children) t.setAttribute("aria-selected", String((t.dataset.view || null) === id));
  }
}
