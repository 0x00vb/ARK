// Rendered Markdown edited in place, as in Obsidian's live preview: the block under the caret
// shows its own source, every other block stays drawn. The text is a list of lines and a save
// writes those lines back, so a block nobody touched keeps its bytes. Each top-level block
// names its lines (`data-lines="a:b"`, end exclusive); typing keeps them true.

import { h } from "./dom.js";
import { markdown } from "./md.js";

const SAVE_MS = 700;

const range = (el) => el.dataset.lines.split(":").map(Number);
const setRange = (el, a, b) => (el.dataset.lines = `${a}:${b}`);

/// Where a click landed, as characters of the block's drawn text (null: not inside it).
function drawnOffset(block, e) {
  const hit = document.caretPositionFromPoint?.(e.clientX, e.clientY);
  const node = hit?.offsetNode ?? document.caretRangeFromPoint?.(e.clientX, e.clientY)?.startContainer;
  const offset = hit?.offset ?? document.caretRangeFromPoint?.(e.clientX, e.clientY)?.startOffset;
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

/// `page` shows `text`; `save(text)` writes it; `drawn(root)` finishes new blocks (images).
export function inlineEditor(page, { text, readOnly, save, drawn }) {
  const crlf = text.includes("\r\n");
  const split = (t) => String(t).replace(/\r\n/g, "\n").split("\n");
  let lines = split(text);
  let box = null;
  let pressing = false;
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

  const flush = () => {
    clearTimeout(timer);
    if (saving) return saving;
    if (!pending) return Promise.resolve();
    saving = (async () => {
      while (pending) {
        pending = false;
        try {
          await save(content());
        } catch {
          pending = true;
          break;
        }
      }
    })().finally(() => (saving = null));
    return saving;
  };
  const changed = () => {
    pending = true;
    clearTimeout(timer);
    timer = setTimeout(flush, SAVE_MS);
  };

  /// Lines `a` to `b` become `next`; the blocks after `from` move with them.
  const splice = (from, a, b, next) => {
    lines.splice(a, b - a, ...next);
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
    if (lines.slice(a, b).every((l) => !l.trim())) {
      // Nothing left in it: a block that was only added goes with its separator, an emptied one
      // goes when the line before is already blank.
      const added = Number(area.dataset.added || 0);
      if (added) a -= added - 1;
      if (added || a === 0 || lines[a - 1] === "") {
        splice(area, a, b, []);
        area.remove();
        changed();
        return flush();
      }
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
    const style = getComputedStyle(block.querySelector("code") || block);
    const outer = getComputedStyle(block);
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
    if (block.dataset.added) area.dataset.added = block.dataset.added;
    area.addEventListener("input", () => {
      const [x, y] = range(area);
      const next = area.value.split("\n");
      splice(area, x, y, next);
      setRange(area, x, x + next.length);
      fit(area);
      changed();
    });
    area.addEventListener("blur", () => {
      if (!pressing) leave();
    });
    area.addEventListener("keydown", (e) => {
      if (e.isComposing) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        leave();
      } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        e.stopPropagation();
        flush();
      } else if (e.key === "ArrowUp" && area.selectionStart === 0 && area.selectionEnd === 0) {
        e.preventDefault();
        step(area, -1);
      } else if (e.key === "ArrowDown" && area.selectionStart === area.value.length) {
        e.preventDefault();
        step(area, 1);
      }
    });
    block.replaceWith(area);
    box = area;
    fit(area);
    area.focus({ preventScroll: true });
    const caret = at === "start" ? 0 : typeof at === "number" ? sourceOffset(block.textContent, source, at) : source.length;
    area.setSelectionRange(caret, caret);
  }

  /// The block before or after `area`, opened at the edge nearest the one just left.
  function step(area, dir) {
    const [a, b] = range(area);
    leave();
    const all = blocks();
    const target = dir < 0 ? all.filter((el) => range(el)[1] <= a).pop() : all.find((el) => range(el)[0] >= b);
    if (target) activate(target, dir < 0 ? "end" : "start");
  }

  /// A new block under the last one: the page is written to there.
  function append() {
    const empty = lines.every((l) => !l.trim());
    const end = lines[lines.length - 1] === "" ? lines.length - 1 : lines.length;
    const added = empty ? [""] : ["", ""];
    lines.splice(empty ? 0 : end, 0, ...added);
    const at = empty ? 0 : end + 1;
    const block = h("p", { "data-lines": `${at}:${at + 1}`, "data-added": String(added.length) });
    page.append(block);
    activate(block, "end");
  }

  page.addEventListener("pointerdown", () => (pressing = true));
  const release = () => setTimeout(() => (pressing = false));
  addEventListener("pointerup", release, true);
  addEventListener("pointercancel", release, true);
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
    dirty: () => pending || !!saving,
    async flush() {
      leave();
      await flush();
    },
    editing: () => !!box,
    line: () => (box ? range(box)[0] + box.value.slice(0, box.selectionStart).split("\n").length : null),
    set(next) {
      if (next === content()) return;
      box = null;
      lines = split(next);
      paint();
    },
    dispose() {
      removeEventListener("pointerup", release, true);
      removeEventListener("pointercancel", release, true);
      clearTimeout(timer);
    },
  };
}
