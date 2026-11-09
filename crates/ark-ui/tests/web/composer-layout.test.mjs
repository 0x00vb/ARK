import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
let browser;
before(async () => { browser = await webkit.launch({ headless: true }); });
after(async () => { await browser?.close(); });

async function fixture(run, { context: used = 0, model = "mock", width = 1100 } = {}, outcome = "success", runtime = "fixture") {
  const projects = [{ id: "a", name: "Notes", workspace: "/notes" }, { id: "b", name: "Writing", workspace: "/writing" }];
  const chats = [{ id: "chat-a", title: "First draft", project: "a" }, { id: "chat-b", title: "Second draft", project: "a" }, { id: "chat-c", title: "Other project", project: "b" }];
  let current = chats[0];
  const summary = (chat = current) => ({ name: "Notes", tagline: "", model, grants: [], project: projects.find(p => p.id === chat.project), session: { ...chat, turns: 0 }, location: { runtime: "fixture", project: chat.project, session: chat.id }, execution_mode: "auto", execution_mode_revision: 0 });
  const events = new Set(), errors = [], calls = [], files = [];
  let release, started, pending = 0;
  const waiting = new Promise(resolve => { started = resolve; });
  const rpc = async (method, params) => {
    calls.push({ method, params });
    if (method === "hello") return { summary: summary(), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime };
    if (method === "summary") return summary();
    if (method === "execution_mode" || method === "chat_execution_mode") return { ...summary(), execution_mode: params.mode, execution_mode_revision: 1 };
    if (method === "chat_read") return { summary: summary(chats.find(chat => chat.id === params.session)), transcript: [], approvals: [], running: null };
    if (method === "projects") return projects.map(p => ({ ...p, current: p.id === current.project }));
    if (method === "sessions") return chats.filter(c => c.project === params.project).map(c => ({ ...c, turns: 1, updated_at: Date.now() }));
    if (["transcript", "plugins", "computers"].includes(method)) return [];
    if (method === "places") return { here: "This computer", places: [] };
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "context") return { total: used, budget: 64000 };
    if (["open_session", "open_project"].includes(method)) {
      current = method === "open_session" ? chats.find(c => c.id === params.id) : chats.find(c => c.project === params.id);
      assert.ok(current);
      for (const stream of events) stream.write(`data: ${JSON.stringify({ kind: "summary", summary: summary() })}\n\n`);
      return null;
    }
    if (method === "resource") {
      const { ref, operation } = params;
      const key = `${ref.kind}:${operation.kind}`;
      const results = { "project:grants": [], "project:skills": [], "project:board": null, "project:schedules": [], "process:processes": [] };
      if (Object.hasOwn(results, key)) return results[key];
      if (key === "file:upload_parts" && outcome === "stop") await new Promise(resolve => { release = resolve; started(); });
      if (["file:upload_parts", "file:upload_cancel"].includes(key)) return null;
      if (key === "file:upload_add") {
        await new Promise(resolve => { release = resolve; started(); });
        if (outcome !== "failure") files.push({ location: ref.location, path: "notes.txt" });
        return { added: ["notes.txt"] };
      }
      throw Error(`Unexpected resource ${key}`);
    }
    throw Error(`Unexpected RPC ${method}`);
  };
  const server = createServer(async (req, res) => {
    pending++;
    try {
      if (req.url === "/manifest.webmanifest") { res.setHeader("content-type", "application/manifest+json"); res.end(JSON.stringify({ name: "Fixture", start_url: "/index.html" })); return; }
      if (req.url === "/events") {
        res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
        events.add(res); req.on("close", () => events.delete(res)); return;
      }
      if (req.url === "/session") { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ signed_in: true, pairing: false })); return; }
      if (req.url === "/rpc") {
        let body = ""; for await (const chunk of req) body += chunk;
        const { method, params } = JSON.parse(body);
        const result = await rpc(method, params);
        if (outcome === "failure" && method === "resource" && params.operation.kind === "upload_add") {
          res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ ok: false, error: "Upload refused" })); return;
        }
        res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ ok: true, result })); return;
      }
      const pathname = new URL(req.url, "http://fixture").pathname;
      const path = resolve(root, `.${pathname === "/" ? "/index.html" : pathname}`);
      assert.ok(path.startsWith(root.endsWith(sep) ? root : root + sep));
      res.setHeader("content-type", path.endsWith(".js") ? "text/javascript" : path.endsWith(".css") ? "text/css" : "text/html");
      res.end(await readFile(path));
    } catch (error) { errors.push(error.message); res.writeHead(500, { "content-type": "application/json" }); res.end(JSON.stringify({ ok: false, error: error.message })); }
    finally { pending--; }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext({ viewport: { width, height: 760 } });
  if (runtime !== "fixture") await context.addInitScript(runtime => {
    const key = `ark-draft:${runtime}:chat-a`;
    if (localStorage.getItem(key) === null) localStorage.setItem(key, "Existing remote draft");
  }, runtime);
  const page = await context.newPage(); page.setDefaultTimeout(5000);
  page.on("pageerror", error => errors.push(error.message));
  const settle = async () => {
    let previous = -1;
    for (let i = 0; i < 100; i++) {
      await page.waitForTimeout(10);
      if (pending === 0 && previous === calls.length) return;
      previous = calls.length;
    }
    throw Error("Fixture RPCs did not settle");
  };
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#agents .agent-btn[data-key="ark"]').waitFor({ state: "attached" });
    await run(page, { files, calls, waiting, release: () => release(), settle });
    await settle();
    assert.deepEqual(errors, []);
  } catch (error) {
    console.error({ errors, calls, body: (await page.locator('body').innerText()).slice(0, 1200) });
    throw error;
  } finally {
    await context.close(); for (const stream of events) stream.end();
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  }
}


const box = (page, selector) => page.evaluate((selector) => {
  const el = document.querySelector(selector);
  if (!el || getComputedStyle(el).display === "none") return null;
  const { left, right } = el.getBoundingClientRect();
  return { left, right };
}, selector);

// Send closes the row on the right whatever else the row draws: a context reading or a narrow window. v0.36.0 lost this when a hidden chip stayed in the row.
for (const [name, options] of [
  ["nothing else drawn", {}],
  ["a context reading", { context: 5000 }],
  ["a narrow window", { width: 390 }],
  ["a narrow window with a context reading", { width: 390, context: 5000 }],
]) {
  test(`send stays at the right edge with ${name}`, async () => fixture(async (page) => {
    await page.locator("#send").waitFor();
    await page.waitForFunction(() => document.querySelector("#ctx-ring") !== null);
    await page.waitForTimeout(100);
    const bar = await box(page, "#composer .composer-bar");
    const send = await box(page, "#send");
    const mic = await box(page, "#dictate");
    assert.ok(Math.abs(bar.right - send.right) < 0.5, `send ends at ${send.right}, the row at ${bar.right}`);
    assert.ok(send.left > mic.right, "send comes after the microphone");
    if (options.context) assert.ok((await box(page, "#ctx-ring")).right <= send.left, "the reading sits before send");
  }, options));
}
