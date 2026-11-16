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
<div id="scroller" style="height:500px;overflow:auto"><div id="thread" class="thread"></div></div>
<script type="module">
import { Thread } from "/js/conversation.js";
const api = { call: async () => ({}), toast() {} };
const thread = new Thread(document.getElementById("thread"), document.getElementById("scroller"), api, { inspect: false });
thread.renderStored([{ input: "Look into the filings", index: 0, answer: "Let me look into it.", posts: [] }]);
window.Thread = Thread;
window.thread = thread;
window.scroller = document.getElementById("scroller");
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


async function page(options = {}) {
  const p = await browser.newPage({ viewport: { width: 700, height: 600 }, ...options });
  await p.goto(`${base}/t.html`);
  await p.waitForFunction(() => window.ready);
  return p;
}

const shots = process.env.ARK_SHOTS;
const shot = async (p, name) => { if (!shots) return; await p.waitForTimeout(600); await p.screenshot({ path: `${shots}/${name}.png` }); };

const message = (text, ok = true) => ({ from: "ceo", title: "Read the filings", text, ok, kind: "message" });
const style = (p, selector, props) => p.evaluate(([s, ps]) => {
  const c = getComputedStyle(document.querySelector(s));
  return Object.fromEntries(ps.map((k) => [k, c[k]]));
}, [selector, props]);

test("an answer that came later draws exactly like a reply: no card, title or caption", async () => {
  const p = await page();
  await p.evaluate((m) => window.thread.post(m), message("The filings show **42**."));
  assert.equal(await p.locator(".post").count(), 0);
  assert.equal(await p.locator("article.turn.message > .agent > .prose.answer").count(), 1);
  assert.match(await p.locator(".turn.message .prose.answer").innerText(), /The filings show 42\./);
  assert.equal(await p.locator(".turn.message strong").count(), 1, "markdown like a reply");
  assert.equal(await p.locator(".post-head, .post-title, .post-from").count(), 0);
  const props = ["fontSize", "lineHeight", "color", "fontFamily", "maxWidth"];
  const reply = await style(p, ".turn:not(.message) .prose.answer", props);
  const later = await style(p, ".turn.message .prose.answer", props);
  assert.deepEqual(later, reply, "the same look as the agent's reply");
  await shot(p, "message-light");
  await p.close();
});

test("an answer that failed is a notice in the agent's column, not a card", async () => {
  const p = await page();
  await p.evaluate((m) => window.thread.post(m), message("the source did not answer", false));
  assert.equal(await p.locator(".post").count(), 0);
  assert.equal(await p.locator(".turn.message > .agent > .notice.tone-error").innerText(), "the source did not answer");
  await shot(p, "message-failed");
  await p.close();
});

test("a scheduled run's result keeps its card", async () => {
  const p = await page();
  await p.evaluate(() => window.thread.post({ from: "ceo", title: "Daily check", text: "All clear", ok: true }));
  assert.equal(await p.locator(".post .post-title").innerText(), "Daily check");
  assert.equal(await p.locator(".turn.message").count(), 0);
  await p.close();
});

test("a stored answer sits after the turn it came after and does not animate", async () => {
  const p = await page();
  const html = await p.evaluate(() => {
    const t = document.createElement("div");
    document.body.append(t);
    const thread = new window.Thread(t, document.getElementById("scroller"), { call: async () => ({}), toast() {} }, { inspect: false });
    thread.renderStored([{ input: "Look into it", index: 0, answer: "Let me look into it.", posts: [
      { from: "ceo", title: "x", text: "The filings show 42.", ok: true, kind: "message" },
      { from: "ceo", title: "Daily", text: "All clear", ok: true } ] }]);
    const turn = t.querySelector(".turn");
    return {
      kids: [...turn.children].map((c) => c.className),
      answers: [...turn.querySelectorAll(".prose.answer")].map((e) => e.textContent),
      animation: getComputedStyle(turn).animationName,
    };
  });
  assert.deepEqual(html.answers, ["Let me look into it.", "The filings show 42."]);
  assert.ok(html.kids.includes("post"), "the result card stays a card");
  assert.equal(html.animation, "none");
  await p.close();
});

test("a long unbroken answer wraps in a narrow window and in dark", async () => {
  for (const colorScheme of ["light", "dark"]) {
    const p = await page({ viewport: { width: 320, height: 600 }, colorScheme });
    await p.evaluate((t) => { document.documentElement.dataset.theme = t; }, colorScheme);
    await p.evaluate((m) => window.thread.post(m), message("x".repeat(400) + "\n\n```\n" + "y".repeat(300) + "\n```"));
    const wide = await p.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    assert.equal(wide, false, `no horizontal page scroll in ${colorScheme}`);
    const same = await Promise.all([style(p, ".turn:not(.message) .prose.answer", ["color"]), style(p, ".turn.message .prose.answer", ["color"])]);
    assert.deepEqual(same[1], same[0], `same text colour in ${colorScheme}`);
    await shot(p, `message-320-${colorScheme}`);
    await p.close();
  }
});

test("an answer that arrives does not take focus from the composer", async () => {
  const p = await page();
  await p.evaluate(() => { const i = document.createElement("textarea"); i.id = "c"; document.body.append(i); i.focus(); });
  await p.evaluate((m) => window.thread.post(m), message("Done."));
  assert.equal(await p.evaluate(() => document.activeElement?.id), "c");
  await p.close();
});

test("a turn the agent continued on its own has no person's line, only the agent's answer (ADR 0126)", async () => {
  const p = await page();
  await p.evaluate(() => window.thread.renderStored([
    { input: "Translate the add-ons", index: 0, answer: "Started three translators.", posts: [] },
    { input: null, index: 1, answer: "All three are done: **combined**.", posts: [] },
  ]));
  // The page starts with one stored turn of its own; the two rendered here follow it.
  const turns = p.locator("article.turn");
  assert.equal(await turns.count(), 3);
  assert.equal(await turns.nth(1).locator(".user .bubble").count(), 1);
  assert.equal(await turns.nth(2).locator(".user").count(), 0, "nobody wrote it");
  assert.match(await turns.nth(2).locator(".agent .prose.answer").innerText(), /All three are done: combined\./);
  await p.close();
});
