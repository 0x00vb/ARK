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
      res.end(`<!doctype html><body><div id="reply"></div><p contenteditable="true"><a href="https://typed.dev">typed</a></p>
<script type="module">
import { markdown } from "/js/md.js";
import { openLinksOutside, openOutside, setOpener } from "/js/links.js";
window.opened = [];
setOpener((url) => window.opened.push(url));
openLinksOutside(document);
window.openOutside = openOutside;
window.show = (text) => { document.getElementById("reply").innerHTML = markdown(text); };
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

async function page() {
  const p = await browser.newPage();
  await p.goto(`${base}/t.html`);
  await p.waitForFunction(() => typeof window.show === "function");
  return p;
}

test("a web link in a reply opens outside and the page stays put", async () => {
  const p = await page();
  await p.evaluate(() => window.show("See [the docs](https://example.com/guide) now."));
  await p.click("#reply a");
  assert.deepEqual(await p.evaluate(() => window.opened), ["https://example.com/guide"]);
  assert.equal(await p.evaluate(() => location.pathname), "/t.html");
  assert.equal(p.context().pages().length, 1, "no window of its own");
  await p.close();
});

test("a link in text being edited, and a page link, are left to the page", async () => {
  const p = await page();
  await p.click("p[contenteditable] a");
  await p.evaluate(() => window.show(`[here](${location.origin}/t.html#x)`));
  await p.click("#reply a");
  assert.deepEqual(await p.evaluate(() => window.opened), []);
  await p.close();
});

test("a relative path is a project file link; other targets are not", async () => {
  const p = await page();
  const html = await p.evaluate(() => {
    window.show("Read [the plan](./PLAN.md), [x](javascript:alert(1)), [y](#top), [r](/etc/passwd), [n](//evil.example/x), [u](../../secret) and [w](www.example.com/a).");
    return document.getElementById("reply").innerHTML;
  });
  assert.match(html, /<a class="file-link" href="#" data-path="\.\/PLAN\.md">the plan<\/a>/);
  assert.doesNotMatch(html, /href="javascript/i);
  for (const bad of ["#top", "/etc/passwd", "//evil.example/x", "../../secret"]) assert.ok(!html.includes(`data-path="${bad}"`), bad);
  assert.match(html, /<a href="https:\/\/www\.example\.com\/a" target="_blank"/);
  await p.close();
});

test("emphasis marks never reach into a link target", async () => {
  const p = await page();
  const html = await p.evaluate(() => {
    window.show("[n](./_draft_/x.md) [m](./my*file*.md) [a](https://x.dev/a_b_c) and _real_ emphasis");
    return document.getElementById("reply").innerHTML;
  });
  assert.ok(html.includes('data-path="./_draft_/x.md"'), html);
  assert.ok(html.includes('data-path="./my*file*.md"'), html);
  assert.ok(html.includes('href="https://x.dev/a_b_c"'), html);
  assert.match(html, /<em>real<\/em>/);
  await p.close();
});

test("a script that opens an address goes through the same opener", async () => {
  const p = await page();
  await p.evaluate(() => window.openOutside("https://example.com/connect"));
  assert.deepEqual(await p.evaluate(() => window.opened), ["https://example.com/connect"]);
  assert.equal(p.context().pages().length, 1);
  await p.close();
});
