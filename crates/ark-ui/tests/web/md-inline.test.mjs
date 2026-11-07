import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
const types = { ".js": "text/javascript", ".css": "text/css", ".html": "text/html" };
let browser;
let server;
let base;

before(async () => {
  browser = await webkit.launch({ headless: true });
  server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    if (url.pathname === "/t.html") {
      res.setHeader("content-type", "text/html");
      res.end(`<!doctype html><link rel="stylesheet" href="/app.css"><body><div id="page" class="prose md-page" style="height:600px;width:520px"></div>
<script type="module">
import { inlineEditor } from "/js/md-inline.js";
window.openEditor = (text, { fail = false, answer = null } = {}) => {
  const saves = [];
  const page = document.getElementById("page");
  page.innerHTML = "";
  const editor = inlineEditor(page, { text, readOnly: false, drawn() {}, async save(t) { if (fail) throw Error("no"); saves.push(t); return answer ?? t; } });
  window.editor = editor; window.saves = saves;
};
</script>`);
      return;
    }
    const file = resolve(root, "." + url.pathname);
    if (!file.startsWith(root + sep)) { res.writeHead(403).end(); return; }
    try {
      const ext = file.slice(file.lastIndexOf("."));
      res.setHeader("content-type", types[ext] || "application/octet-stream");
      res.end(await readFile(file));
    } catch { res.writeHead(404).end(); }
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  base = `http://127.0.0.1:${server.address().port}`;
});
after(async () => {
  await browser?.close();
  server?.close();
});

async function open(text, options) {
  const page = await browser.newPage({ viewport: { width: 700, height: 800 } });
  await page.goto(`${base}/t.html`);
  await page.waitForFunction(() => typeof window.openEditor === "function");
  await page.evaluate(([t, o]) => window.openEditor(t, o), [text, options]);
  return page;
}
const saved = (page) => page.evaluate(async () => { await window.editor.flush(); return window.saves.at(-1); });

test("typing changes only its own block and the file keeps its line endings", async () => {
  const page = await open("# Title\r\n\r\nfirst **bold** line\r\n\r\n- a\r\n- b\r\n");
  await page.locator("h2").click();
  const area = page.locator("textarea.md-edit");
  assert.equal(await area.inputValue(), "# Title");
  await area.press("End");
  await page.keyboard.type(" two");
  assert.equal(await saved(page), "# Title two\r\n\r\nfirst **bold** line\r\n\r\n- a\r\n- b\r\n");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("textarea").count(), 0);
  assert.equal(await page.locator("h2").innerText(), "Title two");
  await page.close();
});

test("a new line splits a block, and blocks after it keep their place", async () => {
  const page = await open("one\n\ntwo\n\nthree\n");
  await page.locator("p", { hasText: "one" }).click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.type("one and a half");
  await page.locator("p", { hasText: "three" }).click();
  assert.equal(await page.locator("p").count(), 3, "the split drew two blocks, the third is open");
  await page.keyboard.press("End");
  await page.keyboard.type("!");
  assert.equal(await saved(page), "one\n\none and a half\n\ntwo\n\nthree!\n");
  await page.close();
});

test("a click puts the caret at the clicked word of the source", async () => {
  const page = await open("Hello **bold** world and more\n");
  const p = page.locator("p");
  const box = await p.boundingBox();
  // The end of the line, past the last word.
  await page.mouse.click(box.x + box.width - 2, box.y + box.height / 2);
  const area = page.locator("textarea.md-edit");
  const at = await area.evaluate((a) => a.selectionStart);
  assert.ok(at >= "Hello **bold** world and".length, `caret at ${at}`);
  await page.close();
});

test("arrows cross blocks, escape leaves, empty edges are tidied", async () => {
  const page = await open("alpha\n\nbeta\n");
  await page.locator("p", { hasText: "alpha" }).click();
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowDown");
  assert.equal(await page.locator("textarea.md-edit").inputValue(), "beta");
  assert.equal(await page.locator("textarea.md-edit").evaluate((a) => a.selectionStart), 0);
  await page.keyboard.press("ArrowUp");
  assert.equal(await page.locator("textarea.md-edit").inputValue(), "alpha");

  // A block added under the last one and left empty leaves the file as it was.
  await page.keyboard.press("Escape");
  await page.locator("#page").click({ position: { x: 300, y: 560 } });
  await page.locator("textarea.md-edit").waitFor();
  await page.keyboard.press("Escape");
  assert.equal(await page.evaluate(() => window.editor.content()), "alpha\n\nbeta\n");
  // And one with words in it is a paragraph of its own.
  await page.locator("#page").click({ position: { x: 300, y: 560 } });
  await page.keyboard.type("gamma");
  assert.equal(await saved(page), "alpha\n\nbeta\n\ngamma\n");
  await page.close();
});

test("an empty file takes its first paragraph", async () => {
  const page = await open("");
  await page.locator("#page").click({ position: { x: 300, y: 300 } });
  await page.keyboard.type("hello");
  assert.equal(await saved(page), "hello\n");
  await page.close();
});

test("a failed save keeps the text unsaved", async () => {
  const page = await open("keep me\n", { fail: true });
  await page.locator("p").click();
  await page.keyboard.press("End");
  await page.keyboard.type("!");
  await page.evaluate(() => window.editor.flush());
  assert.equal(await page.evaluate(() => window.editor.dirty()), true);
  assert.equal(await page.evaluate(() => window.editor.content()), "keep me!\n");
  await page.close();
});

test("leaving by a click elsewhere saves, and an outside update waits for the open block", async () => {
  const page = await open("alpha\n\nbeta\n");
  await page.locator("p", { hasText: "alpha" }).click();
  await page.keyboard.press("End");
  await page.keyboard.type("!");
  assert.equal(await page.evaluate(() => window.editor.dirty()), true, "an open block counts as dirty");
  assert.equal(await page.evaluate(() => window.editor.set("other\n")), false, "an update is refused while it is open");
  assert.equal(await page.evaluate(() => window.editor.content()), "alpha!\n\nbeta\n");
  await page.mouse.click(10, 760);
  await page.waitForFunction(() => window.saves.length === 1);
  assert.equal(await page.evaluate(() => window.saves[0]), "alpha!\n\nbeta\n");
  assert.equal(await page.evaluate(() => window.editor.set("other\n")), true);
  assert.equal(await page.locator("p").innerText(), "other");
  await page.close();
});

test("shift and the arrows select, they do not leave the block", async () => {
  const page = await open("alpha\n\nbeta\n");
  await page.locator("p", { hasText: "alpha" }).click();
  await page.keyboard.press("End");
  await page.keyboard.press("Shift+ArrowDown");
  assert.equal(await page.locator("textarea.md-edit").inputValue(), "alpha");
  await page.close();
});

test("a block emptied of its text goes, with one blank line", async () => {
  const page = await open("a\n\nb\n\nc\n");
  await page.locator("p", { hasText: "b" }).click();
  await page.keyboard.press("Meta+a");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Escape");
  assert.equal(await page.evaluate(() => window.editor.content()), "a\n\nc\n");
  assert.equal(await page.locator("p").count(), 2);
  await page.close();
});

test("a fence typed into a block draws the blocks it swallows as code", async () => {
  const page = await open("a\n\nb\n");
  await page.locator("p", { hasText: "a" }).first().click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.type("```");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("pre.md-code").count(), 1);
  assert.match(await page.locator("pre.md-code").innerText(), /b/);
  await page.close();
});

test("two changes to different lines merge, to the same lines they do not", async () => {
  const page = await open("x\n");
  const merged = await page.evaluate(async () => {
    const { mergeText } = await import("/js/md-inline.js");
    return [
      mergeText("a\nb\nc", "a\nB\nc", "a\nb\nC"),
      mergeText("a\nb\nc", "a\nB\nc", "a\nbee\nc"),
      mergeText("a\r\nb\r\nc", "A\r\nb\r\nc", "a\nb\nC"),
      mergeText("a\nb", "a\nb\nours", "a\nb\ntheirs"),
    ];
  });
  assert.deepEqual(merged, ["a\nB\nC", null, "A\r\nb\r\nC", null]);
  await page.close();
});

test("a save that merged another change shows the merged page and keeps the block open", async () => {
  const page = await open("one\n\ntwo\n", { answer: "one!\n\ntwo, changed elsewhere\n" });
  await page.locator("p", { hasText: "one" }).click();
  await page.keyboard.press("End");
  await page.keyboard.type("!");
  await page.waitForFunction(() => window.saves.length === 1);
  await page.locator("p", { hasText: "changed elsewhere" }).waitFor();
  assert.equal(await page.locator("textarea.md-edit").inputValue(), "one!");
  assert.equal(await page.evaluate(() => window.editor.content()), "one!\n\ntwo, changed elsewhere\n");
  await page.close();
});
