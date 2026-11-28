// A finished turn shows its answer, not its steps. Only a step that went wrong keeps one line,
// named by that step, and the line opens the steps.
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

const page_html = `<!doctype html><link rel="stylesheet" href="/app.css">
<style>html, body { height: 100%; margin: 0; } #host { height: 100%; display: flex; } #host > * { flex: 1; min-width: 0; }</style>
<div id="host"></div>
<script type="module">
import { chatView } from "/js/chat-view.js";
import { Thread } from "/js/conversation.js";
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
const api = {
  call: async (method) => method === "picture" ? { data: PNG, media_type: "image/png" } : ({}), toast() {}, on() { return () => {}; }, openFile() {}, popover() {}, ready() {},
  dictationRoute: () => ({ place: "", session: "s", runtime: "fixture" }),
  summary: () => ({ session: { id: "s" }, location: { runtime: "fixture", project: "p", session: "s" } }),
  pendingPlace: () => null, hereName: () => "This computer",
};
const tool = (call, label) => ({ kind: "tool", label, call, action: "files.write" });
const result = (call, error) => ({ kind: "result", call, label: error ? "No such folder" : "done", tone: error ? "error" : "normal" });
window.view = chatView(document.getElementById("host"), api, { submit: async () => {} });
window.view.render([
  { key: "ok", input: "Write it", answer: "Written.", activity: [tool("a", "Writing videos/demo/package.json"), result("a"), tool("b", "Reading record.mjs"), result("b")] },
  { key: "bad", input: "Move it", answer: "The folder is missing.", activity: [tool("c", "Reading notes.md"), result("c"), tool("d", "Moving out/demo.mp4"), result("d", true)] },
  { key: "live", input: "Again", activity: [tool("e", "Rendering the video")], working: { label: "Rendering the video", since: Date.now() } },
]);
window.live = () => {
  const box = document.createElement("div");
  document.body.append(box);
  const thread = new Thread(box, box, api);
  const turn = thread.begin("Use the app");
  const step = (call, action, label) => thread.event({ kind: "event", event: { type: "tool_call" }, activity: { kind: "tool", label, call, action } });
  const done = (call, action, ok) => thread.event({ kind: "event", event: { type: "tool_result", ok, action, invocation: ok ? "inv-" + call : undefined }, activity: { kind: "result", call, label: ok ? "done" : "Permission denied", tone: ok ? "normal" : "error" } });
  step("s1", "computer.see", "Looking at TextEdit");
  done("s1", "computer.see", true);
  step("s2", "files.write", "Writing notes.md");
  done("s2", "files.write", false);
  step("s3", "files.read", "Reading plan.md");
  window.liveTurn = turn;
  return box;
};
window.ready = true;
</script>`;

before(async () => {
  browser = await webkit.launch({ headless: true });
  server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    if (url.pathname === "/t.html") { res.setHeader("content-type", "text/html"); res.end(page_html); return; }
    const file = resolve(root, "." + url.pathname);
    if (!file.startsWith(root + sep)) { res.writeHead(403).end(); return; }
    try {
      res.setHeader("content-type", types[file.slice(file.lastIndexOf("."))] || "application/octet-stream");
      res.end(await readFile(file));
    } catch { res.writeHead(404).end(); }
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  base = `http://127.0.0.1:${server.address().port}`;
});
after(async () => { await browser?.close(); server?.close(); });

test("a finished turn shows no step line; a failed step keeps one that opens the steps; a running turn names its step", async () => {
  const context = await browser.newContext({ viewport: { width: 900, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(`${base}/t.html`);
  await page.waitForFunction(() => window.ready);
  const turn = (key) => page.locator(".turn").nth(["ok", "bad", "live"].indexOf(key));

  assert.equal(await turn("ok").locator(".work").isVisible(), false, "work that went right has no line");
  assert.equal(await page.getByText(/more step/).count(), 0);
  assert.equal(await turn("ok").locator(".prose.answer").innerText(), "Written.");

  const head = turn("bad").locator(".work-head");
  assert.equal(await head.isVisible(), true, "a failed step keeps its line");
  assert.equal(await head.innerText(), "Moving out/demo.mp4");
  await head.click();
  await turn("bad").locator('.work[data-open="true"] .act[data-status="failed"]').waitFor();
  assert.equal(await turn("bad").locator(".act").count(), 2, "the line opens every step of the block");

  assert.equal(await turn("live").locator(".working-line .shimmer").innerText(), "Rendering the video");
  assert.equal(await turn("live").locator(".act").first().isVisible(), false, "a running turn shows one line, not a stack of rows");
  assert.deepEqual(errors, []);
  await context.close();
});

test("while a turn runs, a failed step shows its line at once and a picture the agent took shows outside the folded steps", async () => {
  const context = await browser.newContext({ viewport: { width: 900, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(`${base}/t.html`);
  await page.waitForFunction(() => window.ready);
  await page.evaluate(() => { window.live().id = "live-box"; });
  const box = page.locator("#live-box");
  const head = box.locator(".work-head");
  await head.waitFor();
  assert.equal(await head.innerText(), "Writing notes.md", "the failed step, before the turn ends");
  assert.equal(await box.locator(".working-line .shimmer").innerText(), "Reading plan.md");
  const shot = box.locator(".act-shot");
  await shot.waitFor();
  assert.equal(await shot.evaluate((el) => !!el.closest(".work")), false, "the picture is not inside the hidden steps");
  assert.equal(await shot.isVisible(), true);
  await page.evaluate(() => window.liveTurn.closeBlocks());
  assert.equal(await shot.isVisible(), true, "and stays after the turn");
  assert.equal(await head.innerText(), "Writing notes.md");
  assert.deepEqual(errors, []);
  await context.close();
});
