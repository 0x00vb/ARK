// Settings: the app's and the account's configuration, as one page in the main column. ARK
// draws the page, lists the sections apps register with `ark.setting`, and adds its own
// (Appearance, and the runtime's when the host has them). A section marked `advanced` is
// listed under Advanced, after the others. A project's own settings (name, folder) are in its
// menu in the sidebar.

import { h, icon, clear } from "./dom.js";
import { registry } from "./ext.js";

export const SETTINGS = "ark:settings";

/// The app's sections first, as registered, then ARK's by title; advanced ones after both.
function ordered() {
  const all = [...registry.settings.values()];
  const group = (advanced) => {
    const in_ = all.filter((s) => !!s.advanced === advanced);
    const own = in_.filter((s) => s.builtin).sort((a, b) => a.title.localeCompare(b.title));
    return [...in_.filter((s) => !s.builtin), ...own];
  };
  return [...group(false), ...group(true)];
}

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
      let advanced = false;
      for (const s of ordered()) {
        if (s.advanced && !advanced) {
          advanced = true;
          list.append(h("div.settings-group", "Advanced"));
        }
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

/// A row of a settings list: what it is on the left, its control on the right.
export function settingRow(title, detail, ...controls) {
  return h("div.plugin-row.setting-row",
    h("div.plugin-main", h("div.plugin-name", title), detail ? h("div.plugin-detail", detail) : null),
    h("div.plugin-actions", ...controls));
}

/// Settings › Appearance: the window's theme (see js/theme.js).
export function registerAppearance(ark) {
  ark.setting({
    id: "appearance",
    title: "Appearance",
    icon: "sun",
    builtin: true,
    render(el) {
      const segs = h("div.segs", { role: "radiogroup", "aria-label": "Theme" });
      const draw = () => {
        const now = window.arkTheme?.choice() || "system";
        clear(segs).append(...[["light", "Light"], ["dark", "Dark"], ["system", "System"]].map(([id, label]) =>
          h("button.seg", { type: "button", role: "radio", "aria-checked": String(now === id), "aria-pressed": String(now === id), onclick: () => { window.arkTheme?.set(id); draw(); } }, label)));
      };
      draw();
      el.append(h("div.plugin-list", settingRow("Theme", null, segs)));
    },
  });
}
