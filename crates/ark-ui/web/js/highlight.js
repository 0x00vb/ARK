// Syntax colours for the text editor: a small tokenizer per language family, no dependencies.
// It returns escaped HTML with <span class="tk-…"> around comments, strings, numbers, keywords
// and literals. The colours are theme variables (app.css: --tok-*), so light, dark and the
// person's accent apply without code here. A file of no known language is not coloured: its
// editor stays the plain one.

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const words = (s) => s.split(/\s+/).filter(Boolean);

// What each family has: comment markers, quote characters (a quoted string closes on its line,
// so an apostrophe in text stays plain), and its keywords and literals.
const FAMILIES = {
  js: {
    line: "//", block: true, quotes: `"'`, template: true,
    keywords: "as async await break case catch class const continue debugger declare default delete do else enum export extends finally for from function if implements import in instanceof interface keyof let namespace new of private protected public readonly return satisfies static super switch throw try type typeof var void while with yield",
    literals: "true false null undefined NaN Infinity this",
  },
  c: {
    line: "//", block: true, quotes: `"'`,
    keywords: "abstract as async await break case catch class const continue default do else enum extends extension final finally for fun func guard if implements import in interface is let namespace new object override package private protected protocol public return sealed static struct super switch throw throws try typedef union using val var void volatile when while",
    literals: "true false null nil this self",
  },
  go: {
    line: "//", block: true, quotes: `"'`, template: true,
    keywords: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var",
    literals: "true false nil iota",
  },
  rust: {
    line: "//", block: true, quotes: `"`, chars: true,
    keywords: "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while",
    literals: "true false None",
  },
  python: {
    hash: true, quotes: `"'`, triple: true,
    keywords: "and as assert async await break case class continue def del elif else except finally for from global if import in is lambda match nonlocal not or pass raise return try while with yield",
    literals: "True False None self",
  },
  shell: {
    hash: true, quotes: `"'`,
    keywords: "case do done elif else esac export fi for function if in local readonly return select then until while",
    literals: "true false",
  },
  ruby: {
    hash: true, quotes: `"'`,
    keywords: "alias and begin break case class def defined do else elsif end ensure for if in module next not or redo rescue retry return super then undef unless until when while yield",
    literals: "true false nil self",
  },
  config: { hash: true, quotes: `"'`, keywords: "", literals: "true false null" },
  json: { line: "//", block: true, quotes: `"`, keywords: "", literals: "true false null" },
  css: { block: true, quotes: `"'`, keywords: "", literals: "" },
  sql: {
    line: "--", block: true, quotes: `'"`, anyCase: true,
    keywords: "add all alter and as asc by case create default delete desc distinct drop else end exists foreign from group having in index inner insert into is join key left like limit not offset on or order outer primary references right select set table then union update values view when where with",
    literals: "true false null",
  },
  lua: {
    line: "--", longComment: true, quotes: `"'`,
    keywords: "and break do else elseif end for function goto if in local not or repeat return then until while",
    literals: "true false nil",
  },
};

const BY_EXT = {
  js: "js mjs cjs jsx ts tsx mts cts",
  c: "c h cc cpp cxx hpp hh cs java kt kts scala swift dart php m mm",
  go: "go",
  rust: "rs",
  python: "py pyi",
  shell: "sh bash zsh",
  ruby: "rb rake gemspec",
  config: "toml yaml yml ini cfg conf env properties",
  json: "json jsonc json5",
  css: "css scss less",
  sql: "sql",
  lua: "lua",
};
const BY_NAME = { makefile: "shell", dockerfile: "shell", gemfile: "ruby", rakefile: "ruby" };

const EXT = {};
for (const [family, exts] of Object.entries(BY_EXT)) for (const e of words(exts)) EXT[e] = family;

/// The language family of a file name, or null when it is not coloured.
export function languageOf(path) {
  const name = String(path).split("/").pop().toLowerCase();
  if (BY_NAME[name]) return BY_NAME[name];
  const dot = name.lastIndexOf(".");
  return dot < 0 ? null : EXT[name.slice(dot + 1)] || null;
}

const reEsc = (s) => s.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");

// Groups: 1 comment, 2 string, 3 number, 4 word.
function compile(f) {
  const comments = [];
  // Lua's long comments, `--[[ … ]]` and `--[=[ … ]=]` (no capture group: it would renumber
  // the groups below).
  if (f.longComment) for (const eq of ["", "=", "=="]) comments.push(String.raw`--\[${eq}\[[\s\S]*?(?:\]${eq}\]|$)`);
  if (f.block) comments.push(String.raw`\/\*[\s\S]*?(?:\*\/|$)`);
  if (f.line) comments.push(`${reEsc(f.line)}[^\\n]*`);
  // A `#` starts a comment at the start of a line or after a space, not inside `${#x}` or `a#b`.
  if (f.hash) comments.push(String.raw`(?:^|[ \t\n])#[^\n]*`);
  const strings = [];
  if (f.triple) strings.push(String.raw`"""[\s\S]*?(?:"""|$)`, String.raw`'''[\s\S]*?(?:'''|$)`);
  if (f.template) strings.push(String.raw`\`(?:\\[\s\S]|[^\`\\])*\`?`);
  for (const q of f.quotes) strings.push(`${q}(?:\\\\.|[^${q}\\\\\\n])*${q}`);
  // A Rust char is one character or one escape, so a lifetime (`'a`) is not a string.
  if (f.chars) strings.push(String.raw`'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^'\\\n])'`);
  const parts = [
    comments.length ? `(${comments.join("|")})` : "(?!)",
    `(${strings.join("|")})`,
    String.raw`(\b\d[\w.]*)`,
    String.raw`([A-Za-z_$][\w$]*)`,
  ];
  const norm = f.anyCase ? (w) => w.toLowerCase() : (w) => w;
  return {
    re: new RegExp(parts.join("|"), "g"),
    keywords: new Set(words(f.keywords)),
    literals: new Set(words(f.literals)),
    norm,
  };
}

const compiled = {};

/// Above this many characters the editor is not coloured, so typing stays fast.
export const MAX_CHARS = 150000;

/// `text` as highlighted HTML for the file at `path`. Always ends with a newline, so the layer
/// under the editor is as tall as the text.
export function highlight(text, path) {
  const family = languageOf(path);
  if (!family || text.length > MAX_CHARS) return esc(text) + "\n";
  const { re, keywords, literals, norm } = (compiled[family] ||= compile(FAMILIES[family]));
  re.lastIndex = 0;
  let out = "";
  let at = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m[0] === "") {
      re.lastIndex++;
      continue;
    }
    let start = m.index;
    let token = m[0];
    let cls = null;
    if (m[1] != null) {
      cls = "com";
      // The space or line break before a `#` comment is not part of it.
      if (/^[ \t\n]#/.test(token)) {
        start += 1;
        token = token.slice(1);
      }
    } else if (m[2] != null) cls = "str";
    else if (m[3] != null) cls = "num";
    else if (keywords.has(norm(m[4]))) cls = "key";
    else if (literals.has(m[4]) || literals.has(norm(m[4]))) cls = "lit";
    if (!cls) continue;
    out += esc(text.slice(at, start)) + `<span class="tk-${cls}">${esc(token)}</span>`;
    at = start + token.length;
  }
  return out + esc(text.slice(at)) + "\n";
}
