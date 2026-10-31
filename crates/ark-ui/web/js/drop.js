// Files and folders dropped on the conversation, or chosen with the composer's add button, go
// into the project's folder (ADR 0035), through the upload calls a folder sent from another
// computer uses (ADR 0030). The composer then names them, so the message the person sends tells
// the agent. A folder chosen from the sidebar becomes a new project the same way. Works the same
// with a runtime on this computer or elsewhere.

import { h, icon, $ } from "./dom.js";

/// The most bytes one call carries (base64 adds a third), and the most files.
const CALL = 2 * 1024 * 1024;
const CALL_FILES = 200;
/// Tries of one call when the connection fails; every upload call can be sent again.
const TRIES = 3;

/// One set of files on its way at a time, from a drop or a picker.
let busy = null;

export function acceptDrops(api, captureDraft, zone, { accepts, captureOrigin } = {}) {
  let depth = 0;
  const hint = h("div.drop-hint", { hidden: true }, h("div.drop-hint-box", icon("plus"), h("span", "Drop to add to the project")));
  zone.append(hint);
  // A document open in the stage takes its own drops.
  const ours = (e) => [...(e.dataTransfer?.types || [])].includes("Files")
    && (accepts ? accepts() : !e.target.closest?.(".stage"));
  const show = (on) => {
    hint.hidden = !on;
    if (!on) depth = 0;
  };

  zone.addEventListener("dragenter", (e) => {
    if (!ours(e)) return;
    e.preventDefault();
    depth++;
    show(true);
  });
  zone.addEventListener("dragover", (e) => {
    if (!ours(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  });
  zone.addEventListener("dragleave", (e) => {
    if (!ours(e)) return;
    if (--depth <= 0) show(false);
  });
  zone.addEventListener("drop", (e) => {
    if (!ours(e)) return;
    e.preventDefault();
    show(false);
    // The dropped items are readable only during this event: take them now.
    const entries = [...(e.dataTransfer.items || [])].map((i) => i.kind === "file" && i.webkitGetAsEntry?.()).filter(Boolean);
    const files = [...e.dataTransfer.files];
    // Entries see into folders. Files that have no place on a disk (made by a page) cannot be
    // read through them: then the files as dropped.
    const plain = () => files.map((file) => ({ path: file.name, file }));
    const target = captureOrigin ? null : api.at(api.summary().location);
    const origin = captureOrigin ? captureOrigin() : { uploadApi: target, appendDraft: captureDraft(target.location) };
    if (!origin) return;
    once(origin.uploadApi, async () => addToProject(origin.uploadApi, origin.appendDraft,
      entries.length ? await walkAll(entries).catch(plain) : plain(), origin.progressTarget));
  });
}

/// The system's file chooser (a phone has no drag and drop): the files go into the project.
export function chooseFiles(api, captureDraft, { captureOrigin, progressTarget } = {}) {
  const target = captureOrigin ? null : api.at(api.summary().location);
  const origin = captureOrigin ? captureOrigin() : {
    uploadApi: target,
    appendDraft: captureDraft(target.location),
    progressTarget,
  };
  choose({ multiple: true }, (files) => {
    once(origin.uploadApi, () => addToProject(origin.uploadApi, origin.appendDraft,
      files.map((file) => ({ path: file.name, file })), origin.progressTarget));
  });
}

/// A folder from this device becomes a new project on the runtime, wherever it runs (ADR 0030
/// calls), and opens. A browser cannot hand the runtime a path, so it sends the files.
export function uploadFolder(api) {
  const windowApi = Object.assign(Object.create(api), {
    call: (method, params) => api.windowCall({
      upload_parts: "upload_project_parts",
      upload_project: "upload_project_finish",
      upload_cancel: "upload_project_cancel",
    }[method] || method, params),
  });
  choose({ multiple: true, webkitdirectory: true }, (files) => once(windowApi, async () => {
    // `top/inner/file.txt`: the folder's name, then the path inside it.
    const name = (files[0]?.webkitRelativePath || "").split("/")[0] || "Project";
    const list = files.map((file) => ({ path: file.webkitRelativePath.split("/").slice(1).join("/") || file.name, file }));
    if (!list.length) return api.toast("There is nothing to add: only files are added, and the folders had none.");
    const progress = new Progress(list, "Uploading", name);
    try {
      const project = await send(windowApi, list, progress, { method: "upload_project", params: { name } });
      await api.openProject(project.id);
    } catch (e) {
      api.toast(progress.stopped ? "Stopped. Nothing was added." : `The folder was not uploaded: ${e.message}`);
    } finally {
      progress.remove();
    }
  }));
}

function once(api, work) {
  if (busy) return api.toast("Files are still being added. Wait for them, or stop them first.");
  busy = work().finally(() => (busy = null));
}

function choose(attrs, picked) {
  const input = h("input", { type: "file", hidden: true, ...attrs });
  input.addEventListener("change", () => {
    const files = [...(input.files || [])];
    input.remove();
    if (files.length) picked(files);
  });
  input.addEventListener("cancel", () => input.remove());
  document.body.append(input);
  input.click();
}

async function addToProject(api, complete, list, progressTarget) {
  if (!list.length) return api.toast("There is nothing to add: only files are added, and the folders had none.");
  const progress = new Progress(list, "Adding", undefined, progressTarget);
  try {
    const { added } = await send(api, list, progress, { method: "upload_add", params: {} });
    complete(`Added to the project: ${added.join(", ")}`);
  } catch (e) {
    api.toast(progress.stopped ? "Stopped. Nothing was added." : `The files were not added: ${e.message}`);
  } finally {
    progress.remove();
  }
}

/// Send `list` in calls of at most `CALL` bytes (a part of a large file, or many small ones),
/// then finish the upload with `last` (into the open project, or a new one). A stop or a
/// failure removes what was sent.
async function send(api, list, progress, last) {
  const upload = "drop-" + [...crypto.getRandomValues(new Uint8Array(12))].map((b) => b.toString(16).padStart(2, "0")).join("");
  const call = (method, params) => retry(() => api.call(method, { upload, ...params }));
  try {
    await call("upload_parts", { parts: [] });
    let batch = [];
    let bytes = 0;
    const flush = async () => {
      if (!batch.length) return;
      if (progress.stopped) throw new Error("stopped");
      await call("upload_parts", { parts: batch });
      progress.sent(bytes);
      batch = [];
      bytes = 0;
    };
    for (const { path, file } of list) {
      let offset = 0;
      do {
        const piece = file.slice(offset, offset + CALL - bytes);
        batch.push({ path, offset, data: await base64(piece) });
        bytes += piece.size;
        offset += piece.size;
        if (bytes >= CALL || batch.length >= CALL_FILES) await flush();
      } while (offset < file.size);
    }
    await flush();
    if (progress.stopped) throw new Error("stopped");
    return await call(last.method, last.params);
  } catch (e) {
    api.call("upload_cancel", { upload }).catch(() => {});
    throw e;
  }
}

/// The line above the composer while files are sent: what, how far, and Stop.
class Progress {
  constructor(list, verb, what, target) {
    this.total = list.reduce((n, f) => n + f.file.size, 0);
    this.done = 0;
    this.stopped = false;
    this.text = h("span.queued-t");
    this.what = what || (list.length === 1 ? list[0].path : `${list.length} files`);
    this.el = h("div.queued.adding", h("span.queued-k", verb), this.text,
      h("button.icon-btn.small", { title: "Stop", "aria-label": "Stop", onclick: () => this.stop() }, icon("x")));
    if (target) target.append(this.el);
    else $("#queue").before(this.el);
    this.paint();
  }
  sent(bytes) {
    this.done += bytes;
    this.paint();
  }
  paint() {
    const p = this.total ? Math.min(100, Math.round((this.done / this.total) * 100)) : 0;
    this.text.textContent = `${this.what} · ${p}%`;
    this.el.style.setProperty("--p", `${p}%`);
  }
  stop() {
    this.stopped = true;
    this.text.textContent = `${this.what} · stopping`;
  }
  remove() {
    this.el.remove();
  }
}

/// Every file under the dropped entries, with its path from the top of the drop.
async function walkAll(entries) {
  const out = [];
  for (const e of entries) await walk(e, "", out);
  return out;
}

async function walk(entry, dir, out) {
  const path = dir ? `${dir}/${entry.name}` : entry.name;
  if (entry.isFile) {
    out.push({ path, file: await new Promise((ok, no) => entry.file(ok, no)) });
  } else if (entry.isDirectory) {
    const reader = entry.createReader();
    // A folder answers in batches until one is empty.
    for (;;) {
      const batch = await new Promise((ok, no) => reader.readEntries(ok, no));
      if (!batch.length) break;
      for (const child of batch) await walk(child, path, out);
    }
  }
}

function base64(blob) {
  return new Promise((ok, no) => {
    const r = new FileReader();
    r.onload = () => ok(String(r.result).split(",", 2)[1] || "");
    r.onerror = () => no(r.error);
    r.readAsDataURL(blob);
  });
}

async function retry(fn) {
  for (let i = 1; ; i++) {
    try {
      return await fn();
    } catch (e) {
      // Only a lost connection (fetch throws a TypeError) is worth another try; the runtime's
      // own answer is final.
      if (i >= TRIES || !(e instanceof TypeError)) throw e;
    }
  }
}
