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

async function fixture(run, { detached = false, empty = false, coarse = false, remote = false, places = { here: "This computer", places: [] } } = {}) {
  const projects = [{ id: "a", name: "Operations", current: true }, { id: "b", name: "Writing", current: false }];
  const sessions = ["chat-a", "chat-b"].map((id, i) => ({ id, title: i ? "Supplier comparison" : "Quarterly review", turns: 1, updated_at: Date.now(), working: i === 1, participants: i ? ["Editor", "Lead", "Reader", "Writer"] : [] }));
  const summary = { name: "ARK", tagline: "", model: "mock", grants: [], project: projects[0], session: sessions[0], location: { runtime: "fixture", project: "a", session: "chat-a" }, execution_mode: "manual", execution_mode_revision: 0 };
  if (remote) summary.place = { id: "remote", name: "Remote computer", state: "ok" };
  if (empty) summary.session = { ...summary.session, turns: 0 };
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
    if (method === "places") return places;
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "context") return { total: 0, budget: 64000 };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "resource") {
      const key = `${params.ref.kind}:${params.operation.kind}`;
      const results = { "project:focus": {}, "project:grants": [], "project:skills": [], "project:board": null, "project:schedules": [], "process:processes": [] };
      if (Object.hasOwn(results, key)) return results[key];
    }
    throw Error(`Unexpected RPC ${method} ${JSON.stringify(params)}`);
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
  const context = await browser.newContext({ viewport: { width: 1180, height: 800 }, hasTouch: coarse });
  const page = await context.newPage();
  page.setDefaultTimeout(7000);
  page.on("pageerror", error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html${detached ? "?detached=chat-b" : ""}`);
    await page.locator('.session[data-id="chat-b"] .session-working').waitFor();
    await run(page, { emit, request, snapshot: items => { approvals = items; }, assign: (id, names) => { sessions.find(session => session.id === id).participants = names; }, hold: () => { holdRead = true; }, reading, release: () => releaseRead() });
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
  const project = page.locator('.project[data-id="a"] .project-row .project-name');
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


test("primary controls have distinct targets and shared icons at desktop and phone widths", () => fixture(async page => {
  for (const width of [1180, 760, 390]) {
    await page.setViewportSize({ width, height: 800 });
    await page.evaluate(() => { const model = document.querySelector(".model-chip"); if (model) model.textContent = "A model with a deliberately long descriptive name";
      const place = document.querySelector("#place-chip"); place.hidden = false; place.textContent = "A computer with a long name";
      document.querySelector("#ctx-ring").textContent = "32k / 64k"; });
    if (process.env.ARK_UI_SHOTS) await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, `icon-harmony-${width}.png`) });
    const controls = page.locator(".composer-bar .chip, .composer-bar .ctx-meter, .topbar .icon-btn, .sidebar-top .icon-btn, .composer-bar .attach, .composer-bar .dictate, .composer-bar .send");
    const boxes = [];
    for (const control of await controls.all()) {
      if (!await control.isVisible()) continue;
      const box = await control.boundingBox();
      assert.ok(box.width >= (await control.evaluate(el => el.classList.contains("model-chip") || el.classList.contains("place-chip")) ? 1 : 32) && box.height >= 32, JSON.stringify({ width, box }));
      assert.ok(box.x >= 0 && box.x + box.width <= width);
      assert.equal(await control.evaluate(el => getComputedStyle(el, "::before").content), "none");
      boxes.push(box);
    }
    for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
      const a = boxes[i], b = boxes[j];
      assert.ok(a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y);
    }
  }
  assert.equal(await page.locator("#attach svg").getAttribute("viewBox"), "0 0 256 256");
  const attach = page.locator("#attach");
  await attach.focus();
  await page.keyboard.down("Space");
  assert.ok(["1", "none"].includes(await attach.evaluate(el => getComputedStyle(el).scale)));
  await page.keyboard.up("Space");
  await page.keyboard.press("Escape");
  await page.emulateMedia({ reducedMotion: "reduce" });
  assert.ok(parseFloat(await attach.evaluate(el => getComputedStyle(el).transitionDuration)) <= 0.001);
}));


test("model follows attachment and dictation in visual and keyboard order", () => fixture(async page => {
  assert.deepEqual(await page.locator(".composer-bar > button").evaluateAll(nodes => nodes.slice(0, 3).map(node => node.id)), ["attach", "dictate", "model-chip"]);
  const [attach, dictate, model, mode] = await Promise.all(["#attach", "#dictate", "#model-chip", "#mode-chip"].map(selector => page.locator(selector).boundingBox()));
  assert.ok(attach.x < dictate.x && dictate.x < model.x && model.x < mode.x);
  assert.ok(model.x - (dictate.x + dictate.width) <= 8);
  assert.equal(await page.locator(".topbar").evaluate(el => el.getBoundingClientRect().height), 44);
}));


test("the sole local computer stays quiet while a real destination choice remains visible", async () => {
  await fixture(async page => { assert.equal(await page.locator("#place-chip").isVisible(), false); }, { empty: true, places: { here: "This Mac", places: [], can_add: true } });
  await fixture(async page => {
    assert.equal(await page.locator("#place-chip").isVisible(), true);
    await page.locator("#place-chip").click();
    assert.equal(await page.getByRole("button", { name: "Studio Mac", exact: true }).isVisible(), true);
  }, { empty: true, places: { here: "This Mac", places: [{ name: "Studio Mac", state: "ok" }], can_add: true } });
});


test("background decisions preserve project-section keyboard focus across sidebar remounts", () => fixture(async (page, { emit, request }) => {
  await page.evaluate(() => {
    window.sectionMounts = 0;
    window.ark.projectSection({ id: "contacts", label: "Agent conversations", render(el) {
      window.sectionMounts++;
      const button = document.createElement("button"); button.textContent = "Chat with Writer"; button.dataset.focusKey = "Writer"; el.append(button);
    } });
  });
  const contact = page.getByRole("button", { name: "Chat with Writer", exact: true });
  await contact.waitFor(); await contact.focus();
  const mounts = await page.evaluate(() => window.sectionMounts);
  emit({ kind: "approval", ...request });
  await page.locator('.session[data-id="chat-b"]').getByText("Needs you", { exact: true }).waitFor();
  assert.ok(await page.evaluate(() => window.sectionMounts) > mounts);
  assert.equal(await contact.evaluate(el => document.activeElement === el), true);
}));


test("chat identity distinguishes neutral, individual and group assignments without changing status", () => fixture(async page => {
  const group = page.locator('.session[data-id="chat-b"]');
  assert.equal(await group.locator(".chat-face").count(), 3);
  assert.equal(await group.locator(".chat-overflow").innerText(), "+1");
  assert.match(await group.getAttribute("aria-description"), /Editor, Lead, Reader, Writer/);
  assert.equal(await group.locator(".session-working").count(), 1);
  assert.equal(await page.locator('.session[data-id="chat-a"] .chat-avatar[data-count="0"]').count(), 1);
  const result = await page.evaluate(async () => {
    const { chatAvatar } = await import("./js/dom.js");
    const single = chatAvatar(["Writer"]); const duplicate = chatAvatar(["Writer", "Writer"]);
    return { single: single.dataset.count, duplicate: duplicate.dataset.count, hidden: single.getAttribute("aria-hidden") };
  });
  assert.deepEqual(result, { single: "1", duplicate: "1", hidden: "true" });
}));


test("a new delegation refreshes participant identities before the chat turn ends", () => fixture(async (page, { emit, assign }) => {
  assign("chat-a", ["Lead", "Writer"]);
  emit({ kind: "orch", event: { type: "task_created", project: "a", task: "new-task" } });
  await page.locator('.session[data-id="chat-a"] .chat-avatar[data-count="2"]').waitFor();
}));


test("touch keeps comfortable targets despite smaller pointer controls", () => fixture(async page => {
  await page.setViewportSize({ width: 390, height: 800 });
  assert.equal(await page.evaluate(() => matchMedia("(pointer: coarse)").matches), true);
  for (const selector of ["#attach", "#dictate", "#send"]) {
    const box = await page.locator(selector).boundingBox();
    assert.ok(box.width >= 40 && box.height >= 40);
  }
}, { coarse: true }));


test("an app identity resolver is shared by individual and group chat avatars", () => fixture(async page => {
  const result = await page.evaluate(async () => {
    window.ark.agentIdentity(name => `agent-${name}`);
    const { chatAvatar } = await import("./js/dom.js");
    return [chatAvatar(["Writer"]), chatAvatar(["Writer", "Editor"])].map(mark => mark.querySelector(".chat-face").dataset.variant);
  });
  assert.deepEqual(result, ["agent-Writer", "agent-Writer"]);
  await page.locator('.session[data-id="chat-b"] .chat-face[data-variant="agent-Editor"]').waitFor();
}));


test("mixed scene files keep accurate labels and compact cards", () => fixture(async page => {
  await page.evaluate(async () => {
    const { Thread } = await import("./js/conversation.js");
    const { setFileKinds } = await import("./js/file.js");
    setFileKinds({ png: "image", glb: "model", txt: "text" }, { image: "image", model: "3D model", text: "text file" });
    const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96"><rect width="96" height="96" fill="blue"/></svg>';
    const api = { openFile: path => { window.openedFile = path; }, call: async method => {
      if (method === "file_media") return { data: btoa(svg), media_type: "image/svg+xml" };
      throw Error(method);
    }, toast() {} };
    const thread = new Thread(document.querySelector("#thread"), document.querySelector("#thread").parentElement, api, { inspect: false });
    thread.renderStored([{ input: "Build a scene", index: 0, answer: "Saved the scene.", activity: [{ kind: "result", files: ["render.png", "scene.glb", "editable.blend", "notes.txt", "archive.unknown"] }] }]);
  });
  await page.waitForFunction(() => document.querySelector(".file-card-image")?.naturalWidth > 0);
  for (const [theme, width] of [["light", 1180], ["dark", 1180], ["light", 390]]) {
    await page.setViewportSize({ width, height: 800 });
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    if (process.env.ARK_UI_SHOTS) {
      await mkdir(process.env.ARK_UI_SHOTS, { recursive: true });
      await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, `scene-files-${theme}-${width}.png`), animations: "disabled" });
    }
    for (const [name, label] of [["editable.blend", "File"], ["archive.unknown", "File"], ["notes.txt", "Text file"], ["scene.glb", "3D model"]]) {
      const card = page.getByRole("button", { name: `Open ${name}`, exact: true });
      assert.equal(await card.locator(".file-card-sub").innerText(), label);
      const box = await card.boundingBox();
      assert.ok(box.height >= 44 && box.height <= 64, `${name} height ${box.height}`);
      assert.ok(box.x >= 0 && box.x + box.width <= width);
      await card.focus(); await page.keyboard.press("Enter");
      assert.equal(await page.evaluate(() => window.openedFile), name);
    }
  }
}));


for (const remote of [false, true]) test(`a view slash command opens outside the fixed rail without sending a message (${remote ? "remote" : "local"})`, () => fixture(async page => {
  await page.evaluate(() => window.ark.view({ id: "agents", title: "Agents", command: "/agents", render(el) {
    el.append(window.ark.h("h1", "Agent canvas"));
  } }));
  assert.equal(await page.locator('#destinations [data-view="agents"]').count(), 0);
  await page.locator("#input").fill("/agents");
  await page.locator("#input").press("Enter");
  await page.getByRole("heading", { name: "Agent canvas", exact: true }).waitFor();
  assert.equal(await page.locator('#destinations [data-view="agents"]').count(), 0);
  await page.getByRole("button", { name: "Chat", exact: true }).click();
  await page.getByRole("heading", { name: "Agent canvas", exact: true }).waitFor({ state: "hidden" });
}, { remote }));


test("view navigation stays immediate while a turn is running", () => fixture(async (page, { emit }) => {
  await page.evaluate(() => window.ark.view({ id: "agents", title: "Agents", command: "/agents", render(el) {
    el.append(window.ark.h("h1", "Agent canvas"));
  } }));
  emit({ kind: "turn_started", input: "Existing work" });
  await page.locator('.app[data-running="true"]').waitFor();
  await page.locator("#input").fill("/agents");
  await page.locator("#input").press("Enter");
  await page.getByRole("heading", { name: "Agent canvas", exact: true }).waitFor();
  assert.equal(await page.locator("#queue").innerText(), "");
  assert.equal(await page.locator(".app").getAttribute("data-running"), "true");
}));


test("agent chat navigation uses the registered conversation view with a session fallback", () => fixture(async page => {
  assert.equal(await page.evaluate(async () => {
    const opened = [];
    const original = window.ark.openSession;
    window.ark.openSession = id => opened.push(id);
    await window.ark.openAgentChat("Editor", "work-conversation");
    window.ark.openSession = original;
    return opened[0];
  }), "work-conversation");
  await page.evaluate(async () => {
    window.ark.view({ id: "owned-agent-chat", title: "Agent", agentConversation: true, resourceScoped: true, render(el, params) { el.textContent = `Conversation with ${params.name}`; } });
    await window.ark.openAgentChat("Editor", "work-conversation");
  });
  await page.getByText("Conversation with Editor", { exact: true }).waitFor();
}));


test("a property menu stays within the window at the bottom edge", () => fixture(async page => {
  const bounds = await page.evaluate(() => {
    const anchor = document.createElement("button");
    anchor.textContent = "Owner";
    Object.assign(anchor.style, { position: "fixed", bottom: "0", right: "12px" });
    document.body.append(anchor);
    const menu = window.ark.popover(anchor, Array.from({ length: 12 }, (_, index) => {
      const button = document.createElement("button"); button.className = "menu-item"; button.textContent = `Agent ${index}`; return button;
    }));
    const rect = menu.getBoundingClientRect();
    return { top: rect.top, bottom: rect.bottom, height: innerHeight };
  });
  assert.ok(bounds.top >= 0 && bounds.bottom <= bounds.height, JSON.stringify(bounds));
  await page.getByRole("button", { name: "Agent 11", exact: true }).click();
}));
