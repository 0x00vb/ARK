// A spreadsheet: the grid of the file the agent works on. Click a cell to select it, type to
// replace it, Enter or a double click to change it; the cell's formula shows in the bar above.
// Each change saves at once as `{cell, value}`, the edit the agent sends too, and the answer
// (with formulas computed again) redraws the grid. Copy and paste work as tab-separated text.

import { h } from "./dom.js";

const ROW_H = 26;
/// Rows drawn at a time; more as the person scrolls.
const CHUNK = 200;
/// Empty rows and columns beyond the data, to type into.
const SPARE_ROWS = 30;
const SPARE_COLS = 4;

export function colName(c) {
  let s = "";
  c += 1;
  while (c > 0) {
    const m = (c - 1) % 26;
    s = String.fromCharCode(65 + m) + s;
    c = Math.floor((c - 1) / 26);
  }
  return s;
}

const ref = (r, c) => `${colName(c)}${r + 1}`;

/// A1 to {r, c}.
function parseRef(s) {
  const m = /^\$?([A-Z]{1,3})\$?(\d+)$/i.exec(String(s).trim());
  if (!m) return null;
  let c = 0;
  for (const ch of m[1].toUpperCase()) c = c * 26 + (ch.charCodeAt(0) - 64);
  return { r: Number(m[2]) - 1, c: c - 1 };
}

export function sheetView(el, file, ctx) {
  el.classList.add("sh");
  const s = {
    file,
    sheet: 0,
    sel: { r: 0, c: 0, r2: 0, c2: 0 },
    drawn: 0,
    editing: null,
    edited: new Set(),
  };
  const nameBox = h("input.sh-ref", { "aria-label": "Cell", spellcheck: "false" });
  const fx = h("input.sh-fx", { "aria-label": "Cell contents", spellcheck: "false" });
  const bar = h("div.wb-bar.sh-bar", nameBox, h("span.sh-fx-mark", "fx"), fx, ctx.tools);
  const head = h("tr");
  const rows = h("tbody");
  const table = h("table.sh-grid", h("thead", head), rows);
  const grid = h("div.sh-scroll", { tabindex: "0", role: "grid", "aria-label": file.path.split("/").pop() }, table);
  const tabs = h("div.sh-tabs", { role: "tablist" });
  el.append(bar, grid, tabs);

  const data = () => s.file.sheets[s.sheet] || { rows: [], cols: 0, name: "" };
  const cell = (r, c) => data().rows[r]?.[c] || null;
  const size = () => {
    const d = data();
    // Spare rows and columns only past the end of the sheet: past a part shown they would
    // be the file's own cells, not shown here.
    return {
      rows: d.rows.length + (d.more_rows ? 0 : SPARE_ROWS),
      cols: Math.max(d.cols, 1) + (d.more_cols ? 0 : SPARE_COLS),
    };
  };

  // ---- drawing ---------------------------------------------------------------------------

  function widths() {
    const d = data();
    const n = size().cols;
    const w = new Array(n).fill(0);
    for (const row of d.rows.slice(0, 400)) {
      row.forEach((x, c) => { if (x) w[c] = Math.max(w[c], String(x.t).length); });
    }
    return w.map((chars) => Math.round(Math.min(220, Math.max(72, chars * 7.4 + 18))));
  }

  function drawTabs() {
    const many = s.file.sheets.length > 1;
    const part = shownPart();
    tabs.replaceChildren(...(many ? s.file.sheets.map((sh, i) =>
      h("button.sh-tab", { type: "button", role: "tab", "aria-selected": String(i === s.sheet), onclick: () => pick(i) }, sh.name)) : []),
      ...(part ? [h("span.sh-part", { role: "status" }, part)] : []));
    tabs.hidden = !many && !part;
  }

  /// What the grid leaves out of a large sheet, in words, or "".
  function shownPart() {
    const d = data();
    const n = (x) => x.toLocaleString();
    const rows = d.more_rows ? `the first ${n(d.rows.length)} of ${n(d.rows.length + d.more_rows)} rows` : "";
    const cols = d.more_cols ? `the first ${n(d.cols)} of ${n(d.cols + d.more_cols)} columns` : "";
    if (!rows && !cols) return "";
    return `Showing ${[rows, cols].filter(Boolean).join(" and ")}. Ask ARK to work with all of them.`;
  }

  function draw() {
    const { cols } = size();
    const w = widths();
    head.replaceChildren(h("th.sh-corner"), ...Array.from({ length: cols }, (_, c) =>
      h("th.sh-col", { style: { width: `${w[c]}px`, minWidth: `${w[c]}px` }, "data-c": c }, colName(c))));
    rows.replaceChildren();
    s.drawn = 0;
    more();
    paintSel();
  }

  /// Draw the next rows when the end comes into view.
  function more() {
    const { rows: total, cols } = size();
    const end = Math.min(total, s.drawn + CHUNK);
    const frag = document.createDocumentFragment();
    for (let r = s.drawn; r < end; r++) {
      const tr = h("tr", { "data-r": r }, h("th.sh-row", r + 1));
      for (let c = 0; c < cols; c++) tr.append(td(r, c));
      frag.append(tr);
    }
    rows.append(frag);
    s.drawn = end;
  }

  function td(r, c) {
    const x = cell(r, c);
    const el = h("td", { "data-r": r, "data-c": c });
    fill(el, x);
    return el;
  }

  function fill(el, x) {
    el.textContent = x ? x.t : "";
    el.className = x ? [x.n ? "num" : "", x.x ? "err" : "", x.b ? "bool" : "", x.f ? "fx" : ""].filter(Boolean).join(" ") : "";
  }

  /// New contents for the cells already drawn (after a save or a change elsewhere).
  function repaint() {
    const { cols } = size();
    const needCols = head.children.length - 1 < cols;
    if (needCols) return draw();
    for (const tr of rows.children) {
      const r = Number(tr.dataset.r);
      for (let c = 0; c < cols; c++) {
        const el = tr.children[c + 1];
        if (el && el !== s.editing?.td) fill(el, cell(r, c));
      }
    }
    if (s.drawn < size().rows && s.drawn < CHUNK) more();
    paintSel();
  }

  grid.addEventListener("scroll", () => {
    if (grid.scrollTop + grid.clientHeight > s.drawn * ROW_H - 400 && s.drawn < size().rows) more();
  });

  // ---- selection ---------------------------------------------------------------------------

  const tdAt = (r, c) => rows.querySelector(`tr[data-r="${r}"]`)?.children[c + 1] || null;
  const box = () => ({
    r0: Math.min(s.sel.r, s.sel.r2), r1: Math.max(s.sel.r, s.sel.r2),
    c0: Math.min(s.sel.c, s.sel.c2), c1: Math.max(s.sel.c, s.sel.c2),
  });
  const rangeName = () => {
    const b = box();
    return b.r0 === b.r1 && b.c0 === b.c1 ? ref(b.r0, b.c0) : `${ref(b.r0, b.c0)}:${ref(b.r1, b.c1)}`;
  };

  function paintSel() {
    for (const x of grid.querySelectorAll(".sel, .in-range, .hl")) x.classList.remove("sel", "in-range", "hl");
    const b = box();
    for (let r = b.r0; r <= Math.min(b.r1, s.drawn - 1); r++) {
      for (let c = b.c0; c <= b.c1; c++) tdAt(r, c)?.classList.add("in-range");
      rows.querySelector(`tr[data-r="${r}"] > th`)?.classList.add("hl");
    }
    for (let c = b.c0; c <= b.c1; c++) head.children[c + 1]?.classList.add("hl");
    tdAt(s.sel.r, s.sel.c)?.classList.add("sel");
    nameBox.value = rangeName();
    if (document.activeElement !== fx) {
      const x = cell(s.sel.r, s.sel.c);
      fx.value = x ? x.e ?? x.t : "";
    }
    ctx.focusChanged();
  }

  function select(r, c, extend) {
    const { rows: nr, cols } = size();
    r = Math.max(0, Math.min(r, nr - 1));
    c = Math.max(0, Math.min(c, cols - 1));
    while (r >= s.drawn && s.drawn < nr) more();
    if (extend) Object.assign(s.sel, { r2: r, c2: c });
    else s.sel = { r, c, r2: r, c2: c };
    paintSel();
    reveal(extend ? s.sel.r2 : r, extend ? s.sel.c2 : c);
  }

  function reveal(r, c) {
    const el = tdAt(r, c);
    if (!el) return;
    const g = grid.getBoundingClientRect();
    const b = el.getBoundingClientRect();
    const headH = head.getBoundingClientRect().height;
    const rowW = head.firstChild.getBoundingClientRect().width;
    if (b.top < g.top + headH) grid.scrollTop -= g.top + headH - b.top;
    else if (b.bottom > g.bottom) grid.scrollTop += b.bottom - g.bottom;
    if (b.left < g.left + rowW) grid.scrollLeft -= g.left + rowW - b.left;
    else if (b.right > g.right) grid.scrollLeft += b.right - g.right;
  }

  let dragging = false;
  grid.addEventListener("mousedown", (e) => {
    const el = e.target.closest("td");
    if (!el || e.target.closest(".sh-edit")) return;
    if (s.editing) commit();
    e.preventDefault();
    grid.focus({ preventScroll: true });
    select(Number(el.dataset.r), Number(el.dataset.c), e.shiftKey);
    dragging = true;
  });
  grid.addEventListener("mouseover", (e) => {
    const el = dragging && e.target.closest("td");
    if (el) select(Number(el.dataset.r), Number(el.dataset.c), true);
  });
  const up = () => (dragging = false);
  addEventListener("mouseup", up);
  grid.addEventListener("dblclick", (e) => {
    const el = e.target.closest("td");
    if (el) edit(null);
  });
  // A column or row header selects it.
  head.addEventListener("mousedown", (e) => {
    const th = e.target.closest("th.sh-col");
    if (!th) return;
    e.preventDefault();
    grid.focus({ preventScroll: true });
    const c = Number(th.dataset.c);
    s.sel = { r: 0, c, r2: Math.max(0, data().rows.length - 1), c2: c };
    paintSel();
  });

  // ---- editing -----------------------------------------------------------------------------

  /// Edit the selected cell in place; `first` is the key that started it (it replaces the cell).
  function edit(first) {
    const el = tdAt(s.sel.r, s.sel.c);
    if (!el) return;
    const x = cell(s.sel.r, s.sel.c);
    const input = h("input.sh-edit", { spellcheck: "false", "aria-label": ref(s.sel.r, s.sel.c) });
    input.value = first ?? (x ? x.e ?? x.t : "");
    el.classList.add("editing");
    el.append(input);
    s.editing = { td: el, input, r: s.sel.r, c: s.sel.c, before: x ? x.e ?? x.t : "" };
    input.focus();
    const end = input.value.length;
    input.setSelectionRange(end, end);
    input.addEventListener("input", () => (fx.value = input.value));
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); commit(); select(s.sel.r + (e.shiftKey ? -1 : 1), s.sel.c); grid.focus(); }
      else if (e.key === "Tab") { e.preventDefault(); commit(); select(s.sel.r, s.sel.c + (e.shiftKey ? -1 : 1)); grid.focus(); }
      else if (e.key === "Escape") { e.preventDefault(); cancel(); grid.focus(); }
    });
    input.addEventListener("blur", () => s.editing?.input === input && commit());
  }

  function cancel() {
    const ed = s.editing;
    if (!ed) return;
    s.editing = null;
    ed.input.remove();
    ed.td.classList.remove("editing");
    paintSel();
  }

  function commit(value) {
    const ed = s.editing;
    const v = value ?? ed?.input.value;
    if (ed) {
      s.editing = null;
      ed.input.remove();
      ed.td.classList.remove("editing");
      if (v === ed.before) return paintSel();
      put(ed.r, ed.c, v);
    }
  }

  /// Show the new value now and save it; the answer brings the computed values.
  async function put(r, c, v) {
    const el = tdAt(r, c);
    if (el) fill(el, v === "" ? null : { t: v, n: /^-?[\d.]+%?$/.test(v) });
    s.edited.add(ref(r, c));
    await save([{ sheet: data().name, cell: ref(r, c), value: v === "" ? null : v }]);
  }

  async function save(edits) {
    try {
      // A cell's address does not move, so the person's change applies to the latest file.
      const file = await ctx.save(edits, { base: false });
      if (file) apply(file);
    } catch {
      apply(s.file);
    }
  }

  function apply(file) {
    const name = data().name;
    s.file = file;
    const i = file.sheets.findIndex((x) => x.name === name);
    if (i >= 0) s.sheet = i;
    else s.sheet = Math.min(s.sheet, file.sheets.length - 1);
    drawTabs();
    repaint();
  }

  function clearSel() {
    const b = box();
    for (let r = b.r0; r <= b.r1; r++) for (let c = b.c0; c <= b.c1; c++) {
      if (cell(r, c)) s.edited.add(ref(r, c));
      const el = tdAt(r, c);
      if (el) fill(el, null);
    }
    const one = b.r0 === b.r1 && b.c0 === b.c1;
    save([one ? { sheet: data().name, cell: rangeName(), value: null } : { sheet: data().name, clear: rangeName() }]);
  }

  grid.addEventListener("keydown", (e) => {
    if (s.editing) return;
    const mod = e.metaKey || e.ctrlKey;
    const k = e.key;
    const move = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[k];
    if (move) {
      e.preventDefault();
      const from = e.shiftKey ? { r: s.sel.r2, c: s.sel.c2 } : s.sel;
      select(from.r + move[0], from.c + move[1], e.shiftKey);
    } else if (k === "Enter" || k === "F2") {
      e.preventDefault();
      edit(null);
    } else if (k === "Tab") {
      e.preventDefault();
      select(s.sel.r, s.sel.c + (e.shiftKey ? -1 : 1));
    } else if (k === "Backspace" || k === "Delete") {
      e.preventDefault();
      clearSel();
    } else if (k === "Escape") {
      // Leave the grid; the window's own Escape does the rest.
      grid.blur();
    } else if (!mod && k.length === 1) {
      e.preventDefault();
      edit(k);
    } else return;
    e.stopPropagation();
  });

  grid.addEventListener("copy", (e) => {
    if (s.editing) return;
    const b = box();
    const lines = [];
    for (let r = b.r0; r <= b.r1; r++) {
      const row = [];
      for (let c = b.c0; c <= b.c1; c++) row.push(cell(r, c)?.t ?? "");
      lines.push(row.join("\t"));
    }
    e.clipboardData.setData("text/plain", lines.join("\n"));
    e.preventDefault();
  });

  grid.addEventListener("paste", (e) => {
    if (s.editing) return;
    const text = e.clipboardData.getData("text/plain");
    if (!text) return;
    e.preventDefault();
    const values = text.replace(/\r?\n$/, "").split(/\r?\n/).map((l) => l.split("\t"));
    values.forEach((row, dr) => row.forEach((v, dc) => {
      s.edited.add(ref(s.sel.r + dr, s.sel.c + dc));
      const el = tdAt(s.sel.r + dr, s.sel.c + dc);
      if (el) fill(el, { t: v });
    }));
    save([{ sheet: data().name, cell: ref(s.sel.r, s.sel.c), rows: values }]);
  });

  // The bar: a cell name to go to, and the selected cell's contents to change.
  nameBox.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key !== "Enter") return;
    const p = parseRef(nameBox.value);
    if (p) select(p.r, p.c);
    grid.focus();
  });
  nameBox.addEventListener("focus", () => nameBox.select());
  fx.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      const x = cell(s.sel.r, s.sel.c);
      if (fx.value !== (x ? x.e ?? x.t : "")) put(s.sel.r, s.sel.c, fx.value);
      select(s.sel.r + 1, s.sel.c);
      grid.focus();
    } else if (e.key === "Escape") {
      const x = cell(s.sel.r, s.sel.c);
      fx.value = x ? x.e ?? x.t : "";
      grid.focus();
    }
  });

  function pick(i) {
    if (s.editing) commit();
    s.sheet = i;
    s.sel = { r: 0, c: 0, r2: 0, c2: 0 };
    drawTabs();
    draw();
    grid.scrollTo(0, 0);
  }

  // What the person changed reaches the agent with their next message, then starts again.
  const off = ctx.ark.on("ark:turn_finished", () => s.edited.clear());

  drawTabs();
  draw();

  return {
    focus() {
      const sel = { cell: ref(s.sel.r, s.sel.c), sheet: data().name };
      const b = box();
      if (b.r0 !== b.r1 || b.c0 !== b.c1) sel.range = rangeName();
      if (s.edited.size) sel.edited = [...s.edited].slice(0, 30).join(", ");
      return { selection: sel };
    },
    dirty: () => !!s.editing,
    update(f) {
      if (s.editing) return;
      apply(f);
    },
    reveal(loc) {
      const [sheet, at] = loc.includes("!") ? loc.split("!") : [null, loc];
      if (sheet) {
        const i = s.file.sheets.findIndex((x) => x.name.toLowerCase() === sheet.replace(/^'|'$/g, "").toLowerCase());
        if (i >= 0 && i !== s.sheet) pick(i);
      }
      const p = parseRef(at.split(":")[0]);
      if (p) select(p.r, p.c);
    },
    flush: () => s.editing && commit(),
    dispose() {
      off();
      removeEventListener("mouseup", up);
    },
  };
}
