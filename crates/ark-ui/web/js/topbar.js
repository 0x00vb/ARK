// The top bar's views of the current project: "Chat" (the conversation), then the app's views
// marked `nav` as tabs. The app's panels marked `nav` open from buttons on the right, next to
// the side pane they open in.

import { h, icon, clear, $ } from "./dom.js";
import { registry } from "./ext.js";

export class Topbar {
  constructor(api) {
    this.api = api;
    this.tabs = $("#view-tabs");
    this.launchers = $("#panel-launchers");
    this.view = null;
  }

  render() {
    const views = [...registry.views.values()].filter((v) => v.nav);
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
    for (const p of registry.panels.values()) {
      if (!p.nav) continue;
      const label = p.nav === true ? p.title : p.nav;
      this.launchers.append(h("button.icon-btn", { type: "button", title: label, "aria-label": label, "data-panel": p.id, onclick: () => this.api.openPanel(p.id) }, icon(p.icon || "box")));
    }
    this.mark(this.view);
  }

  /// Mark the view the main column shows (null: the conversation).
  mark(id) {
    this.view = id;
    for (const t of this.tabs.children) t.setAttribute("aria-selected", String((t.dataset.view || null) === id));
  }
}
