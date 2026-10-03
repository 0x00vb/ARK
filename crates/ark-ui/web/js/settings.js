// Settings: the app's and the account's configuration, as one page in the main column. ARK
// draws the page and lists the sections apps register with `ark.setting`; it has none of its
// own. A project's own settings (name, folder) are in its menu in the sidebar.

import { h, icon, clear } from "./dom.js";
import { registry } from "./ext.js";

export const SETTINGS = "ark:settings";

export const settingsView = {
  id: SETTINGS,
  title: "Settings",
  render(el, params) {
    el.classList.add("settings");
    const list = h("nav.settings-list", { "aria-label": "Settings" });
    const body = h("div.settings-body");
    el.append(list, body);
    let current = null;
    let ctl = {};

    // The app's sections first, as registered; then ARK's own (Skills, Tool servers) by title.
    const ordered = () => {
      const all = [...registry.settings.values()];
      const own = all.filter((s) => s.builtin).sort((a, b) => a.title.localeCompare(b.title));
      return [...all.filter((s) => !s.builtin), ...own];
    };

    const pick = (id) => {
      const s = registry.settings.get(id) || ordered()[0];
      if (!s) {
        clear(body).append(h("p.settings-none", "Nothing to set here."));
        return;
      }
      // The same section again: a section added since still joins the list.
      if (current === s.id) {
        mark();
        return ctl.refresh?.();
      }
      try { ctl.dispose?.(); } catch (e) { console.error(e); }
      current = s.id;
      const host = h("div.settings-section", { "data-id": s.id });
      clear(body).append(h("h1.settings-title", s.title), host);
      try {
        ctl = s.render(host, window.ark) || {};
      } catch (e) {
        console.error(e);
        host.append(h("p.settings-none", String(e)));
        ctl = {};
      }
      mark();
    };

    // A list only when there is more than one section.
    const mark = () => {
      clear(list);
      list.hidden = registry.settings.size < 2;
      for (const s of ordered()) {
        list.append(h("button.nav-row", { type: "button", "aria-current": String(s.id === current), onclick: () => pick(s.id) },
          h("span.nav-icon", icon(s.icon || "settings")), h("span.nav-label", s.title)));
      }
    };

    pick(params?.section);
    return {
      update: (p) => pick(p?.section),
      refresh: () => { mark(); ctl.refresh?.(); },
      focus: () => ctl.focus?.() || null,
    };
  },
};
