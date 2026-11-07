// Rendered Markdown edited in place, as in Obsidian's live preview: the block under the caret
// shows its own source, every other block stays drawn. The text is a list of lines and a save
// writes those lines back, so a block nobody touched keeps its bytes. Each top-level block
// names its lines (`data-lines="a:b"`, end exclusive); typing keeps them true.

import { h } from "./dom.js";
import { markdown } from "./md.js";

const SAVE_MS = 700;

const range = (el) => el.dataset.lines.split(":").map(Number);
const setRange = (el, a, b) => (el.dataset.lines = `${a}:${b}`);
const blank = (l) => !l.trim();
const split = (t) => String(t).replace(/\r\n/g, "\n").split("\n");

/// Where a click landed, as characters of the block's drawn text (null: not inside it).
function drawnOffset(block, e) {
  const hit = document.caretPositionFromPoint?.(e.clientX, e.clientY);
  const caret = hit ? null : document.caretRangeFromPoint?.(e.clientX, e.clientY);
  const node = hit?.offsetNode ?? caret?.startContainer;
  const offset = hit?.offset ?? caret?.startOffset;
  if (!node || !block.contains(node)) return null;
  const before = document.createRange();
  before.selectNodeContents(block);
  before.setEnd(node, offset);
  return before.toString().length;
}

/// The drawn text is the source without its marks: walk both, skipping what was not drawn.
function sourceOffset(drawn, source, count) {
  let at = 0;
  for (let i = 0; i < count; i++) {
    const c = drawn[i];
    const next = /\s/.test(c) ? source.slice(at).search(/\s/) : source.indexOf(c, at) - at;
    if (next < 0) break;
    at += next + 1;
  }
  return at;
}

/// The edit that turns `a` into `b`: the lines between their common start and end.
function change(a, b) {
  let from = 0;
  while (from < a.length && from < b.length && a[from] === b[from]) from++;
  let end = 0;
  while (end < a.length - from && end < b.length - from && a[a.length - 1 - end] === b[b.length - 1 - end]) end++;
  return { from, to: a.length - end, lines: b.slice(from, b.length - end) };
}

/// `ours` and `theirs` both changed `base`: both changes together when they touch different
/// lines, else null.
export function mergeText(base, ours, theirs) {
  const eol = ours.includes("\r\n") ? "\r\n" : "\n";
  const lines = split(base);
  const mine = change(lines, split(ours));
  const other = change(lines, split(theirs));
  const apart = (mine.to <= other.from || other.to <= mine.from) && mine.from !== other.from;
  if (!apart) return null;
  for (const edit of [mine, other].sort((x, y) => y.from - x.from)) lines.splice(edit.from, edit.to - edit.from, ...edit.lines);
  return lines.join(eol);
}

/// `page` shows `text`. `save(text)` writes it and returns the text now in the file (it may
/// have merged another change); `drawn(root)` finishes new blocks (images); `idle()` runs when
/// nothing is open or unsaved, so the file can be read again.
export function inlineEditor(page, { text, readOnly, save, drawn, idle }) {
  let crlf = text.includes("\r\n");
  let lines = split(text);
  let savedText = text;
  let box = null;
  let pressing = false;
  let composing = false;
  let pending = false;
  let saving = null;
  let timer = 0;

  const content = () => lines.join(crlf ? "\r\n" : "\n");
  const blocks = () => [...page.children].filter((el) => el.dataset.lines);
  const decorate = (root) => {
    if (!readOnly) for (const el of root.children) if (el.dataset.lines) el.tabIndex = 0;
    drawn(root);
  };
  const paint = () => {
    page.innerHTML = markdown(lines.join("\n"), { files: true, lines: true });
    decorate(page);
  };
  const focusAt = (line) => (blocks().find((el) => range(el)[1] > line) ?? blocks().pop())?.focus({ preventScroll: true });

  const flush = () => {
    clearTimeout(timer);
    if (saving) return saving;
    saving = (async () => {
      while (pending) {
        pending = false;
        const body = content();
        if (body === savedText) continue;
        try {
          const written = await save(body);
          savedText = written ?? body;
          if (written != null && written !== body) reset(written);
        } catch {
          pending = true;
          break;
        }
      }
    })().finally(() => {
      saving = null;
      if (!box && !pending) idle?.();
    });
    return saving;
  };
  const changed = () => {
    pending = true;
    clearTimeout(timer);
    timer = setTimeout(flush, SAVE_MS);
  };

  /// The file now reads `next`, from a merge: draw it again with the open block still open.
  function reset(next) {
    const keep = box && { line: range(box)[0], at: box.selectionStart };
    box = null;
    crlf = next.includes("\r\n");
    lines = split(next);
    savedText = next;
    paint();
    const el = keep && blocks().find((b) => range(b)[1] > keep.line);
    if (el) activate(el, { source: keep.at });
  }

  /// Lines `a` to `b` become `next`; the blocks after `from` move with them.
  const splice = (from, a, b, next) => {
    lines.splice(a, b - a, ...next);
    if (!lines.length) lines.push("");
    const delta = next.length - (b - a);
    if (!delta) return;
    for (let el = from.nextElementSibling; el; el = el.nextElementSibling) {
      if (!el.dataset.lines) continue;
      const [x, y] = range(el);
      setRange(el, x + delta, y + delta);
    }
  };

  const fit = (area) => {
    area.style.height = "auto";
    area.style.height = `${area.scrollHeight}px`;
  };

  function leave() {
    if (!box) return;
    const area = box;
    box = null;
    let [a, b] = range(area);
    if (lines.slice(a, b).every(blank)) {
      // Nothing left in it: it goes, with one blank line beside it.
      if (a > 0 && blank(lines[a - 1])) a--;
      else if (b < lines.length - 1 && blank(lines[b])) b++;
      splice(area, a, b, []);
      area.remove();
      changed();
      return flush();
    }
    if (lines.slice(a, b).some((l) => l.trim().startsWith("```"))) {
      // A fence changes what the blocks after it are: draw the whole page again.
      const top = page.scrollTop;
      paint();
      page.scrollTop = top;
      return flush();
    }
    const drawnBlocks = document.createElement("div");
    drawnBlocks.innerHTML = markdown(lines.slice(a, b).join("\n"), { files: true, lines: true });
    for (const el of drawnBlocks.children) {
      const [x, y] = range(el);
      setRange(el, x + a, y + a);
    }
    decorate(drawnBlocks);
    area.replaceWith(...drawnBlocks.childNodes);
    return flush();
  }

  /// The block opens as its source, with the caret at a character of it or at an end.
  function activate(block, at) {
    leave();
    const [a, b] = range(block);
    const source = lines.slice(a, b).join("\n");
    const outer = getComputedStyle(block);
    const style = block.matches("pre") ? getComputedStyle(block.querySelector("code") || block) : outer;
    const area = h("textarea.md-edit", { rows: 1, "aria-label": "Edit" });
    Object.assign(area.style, {
      fontFamily: style.fontFamily,
      fontSize: style.fontSize,
      fontWeight: style.fontWeight,
      fontStyle: style.fontStyle,
      lineHeight: outer.lineHeight,
      letterSpacing: style.letterSpacing,
      color: outer.color,
      marginTop: outer.marginTop,
      marginBottom: outer.marginBottom,
    });
    area.value = source;
    setRange(area, a, b);
    area.addEventListener("input", () => {
      const [x, y] = range(area);
      const next = area.value.split("\n");
      splice(area, x, y, next);
      setRange(area, x, x + next.length);
      fit(area);
      if (!composing) changed();
    });
    area.addEventListener("compositionstart", () => (composing = true));
    area.addEventListener("compositionend", () => {
      composing = false;
      changed();
    });
    area.addEventListener("blur", () => {
      if (!pressing) leave();
    });
    area.addEventListener("keydown", (e) => {
      if (e.isComposing) {
        if (e.key === "Escape") e.stopPropagation();
        return;
      }
      const plain = !e.shiftKey && !e.metaKey && !e.ctrlKey && !e.altKey;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        const [line] = range(area);
        leave();
        focusAt(line);
      } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        e.stopPropagation();
        flush();
      } else if (plain && e.key === "ArrowUp" && area.selectionStart === 0 && area.selectionEnd === 0) {
        e.preventDefault();
        step(area, -1);
      } else if (plain && e.key === "ArrowDown" && area.selectionStart === area.value.length) {
        e.preventDefault();
        step(area, 1);
      }
    });
    block.replaceWith(area);
    box = area;
    fit(area);
    area.focus({ preventScroll: true });
    const caret =
      at === "start" ? 0
      : typeof at === "number" ? sourceOffset(block.textContent, source, at)
      : at?.source != null ? Math.min(at.source, source.length)
      : source.length;
    area.setSelectionRange(caret, caret);
  }

  /// The block before or after `area`, opened at the edge nearest the one just left.
  function step(area, dir) {
    let target = dir < 0 ? area.previousElementSibling : area.nextElementSibling;
    while (target && !target.dataset.lines) target = dir < 0 ? target.previousElementSibling : target.nextElementSibling;
    leave();
    if (target?.isConnected) activate(target, dir < 0 ? "end" : "start");
  }

  /// A new block under the last one: the page is written to there.
  function append() {
    const empty = lines.every(blank);
    const end = lines[lines.length - 1] === "" ? lines.length - 1 : lines.length;
    lines.splice(empty ? 0 : end, 0, ...(empty ? [""] : ["", ""]));
    const at = empty ? 0 : end + 1;
    const block = h("p", { "data-lines": `${at}:${at + 1}` });
    page.append(block);
    activate(block, "end");
  }

  const flushNow = () => {
    leave();
    return flush();
  };
  page.addEventListener("pointerdown", () => (pressing = true));
  // After the click that follows: a press that ended elsewhere leaves the block it blurred.
  const release = () =>
    setTimeout(() => {
      pressing = false;
      if (box && document.activeElement !== box) leave();
    });
  addEventListener("pointerup", release, true);
  addEventListener("pointercancel", release, true);
  addEventListener("pagehide", flushNow);
  page.addEventListener("click", (e) => {
    pressing = false;
    if (readOnly || (box && box.contains(e.target)) || e.target.closest("a[href]")) return;
    if (!getSelection().isCollapsed) return;
    let block = e.target;
    while (block && block.parentElement !== page) block = block.parentElement;
    if (block?.dataset.lines) return activate(block, drawnOffset(block, e) ?? "end");
    const last = blocks().pop();
    if (e.target === page && (!last || e.clientY > last.getBoundingClientRect().bottom)) return append();
    leave();
  });
  page.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !readOnly && e.target !== box && e.target.parentElement === page && e.target.dataset.lines) {
      e.preventDefault();
      activate(e.target, "end");
    }
  });
  paint();

  return {
    content,
    // An open block counts: the file is not read again over what is being typed.
    dirty: () => !!box || pending || !!saving,
    flush: flushNow,
    line: () => (box ? range(box)[0] + box.value.slice(0, box.selectionStart).split("\n").length : null),
    /// The file changed elsewhere. With nothing open or unsaved, draw it. Returns whether it did.
    set(next) {
      if (box || pending || saving) return false;
      if (next !== content()) {
        crlf = next.includes("\r\n");
        lines = split(next);
        paint();
      }
      savedText = next;
      return true;
    },
    dispose() {
      flushNow();
      removeEventListener("pointerup", release, true);
      removeEventListener("pointercancel", release, true);
      removeEventListener("pagehide", flushNow);
      clearTimeout(timer);
    },
  };
}
