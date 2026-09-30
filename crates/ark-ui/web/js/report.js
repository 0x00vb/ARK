// Renders an ark-interact `Report` (a command's structured result). The same structure the
// terminal prints as aligned text, laid out with type and spacing here.

import { h, tokens } from "./dom.js";

const toneClass = (t) => (t && t !== "normal" ? `.tone-${t}` : "");

export function renderReport(report, { onCommand } = {}) {
  const root = h("div.report");
  if (report.title) root.append(h("div.report-title", report.title));
  for (const b of report.blocks || []) {
    switch (b.type) {
      case "heading":
        root.append(h("div.report-heading", b.text));
        break;
      case "text":
        root.append(h(`p.report-text${toneClass(b.tone)}`, b.text));
        break;
      case "fields": {
        const dl = h("dl.report-fields");
        for (const f of b.fields) dl.append(h("dt", f.label), h(`dd${toneClass(f.tone)}`, f.value));
        root.append(dl);
        break;
      }
      case "table": {
        const t = h("table.report-table");
        if (b.columns.some((c) => c.title)) {
          t.append(h("thead", h("tr", b.columns.map((c) => h(`th${c.align === "right" ? ".num" : ""}`, c.title)))));
        }
        const body = h("tbody");
        for (const r of b.rows) {
          const tr = h(`tr${toneClass(r.tone)}`,
            r.cells.map((c, i) => h(`td${b.columns[i]?.align === "right" ? ".num" : ""}${toneClass(c.tone)}`, c.text)));
          if (r.command && onCommand) {
            tr.classList.add("clickable");
            tr.addEventListener("click", () => onCommand(r.command));
          }
          body.append(tr);
        }
        t.append(body);
        root.append(t);
        break;
      }
      case "code":
        root.append(h("pre.report-code", b.text));
        break;
      case "meter": {
        const pct = b.total ? Math.min(100, (b.used / b.total) * 100) : 0;
        root.append(
          h("div.meter",
            h("div.meter-bar", h("span", { style: { width: `${pct}%` } })),
            h("span.meter-label", `${tokens(b.used)} of ${tokens(b.total)}`)),
        );
        break;
      }
      case "actions":
        root.append(h("div.report-actions", b.actions.map((a) =>
          h("button.link", { onclick: () => onCommand && onCommand(a.command.includes("<") ? a.command.split("<")[0] : a.command) }, a.label))));
        break;
    }
  }
  return root;
}
