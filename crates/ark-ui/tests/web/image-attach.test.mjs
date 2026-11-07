import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
let browser;
before(async () => { browser = await webkit.launch({ headless: true }); });
after(async () => { await browser?.close(); });

// The real page over a runtime that records what it is asked: uploads, sends, steering.
async function fixture(run) {
  const project = { id: "a", name: "Notes", workspace: "/notes" };
  const chats = [{ id: "chat-a", title: "First draft", project: "a" }, { id: "chat-b", title: "Second draft", project: "a" }];
  let current = chats[0];
  const summary = (chat = current) => ({ name: "Notes", tagline: "", model: "mock", grants: [], project, session: { ...chat, turns: 0 }, location: { runtime: "fixture", project: "a", session: chat.id }, execution_mode: "manual", execution_mode_revision: 0 });
  const errors = [], calls = [], uploads = new Map(), events = new Set();
  let pending = 0;
  const rpc = async (method, params) => {
    calls.push({ method, params });
    if (method === "hello") return { summary: summary(), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture" };
    if (method === "summary") return summary();
    if (method === "execution_mode" || method === "chat_execution_mode") return { ...summary(), execution_mode: params.mode, execution_mode_revision: 1 };
    if (method === "projects") return [{ ...project, current: true }];
    if (method === "sessions") return chats.map((c) => ({ ...c, turns: 1, updated_at: Date.now() }));
    if (method === "chat_read") return { summary: summary(chats.find((c) => c.id === params.session)), transcript: [], approvals: [], running: null };
    if (method === "open_session") {
      current = chats.find((c) => c.id === params.id);
      for (const stream of events) stream.write(`data: ${JSON.stringify({ kind: "summary", summary: summary() })}\n\n`);
      return null;
    }
    if (["transcript", "plugins", "computers"].includes(method)) return [];
    if (method === "places") return { here: "This computer", places: [] };
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "context") return { total: 0, budget: 64000 };
    if (method === "send" || method === "chat_send" || method === "steer") return { started: true, queued: true };
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

/// A picture to drop: a small gradient, so a screenshot of the page shows something.
const makeFile = async (name, type) => {
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 160;
  const g = canvas.getContext("2d");
  const fill = g.createLinearGradient(0, 0, 160, 160);
  fill.addColorStop(0, "#5b8def"); fill.addColorStop(1, "#f0a0c0");
  g.fillStyle = fill; g.fillRect(0, 0, 160, 160);
  return new File([await new Promise((ok) => canvas.toBlob(ok, "image/png"))], name, { type });
};

/// What the browser does when files are dropped on the page.
const dropFiles = (page, files) => page.evaluate(async ({ files, make }) => {
  const dt = new DataTransfer();
  const build = eval(`(${make})`);
  for (const [name, type] of files) dt.items.add(await build(name, type));
  const main = document.querySelector(".main");
  for (const kind of ["dragenter", "dragover", "drop"]) main.dispatchEvent(new DragEvent(kind, { dataTransfer: dt, bubbles: true, cancelable: true }));
}, { files, make: makeFile.toString() });

const pasteFiles = (page, files) => page.evaluate(async ({ files, make }) => {
  const dt = new DataTransfer();
  const build = eval(`(${make})`);
  for (const [name, type] of files) dt.items.add(await build(name, type));
  document.querySelector("#input").dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
}, { files, make: makeFile.toString() });

/// `ARK_UI_SHOTS=<folder>` keeps pictures (light and dark).
const shoot = async (page, name) => {
  if (!process.env.ARK_UI_SHOTS) return;
  await mkdir(process.env.ARK_UI_SHOTS, { recursive: true });
  for (const theme of ["light", "dark"]) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
    await page.waitForTimeout(250);
    await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, `${name}-${theme}.png`), animations: "disabled" });
  }
};

const chips = (page) => page.locator("#composer .thumb-chip");
const sends = (calls) => calls.filter((c) => c.method === "send");
const chatSends = (calls) => calls.filter((c) => c.method === "chat_send");

test("a dropped image is a small picture in the composer and no text", () => fixture(async (page, { calls, settle }) => {
  await dropFiles(page, [["shot.png", "image/png"]]);
  await chips(page).first().waitFor();
  await page.waitForFunction(() => !document.querySelector("#composer .thumb-chip img").hidden);
  assert.equal(await chips(page).count(), 1);
  assert.equal(await page.locator("#input").inputValue(), "", "nothing is written in the draft");
  assert.equal(await page.locator("#composer").getAttribute("data-empty"), "false", "Send is ready with a picture alone");
  await shoot(page, "image-chip");
  const size = await chips(page).first().boundingBox();
  assert.ok(size.width <= 64 && size.height <= 64, `small: ${size.width}x${size.height}`);
  await settle();
  assert.ok(calls.some((c) => c.params?.operation?.kind === "upload_add"), "it went into the project");
  assert.equal(sends(calls).length, 0, "and nothing was sent");
}));

test("the remove button takes the picture back, and Backspace in an empty line does too", () => fixture(async (page) => {
  await dropFiles(page, [["a.png", "image/png"], ["b.png", "image/png"]]);
  await page.waitForFunction(() => document.querySelectorAll("#composer .thumb-chip").length === 2);
  await chips(page).first().hover();
  await chips(page).first().getByRole("button", { name: "Remove a.png" }).click();
  assert.equal(await chips(page).count(), 1);
  await page.locator("#input").focus();
  await page.keyboard.press("Backspace");
  assert.equal(await chips(page).count(), 0);
  assert.equal(await page.locator("#composer").getAttribute("data-empty"), "true");
}));

test("a picture alone sends with its file name, and the sent message shows the picture", () => fixture(async (page, { calls, settle }) => {
  await dropFiles(page, [["shot.png", "image/png"]]);
  await chips(page).first().waitFor();
  await page.locator("#input").focus();
  await page.keyboard.press("Enter");
  await page.locator(".thread .user-image").waitFor();
  await page.waitForFunction(() => !document.querySelector(".thread .user-image img").hidden);
  await settle();
  assert.deepEqual(sends(calls).map((c) => [c.params.text, c.params.images]), [["shot.png", ["shot.png"]]]);
  assert.equal(await chips(page).count(), 0, "the composer is clear");
  assert.equal(await page.locator(".thread .user .bubble").count(), 0, "the name is not said twice");
}));

test("words and a picture go together", () => fixture(async (page, { calls, settle }) => {
  await pasteFiles(page, [["image.png", "image/png"]]);
  await chips(page).first().waitFor();
  await page.locator("#input").fill("what is this?");
  await page.keyboard.press("Enter");
  await page.locator(".thread .bubble", { hasText: "what is this?" }).waitFor();
  await settle();
  await page.waitForFunction(() => !document.querySelector(".thread .user-image img").hidden);
  await shoot(page, "image-sent");
  assert.deepEqual(sends(calls).map((c) => [c.params.text, c.params.images]), [["what is this?", ["image.png"]]]);
  assert.equal(await page.locator(".thread .user-image").count(), 1);
}));

test("two pasted screenshots with one name are two files", () => fixture(async (page, { calls }) => {
  await pasteFiles(page, [["image.png", "image/png"], ["image.png", "image/png"]]);
  await page.waitForFunction(() => document.querySelectorAll("#composer .thumb-chip").length === 2);
  const paths = calls.filter((c) => c.params?.operation?.kind === "upload_parts").flatMap((c) => c.params.operation.input.parts.map((p) => p.path)).filter(Boolean);
  assert.deepEqual([...new Set(paths)].sort(), ["image 2.png", "image.png"]);
}));

test("other files still go in the draft as a line, beside the picture", () => fixture(async (page) => {
  await dropFiles(page, [["shot.png", "image/png"], ["data.csv", "text/csv"]]);
  await chips(page).first().waitFor();
  assert.equal(await chips(page).count(), 1);
  assert.equal((await page.locator("#input").inputValue()).trim(), "Added to the project: data.csv");
}));

test("while the agent works a picture queues with its words instead of steering", () => fixture(async (page, { calls, settle }) => {
  await page.locator("#input").fill("start");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector("#composer").dataset.running === "true");
  await dropFiles(page, [["shot.png", "image/png"]]);
  await chips(page).first().waitFor();
  await page.locator("#input").fill("and this");
  await page.keyboard.press("Enter");
  await page.locator("#queue .queued-thumb").waitFor();
  await settle();
  assert.equal(calls.filter((c) => c.method === "steer").length, 0, "steering carries text only");
  assert.equal(await page.locator("#queue .queued-t").innerText(), "and this");
  assert.equal(await chips(page).count(), 0);
}));

test("a picture put in a split chat is sent from that chat", () => fixture(async (page, { calls, settle }) => {
  // Chat B opens (the tab strip appears), then chat A splits off from its tab.
  await page.locator('.session[data-id="chat-b"]').click();
  await page.locator('.chat-tab-item[data-key="chat-a"]').waitFor();
  await page.locator('.chat-tab-item[data-key="chat-a"]').click({ button: "right" });
  await page.getByRole("menuitem", { name: "Split right" }).click();
  const pane = page.locator('.chat-surface[data-session="chat-a"]');
  await pane.waitFor();
  await dropFiles(page, [["shot.png", "image/png"]]);
  await pane.locator(".thumb-chip img").waitFor({ state: "visible" });
  assert.equal(await page.locator('.chat-surface[data-session="chat-b"] .thumb-chip').count(), 0, "not in the other chat");
  await pane.locator("textarea").fill("and this?");
  await pane.locator("textarea").press("Enter");
  await pane.locator(".user-image img").waitFor({ state: "visible" });
  await settle();
  assert.deepEqual(chatSends(calls).map((c) => [c.params.session, c.params.text, c.params.images]), [["chat-a", "and this?", ["shot.png"]]]);
}));

test("a picture stays with its chat while another chat is shown", () => fixture(async (page) => {
  await dropFiles(page, [["shot.png", "image/png"]]);
  await chips(page).first().waitFor();
  await page.locator('.session[data-id="chat-b"]').click();
  await page.waitForFunction(() => document.querySelector("#composer").dataset.empty === "true");
  assert.equal(await chips(page).count(), 0, "chat B does not carry chat A's picture");
  await page.locator('.session[data-id="chat-a"]').click();
  await chips(page).first().waitFor();
  assert.equal(await chips(page).count(), 1, "chat A has it back");
}));

test("words pasted with a picture are a paste of words", () => fixture(async (page) => {
  const taken = await page.evaluate(async () => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "image.png", { type: "image/png" }));
    dt.setData("text/plain", "copied words");
    return !document.querySelector("#input").dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  assert.equal(taken, false, "the paste is left to the browser");
  assert.equal(await chips(page).count(), 0);
}));

test("an image over the limit is made smaller before it is sent", () => fixture(async (page, { calls, settle }) => {
  // Noise does not compress: a large display's screenshot of it is far over 3.5 MB.
  await page.evaluate(async () => {
    const canvas = document.createElement("canvas");
    canvas.width = 2600; canvas.height = 1800;
    const g = canvas.getContext("2d");
    const pixels = g.createImageData(2600, 1800);
    for (let i = 0; i < pixels.data.length; i += 4) { pixels.data[i] = Math.random() * 255; pixels.data[i + 1] = Math.random() * 255; pixels.data[i + 2] = Math.random() * 255; pixels.data[i + 3] = 255; }
    g.putImageData(pixels, 0, 0);
    const file = new File([await new Promise((ok) => canvas.toBlob(ok, "image/png"))], "Screenshot.png", { type: "image/png" });
    window.bigSize = file.size;
    const dt = new DataTransfer();
    dt.items.add(file);
    const main = document.querySelector(".main");
    for (const kind of ["dragenter", "dragover", "drop"]) main.dispatchEvent(new DragEvent(kind, { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
  await chips(page).first().waitFor({ timeout: 20000 });
  await settle();
  assert.ok(await page.evaluate(() => window.bigSize) > 3_500_000, "it started over the limit");
  const parts = calls.filter((c) => c.params?.operation?.kind === "upload_parts").flatMap((c) => c.params.operation.input.parts).filter((p) => p.data);
  const bytes = parts.reduce((n, p) => n + p.data.length * 3 / 4, 0);
  assert.ok(bytes > 0 && bytes <= 3_500_000, `sent ${bytes} bytes`);
  assert.match(parts[0].path, /\.(png|jpg)$/);
}));
