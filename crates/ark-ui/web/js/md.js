// Markdown for agent text. Safe by construction: every character is escaped before any tag is
// added, and links only keep http(s) targets. Evidence citations (`[inv:1a2b3c4d]`) become
// links that open the evidence beside the conversation. Rendering the whole text again on each streamed
// chunk is cheap at these sizes and keeps half-finished markup well formed.

import { esc } from "./dom.js";

function inline(s) {
  let out = esc(s);
  const codes = [];
  out = out.replace(/`([^`]+)`/g, (_, c) => {
    codes.push(c);
    return `\u0000${codes.length - 1}\u0000`;
  });
  out = out
    .replace(/\[inv:([0-9a-f]{4,})\]/gi, (_, id) => `<a class="cite" data-inv="${id}" href="#">${id.slice(0, 6)}</a>`)
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>')
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
    .replace(/(^|\W)_([^_\s][^_]*)_(?=\W|$)/g, "$1<em>$2</em>");
  return out.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${codes[Number(i)]}</code>`);
}

function table(rows) {
  // A table still streaming in may have its header and no complete separator row yet.
  if (rows.length < 2) return `<p>${inline(rows[0] || "")}</p>`;
  const cells = (r) => r.replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const head = cells(rows[0]);
  const aligns = cells(rows[1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : ""));
  const td = (tag, c, i) => `<${tag}${aligns[i] ? ` style="text-align:${aligns[i]}"` : ""}>${inline(c)}</${tag}>`;
  let html = `<div class="md-table"><table><thead><tr>${head.map((c, i) => td("th", c, i)).join("")}</tr></thead><tbody>`;
  for (const r of rows.slice(2)) html += `<tr>${cells(r).map((c, i) => td("td", c, i)).join("")}</tr>`;
  return html + "</tbody></table></div>";
}

export function markdown(src) {
  const lines = String(src || "").replace(/\r\n/g, "\n").split("\n");
  let html = "";
  let para = [];
  let list = null; // { type, items: [] }
  const flushPara = () => {
    if (para.length) html += `<p>${inline(para.join(" "))}</p>`;
    para = [];
  };
  const flushList = () => {
    if (list) html += `<${list.type}>${list.items.map((i) => `<li>${inline(i)}</li>`).join("")}</${list.type}>`;
    list = null;
  };
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const line = raw.trim();
    const fence = line.match(/^```\s*([\w+-]*)/);
    if (fence) {
      flushPara();
      flushList();
      const lang = fence[1];
      const body = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith("```")) body.push(lines[i++]);
      html += `<pre class="md-code"${lang ? ` data-lang="${esc(lang)}"` : ""}><code>${esc(body.join("\n"))}</code></pre>`;
      continue;
    }
    if (!line) {
      flushPara();
      flushList();
      continue;
    }
    if (/^\|.*\|$/.test(line) && i + 1 < lines.length && /^\|?\s*:?-{2,}/.test(lines[i + 1].trim())) {
      flushPara();
      flushList();
      const rows = [];
      while (i < lines.length && /^\|.*\|$/.test(lines[i].trim())) rows.push(lines[i++].trim());
      i--;
      html += table(rows);
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)/);
    if (heading) {
      flushPara();
      flushList();
      const n = Math.min(heading[1].length + 1, 5);
      html += `<h${n}>${inline(heading[2])}</h${n}>`;
      continue;
    }
    if (/^(-{3,}|\*{3,})$/.test(line)) {
      flushPara();
      flushList();
      html += "<hr />";
      continue;
    }
    if (line.startsWith(">")) {
      flushPara();
      flushList();
      html += `<blockquote>${inline(line.replace(/^>\s?/, ""))}</blockquote>`;
      continue;
    }
    const ul = line.match(/^[-*•]\s+(.*)/);
    const ol = line.match(/^\d+[.)]\s+(.*)/);
    if (ul || ol) {
      flushPara();
      const type = ul ? "ul" : "ol";
      if (list && list.type !== type) flushList();
      if (!list) list = { type, items: [] };
      list.items.push((ul || ol)[1]);
      continue;
    }
    if (list && /^\s{2,}/.test(raw)) {
      list.items[list.items.length - 1] += " " + line;
      continue;
    }
    flushList();
    para.push(line);
  }
  flushPara();
  flushList();
  return html;
}
