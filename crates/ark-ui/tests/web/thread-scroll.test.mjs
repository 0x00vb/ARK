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
      res.end(`<!doctype html><link rel="stylesheet" href="/app.css"><body>
<div id="scroller" style="height:400px;width:600px;overflow:auto"><div id="thread" class="thread"></div></div>
<script type="module">
import { Thread } from "/js/conversation.js";
const api = { call: async () => ({}), toast() {} };
const thread = new Thread(document.getElementById("thread"), document.getElementById("scroller"), api, { inspect: false });
const lines = [];
for (let i = 0; i < 20; i++) lines.push("Earlier paragraph " + i + ".");
thread.renderStored([{ input: "Hello", index: 0, answer: lines.join("\\n\\n") }]);
window.thread = thread;
window.scroller = document.getElementById("scroller");
// A live turn streams a chunk every 60 ms until stopped.
window.stream = () => {
  thread.begin("Go");
  window.chunks = 0;
  window.timer = setInterval(() => thread.event({ kind: "text", text: "Streaming sentence " + window.chunks++ + ". " }), 60);
};
window.stopStream = () => { clearInterval(window.timer); thread.finish({ index: 1 }); };
window.ready = true;
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

async function streaming() {
  const page = await browser.newPage({ viewport: { width: 700, height: 600 } });
  await page.goto(`${base}/t.html`);
  await page.waitForFunction(() => window.ready);
  await page.evaluate(() => window.stream());
  // Live and following: several chunks in, the page sits on the end.
  await page.waitForFunction(() => window.chunks > 5 && window.thread.stick && scroller.scrollTop > 1000);
  await page.mouse.move(300, 200);
  return page;
}
const gap = (page) => page.evaluate(() => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight);

test("a small scroll up while text streams stays where the person put it", async () => {
  const page = await streaming();
  const top = await page.evaluate(() => scroller.scrollTop);
  await page.mouse.wheel(0, -30);
  await page.waitForFunction((t) => scroller.scrollTop < t, top);
  await page.waitForTimeout(500);
  assert.ok(await gap(page) >= 30, `still near the bottom: gap ${await gap(page)}`);
  await page.close();
});

test("a long scroll up reaches older content and stays while text streams", async () => {
  const page = await streaming();
  await page.mouse.wheel(0, -600);
  await page.waitForTimeout(100);
  const at = await page.evaluate(() => scroller.scrollTop);
  await page.waitForTimeout(500);
  assert.equal(await page.evaluate(() => scroller.scrollTop), at);
  await page.close();
});

test("scrolling back to the bottom follows the stream again, and a new message always does", async () => {
  const page = await streaming();
  await page.mouse.wheel(0, -200);
  await page.waitForTimeout(200);
  await page.evaluate(() => { scroller.scrollTop = scroller.scrollHeight; });
  await page.waitForTimeout(400);
  const before = await page.evaluate(() => scroller.scrollHeight);
  await page.waitForTimeout(300);
  assert.ok(await page.evaluate(() => scroller.scrollHeight) > before, "still streaming");
  assert.ok(await gap(page) < 4, `follows again: gap ${await gap(page)}`);
  await page.mouse.wheel(0, -200);
  await page.waitForTimeout(100);
  await page.evaluate(() => { window.stopStream(); window.thread.begin("Again"); });
  await page.waitForTimeout(200);
  assert.ok(await gap(page) < 4, "a new turn scrolls to its start");
  await page.close();
});

test("a wheel over a code block that scrolls itself does not stop the stream following", async () => {
  const page = await streaming();
  // A block at the end of the thread with its own scroll, already scrolled down a little.
  await page.evaluate(() => {
    const inner = document.createElement("div");
    inner.id = "inner";
    inner.style.cssText = "height:60px;overflow:auto";
    inner.innerHTML = '<div style="height:600px">code</div>';
    window.thread.root.append(inner);
    inner.scrollTop = 100;
    scroller.scrollTop = scroller.scrollHeight;
  });
  await page.waitForFunction(() => window.thread.stick);
  const box = await page.locator("#inner").boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -30);
  await page.waitForFunction(() => document.querySelector("#inner").scrollTop < 100);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(() => window.thread.stick), true, "the thread still follows");
  await page.close();
});

test("a click in the margin beside the thread does not stop the stream following", async () => {
  const page = await streaming();
  await page.evaluate(() => { document.getElementById("thread").style.maxWidth = "300px"; });
  await page.mouse.click(20, 200);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(() => window.thread.stick), true);
  await page.close();
});
