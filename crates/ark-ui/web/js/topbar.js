// The title bar names the current work. The sidebar's foot holds the app's views. On the right, a
// + menu opens panels marked `nav` (Terminal, Browser, Files, then the app's) in the side pane.

import { h, icon, clear, $ } from "./dom.js";
import { label as keyLabel } from "./keys.js";
import { registry } from "./ext.js";

export class Topbar {
  constructor(api, sidebar) {
    this.api = api;
    this.sidebar = sidebar;
    this.tabs = $("#view-tabs");
    this.launchers = $("#panel-launchers");
    this.view = null;
  }

  render() {
    const project = this.api.project();
    const views = [...registry.views.values()].filter((v) => v.nav && (!v.when || v.when({ project })));
    clear(this.tabs);
    this.tabs.hidden = true;
    this.sidebar.setViews(views);
    clear(this.launchers);
    if ([...registry.panels.values()].some((p) => p.nav && (!p.when || p.when({ project })))) {
      this.launchers.append(h("button.icon-btn", {
        type: "button", id: "open-menu", title: "Open", "aria-label": "Open a panel", "aria-haspopup": "menu",
        onclick: (e) => this.menu(e.currentTarget),
      }, icon("plus")));
    }
    this.mark(this.view);
  }

  /// The + menu: every `nav` panel, with its key when it has one.
  menu(anchor) {
    const project = this.api.project();
    const panels = [...registry.panels.values()].filter((p) => p.nav && (!p.when || p.when({ project })));
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
    this.sidebar.markView(id);
  }
}
