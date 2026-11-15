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

// The real page over a runtime that records its calls, with a chat view the way an app mounts one:
// the origin says it attaches images (the runtime's hello says it takes them).
async function fixture(run, { images = true } = {}) {
  const project = { id: "a", name: "Notes", workspace: "/notes" };
  const chat = { id: "chat-a", title: "First draft", project: "a" };
  const summary = () => ({ name: "Notes", tagline: "", model: "mock", grants: [], project, session: { ...chat, turns: 0 }, location: { runtime: "fixture", project: "a", session: chat.id }, execution_mode: "auto", execution_mode_revision: 0 });
  const errors = [], calls = [], uploads = new Map(), events = new Set();
  let pending = 0;
  const rpc = async (method, params) => {
    calls.push({ method, params });
    if (method === "hello") return { summary: summary(), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture", ...(images ? { images: true } : {}) };
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
    }
    throw Error(`Unexpected RPC ${method}`);
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
    } catch (error) { errors.push(error.message); res.writeHead(500, { "content-type": "application/json" }); res.end(JSON.stringify({ ok: false, error: error.message })); }
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
      window.sent = [];
      window.view = window.ark.chatView(mount, {
        submit: async (text, options) => { window.sent.push({ text, images: options?.images?.map((i) => i.path), named: options?.named }); },
        files: {
          captureOrigin: () => ({
            uploadApi: window.ark.at(window.ark.summary().location),
            progressTarget: window.view.uploads,
            attachImages: window.ark.taskImages(),
            appendDraft: (line, pictures = []) => {
              if (pictures.length) window.view.addImages(pictures);
              if (line) window.view.addLine(line, { focus: false });
            },
          }),
        },
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
const file = (name, buffer = PNG, mimeType = "image/png") => ({ name, mimeType, buffer });

/// The view's own add button, then the files the person picks.
async function choose(page, files) {
  const chooser = page.waitForEvent("filechooser");
  await page.locator(".chat-view [aria-label='Add files']").click();
  await (await chooser).setFiles(files);
}

const chips = (page) => page.locator(".chat-view .thumb-chip");
const field = (page) => page.locator(".chat-view textarea");
const sent = (page) => page.evaluate(() => window.sent);

test("a chosen picture is a thumbnail in the view's composer and no text in the draft", () => fixture(async (page) => {
  await choose(page, [file("shot.png")]);
  await chips(page).first().waitFor();
  assert.equal(await chips(page).count(), 1);
  assert.equal(await field(page).inputValue(), "", "no 'Added to the project' line");
  assert.equal(await page.locator(".chat-view form").getAttribute("data-empty"), "false", "Send is ready with a picture alone");
  assert.deepEqual(await sent(page), [], "nothing is sent by adding it");
}));

test("Send hands the pictures to submit, and a picture alone sends its file name", () => fixture(async (page) => {
  await choose(page, [file("shot.png")]);
  await chips(page).first().waitFor();
  await field(page).focus();
  await page.keyboard.press("Enter");
  assert.deepEqual(await sent(page), [{ text: "shot.png", images: ["shot.png"], named: true }]);
  assert.equal(await chips(page).count(), 0, "the composer is clear");
}));

test("words and a picture go together", () => fixture(async (page) => {
  await choose(page, [file("shot.png")]);
  await chips(page).first().waitFor();
  await field(page).fill("what is this?");
  await page.keyboard.press("Enter");
  assert.deepEqual(await sent(page), [{ text: "what is this?", images: ["shot.png"], named: false }]);
}));

test("the remove button and Backspace in an empty line take a picture back, and the field keeps focus", () => fixture(async (page) => {
  await choose(page, [file("a.png"), file("b.png")]);
  await page.waitForFunction(() => document.querySelectorAll(".chat-view .thumb-chip").length === 2);
  await chips(page).first().hover();
  await chips(page).first().getByRole("button", { name: "Remove a.png" }).click();
  assert.equal(await chips(page).count(), 1);
  await field(page).focus();
  await page.keyboard.press("Backspace");
  assert.equal(await chips(page).count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement === window.view.input), true);
  assert.equal(await page.locator(".chat-view form").getAttribute("data-empty"), "true");
}));

test("a message the view cannot take now puts the words and the pictures back", () => fixture(async (page) => {
  await page.evaluate(() => window.view.setComposer({ submitDisabled: true }));
  await choose(page, [file("shot.png")]);
  await chips(page).first().waitFor();
  await field(page).fill("hold this");
  await page.keyboard.press("Enter");
  assert.equal(await chips(page).count(), 1);
  assert.equal(await field(page).inputValue(), "hold this");
  assert.deepEqual(await sent(page), []);
  await page.evaluate(() => window.view.setComposer({ submitDisabled: false }));
  await page.keyboard.press("Enter");
  assert.deepEqual(await sent(page), [{ text: "hold this", images: ["shot.png"], named: false }]);
}));

test("a failed send gives the pictures back with addImages, and stashImages keeps them for another chat", () => fixture(async (page) => {
  await choose(page, [file("shot.png")]);
  await chips(page).first().waitFor();
  const kept = await page.evaluate(() => window.view.stashImages().map((i) => i.path));
  assert.deepEqual(kept, ["shot.png"]);
  assert.equal(await chips(page).count(), 0);
  await page.evaluate((paths) => window.view.addImages(paths.map((path) => ({ path, name: path }))), kept);
  await chips(page).first().waitFor();
  assert.equal(await chips(page).count(), 1);
}));

test("a ninth picture is refused with the composer's message", () => fixture(async (page) => {
  await choose(page, Array.from({ length: 9 }, (_, i) => file(`${i}.png`)));
  await page.waitForFunction(() => document.querySelectorAll(".chat-view .thumb-chip").length === 8);
  await page.waitForFunction(() => document.querySelector("#toast")?.textContent.includes("At most 8 images"));
}));

test("a picture too large to fit is refused before send and still goes into the project", () => fixture(async (page) => {
  await choose(page, [file("big.png", Buffer.alloc(3_600_000, 7))]);
  await page.waitForFunction(() => document.querySelector("#toast")?.textContent === "big.png is too large to show the agent (3.5 MB at most).");
  await page.waitForFunction(() => window.view.input.value.trim() === "Added to the project: big.png");
  assert.equal(await chips(page).count(), 0);
}));

test("a message typed while the agent works carries no pictures", () => fixture(async (page) => {
  await page.evaluate(() => window.view.setComposer({ running: true }));
  await field(page).fill("one more thing");
  await page.keyboard.press("Enter");
  assert.deepEqual(await sent(page), [{ text: "one more thing", images: [], named: false }]);
}));

test("a runtime without the flag keeps today's line: the file is named in the draft", () => fixture(async (page) => {
  await choose(page, [file("shot.png")]);
  await page.waitForFunction(() => window.view.input.value.includes("Added to the project: shot.png"));
  assert.equal(await chips(page).count(), 0);
}, { images: false }));
