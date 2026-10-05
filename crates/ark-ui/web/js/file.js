// A file of the project in its own tab: the same file the agent reads and changes. The host
// says what kind it is (ark-files decides, once); this module picks the view for that kind,
// saves the person's changes as the same edits the agent sends, and follows changes made
// elsewhere (the agent, another agent, a program it ran, another app) by the file's version.
// A stored artifact that is a copy of a project file opens here too, as the file.
//
// A view is `render(el, file, ctx)` returning `{ update(file), focus(), dirty(), flush() }`.
// `ctx.save(edits)` saves and answers the file as saved; `ctx.status(state)` shows saving.

import { h, icon, clear } from "./dom.js";
import { markdown } from "./md.js";
import { sheetView } from "./sheet.js";
import { documentView } from "./document.js";

/// The view for each kind of file (`ark_files::Kind`). A new kind is one more entry; a new
/// format of an existing kind needs none.
const VIEWS = { text: textView, markdown: markdownView, sheet: sheetView, document: documentView, pdf: pdfView, image: imageView };

export const KIND_ICON = { text: "file", markdown: "doc", sheet: "sheet", document: "doc", pdf: "doc", image: "image" };

/// Extensions and the kind each reads as, and each kind's name, from the host
/// (`hello.file_kinds`, `hello.kind_nouns`).
const KINDS = new Map();
const NOUNS = new Map();
export function setFileKinds(map, nouns) {
  KINDS.clear();
  for (const [ext, kind] of Object.entries(map || {})) KINDS.set(ext, kind);
  NOUNS.clear();
  for (const [kind, noun] of Object.entries(nouns || {})) NOUNS.set(kind, noun);
}

/// What a kind is called, capitalized for a card ("Spreadsheet", "PDF").
export function kindNoun(kind) {
  const n = NOUNS.get(kind) || "file";
  return n[0].toUpperCase() + n.slice(1);
}

/// The kind of a file from its name, before it is read.
export function kindOf(path) {
  const ext = String(path).split("/").pop().split(".").slice(1).pop()?.toLowerCase();
  return (ext && KINDS.get(ext)) || "text";
}

export function fileView(el, path, ark, params) {
  el.classList.add("fv");
  const s = { path, file: null, view: null, version: null, busy: 0, offs: [], alive: true, ctl: null };
  const status = h("span.fv-status", { "aria-live": "polite" });
  const show = (state) => {
    status.dataset.state = state || "";
    status.title = { saving: "Saving", saved: "Saved", error: "Not saved" }[state] || "";
    if (state === "saved") {
      clearTimeout(s.statusTimer);
      s.statusTimer = setTimeout(() => (status.dataset.state = ""), 1400);
    }
  };
  const tools = h("div.fv-tools", status,
    ark.isLocal() && kindOf(path) !== "text"
      ? h("button.icon-btn", { type: "button", title: "Open in its app", "aria-label": "Open in its app", onclick: () => ark.call("file_open", { path: s.path }).catch((e) => ark.toast(e.message)) }, icon("external"))
      : null);
  const body = h("div.fv-body");
  el.append(body);

  const ctx = {
    ark,
    tools,
    status: show,
    // A view that saves on its own path (text) tells which version it wrote.
    saved: (version) => (s.version = version),
    focusChanged: () => ark.reportFocus(),
    async save(edits, { base = true } = {}) {
      s.busy++;
      show("saving");
      try {
        const file = await ark.call("file_edit", { path: s.path, edits, base: base ? s.version : undefined });
        s.version = file.version;
        s.file = file;
        show("saved");
        return file;
      } catch (e) {
        show("error");
        if (/changed since it was read/.test(e.message)) {
          await load(true);
          ark.toast("Reloaded with the latest changes");
          return null;
        }
        ark.toast(e.message);
        throw e;
      } finally {
        s.busy--;
      }
    },
  };

  async function load(force) {
    let file;
    try {
      file = await ark.call("file_read", { path: s.path });
    } catch (e) {
      return clear(body).append(h("div.wb-empty", e.message));
    }
    if (!s.alive) return;
    if (!force && file.version && file.version === s.version) return;
    s.version = file.version;
    s.file = file;
    const kind = file.text == null && !file.kind ? null : file.kind || "text";
    if (!kind || (kind === "text" && file.text == null)) {
      s.ctl?.dispose?.();
      s.ctl = null;
      return clear(body).append(h("div.wb-empty", `${file.path.split("/").pop()} is ${file.why || "not shown here"}.`));
    }
    if (s.ctl && s.kind === kind && s.ctl.update) return s.ctl.update(file);
    s.ctl?.dispose?.();
    s.kind = kind;
    clear(body);
    s.ctl = VIEWS[kind](body, file, ctx, params) || {};
    ark.reportFocus();
  }

  /// Someone else may have changed the file: read it again unless the person has unsaved
  /// changes (their save then meets the new version and reloads).
  async function check() {
    if (!s.alive || s.busy || s.ctl?.dirty?.()) return;
    try {
      const { version } = await ark.call("file_version", { path: s.path });
      if (version !== s.version) await load(false);
    } catch {}
  }
  const wrote = (files) => (files || []).includes(s.path) && setTimeout(check, 60);
  s.offs.push(ark.on("ark:event", (m) => m.event?.type === "tool_result" && wrote(m.event.files)));
  s.offs.push(ark.on("ark:orch", (m) => wrote(m.activity?.files)));
  const onFocus = () => check();
  addEventListener("focus", onFocus);
  load(true);

  return {
    focus: () => {
      const base = { surface: { kind_tag: s.kind || "file", title: s.path.split("/").pop(), path: s.path } };
      try {
        const f = s.ctl?.focus?.();
        if (f?.selection) base.selection = f.selection;
      } catch {}
      return base;
    },
    update: (p) => p?.locator && s.ctl?.reveal?.(p.locator),
    refresh: () => check(),
    dispose() {
      s.alive = false;
      s.ctl?.flush?.();
      s.ctl?.dispose?.();
      removeEventListener("focus", onFocus);
      for (const off of s.offs) off();
    },
  };
}

// ---- text --------------------------------------------------------------------------------------

/// Text, as before: a plain editor, saved with ⌘S or Save.
function textView(el, file, ctx, params) {
  const { ark } = ctx;
  const s = { text: file.text, dirty: false, line: 1, version: file.version, path: file.path };
  const name = file.path.split("/").pop();
  const area = h("textarea.wb-editor", { spellcheck: "false", "aria-label": name });
  const save = h("button.wb-save", { type: "button", disabled: true, onclick: () => store() }, "Save");
  const state = h("span.wb-status");
  const mark = () => {
    s.dirty = area.value !== s.text;
    save.disabled = !s.dirty;
    state.textContent = s.dirty ? "Edited" : "";
  };
  const where = () => {
    s.line = area.value.slice(0, area.selectionStart).split("\n").length;
    ctx.focusChanged();
  };
  async function store() {
    if (!s.dirty) return;
    try {
      const r = await ark.call("file_write", { path: s.path, text: area.value, base: s.version });
      s.version = r.version;
      ctx.saved(r.version);
      s.text = area.value;
      mark();
      ark.toast(`Saved ${name}`);
    } catch (e) {
      ark.toast(e.message);
    }
  }
  area.value = file.text;
  area.addEventListener("input", mark);
  area.addEventListener("click", where);
  area.addEventListener("keyup", where);
  area.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") { e.preventDefault(); e.stopPropagation(); store(); }
  });
  el.append(h("div.wb-bar", h("span.wb-file-name", file.path), h("span.spacer"), state, save, ctx.tools), area);
  if (params?.locator && /^\d+$/.test(params.locator)) goToLine(area, Number(params.locator));
  return {
    focus: () => ({ selection: { line: s.line } }),
    dirty: () => s.dirty,
    update(f) {
      if (s.dirty) return;
      s.text = area.value = f.text;
      s.version = f.version;
      mark();
    },
    reveal: (loc) => /^\d+$/.test(loc) && goToLine(area, Number(loc)),
  };
}

function goToLine(area, n) {
  const lines = area.value.split("\n");
  const at = lines.slice(0, Math.max(0, n - 1)).join("\n").length + (n > 1 ? 1 : 0);
  area.focus();
  area.setSelectionRange(at, at);
}

// ---- Markdown ----------------------------------------------------------------------------------

/// Markdown as a page, with the project's images it links to; Edit shows the text to change.
function markdownView(el, file, ctx, params) {
  let edit = null;
  let page = null;
  let current = file;
  const show = () => {
    revokeAll(el);
    clear(el);
    edit = null;
    const toggle = h("button.btn.small", { type: "button", onclick: () => editing() }, "Edit");
    page = h("div.prose.md-page");
    el.append(h("div.wb-bar", h("span.wb-file-name", current.path), h("span.spacer"), toggle, ctx.tools), page);
    page.innerHTML = markdown(current.text, { files: true });
    for (const img of page.querySelectorAll("img[data-file]")) {
      media(ctx.ark, near(current.path, img.dataset.file))
        .then((m) => {
          keep(el, m.url);
          img.src = m.url;
        })
        .catch(() => img.replaceWith(h("span.md-missing", img.alt || img.dataset.file)));
    }
  };
  const editing = () => {
    clear(el);
    const box = h("div.fv-body");
    el.append(box);
    edit = textView(box, current, ctx, params);
    const bar = box.querySelector(".wb-bar");
    bar?.insertBefore(h("button.btn.small", { type: "button", onclick: () => done() }, "Done"), bar.querySelector(".wb-save"));
  };
  // Back to the page, as the file now is (the person may have saved).
  const done = async () => {
    if (edit?.dirty?.()) return ctx.ark.toast("Save or undo your changes first");
    try {
      current = await ctx.ark.call("file_read", { path: current.path });
    } catch {}
    show();
  };
  show();
  return {
    dirty: () => !!edit?.dirty?.(),
    focus: () => edit?.focus?.() || null,
    update(f) {
      current = f;
      if (edit) edit.update(f);
      else show();
    },
    dispose: () => revokeAll(el),
  };
}

/// `rel` as written in the document at `doc`, as a path from the project folder.
function near(doc, rel) {
  const parts = doc.split("/").slice(0, -1);
  for (const p of rel.split("/")) {
    if (p === "..") parts.pop();
    else if (p && p !== ".") parts.push(p);
  }
  return parts.join("/");
}

// ---- PDFs and images: the file's own bytes -----------------------------------------------------

/// A file's bytes as a URL this page can show (`file_media`). Freed when its view goes.
async function media(ark, path) {
  const m = await ark.call("file_media", { path });
  const bytes = Uint8Array.from(atob(m.data), (c) => c.charCodeAt(0));
  return { url: URL.createObjectURL(new Blob([bytes], { type: m.media_type })), ...m };
}

const URLS = new WeakMap();
function keep(el, url) {
  const list = URLS.get(el) || [];
  list.push(url);
  URLS.set(el, list);
}
function revokeAll(el) {
  for (const u of URLS.get(el) || []) URL.revokeObjectURL(u);
  URLS.delete(el);
}

/// A view that shows the file itself, drawn again when its version changes.
function mediaView(draw) {
  return (el, file, ctx) => {
    let version = null;
    const body = h("div.media-body");
    el.append(h("div.wb-bar", h("span.wb-file-name", file.path), h("span.spacer"), ctx.tools), body);
    const load = async (f) => {
      if (f.version && f.version === version) return;
      version = f.version;
      try {
        const m = await media(ctx.ark, f.path);
        revokeAll(body);
        keep(body, m.url);
        clear(body).append(draw(m, f));
      } catch (e) {
        clear(body).append(h("div.wb-empty", e.message));
      }
    };
    load(file);
    return { update: load, dispose: () => revokeAll(body) };
  };
}

function imageView(el, file, ctx) {
  return mediaView((m, f) => h("img.media-image", { src: m.url, alt: f.name }))(el, file, ctx);
}

/// The PDF's pages, drawn by the browser's own viewer.
function pdfView(el, file, ctx) {
  return mediaView((m, f) => h("iframe.media-pdf", { src: m.url, title: f.name }))(el, file, ctx);
}
