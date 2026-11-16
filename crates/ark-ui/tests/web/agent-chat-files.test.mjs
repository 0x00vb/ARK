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

// The real page over a runtime that records its calls, with a chat view the way Desktop mounts an
// agent's chat: its own file location names the agent's chat (ADR 0124) when the runtime's hello
// says it resolves one in the agent's folder.
async function fixture(run, { chatFiles = true } = {}) {
  const project = { id: "a", name: "Notes", workspace: "/notes" };
  const chat = { id: "chat-a", title: "First draft", project: "a" };
  const summary = () => ({ name: "Notes", tagline: "", model: "mock", grants: [], project, session: { ...chat, turns: 0 }, location: { runtime: "fixture", project: "a", session: chat.id }, execution_mode: "auto", execution_mode_revision: 0 });
  const errors = [], calls = [], uploads = new Map(), events = new Set();
  let pending = 0;
  const rpc = async (method, params) => {
    calls.push({ method, params });
    if (method === "hello") return { summary: summary(), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture", file_kinds: { png: "image", md: "text", xlsx: "sheet" }, kind_nouns: { image: "Image", text: "Text", sheet: "Spreadsheet" }, ...(chatFiles ? { agent_chat_files: true } : {}) };
    if (method === "summary") return summary();
    if (method === "execution_mode" || method === "chat_execution_mode") return { ...summary(), execution_mode: params.mode, execution_mode_revision: 1 };
    if (method === "projects") return [{ ...project, current: true }];
    if (method === "sessions") return [{ ...chat, turns: 1, updated_at: Date.now() }];
    if (method === "chat_read") return { summary: summary(), transcript: [], approvals: [], running: null };
    if (["transcript", "plugins", "computers"].includes(method)) return [];
    if (method === "places") return { here: "This computer", places: [] };
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "context") return { total: 0, budget: 64000 };
    if (method === "resource") {
      const { operation } = params;
      const { upload, parts } = operation.input;
      const background = { "project:grants": [], "project:skills": [], "project:board": null, "project:schedules": [], "process:processes": [] };
      const key = `${params.ref.kind}:${operation.kind}`;
      if (Object.hasOwn(background, key)) return background[key];
      if (operation.kind === "upload_parts") {
        const names = uploads.get(upload) || new Set();
        for (const part of parts) names.add(part.path.split("/")[0]);
        uploads.set(upload, names);
        return null;
      }
      if (operation.kind === "upload_add") {
        const names = [...uploads.get(upload)].sort();
        return { added: names, as: Object.fromEntries(names.map((n) => [n, n])) };
      }
      if (operation.kind === "upload_cancel") return null;
      if (operation.kind === "file_media") return { data: PNG.toString("base64"), media_type: "image/png" };
      if (["file_read", "file_version", "file_probe"].includes(operation.kind)) {
        if (params.ref.id.startsWith("missing")) throw Error(`${params.ref.id.split("/").pop()} is not in ec`);
        return operation.kind === "file_version" ? { version: "v1" } : { path: params.ref.id, kind: "text", text: "client list\n", version: "v1" };
      }
      if (["subscribe", "unsubscribe"].includes(operation.kind)) return {};
      if (operation.kind === "focus") return null;
    }
    throw Error(`Unexpected RPC ${method} ${params?.operation?.kind || ""}`);
  };
  const server = createServer(async (req, res) => {
    pending++;
    try {
      if (req.url === "/manifest.webmanifest") { res.setHeader("content-type", "application/manifest+json"); res.end(JSON.stringify({ name: "Fixture", start_url: "/index.html" })); return; }
      if (req.url === "/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); events.add(res); req.on("close", () => events.delete(res)); return; }
      if (req.url === "/session") { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ signed_in: true, pairing: false })); return; }
      if (req.url === "/rpc") {
        let body = ""; for await (const chunk of req) body += chunk;
        const { method, params } = JSON.parse(body);
        res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ ok: true, result: await rpc(method, params) })); return;
      }
      const pathname = new URL(req.url, "http://fixture").pathname;
      const path = resolve(root, `.${pathname === "/" ? "/index.html" : pathname}`);
      assert.ok(path.startsWith(root.endsWith(sep) ? root : root + sep));
      res.setHeader("content-type", path.endsWith(".js") ? "text/javascript" : path.endsWith(".css") ? "text/css" : "text/html");
      res.end(await readFile(path));
    } catch (error) { if (!/ is not in ec$/.test(error.message)) errors.push(error.message); res.writeHead(500, { "content-type": "application/json" }); res.end(JSON.stringify({ ok: false, error: error.message })); }
    finally { pending--; }
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
  const page = await context.newPage(); page.setDefaultTimeout(5000);
  page.on("pageerror", (error) => errors.push(error.message));
  const settle = async () => {
    let previous = -1;
    for (let i = 0; i < 100; i++) {
      await page.waitForTimeout(20);
      if (pending === 0 && previous === calls.length) return;
      previous = calls.length;
    }
    throw Error("Fixture RPCs did not settle");
  };
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator("#input").waitFor();
    await page.waitForFunction(() => document.querySelector("#ctx-ring") !== null);
    await page.evaluate(() => {
      const mount = document.createElement("div");
      document.querySelector(".main").append(mount);
      const own = window.ark.summary().location;
      const agentChat = { ...own, session: "agent-chat" };
      window.chatLocation = () => (window.ark.at(own).agentChatFiles() ? agentChat : own);
      window.view = window.ark.chatView(mount, {
        submit: async () => {},
        location: () => own,
        fileLocation: () => window.chatLocation(),
      });
      window.view.render([]);
    });
    await run(page, { calls, settle });
    await settle();
    assert.deepEqual(errors, []);
  } catch (error) {
    console.error({ errors, calls: calls.slice(-8) });
    throw error;
  } finally {
    await context.close(); for (const stream of events) stream.end(); server.closeAllConnections(); await new Promise((ok) => server.close(ok));
  }
}

const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==", "base64");

const files = (calls, operation) => calls
  .filter((c) => c.method === "resource" && c.params.ref.kind === "file" && (!operation || c.params.operation.kind === operation))
  .map((c) => ({ op: c.params.operation.kind, id: c.params.ref.id, session: c.params.ref.location.session }));

const turn = (answer) => ({ key: "t1", index: 0, input: "list them", answer, activity: [{ kind: "result", files: ["brand/BRAND-GUIDE.md", "shot.png"] }] });
const draw = (page, answer = "Open [the crm](product/04-client-crm.xlsx).") => page.evaluate((t) => window.view.render([t]), turn(answer));

test("a file link in an agent chat opens in the agent's chat location", () => fixture(async (page, { calls, settle }) => {
  await draw(page);
  await page.locator(".chat-view a.file-link").click();
  await page.locator("textarea.wb-editor, .fv-body").first().waitFor();
  await settle();
  const reads = files(calls, "file_read");
  assert.ok(reads.length > 0, "the file was read");
  assert.ok(reads.every((r) => r.session === "agent-chat" && r.id === "product/04-client-crm.xlsx"), JSON.stringify(reads));
}));

test("a file card and a thumbnail read in the agent's chat location", () => fixture(async (page, { calls, settle }) => {
  await draw(page);
  await page.locator(".chat-view .file-card", { hasText: "BRAND-GUIDE.md" }).click();
  await page.locator("textarea.wb-editor, .fv-body").first().waitFor();
  await page.locator(".chat-view .file-card-image").first().waitFor({ state: "attached" });
  await settle();
  const sessions = (op) => [...new Set(files(calls, op).map((r) => r.session))];
  assert.deepEqual(sessions("file_read"), ["agent-chat"]);
  assert.deepEqual(sessions("file_media"), ["agent-chat"], "the thumbnail is read there");
}));

test("a file that is not in the agent's folder says so in the file view", () => fixture(async (page, { settle }) => {
  await draw(page, "Open [gone](missing/04-client-crm.xlsx).");
  await page.locator(".chat-view a.file-link").click();
  await page.getByText("04-client-crm.xlsx is not in ec").waitFor();
  await settle();
  assert.equal(await page.getByText(/os error/).count(), 0);
}));

test("a runtime without the flag keeps the window's location for files", () => fixture(async (page, { calls, settle }) => {
  await draw(page);
  await page.locator(".chat-view a.file-link").click();
  await page.locator("textarea.wb-editor, .fv-body").first().waitFor();
  await settle();
  assert.deepEqual([...new Set(files(calls).map((r) => r.session))], ["chat-a"]);
}, { chatFiles: false }));

test("a chat asks for its own runtime: another computer is never told an agent chat location", () => fixture(async (page) => {
  const flags = await page.evaluate(() => ({
    page: window.ark.agentChatFiles(),
    own: window.ark.at(window.ark.summary().location).agentChatFiles(),
    far: window.ark.at({ runtime: "far", project: "p", session: "s" }).agentChatFiles(),
  }));
  assert.deepEqual(flags, { page: true, own: true, far: false });
}));
