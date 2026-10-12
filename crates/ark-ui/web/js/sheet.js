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
const NS = "http://www.w3.org/2000/svg";
const SERIES = ["var(--accent)", "var(--seg-2)", "var(--seg-1)", "var(--seg-3)", "var(--seg-5)", "var(--seg-6)"];

function svg(name, attrs = {}, ...children) {
  const el = document.createElementNS(NS, name);
  for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value);
  for (const child of children.flat()) if (child != null) el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  return el;
}

const number = (n) => Number(n).toLocaleString(undefined, { maximumFractionDigits: 2 });

function chartFigure(chart, remote = false) {
  if (chart.type === "unsupported" || !(chart.series || []).some((s) => s.values?.length)) {
    return h("figure.sh-chart.unsupported", h("figcaption", h("h2", chart.title || "Chart unavailable"), h("p", `${chart.type === "unsupported" ? "This chart type" : "This chart"} is not shown here.`)));
  }
  const canvas = svg("svg", { viewBox: "0 0 720 360", role: "img", "aria-label": `${chart.title}, ${chart.type} chart` });
  canvas.append(svg("title", {}, chart.title));
  if (["pie", "doughnut"].includes(chart.type)) drawPie(canvas, chart);
  else if (["bar", "column"].includes(chart.type)) drawBars(canvas, chart, chart.type === "bar");
  else drawLines(canvas, chart);
  const pie = ["pie", "doughnut"].includes(chart.type);
  const legendItems = pie ? (chart.series[0].categories || chart.series[0].values.map((_, i) => String(i + 1))) : chart.series;
  const legend = h("ul.sh-chart-legend", legendItems.map((item, i) => h("li",
    h("span.sh-chart-key", { style: { "--series": SERIES[i % SERIES.length] }, dataset: { shape: String(i % 3), dash: String(i % 4) } }),
    h("span", pie ? item : (item.name || `Series ${i + 1}`)))));
  const categories = chart.series?.find((s) => s.categories?.length)?.categories || [];
  const rows = Array.from({ length: Math.max(categories.length, ...chart.series.map((s) => s.values?.length || 0)) }, (_, i) =>
    h("tr", h("th", categories[i] || String(i + 1)), chart.series.map((s) => h("td", s.values?.[i] == null ? "" : number(s.values[i])))));
  const table = h("table.sr-only", { "aria-label": `${chart.title} data` },
    h("thead", h("tr", h("th", "Category"), chart.series.map((s, i) => h("th", s.name || `Series ${i + 1}`)))), h("tbody", rows));
  const points = Math.max(...chart.series.map((s) => s.values?.length || 0));
  return h("figure.sh-chart", h("figcaption", h("h2", chart.title), h("p", `${chart.sheet} · ${points.toLocaleString()} point${points === 1 ? "" : "s"}`)),
    h("div.sh-chart-canvas", canvas), legend,
    chart.sampled ? h("p.sh-chart-note", { role: "status" }, "Showing a representative sample of 1,000 points.") : null,
    remote ? h("p.sh-chart-note", "To open or change this chart in its original app, use the computer that holds this project.") : null,
    table);
}

function domain(chart) {
  const values = chart.series.flatMap((s) => s.values || []).filter(Number.isFinite);
  let min = Math.min(0, ...values), max = Math.max(0, ...values);
  if (min === max) max = min + 1;
  return { min, max, y: (v) => 310 - (v - min) / (max - min) * 268 };
}

function axes(canvas, d, categories, horizontal = false) {
  canvas.append(svg("line", { x1: 56, y1: 310, x2: 690, y2: 310, class: "axis" }), svg("line", { x1: 56, y1: 30, x2: 56, y2: 310, class: "axis" }));
  if (!horizontal) {
    for (let i = 0; i <= 4; i++) {
      const y = 310 - i * 67;
      canvas.append(svg("line", { x1: 56, y1: y, x2: 690, y2: y, class: "grid" }), svg("text", { x: 48, y: y + 4, "text-anchor": "end", class: "tick" }, number(d.min + (d.max - d.min) * i / 4)));
    }
  }
  const step = Math.max(1, Math.ceil(categories.length / 8));
  categories.forEach((label, i) => {
    if (i % step) return;
    const x = 56 + (i + 0.5) / Math.max(1, categories.length) * 634;
    const text = String(label);
    canvas.append(svg("text", { x, y: 332, "text-anchor": "middle", class: "tick" }, text.length > 14 ? `${text.slice(0, 13)}…` : text));
  });
}

function drawBars(canvas, chart, horizontal = false) {
  const categories = chart.series.find((s) => s.categories?.length)?.categories || chart.series[0].values.map((_, i) => String(i + 1));
  const d = domain(chart);
  if (horizontal) {
    const values = chart.series.flatMap((s) => s.values || []).filter(Number.isFinite);
    const min = Math.min(0, ...values), max = Math.max(0, ...values);
    const x = (v) => 150 + (v - min) / Math.max(1e-9, max - min) * 540;
    const group = 280 / Math.max(1, categories.length);
    const height = Math.max(2, group * 0.72 / chart.series.length);
    for (let i = 0; i <= 4; i++) {
      const gx = 150 + i * 135;
      canvas.append(svg("line", { x1: gx, y1: 30, x2: gx, y2: 310, class: "grid" }), svg("text", { x: gx, y: 332, "text-anchor": "middle", class: "tick" }, number(min + (max - min) * i / 4)));
    }
    const zero = x(0);
    canvas.append(svg("line", { x1: zero, y1: 30, x2: zero, y2: 310, class: "axis" }));
    chart.series.forEach((series, si) => series.values.forEach((value, i) => {
      const y = 30 + i * group + group * 0.14 + si * height;
      canvas.append(svg("rect", { x: Math.min(x(value), zero), y, width: Math.max(1, Math.abs(x(value) - zero)), height, fill: SERIES[si % SERIES.length], rx: 1.5 }, svg("title", {}, `${categories[i] || i + 1}: ${number(value)}`)));
    }));
    const step = Math.max(1, Math.ceil(categories.length / 9));
    categories.forEach((label, i) => {
      if (i % step) return;
      const text = String(label);
      canvas.append(svg("text", { x: 142, y: 30 + (i + .5) * group + 4, "text-anchor": "end", class: "tick" }, text.length > 15 ? `${text.slice(0, 14)}…` : text));
    });
    return;
  }
  axes(canvas, d, categories);
  const group = 634 / Math.max(1, categories.length);
  const width = Math.max(2, group * 0.72 / chart.series.length);
  chart.series.forEach((series, si) => series.values.forEach((value, i) => {
    const x = 56 + i * group + group * 0.14 + si * width;
    const zero = d.y(0), y = d.y(value);
    canvas.append(svg("rect", { x, y: Math.min(y, zero), width, height: Math.max(1, Math.abs(zero - y)), fill: SERIES[si % SERIES.length], rx: 1.5 }, svg("title", {}, `${categories[i] || i + 1}: ${number(value)}`)));
  }));
}

function marker(x, y, index, title) {
  const color = SERIES[index % SERIES.length];
  const shape = index % 3;
  if (shape === 1) return svg("rect", { x: x - 3.5, y: y - 3.5, width: 7, height: 7, fill: color, transform: `rotate(45 ${x} ${y})` }, svg("title", {}, title));
  if (shape === 2) return svg("path", { d: `M${x} ${y - 4} L${x + 4} ${y + 3.5} L${x - 4} ${y + 3.5} Z`, fill: color }, svg("title", {}, title));
  return svg("circle", { cx: x, cy: y, r: 3.5, fill: color }, svg("title", {}, title));
}

function drawLines(canvas, chart) {
  const categories = chart.series.find((s) => s.categories?.length)?.categories || chart.series[0].values.map((_, i) => String(i + 1));
  const d = domain(chart);
  axes(canvas, d, categories);
  chart.series.forEach((series, si) => {
    const xs = series.x_values?.length ? series.x_values : series.values.map((_, i) => i);
    const xmin = Math.min(...xs), xmax = Math.max(...xs);
    const x = (v, i) => series.x_values?.length ? 56 + (v - xmin) / Math.max(1e-9, xmax - xmin) * 634 : 56 + (i + 0.5) / Math.max(1, series.values.length) * 634;
    const pts = series.values.map((v, i) => `${x(xs[i], i)},${d.y(v)}`).join(" ");
    if (chart.type === "area") canvas.append(svg("polygon", { points: `56,${d.y(0)} ${pts} 690,${d.y(0)}`, fill: SERIES[si % SERIES.length], opacity: 0.16 }));
    canvas.append(svg("polyline", { points: pts, fill: "none", stroke: SERIES[si % SERIES.length], "stroke-width": 2.2, "stroke-dasharray": ["", "7 4", "2 3", "10 3 2 3"][si % 4] }));
    series.values.forEach((v, i) => canvas.append(marker(x(xs[i], i), d.y(v), si, `${series.name}: ${number(v)}`)));
  });
}

function drawPie(canvas, chart) {
  const series = chart.series[0];
  const values = series.values.map((v) => Math.max(0, v));
  const total = values.reduce((a, b) => a + b, 0) || 1;
  let angle = -Math.PI / 2;
  values.forEach((value, i) => {
    const next = angle + value / total * Math.PI * 2;
    const r = 120, x1 = 360 + Math.cos(angle) * r, y1 = 180 + Math.sin(angle) * r, x2 = 360 + Math.cos(next) * r, y2 = 180 + Math.sin(next) * r;
    const large = next - angle > Math.PI ? 1 : 0;
    canvas.append(svg("path", { d: `M360 180 L${x1} ${y1} A${r} ${r} 0 ${large} 1 ${x2} ${y2} Z`, fill: SERIES[i % SERIES.length], stroke: "var(--bg)", "stroke-width": 2 },
      svg("title", {}, `${series.categories?.[i] || i + 1}: ${number(value)}`)));
    angle = next;
  });
  if (chart.type === "doughnut") canvas.append(svg("circle", { cx: 360, cy: 180, r: 62, fill: "var(--bg)" }));
}

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
  const charts = h("div.sh-charts", { role: "tabpanel", hidden: true });
  const cells = h("div.sh-cells", { role: "tabpanel" }, grid);
  const viewTabs = h("div.sh-view-tabs", { role: "tablist", "aria-label": "Spreadsheet view" });
  const tabs = h("div.sh-tabs", { role: "tablist" });
  el.append(bar, viewTabs, cells, charts, tabs);

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

  function drawViewTabs() {
    const available = (s.file.charts?.length || 0) + (s.file.chart_warnings?.length || 0) > 0;
    viewTabs.hidden = !available;
    if (!available) { viewTabs.replaceChildren(); cells.hidden = false; charts.hidden = true; return; }
    const active = charts.hidden ? "cells" : "charts";
    const choose = (name) => {
      const showingCharts = name === "charts";
      cells.hidden = showingCharts;
      charts.hidden = !showingCharts;
      for (const button of viewTabs.children) {
        const selected = button.dataset.view === name;
        button.setAttribute("aria-selected", String(selected));
        button.tabIndex = selected ? 0 : -1;
      }
      if (showingCharts) drawCharts();
    };
    const buttons = [["cells", "Cells"], ["charts", "Charts"]].map(([name, label]) => h("button.sh-view-tab", {
      type: "button", role: "tab", dataset: { view: name }, "aria-selected": String(name === active), tabindex: name === active ? "0" : "-1",
      onclick: () => choose(name),
      onkeydown: (e) => {
        const order = [...viewTabs.children];
        let i = order.indexOf(e.currentTarget);
        if (e.key === "ArrowRight" || e.key === "ArrowLeft") i = (i + (e.key === "ArrowRight" ? 1 : -1) + order.length) % order.length;
        else if (e.key === "Home") i = 0;
        else if (e.key === "End") i = order.length - 1;
        else return;
        e.preventDefault();
        choose(order[i].dataset.view);
        order[i].focus();
      },
    }, label));
    viewTabs.replaceChildren(...buttons);
  }

  function drawCharts() {
    const remote = !ctx.ark.isLocal();
    charts.replaceChildren(...(s.file.charts || []).map((chart) => chartFigure(chart, remote)), ...(s.file.chart_warnings || []).map((warning) =>
      h("figure.sh-chart.unsupported", h("figcaption", h("h2", "Chart unavailable"), h("p", warning),
        remote ? h("p", "Open it on the computer that holds this project.") : null))));
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
    drawViewTabs();
    if (!charts.hidden) drawCharts();
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
  drawViewTabs();
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
