// Projects and their chats. A project is a folder; its chats are listed beneath it. The current
// project is unfolded, the others fold away and unfold with their chevron. Everything comes from
// the host (`projects`, `sessions`); nothing is kept here that could drift from the store, except
// which projects the person folded or unfolded since they last changed project.

import { h, icon, clear, ago, clip, touch, $ } from "./dom.js";
import { registry } from "./ext.js";
import { SETTINGS } from "./settings.js";
import { uploadFolder } from "./drop.js";

// Chats listed per project before "Show more".
const SHOWN = 8;

export class Sidebar {
  constructor(api) {
    this.api = api;
    this.tree = $("#projects");
    this.status = $("#status-ext");
    this.top = $("#sidebar-ext");
    this.settings = $("#open-settings");
    this.gen = 0;
    // Project id -> unfolded (true) or folded (false), where the person chose; else the
    // current project is unfolded and the others folded.
    this.folds = {};
    this.current = null;
    this.all = new Set();
    // Chats running a turn now, and their projects (`working` from the host).
    this.working = new Set();
    this.workingProjects = new Set();
    $("#new-session").addEventListener("click", () => api.newSession());
    $("#add-project").addEventListener("click", (e) => this.addMenu(e.currentTarget));
    this.settings.addEventListener("click", () => api.openSettings());
  }

  /// Mark the chats that are working; a folded project shows it on its row.
  setWorking(list) {
    const ids = new Set(list.map((w) => w.session));
    const same = ids.size === this.working.size && [...ids].every((x) => this.working.has(x));
    this.working = ids;
    this.workingProjects = new Set(list.map((w) => w.project));
    if (!same) this.refresh();
  }

  unfolded(p) {
    return this.folds[p.id] ?? p.current;
  }

  fold(p, open) {
    if (open === p.current) delete this.folds[p.id];
    else this.folds[p.id] = open;
    this.refresh();
  }

  async refresh(currentId) {
    if (currentId !== undefined) this.currentId = currentId;
    const gen = ++this.gen;
    let projects;
    try {
      projects = await this.api.call("projects");
    } catch {
      return; // busy while a turn runs; the list refreshes when it ends
    }
    // Another project is current: start from its defaults again.
    const current = projects.find((p) => p.current)?.id || null;
    if (current !== this.current) {
      this.current = current;
      this.folds = {};
    }
    const chats = await Promise.all(projects.map((p) =>
      this.unfolded(p) ? this.api.call("sessions", { project: p.id }).catch(() => []) : null));
    // A newer refresh started meanwhile; it draws.
    if (gen !== this.gen) return;
    clear(this.tree);
    projects.forEach((p, i) => this.tree.append(this.project(p, chats[i])));
  }

  project(p, sessions) {
    const open = this.unfolded(p);
    const row = h("div.project-row", {
      role: "treeitem", tabindex: "0", "aria-expanded": String(open), "aria-current": p.current ? "true" : null,
      title: p.workspace || "No folder yet",
    },
      h("button.project-chev", { type: "button", tabindex: "-1", "aria-label": open ? "Fold" : "Unfold", onclick: (e) => { e.stopPropagation(); this.fold(p, !open); } }, icon("chevron-right")),
      h("span.project-name", p.name),
      !open && this.workingProjects.has(p.id) ? h("span.working-dot", { title: "Working", "aria-label": "Working" }) : null,
      h("button.icon-btn.small.project-more", { type: "button", title: "Project", "aria-label": `${p.name}: options`, onclick: (e) => { e.stopPropagation(); this.projectMenu(e.currentTarget, p); } }, icon("more")));
    // The current project folds; another one opens where it left off.
    const pick = () => (p.current ? this.fold(p, !open) : this.openProject(p));
    row.addEventListener("click", pick);
    row.addEventListener("keydown", (e) => e.key === "Enter" && e.target === row && pick());
    row.addEventListener("dblclick", (e) => { e.preventDefault(); this.renameProject(row, p); });
    return h("div.project", { role: "group", "data-id": p.id }, row, open ? this.chats(p, sessions || []) : null);
  }

  chats(p, sessions) {
    // A chat shows once it has a turn, or while its first one runs.
    const shown = sessions.filter((s) => s.turns > 0 || s.working || this.working.has(s.id));
    if (!shown.length) return null;
    const box = h("div.chats", { role: "group" });
    const all = this.all.has(p.id);
    (all ? shown : shown.slice(0, SHOWN)).forEach((s, i) => box.append(this.chat(s, i)));
    if (shown.length > SHOWN) {
      box.append(h("button.chats-more", { type: "button", onclick: () => { all ? this.all.delete(p.id) : this.all.add(p.id); this.refresh(); } },
        all ? "Show less" : `Show ${shown.length - SHOWN} more`));
    }
    return box;
  }

  chat(s, i) {
    const title = s.title || "Untitled";
    const row = h("div.session", {
      role: "treeitem", tabindex: "0", "data-id": s.id, "aria-current": s.id === this.currentId ? "true" : null,
      style: { "--i": Math.min(i, 12) },
    },
      h("span.session-title", clip(title, 60)),
      // A chat that runs on another computer: its mark, muted while that computer is away.
      s.place ? h("span.session-place", { title: `Runs on ${s.place.name}`, "data-state": s.place.state }, icon("server")) : null,
      // A chat working in the background: a dot where its time would be.
      s.working || this.working.has(s.id)
        ? h("span.working-dot", { title: "Working", "aria-label": "Working" })
        : h("span.session-time", ago(s.updated_at)));
    row.title = `${title}\n${s.turns} turn${s.turns === 1 ? "" : "s"} · double-click to rename`;
    row.addEventListener("click", () => this.api.openSession(s.id));
    row.addEventListener("keydown", (e) => e.key === "Enter" && this.api.openSession(s.id));
    row.addEventListener("dblclick", () => this.inline(row, ".session-title", s.title || "", "Chat title", (title) => this.api.call("rename_session", { id: s.id, title })));
    return row;
  }

  /// Mark the view the main column shows (none: the conversation).
  markView(id) {
    this.view = id;
    this.tree.dataset.dim = String(!!id);
    this.settings.setAttribute("aria-current", String(id === SETTINGS));
  }

  openProject(p) {
    return this.api.openProject(p.id);
  }

  renameProject(row, p) {
    this.inline(row, ".project-name", p.name, "Project name", (name) => this.api.call("rename_project", { id: p.id, name }));
  }

  /// Edit a row's label in place; `save` gets the new text when it changed.
  inline(row, sel, value, label, save) {
    const el = row.querySelector(sel);
    if (!el) return;
    const input = h("input.session-rename", { value, "aria-label": label });
    input.addEventListener("click", (e) => e.stopPropagation());
    el.replaceWith(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (keep) => {
      if (done) return;
      done = true;
      const text = input.value.trim();
      if (keep && text && text !== value) {
        try {
          await save(text);
        } catch (e) {
          this.api.toast(e.message);
        }
      }
      this.refresh();
    };
    input.addEventListener("blur", () => finish(true));
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") finish(true);
      if (e.key === "Escape") finish(false);
    });
  }

  projectMenu(anchor, p) {
    const item = (label, run) => h("button.menu-item", { type: "button", onclick: () => { menu.close(); run(); } }, h("span", label));
    const menu = this.api.popover(anchor, [
      h("div.menu-sub.pad", { title: p.workspace || "" }, p.workspace ? tail(p.workspace, 44) : "No folder yet"),
      ...(p.workspace ? [item("Open Terminal", () => this.api.openView("project-terminal", { project: p }))] : []),
      item("New chat", async () => {
        if (!p.current) await this.openProject(p);
        this.api.newSession();
      }),
      item("Rename", () => {
        const row = this.tree.querySelector(`.project[data-id="${p.id}"] .project-row`);
        if (row) this.renameProject(row, p);
      }),
      item(p.workspace ? "Change folder…" : "Choose folder…", async () => {
        const path = await this.folder(anchor, "Choose the folder this project works in");
        if (!path) return;
        try {
          await this.api.call("project_folder", { id: p.id, path });
        } catch (e) {
          this.api.toast(e.message);
        }
      }),
      // The app's own actions on a project (`ark.action({ where: "project" })`).
      ...registry.actions
        .filter((a) => {
          if (a.where !== "project") return false;
          try { return !a.when || a.when({ project: p }); } catch { return false; }
        })
        .map((a) => item(a.label, async () => {
          try { await a.run({ project: p }, window.ark); } catch (e) { this.api.toast(e.message || String(e)); }
        })),
    ]);
  }

  addMenu(anchor) {
    const name = h("input.menu-input", { placeholder: "New project…", "aria-label": "New project name" });
    const item = (label, run) => h("button.menu-item", { type: "button", onclick: () => { menu.close(); run(); } }, h("span", label));
    // A folder on the runtime's computer: picked in the window, typed in a browser there. A
    // folder on this device, wherever the runtime runs: sent (a browser cannot name a path).
    const menu = this.api.popover(anchor, [
      (this.api.canPickFolder || this.api.isLocal()) && item("Choose a folder…", async () => {
        const path = await this.folder(anchor, "Open a folder as a project");
        if (path) this.api.addProject({ path });
      }),
      !this.api.canPickFolder && item("Upload a folder…", () => uploadFolder(this.api)),
      h("div.menu-sep"),
      name,
    ].filter(Boolean));
    name.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && name.value.trim()) {
        menu.close();
        this.api.addProject({ name: name.value.trim() });
      }
    });
    if (!touch()) name.focus();
  }

  /// A folder from the window's picker; where there is none (a browser), a typed path.
  async folder(anchor, title) {
    const picked = await this.api.pickFolder({ title });
    if (picked !== undefined) return picked;
    return new Promise((done) => {
      const input = h("input.menu-input", { placeholder: "/full/path/to/folder", "aria-label": title, spellcheck: "false" });
      const menu = this.api.popover(anchor, [h("div.menu-head", title), input]);
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && input.value.trim()) {
          menu.close();
          done(input.value.trim());
        }
      });
      input.focus();
    });
  }

  /// The app's rows above the projects and status items, and Settings once an app has
  /// registered a section.
  renderExtensions() {
    this.settings.hidden = !registry.settings.size;
    clear(this.top);
    this.top.hidden = !registry.sidebars.length;
    for (const s of registry.sidebars) {
      const el = h("div.sidebar-item", { "data-id": s.id });
      this.top.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
    clear(this.status);
    for (const s of registry.statuses) {
      const el = h("div.status-item", { "data-id": s.id });
      this.status.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
  }
}

/// The end of a path, which names the folder: `…/code/project`.
function tail(path, n) {
  return path.length > n ? `…${path.slice(path.length - n + 1)}` : path;
}
