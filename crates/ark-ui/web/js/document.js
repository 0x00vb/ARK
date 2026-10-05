// A document: the blocks of the file the agent works on, editable in place. Typing changes a
// block; Enter starts a new one; "# ", "- " and "1. " at the start of a line make a heading or
// a list; ⌘B and ⌘I mark text. Changes save on their own a moment after typing stops, as the
// edits the agent sends (`{block, text}`, `{after, text}`, `{delete}`), so both always work on
// the one file.

import { h, clear } from "./dom.js";

/// Quiet time before typing is saved.
const SAVE_AFTER = 700;

// ---- inline Markdown (**bold**, *italic*), as ark-files reads and writes it -------------------

const WORD = /[\p{L}\p{N}]/u;

export function inlineRuns(text) {
  const out = [];
  const ch = [...String(text ?? "")];
  let b = false, it = false, cur = "";
  const push = () => { if (cur) { out.push({ t: cur, b, i: it }); cur = ""; } };
  const closes = (from, mark) => {
    const c = ch[from];
    if (c === undefined || /\s/.test(c)) return false;
    return ch.slice(from + 1).join("").includes(mark);
  };
  for (let i = 0; i < ch.length;) {
    const c = ch[i];
    if (c === "\\" && i + 1 < ch.length && "*_\\".includes(ch[i + 1])) { cur += ch[i + 1]; i += 2; continue; }
    if (c === "*" && ch[i + 1] === "*") {
      if (b || closes(i + 2, "**")) { push(); b = !b; i += 2; continue; }
    } else if ((c === "*" || c === "_") && (it || closes(i + 1, c))) {
      const inner = c === "_" && i > 0 && WORD.test(ch[i - 1]) && ch[i + 1] && WORD.test(ch[i + 1]);
      if (!inner) { push(); it = !it; i++; continue; }
    }
    cur += c;
    i++;
  }
  push();
  return out;
}

const escHtml = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);

export function inlineHtml(text) {
  return inlineRuns(text).map(({ t, b, i }) => {
    let x = escHtml(t).replace(/\n/g, "<br>");
    if (i) x = `<i>${x}</i>`;
    if (b) x = `<b>${x}</b>`;
    return x;
  }).join("");
}

function runsMd(runs) {
  let out = "";
  for (const { t, b, i } of runs) {
    if (!t) continue;
    const lead = t.length - t.trimStart().length;
    const trail = t.length - t.trimEnd().length;
    const core = t.slice(lead, t.length - trail);
    out += t.slice(0, lead);
    if (!core) { out += t.slice(lead); continue; }
    const esc = core.replace(/[\\*_]/g, (c) => `\\${c}`);
    out += b && i ? `***${esc}***` : b ? `**${esc}**` : i ? `*${esc}*` : esc;
    out += t.slice(t.length - trail);
  }
  return out;
}

/// An element's text as inline Markdown; a line break is `\n`.
export function inlineMd(el) {
  const runs = [];
  const push = (t, b, i) => {
    const last = runs[runs.length - 1];
    if (last && last.b === b && last.i === i) last.t += t;
    else runs.push({ t, b, i });
  };
  const walk = (n, b, i) => {
    for (const c of n.childNodes) {
      if (c.nodeType === Node.TEXT_NODE) push(c.data.replace(/ /g, " "), b, i);
      else if (c.nodeType === Node.ELEMENT_NODE) {
        if (c.tagName === "BR") { push("\n", b, i); continue; }
        const w = c.style?.fontWeight;
        const bold = b || c.tagName === "B" || c.tagName === "STRONG" || w === "bold" || Number(w) >= 600;
        const ital = i || c.tagName === "I" || c.tagName === "EM" || c.style?.fontStyle === "italic";
        walk(c, bold, ital);
      }
    }
  };
  walk(el, false, false);
  // An editable block keeps a trailing <br> so the caret has a line; it is not text.
  const last = runs[runs.length - 1];
  if (last && last.t.endsWith("\n")) last.t = last.t.slice(0, -1);
  return runsMd(runs);
}

// ---- blocks ----------------------------------------------------------------------------------

const TEXT_KINDS = new Set(["heading", "paragraph", "bullet", "numbered"]);

function blockEl(b) {
  const kind = b.kind;
  if (kind === "table") {
    const rows = (b.rows || []).map((r) => h("tr", r.map((c) => h("td", { html: escHtml(c).replace(/\n/g, "<br>") || "<br>" }))));
    return h("div.dv-b", { "data-kind": "table" }, h("table.dv-table", h("tbody", rows)));
  }
  if (kind === "other") {
    const label = (b.label || "content").replace(/^./, (c) => c.toUpperCase());
    return h("div.dv-b", { "data-kind": "other", contenteditable: "false" }, h("span.dv-other", label));
  }
  const el = h("div.dv-b", { "data-kind": kind, "data-level": String(b.level || 0) });
  el.innerHTML = inlineHtml(b.text) || "<br>";
  return el;
}

/// A block as the Markdown `files.edit` takes.
function blockMd(el) {
  const kind = el.dataset.kind;
  if (kind === "other") return el._md ?? "";
  if (kind === "table") {
    const rows = [...el.querySelectorAll("tr")].map((tr) => [...tr.children].map((td) => inlineMd(td).replace(/\\([*_\\])/g, "$1").replace(/\|/g, "\\|").replace(/\n/g, "<br>")));
    if (!rows.length) return "";
    const cols = Math.max(...rows.map((r) => r.length));
    const line = (r) => `| ${Array.from({ length: cols }, (_, i) => r[i] ?? "").join(" | ")} |`;
    return [line(rows[0]), `|${" --- |".repeat(cols)}`, ...rows.slice(1).map(line)].join("\n");
  }
  const text = inlineMd(el).replace(/\n/g, "<br>");
  if (!text.trim()) return "";
  const level = Number(el.dataset.level || 0);
  if (kind === "heading") return `${"#".repeat(Math.max(1, level))} ${text}`;
  if (kind === "bullet") return `${"  ".repeat(level)}- ${text}`;
  if (kind === "numbered") return `${"  ".repeat(level)}1. ${text}`;
  return text;
}

const isEmpty = (el) => !el.textContent.replace(/ /g, " ").trim() && !el.querySelector("table, .dv-other");

export function documentView(el, file, ctx) {
  el.classList.add("dv");
  const name = file.path.split("/").pop();
  const page = h("div.dv-page", { contenteditable: "true", spellcheck: "true", role: "textbox", "aria-multiline": "true", "aria-label": name });
  const scroll = h("div.dv-scroll", page);
  // No bar: the tab names the file; saving and "open in its app" float in the corner.
  ctx.tools.classList.add("dv-tools");
  el.append(scroll, ctx.tools);
  const s = { count: 0, timer: null, saving: null, again: false, touched: new Set() };

  function render(blocks) {
    const top = scroll.scrollTop;
    clear(page);
    blocks.forEach((b, i) => {
      const x = blockEl(b);
      x.dataset.orig = String(i + 1);
      page.append(x);
      x._md = b.kind === "other" ? "" : blockMd(x);
    });
    if (!blocks.length) page.append(fresh("paragraph", 0));
    s.count = blocks.length;
    scroll.scrollTop = top;
  }

  function fresh(kind, level) {
    const x = h("div.dv-b", { "data-kind": kind, "data-level": String(level || 0) });
    x.innerHTML = "<br>";
    x._md = "";
    return x;
  }

  /// Every child of the page is a block: stray text or elements the browser made are wrapped.
  function normalize() {
    for (const n of [...page.childNodes]) {
      if (n.nodeType === Node.ELEMENT_NODE && n.classList.contains("dv-b")) continue;
      if (n.nodeType === Node.TEXT_NODE && !n.data.trim()) { n.remove(); continue; }
      const x = fresh("paragraph", 0);
      x.textContent = "";
      n.replaceWith(x);
      x.append(n);
    }
    if (!page.firstElementChild) page.append(fresh("paragraph", 0));
  }

  const blockOf = (node) => (node?.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest?.(".dv-b");
  const caret = () => {
    const sel = getSelection();
    return sel.rangeCount ? sel.getRangeAt(0) : null;
  };
  /// Whether the caret is at the very start of `blk`.
  const atStart = (blk, r) => {
    if (!r.collapsed) return false;
    const pre = document.createRange();
    pre.selectNodeContents(blk);
    pre.setEnd(r.startContainer, r.startOffset);
    return pre.toString().length === 0;
  };
  const place = (node, atEnd) => {
    const r = document.createRange();
    r.selectNodeContents(node);
    r.collapse(!atEnd);
    const sel = getSelection();
    sel.removeAllRanges();
    sel.addRange(r);
  };
  const setKind = (blk, kind, level) => {
    blk.dataset.kind = kind;
    blk.dataset.level = String(level ?? 0);
  };

  function changed() {
    clearTimeout(s.timer);
    s.timer = setTimeout(save, SAVE_AFTER);
  }

  // ---- keys ------------------------------------------------------------------------------

  page.addEventListener("keydown", (e) => {
    const mod = e.metaKey || e.ctrlKey;
    const k = e.key.toLowerCase();
    if (mod && (k === "b" || k === "i")) {
      e.preventDefault();
      e.stopPropagation();
      document.execCommand("styleWithCSS", false, false);
      document.execCommand(k === "b" ? "bold" : "italic");
      return changed();
    }
    // Marks a file cannot keep are not offered.
    if (mod && k === "u") return e.preventDefault();
    if (mod && k === "s") { e.preventDefault(); e.stopPropagation(); return save(); }
    if (e.key === "Escape") return page.blur();
    const r = caret();
    const blk = r && blockOf(r.startContainer);
    if (!blk) return;
    const kind = blk.dataset.kind;
    if (e.key === "Enter") {
      e.preventDefault();
      if (kind === "table" || e.shiftKey) {
        document.execCommand("insertLineBreak");
        return changed();
      }
      split(blk, r);
      return changed();
    }
    if (e.key === "Backspace" && atStart(blk, r)) {
      if (kind === "heading" || kind === "bullet" || kind === "numbered") {
        e.preventDefault();
        setKind(blk, "paragraph");
        return changed();
      }
      const prev = blk.previousElementSibling;
      if (!prev) return e.preventDefault();
      if (TEXT_KINDS.has(prev.dataset.kind) && kind === "paragraph") {
        e.preventDefault();
        if (prev.lastChild?.nodeName === "BR") prev.lastChild.remove();
        const mark = document.createTextNode("");
        prev.append(mark);
        while (blk.firstChild) prev.append(blk.firstChild);
        blk.remove();
        const rr = document.createRange();
        rr.setStart(mark, 0);
        rr.collapse(true);
        getSelection().removeAllRanges();
        getSelection().addRange(rr);
        if (!prev.textContent && !prev.querySelector("br")) prev.innerHTML = "<br>";
        return changed();
      }
      if (isEmpty(blk)) {
        e.preventDefault();
        blk.remove();
        place(prev, true);
        return changed();
      }
      return e.preventDefault();
    }
    if (e.key === "Tab" && (kind === "bullet" || kind === "numbered")) {
      e.preventDefault();
      e.stopPropagation();
      const level = Math.max(0, Math.min(8, Number(blk.dataset.level || 0) + (e.shiftKey ? -1 : 1)));
      blk.dataset.level = String(level);
      return changed();
    }
  });

  /// Enter: what follows the caret starts a new block (a list goes on; an empty item ends it).
  function split(blk, r) {
    const kind = blk.dataset.kind;
    const level = Number(blk.dataset.level || 0);
    if ((kind === "bullet" || kind === "numbered") && isEmpty(blk)) {
      setKind(blk, "paragraph");
      return;
    }
    if (!r.collapsed) r.deleteContents();
    const after = document.createRange();
    after.selectNodeContents(blk);
    after.setStart(r.endContainer, r.endOffset);
    const frag = after.extractContents();
    const next = fresh(kind === "heading" ? "paragraph" : kind, kind === "heading" ? 0 : level);
    if (frag.textContent.length || frag.querySelector?.("br + *")) {
      next.textContent = "";
      next.append(frag);
    }
    if (!blk.textContent && !blk.querySelector("br")) blk.innerHTML = "<br>";
    blk.after(next);
    place(next, false);
  }

  // "# ", "## ", "- " and "1. " at the start of a paragraph change its kind.
  page.addEventListener("input", () => {
    normalize();
    const r = caret();
    const blk = r && blockOf(r.startContainer);
    if (blk) s.touched.add(blk);
    if (blk?.dataset.kind === "paragraph") {
      const t = blk.textContent.replace(/ /g, " ");
      const m = /^(#{1,3}) |^([-*]) |^(1[.)]) /.exec(t);
      if (m && atPrefixEnd(blk, r, m[0].length)) {
        const del = document.createRange();
        const first = firstText(blk);
        if (first && first.data.replace(/ /g, " ").startsWith(m[0])) {
          del.setStart(first, 0);
          del.setEnd(first, m[0].length);
          del.deleteContents();
          if (m[1]) setKind(blk, "heading", m[1].length);
          else setKind(blk, m[2] ? "bullet" : "numbered", 0);
          if (!blk.textContent) blk.innerHTML = "<br>";
          place(blk, false);
        }
      }
    }
    changed();
  });

  page.addEventListener("paste", (e) => {
    e.preventDefault();
    const text = e.clipboardData.getData("text/plain");
    if (!text) return;
    const lines = text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
    document.execCommand("insertText", false, lines[0]);
    for (const line of lines.slice(1)) {
      const r = caret();
      const blk = r && blockOf(r.startContainer);
      if (!blk || blk.dataset.kind === "table") {
        document.execCommand("insertLineBreak");
        document.execCommand("insertText", false, line);
        continue;
      }
      split(blk, r);
      if (line) document.execCommand("insertText", false, line);
    }
    changed();
  });
  page.addEventListener("drop", (e) => e.preventDefault());
  page.addEventListener("blur", () => {
    if (s.timer) { clearTimeout(s.timer); save(); }
  });
  const onSel = () => {
    if (document.activeElement === page) ctx.focusChanged();
  };
  document.addEventListener("selectionchange", onSel);

  // ---- saving ----------------------------------------------------------------------------

  /// The blocks as they are now, against the file as last saved: the edits that make one the
  /// other. A block keeps its number from the last save (`orig`); new ones go after the block
  /// before them.
  async function save() {
    clearTimeout(s.timer);
    s.timer = null;
    if (s.saving) { s.again = true; return s.saving; }
    normalize();
    const els = [...page.children];
    const edits = [];
    const seen = new Set();
    const plan = [];
    let last = 0;
    let insert = null;
    const flush = () => {
      if (insert) edits.push({ after: insert.after, text: insert.lines.join("\n") });
      insert = null;
    };
    for (const x of els) {
      let orig = Number(x.dataset.orig) || 0;
      // The browser copies a block's attributes when it splits one: the first keeps the number.
      if (orig && seen.has(orig)) orig = 0;
      const md = blockMd(x);
      if (orig) {
        seen.add(orig);
        flush();
        if (md !== x._md) edits.push({ block: orig, text: md });
        plan.push({ x, md, slot: md !== "" || x._md === "" });
        last = orig;
      } else if (!md.trim()) {
        plan.push({ x, md, slot: false });
      } else {
        insert ||= { after: last, lines: [] };
        insert.lines.push(md);
        plan.push({ x, md, slot: true });
      }
    }
    flush();
    for (let i = 1; i <= s.count; i++) if (!seen.has(i)) edits.push({ delete: i });
    if (!edits.length) return;
    s.saving = (async () => {
      let saved = null;
      try {
        saved = await ctx.save(edits);
      } catch {}
      s.saving = null;
      if (!saved) return;
      const slotted = plan.filter((p) => p.slot);
      if (saved.blocks.length !== slotted.length || plan.some((p) => p.slot && !p.x.isConnected)) {
        // The file did not take the blocks one for one (a pasted table, say): show it as saved.
        render(saved.blocks);
      } else {
        let n = 0;
        for (const p of plan) {
          if (p.slot) {
            p.x.dataset.orig = String(++n);
            p.x._md = p.md;
          } else {
            delete p.x.dataset.orig;
            p.x._md = "";
          }
        }
        s.count = saved.blocks.length;
      }
      if (s.again) {
        s.again = false;
        save();
      }
    })();
    return s.saving;
  }

  // What the person changed reaches the agent with their next message, then starts again.
  const off = ctx.ark.on("ark:turn_finished", () => s.touched.clear());

  render(file.blocks);

  return {
    focus() {
      const r = caret();
      const blk = r && page.contains(r.startContainer) ? blockOf(r.startContainer) : null;
      const kids = [...page.children];
      const sel = {};
      if (blk) sel.block = kids.indexOf(blk) + 1;
      const text = r && !r.collapsed && page.contains(r.startContainer) ? r.toString() : "";
      if (text) sel.text = text;
      const edited = [...s.touched].filter((x) => x.isConnected).map((x) => kids.indexOf(x) + 1).sort((a, b) => a - b);
      if (edited.length) sel.edited = `blocks ${edited.slice(0, 20).join(", ")}`;
      return Object.keys(sel).length ? { selection: sel } : {};
    },
    dirty: () => !!s.timer || !!s.saving,
    update(f) {
      render(f.blocks);
    },
    flush: () => s.timer && save(),
    dispose() {
      off();
      document.removeEventListener("selectionchange", onSel);
    },
  };
}

function firstText(el) {
  const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  return w.nextNode();
}

function atPrefixEnd(blk, r, n) {
  if (!r?.collapsed) return false;
  const pre = document.createRange();
  pre.selectNodeContents(blk);
  pre.setEnd(r.startContainer, r.startOffset);
  return pre.toString().length === n;
}
