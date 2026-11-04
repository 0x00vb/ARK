import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
let browser;
before(async () => { browser = await webkit.launch({ headless: true }); });
after(async () => { await browser?.close(); });

async function fixture(run, { detached = false } = {}) {
  const projects = [{ id: "a", name: "Operations", current: true }, { id: "b", name: "Writing", current: false }];
  const sessions = ["chat-a", "chat-b"].map((id, i) => ({ id, title: i ? "Supplier comparison" : "Quarterly review", turns: 1, updated_at: Date.now(), working: i === 1 }));
  const summary = { name: "ARK", tagline: "", model: "mock", grants: [], project: projects[0], session: sessions[0], location: { runtime: "fixture", project: "a", session: "chat-a" }, execution_mode: "manual", execution_mode_revision: 0 };
  const request = { id: 7, session: "chat-b", chat: "Supplier comparison", project: null, line: "Send the supplier summary?", request: {}, choices: [] };
  let approvals = [], stream, holdRead = false, releaseRead, readStarted;
  const reading = new Promise(resolve => { readStarted = resolve; });
  const errors = [];
  const emit = message => stream.write(`data: ${JSON.stringify(message)}\n\n`);
  const rpc = async (method, params) => {
    if (method === "hello") return { summary, ui: {}, commands: [], seq: 0, working: [{ session: "chat-b", project: "a" }], approvals, runtime: "fixture" };
    if (method === "projects") return projects;
    if (method === "sessions") return params.project === "a" ? sessions : [];
    if (method === "summary") return summary;
    if (method === "chat_read") return { summary: { ...summary, session: sessions[1], location: { ...summary.location, session: "chat-b" } }, transcript: [], approvals: approvals.filter(a => a.session === "chat-b"), running: null };
    if (method === "transcript") {
      if (holdRead) { readStarted(); await new Promise(resolve => { releaseRead = resolve; }); holdRead = false; }
      return [];
    }
    if (["plugins", "computers"].includes(method)) return [];
    if (method === "places") return { here: "This computer", places: [] };
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "context") return { total: 0, budget: 64000 };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "resource") {
      const key = `${params.ref.kind}:${params.operation.kind}`;
      const results = { "project:grants": [], "project:skills": [], "project:board": null, "project:schedules": [], "process:processes": [] };
      if (Object.hasOwn(results, key)) return results[key];
    }
    throw Error(`Unexpected RPC ${method}`);
  };
  const server = createServer(async (req, res) => {
    try {
      if (req.url === "/manifest.webmanifest") { res.setHeader("content-type", "application/manifest+json"); res.end(JSON.stringify({ name: "ARK fixture", start_url: "/index.html" })); return; }
      if (req.url === "/events") {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.write(": ready\n\n"); stream = res; return;
      }
      if (req.url === "/session") { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ signed_in: true, pairing: false })); return; }
      if (req.url === "/rpc") {
        let body = ""; for await (const chunk of req) body += chunk;
        const { method, params } = JSON.parse(body);
        const result = await rpc(method, params);
        res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ ok: true, result })); return;
      }
      const path = resolve(root, `.${new URL(req.url, "http://fixture").pathname === "/" ? "/index.html" : new URL(req.url, "http://fixture").pathname}`);
      assert.ok(path.startsWith(root + sep));
      res.setHeader("content-type", path.endsWith(".js") ? "text/javascript" : path.endsWith(".css") ? "text/css" : "text/html");
      res.end(await readFile(path));
    } catch (error) { errors.push(error.message); res.writeHead(500); res.end(error.message); }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext({ viewport: { width: 1180, height: 800 } });
  const page = await context.newPage();
  page.setDefaultTimeout(7000);
  page.on("pageerror", error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html${detached ? "?detached=chat-b" : ""}`);
    await page.locator('.session[data-id="chat-b"] .session-working').waitFor();
    await run(page, { emit, request, snapshot: items => { approvals = items; }, hold: () => { holdRead = true; }, reading, release: () => releaseRead() });
    assert.deepEqual(errors, []);
  } finally {
    await context.close(); stream?.end(); server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
}

test("pending decisions take precedence over work, deduplicate, resolve and reconcile on reload", () => fixture(async (page, { emit, request, snapshot }) => {
  const chat = page.locator('.session[data-id="chat-b"]');
  await chat.focus();
  emit({ kind: "approval", ...request });
  await chat.getByText("Needs you", { exact: true }).waitFor();
  assert.equal(await chat.evaluate(node => document.activeElement === node), true);
  assert.equal(await chat.locator(".session-working").count(), 0);
  emit({ kind: "approval", ...request });
  await page.locator('.project[data-id="a"] .project-chev').click();
  await page.locator('.project[data-id="a"] > .project-row .session-attention').waitFor();
  assert.equal(await page.locator('.project[data-id="a"] > .project-row .session-attention').innerText(), "1");
  assert.equal(await page.locator('.project[data-id="b"] .session-attention').count(), 0);
  const project = page.locator('.project[data-id="a"] .project-row');
  await project.focus();
  emit({ kind: "approval", ...request, id: 8 });
  await page.locator('.project[data-id="a"] > .project-row .session-attention').getByText("2", { exact: true }).waitFor();
  assert.equal(await project.evaluate(node => document.activeElement === node), true);
  emit({ kind: "approval_closed", id: 8, session: request.session });
  await page.locator('.project[data-id="a"] .project-chev').click();
  emit({ kind: "approval_closed", id: request.id, session: request.session });
  await chat.locator(".session-working").waitFor();
  assert.equal(await chat.locator(".session-attention").count(), 0);
  emit({ kind: "reconnecting", delay: 1000 });
  await chat.locator(".session-progress").waitFor({ state: "hidden" });
  emit({ kind: "working", working: [{ session: "chat-b", project: "a" }] });
  assert.equal(await chat.locator(".session-progress").isVisible(), false);
  snapshot([request]);
  await page.reload();
  await chat.getByText("Needs you", { exact: true }).waitFor();
  snapshot([]);
  await page.reload();
  await chat.locator(".session-working").waitFor();
  assert.equal(await chat.locator(".session-attention").count(), 0);
}));

test("detached chat decisions survive snapshot reload and clear when answered", () => fixture(async (page, { emit, request, snapshot }) => {
  snapshot([request]);
  await page.reload();
  const chat = page.locator('.session[data-id="chat-b"]');
  await chat.getByText("Needs you", { exact: true }).waitFor();
  emit({ kind: "approval_closed", id: request.id, session: request.session });
  await chat.locator(".session-working").waitFor();
  assert.equal(await chat.locator(".session-attention").count(), 0);
}, { detached: true }));

test("detached snapshot keeps pending decisions from other chats", () => fixture(async (page, { request, snapshot }) => {
  snapshot([{ ...request, session: "chat-a", chat: "Quarterly review" }]);
  await page.reload();
  await page.locator('.session[data-id="chat-a"]').getByText("Needs you", { exact: true }).waitFor();
}, { detached: true }));

test("a late snapshot cannot restore Working after a newer disconnect", () => fixture(async (page, { emit, hold, reading, release }) => {
  hold();
  emit({ kind: "resync" });
  await reading;
  emit({ kind: "reconnecting", delay: 1000 });
  const progress = page.locator('.session[data-id="chat-b"] .session-progress');
  await progress.waitFor({ state: "hidden" });
  await progress.evaluate(node => { window.disconnectedMark = node; });
  const response = page.waitForResponse(response => response.url().endsWith("/rpc") && response.request().postDataJSON().method === "transcript");
  release();
  await response;
  await page.waitForFunction(() => document.querySelector('.session[data-id="chat-b"] .session-progress') !== window.disconnectedMark);
  assert.equal(await progress.isVisible(), false);
}));

test("Working returns after transport resync then connected and fresh snapshot", () => fixture(async (page, { emit, hold, reading, release }) => {
  const progress = page.locator('.session[data-id="chat-b"] .session-progress');
  emit({ kind: "reconnecting", delay: 1000 });
  await progress.waitFor({ state: "hidden" });
  hold();
  emit({ kind: "resync" });
  await reading;
  await progress.evaluate(node => { window.disconnectedMark = node; });
  emit({ kind: "connected" });
  const response = page.waitForResponse(response => response.url().endsWith("/rpc") && response.request().postDataJSON().method === "transcript");
  release();
  await response;
  await page.waitForFunction(() => document.querySelector('.session[data-id="chat-b"] .session-progress') !== window.disconnectedMark);
  await progress.waitFor({ state: "visible" });
}));

test("files expose an open action and preserve removed state across light, dark and narrow layouts", () => fixture(async page => {
  await page.evaluate(async () => {
    const { Thread } = await import("./js/conversation.js");
    const api = { openFile: path => { window.openedFile = path; }, call: async method => { if (method !== "file_restore") throw Error(method); return {}; }, toast() {} };
    const thread = new Thread(document.querySelector("#thread"), document.querySelector("#thread").parentElement, api, { inspect: false });
    thread.renderStored([{ input: "Compare the quotes", index: 0, answer: "The comparison is ready.", activity: [{ kind: "result", files: ["Supplier comparison.md"], before: [{ path: "Removed.txt", artifact: "old" }] }] }]);
  });
  const file = page.getByRole("button", { name: "Open Supplier comparison.md", exact: true });
  const removed = page.getByRole("button", { name: "Removed.txt, removed", exact: true });
  assert.equal(await removed.isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "Restore", exact: true }).count(), 1);
  await page.getByRole("button", { name: "Restore", exact: true }).click();
  const restored = page.getByRole("button", { name: "Open Removed.txt", exact: true });
  assert.equal(await restored.isEnabled(), true);
  assert.equal(await restored.locator(".file-card-open").isVisible(), true);
  for (const [theme, width] of [["light", 1180], ["dark", 1180], ["light", 390]]) {
    await page.setViewportSize({ width, height: 800 });
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    await file.click();
    assert.equal(await page.evaluate(() => window.openedFile), "Supplier comparison.md");
    const box = await file.boundingBox();
    assert.ok(box.height >= 44 && box.x >= 0 && box.x + box.width <= width);
    await file.focus();
    await page.keyboard.press("Enter");
    if (process.env.ARK_UI_SHOTS) {
      await mkdir(process.env.ARK_UI_SHOTS, { recursive: true });
      await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, `work-files-${theme}-${width}.png`), animations: "disabled" });
    }
  }
}));
