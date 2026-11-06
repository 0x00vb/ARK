// The fixed rail holds the current project's destinations. Chat and Schedules are stable;
// extensions such as Agents and Board join the same rail. The controls become a bottom bar on
// a narrow window, so no destination is hidden inside the project drawer.

import { $, $$, clear, h, icon } from "./dom.js";
import { registry } from "./ext.js";

export class Destinations {
  constructor(open, openView) {
    this.open = open;
    this.openView = openView;
    this.el = $("#destinations");
    this.workspace = $("#workspace-destinations");
    this.pins = $("#rail-pins");
    this.mounts = [];
    this.buttons = $$(".destination[data-destination]", this.el);
    for (const button of this.buttons) {
      button.addEventListener("click", (event) => {
        this.open(button.dataset.destination, { keyboard: event.detail === 0, trigger: button });
      });
    }
  }

  /// Project views are destinations, not a second tab system in the title bar.
  setWorkspaceViews(views) {
    clear(this.workspace);
    for (const view of views) {
      const label = view.nav === true ? view.title : view.nav;
      const button = h("button.destination.workspace-destination", {
        type: "button", "data-view": view.id, title: view.title, "aria-label": label,
        onclick: (event) => this.openView(view.id, { keyboard: event.detail === 0, trigger: event.currentTarget }),
      }, h("span.destination-icon", icon(view.icon || "box")), h("span.destination-label", label));
      this.workspace.append(button);
    }
    this.markWorkspace(null);
  }

  /// What the app pinned to the rail. Each mount draws into its own element and is released
  /// before the next drawing, so a pin that left cannot keep listening.
  setRails() {
    for (const mount of this.mounts) {
      try { mount.dispose?.(); } catch (error) { console.error(error); }
    }
    this.mounts = [];
    clear(this.pins);
    for (const rail of registry.rails) {
      const el = h("div.rail-item", { "data-id": rail.id });
      this.pins.append(el);
      try {
        const mount = rail.render(el, window.ark);
        if (mount) this.mounts.push(mount);
      } catch (error) {
        console.error(error);
        el.remove();
      }
    }
  }

  markWorkspace(id) {
    for (const button of $$(".workspace-destination", this.workspace)) {
      if (button.dataset.view === id) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
  }

  mark(destination) {
    for (const button of this.buttons) {
      if (button.dataset.destination === destination) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
  }
}
