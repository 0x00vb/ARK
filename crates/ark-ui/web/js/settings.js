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
  const primary = ["account", "models", "appearance", "startup", "computers", "devices", "permissions"];
  const rank = section => { const index = primary.indexOf(section.id); return index < 0 ? -1 : index; };
  return [...group(false).sort((a, b) => rank(a) - rank(b)), ...group(true)];
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
    let advancedOpen = false;

    const pick = (id) => {
      const s = registry.settings.get(id) || ordered()[0];
      if (!s) {
        clear(body).append(h("p.settings-none", "Nothing to set here."));
        return;
      }
      if (s.advanced) advancedOpen = true;
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
          list.append(h("button.nav-row.settings-advanced", { type: "button", "aria-expanded": String(advancedOpen), onclick: () => { advancedOpen = !advancedOpen; mark(); list.querySelector(".settings-advanced")?.focus(); } },
            h("span.nav-icon", icon("settings")), h("span.nav-label", "Advanced")));
        }
        list.append(h("button.nav-row", { type: "button", hidden: s.advanced && !advancedOpen, "aria-current": String(s.id === current), onclick: () => pick(s.id) },
          h("span.nav-icon", icon(s.icon || "settings")), h("span.nav-label", s.title)));
      }
    };

    pick(params?.section);
    // On a phone the sections are a row: the one shown is in view.
    list.querySelector('[aria-current="true"]')?.scrollIntoView({ block: "nearest", inline: "center" });
    return {
      update: (p) => pick(p?.section),
      refresh: () => { mark(); ctl.refresh?.(); },
      focus: () => ctl.focus?.() || null,
      dispose: () => ctl.dispose?.(),
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
      const modes = [["system", "System"], ["light", "Light"], ["dark", "Dark"]];
      const names = { light: "Light", dark: "Dark", monokai: "Monokai", dracula: "Dracula", github: "GitHub", xcode: "Xcode" };
      const group = (name, className) => h(`div.${className}`, { role: "radiogroup", "aria-label": name });
      const themeGroup = h("div.appearance-theme-group");
      const modeGroup = group("Mode", "appearance-mode-grid");
      const paletteGroup = group("Palette", "appearance-palette-grid");
      const materialGroup = group("Window style", "appearance-material-grid");
      let pendingFocus = null;
      const preview = (kind) => h("span.appearance-preview", { "data-kind": kind, "aria-hidden": "true" },
        h("span.appearance-preview-rail"),
        h("span.appearance-preview-page", h("i"), h("i"), h("i")));
      const option = (id, label, name, checked, change, kind = id) =>
        h("label.appearance-choice", { "data-palette": id },
          h("input", { type: "radio", name, value: id, checked, onchange: change }),
          preview(kind), h("span.appearance-choice-name", label));
      const draw = () => {
        const focused = pendingFocus || (el.contains(document.activeElement)
          ? { name: document.activeElement.name, value: document.activeElement.value }
          : null);
        pendingFocus = null;
        const now = window.arkTheme?.mode?.() || "system";
        clear(modeGroup).append(...modes.map(([id, label]) =>
          option(id, label, "ark-theme-mode", now === id, () => { pendingFocus = { name: "ark-theme-mode", value: id }; window.arkTheme?.setMode(id); })));
        const currentPalette = window.arkTheme?.palette?.() || document.documentElement.dataset.theme;
        const palettes = window.arkTheme?.palettes?.() || ["light", "github", "xcode"];
        clear(paletteGroup).append(...palettes.map((id) =>
          option(id, names[id], "ark-theme-palette", currentPalette === id, () => { pendingFocus = { name: "ark-theme-palette", value: id }; window.arkTheme?.setPalette(id); })));
        const material = window.arkMaterial?.choice() || "solid";
        clear(materialGroup).append(...[["solid", "Solid"], ["translucent", "Translucent"]].map(([id, label]) =>
          option(id, label, "ark-material", material === id, () => { pendingFocus = { name: "ark-material", value: id }; window.arkMaterial?.set(id); }, `material-${id}`)));
        if (focused?.name) el.querySelector(`input[name="${focused.name}"][value="${focused.value}"]`)?.focus();
      };
      const keys = (container) => container.addEventListener("keydown", (e) => {
          if (e.key !== "Home" && e.key !== "End") return;
          e.preventDefault();
          const inputs = [...container.querySelectorAll("input")];
          const input = e.key === "Home" ? inputs[0] : inputs.at(-1);
          input?.focus();
          input?.click();
        });
      [modeGroup, paletteGroup, materialGroup].forEach(keys);
      const themeChanged = () => draw();
      window.addEventListener("ark:theme", themeChanged);
      draw();
      el.classList.add("appearance-settings");
      el.append(
        themeGroup);
      themeGroup.append(
        h("section.appearance-section", modeGroup));
      const more = h("details.settings-options", h("summary", "More appearance options"),
        h("section.appearance-section", h("h2", "Palette"), paletteGroup));
      if (document.querySelector('.app[data-shell="desktop"][data-os="macos"]'))
        more.append(h("section.appearance-section", h("h2", "Window"), materialGroup));
      el.append(more);
      return { dispose: () => window.removeEventListener("ark:theme", themeChanged) };
    },
  });
}
