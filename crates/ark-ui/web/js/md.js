// Markdown for agent text. Safe by construction: every character is escaped before any tag is
// added. Links keep http(s) targets; a relative path is a project file. Evidence citations
// (`[inv:1a2b3c4d]`) become links that open the evidence beside the conversation. Rendering the
// whole text again on each streamed chunk is cheap at these sizes and keeps half-finished markup
// well formed.

import { esc } from "./dom.js";

/// Set while rendering a document that may show the project's images (`markdown(src, { files: true })`).
let fileImages = false;
/// Set while rendering a page that is edited in place: each block names its source lines.
let blockLines = false;

/// ` data-lines="a:b"` for the block drawn from source lines `a` to `b` (end exclusive).
const at = (a, b) => (blockLines ? ` data-lines="${a}:${b}"` : "");

/// A path inside the project: relative, no scheme, no `..`, not a page anchor.
function projectPath(path) {
  return !/^([a-z][a-z0-9+.-]*:|\/|#|~)/i.test(path) && !path.split("/").includes("..");
}

function inline(s) {
  let out = esc(s);
  // An image: in a document of the project, one of its files (the view loads it by `data-file`);
  // anywhere else only its words.
  // Kept aside like code, so no later rule reaches inside the tag.
  const images = [];
  out = out.replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, (_, alt, src) => {
    if (!fileImages || /^[a-z][a-z0-9+.-]*:|^\//i.test(src)) return alt;
    images.push(`<img class="md-img" data-file="${src}" alt="${alt}">`);
    return `\u0001${images.length - 1}\u0001`;
  });
  const opens = [];
  const keep = (tag, text) => {
    opens.push(tag);
    return `\u0002${opens.length - 1}\u0002${text}</a>`;
  };
  const codes = [];
  out = out.replace(/`([^`]+)`/g, (_, c) => {
    codes.push(c);
    return `\u0000${codes.length - 1}\u0000`;
  });
  out = out
    .replace(/\[inv:([0-9a-f]{4,})\]/gi, (_, id) => `<a class="cite" data-inv="${id}" href="#">${id.slice(0, 6)}</a>`)
    // A link's target is kept aside like code, so the emphasis rules never reach into it.
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, (_, text, url) => keep(`<a href="${url}" target="_blank" rel="noreferrer">`, text))
    .replace(/\[([^\]]+)\]\(www\.([^)\s]+)\)/g, (_, text, rest) => keep(`<a href="https://www.${rest}" target="_blank" rel="noreferrer">`, text))
    // A relative path is a file of the project: it opens in the file view.
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (all, text, path) => (projectPath(path) ? keep(`<a class="file-link" href="#" data-path="${path}">`, text) : all))
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
    .replace(/(^|\W)_([^_\s][^_]*)_(?=\W|$)/g, "$1<em>$2</em>");
  return out
    .replace(/\u0002(\d+)\u0002/g, (_, i) => opens[Number(i)])
    .replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${codes[Number(i)]}</code>`)
    .replace(/\u0001(\d+)\u0001/g, (_, i) => images[Number(i)]);
}

function table(rows, range) {
  // A table still streaming in may have its header and no complete separator row yet.
  if (rows.length < 2) return `<p${range}>${inline(rows[0] || "")}</p>`;
  const cells = (r) => r.replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const head = cells(rows[0]);
  const aligns = cells(rows[1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : ""));
  const td = (tag, c, i) => `<${tag}${aligns[i] ? ` style="text-align:${aligns[i]}"` : ""}>${inline(c)}</${tag}>`;
  let html = `<div class="md-table"${range}><table><thead><tr>${head.map((c, i) => td("th", c, i)).join("")}</tr></thead><tbody>`;
  for (const r of rows.slice(2)) html += `<tr>${cells(r).map((c, i) => td("td", c, i)).join("")}</tr>`;
  return html + "</tbody></table></div>";
}

export function markdown(src, { files = false, lines = false } = {}) {
  fileImages = files;
  blockLines = lines;
  try {
    return render(src);
  } finally {
    fileImages = false;
    blockLines = false;
  }
}

function render(src) {
  const lines = String(src || "").replace(/\r\n/g, "\n").split("\n");
  let html = "";
  let para = [];
  let paraFrom = 0;
  let list = null; // { type, items: [], from }
  // A paragraph or list ends at the line being read.
  let now = 0;
  const flushPara = () => {
    if (para.length) html += `<p${at(paraFrom, now)}>${inline(para.join(" "))}</p>`;
    para = [];
  };
  const flushList = () => {
    if (list) html += `<${list.type}${at(list.from, now)}>${list.items.map((i) => `<li>${inline(i)}</li>`).join("")}</${list.type}>`;
    list = null;
  };
  for (let i = 0; i < lines.length; i++) {
    now = i;
    const raw = lines[i];
    const line = raw.trim();
    const fence = line.match(/^```\s*([\w+-]*)/);
    if (fence) {
      flushPara();
      flushList();
      const lang = fence[1];
      const body = [];
      const from = i;
      i++;
      while (i < lines.length && !lines[i].trim().startsWith("```")) body.push(lines[i++]);
      html += `<pre class="md-code"${lang ? ` data-lang="${esc(lang)}"` : ""}${at(from, Math.min(i + 1, lines.length))}><code>${esc(body.join("\n"))}</code></pre>`;
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
      const from = i;
      while (i < lines.length && /^\|.*\|$/.test(lines[i].trim())) rows.push(lines[i++].trim());
      i--;
      html += table(rows, at(from, i + 1));
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)/);
    if (heading) {
      flushPara();
      flushList();
      const n = Math.min(heading[1].length + 1, 5);
      html += `<h${n}${at(i, i + 1)}>${inline(heading[2])}</h${n}>`;
      continue;
    }
    if (/^(-{3,}|\*{3,})$/.test(line)) {
      flushPara();
      flushList();
      html += `<hr${at(i, i + 1)} />`;
      continue;
    }
    if (line.startsWith(">")) {
      flushPara();
      flushList();
      html += `<blockquote${at(i, i + 1)}>${inline(line.replace(/^>\s?/, ""))}</blockquote>`;
      continue;
    }
    const ul = line.match(/^[-*•]\s+(.*)/);
    const ol = line.match(/^\d+[.)]\s+(.*)/);
    if (ul || ol) {
      flushPara();
      const type = ul ? "ul" : "ol";
      if (list && list.type !== type) flushList();
      if (!list) list = { type, items: [], from: i };
      list.items.push((ul || ol)[1]);
      continue;
    }
    if (list && /^\s{2,}/.test(raw)) {
      list.items[list.items.length - 1] += " " + line;
      continue;
    }
    flushList();
    if (!para.length) paraFrom = i;
    para.push(line);
  }
  now = lines.length;
  flushPara();
  flushList();
  return html;
}
