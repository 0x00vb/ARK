// Lightweight navigation: the app, new session, the app's own entries, the project, and its
// sessions. Everything comes from the host (`sessions`, `projects`); nothing is kept here that
// could drift from the store.

import { h, icon, clear, ago, clip, $ } from "./dom.js";
import { registry } from "./ext.js";

export class Sidebar {
  constructor(api) {
    this.api = api;
    this.list = $("#session-list");
    this.nav = $("#nav-ext");
    this.status = $("#status-ext");
    $("#new-session").addEventListener("click", () => api.newSession());
    $("#project-switch").addEventListener("click", (e) => this.projectMenu(e.currentTarget));
  }

  brand(summary, ui) {
    $("#brand-name").textContent = summary.name;
    const logo = $("#brand-logo");
    clear(logo);
    if (ui.logo) logo.append(h("img", { src: ui.logo, alt: "" }));
    else logo.textContent = (summary.name || "A").slice(0, 1);
  }

  setProject(p) {
    $("#project-name").textContent = p?.name || "";
  }

  async refresh(currentId) {
    let sessions;
    try {
      sessions = await this.api.call("sessions", { all: false });
    } catch {
      return; // busy while a turn runs; the list refreshes when it ends
    }
    clear(this.list);
    const shown = sessions.filter((s) => s.turns > 0);
    if (!shown.length) this.list.append(h("div.session-empty", "No sessions yet"));
    shown.forEach((s, i) => {
      const title = s.title || (s.turns ? "Untitled" : "New session");
      const row = h("div.session", {
        role: "listitem", tabindex: "0", "data-id": s.id, "aria-current": s.id === currentId ? "true" : null,
        style: { "--i": Math.min(i, 12) },
      },
        h("span.session-title", clip(title, 60)),
        h("span.session-time", ago(s.updated_at)));
      row.title = `${title}\n${s.turns} turn${s.turns === 1 ? "" : "s"} · double-click to rename`;
      row.addEventListener("click", () => this.api.openSession(s.id));
      row.addEventListener("keydown", (e) => e.key === "Enter" && this.api.openSession(s.id));
      row.addEventListener("dblclick", () => this.rename(row, s));
      this.list.append(row);
    });
  }

  /// Mark the view the main column shows (none: the conversation).
  markView(id) {
    this.view = id;
    for (const row of this.nav.querySelectorAll("[data-view]")) {
      row.setAttribute("aria-current", String(row.dataset.view === id));
    }
    this.list.dataset.dim = String(!!id);
  }

  rename(row, s) {
    const input = h("input.session-rename", { value: s.title || "", "aria-label": "Session title" });
    const titleEl = row.querySelector(".session-title");
    titleEl.replaceWith(input);
    input.focus();
    input.select();
    const done = async (save) => {
      input.removeEventListener("blur", onBlur);
      if (save && input.value.trim() && input.value.trim() !== s.title) {
        try {
          await this.api.call("rename_session", { id: s.id, title: input.value.trim() });
        } catch (e) {
          this.api.toast(e.message);
        }
      }
      this.api.refreshSidebar();
    };
    const onBlur = () => done(true);
    input.addEventListener("blur", onBlur);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") done(true);
      if (e.key === "Escape") done(false);
    });
  }

  async projectMenu(anchor) {
    let projects = [];
    try {
      projects = await this.api.call("projects");
    } catch (e) {
      return this.api.toast(e.message);
    }
    const input = h("input.menu-input", { placeholder: "New project…", "aria-label": "New project name" });
    const menu = this.api.popover(anchor, [
      ...projects.map((p) => h("button.menu-item", { "aria-current": p.current ? "true" : null, onclick: () => { menu.close(); if (!p.current) this.api.switchProject(p.name); } },
        h("span", p.name), p.target ? h("span.menu-sub", clip(p.target, 40)) : null)),
      h("div.menu-sep"),
      input,
    ]);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && input.value.trim()) {
        menu.close();
        this.api.switchProject(input.value.trim());
      }
    });
  }

  /// The app's own navigation (views and panels marked `nav`) and status items.
  renderExtensions() {
    clear(this.nav);
    for (const v of registry.views.values()) {
      if (!v.nav) continue;
      const row = h("button.nav-row", { "data-view": v.id, onclick: () => this.api.openView(v.id) },
        h("span.nav-icon", icon(v.icon || "box")), h("span.nav-label", v.nav === true ? v.title : v.nav));
      this.nav.append(row);
    }
    for (const p of registry.panels.values()) {
      if (!p.nav) continue;
      const row = h("button.nav-row", { onclick: () => this.api.openPanel(p.id) },
        h("span.nav-icon", icon(p.icon || "box")), h("span.nav-label", p.nav === true ? p.title : p.nav));
      this.nav.append(row);
    }
    this.markView(this.view);
    clear(this.status);
    for (const s of registry.statuses) {
      const el = h("div.status-item", { "data-id": s.id });
      this.status.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
  }
}
