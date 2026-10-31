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

async function fixture(run, outcome = "success", runtime = "fixture") {
  const projects = [{ id: "a", name: "Notes", workspace: "/notes" }, { id: "b", name: "Writing", workspace: "/writing" }];
  const chats = [{ id: "chat-a", title: "First draft", project: "a" }, { id: "chat-b", title: "Second draft", project: "a" }, { id: "chat-c", title: "Other project", project: "b" }];
  let current = chats[0];
  const summary = (chat = current) => ({ name: "Notes", tagline: "", model: "mock", grants: [], project: projects.find(p => p.id === chat.project), session: { ...chat, turns: 0 }, location: { runtime: "fixture", project: chat.project, session: chat.id }, execution_mode: "manual", execution_mode_revision: 0 });
  const events = new Set(), errors = [], calls = [], files = [];
  let release, started, pending = 0;
  const waiting = new Promise(resolve => { started = resolve; });
  const rpc = async (method, params) => {
    calls.push({ method, params });
    if (method === "hello") return { summary: summary(), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime };
    if (method === "summary") return summary();
    if (method === "chat_read") return { summary: summary(chats.find(chat => chat.id === params.session)), transcript: [], approvals: [], running: null };
    if (method === "projects") return projects.map(p => ({ ...p, current: p.id === current.project }));
    if (method === "sessions") return chats.filter(c => c.project === params.project).map(c => ({ ...c, turns: 1, updated_at: Date.now() }));
    if (["transcript", "plugins", "computers"].includes(method)) return [];
    if (method === "places") return { here: "This computer", places: [] };
    if (method === "attention_all") return { items: [], notes: [], away: [] };
    if (method === "dictation_status") return { status: "unavailable" };
    if (method === "context") return { total: 0, budget: 64000 };
    if (["open_session", "open_project"].includes(method)) {
      current = method === "open_session" ? chats.find(c => c.id === params.id) : chats.find(c => c.project === params.id);
      assert.ok(current);
      for (const stream of events) stream.write(`data: ${JSON.stringify({ kind: "summary", summary: summary() })}\n\n`);
      return null;
    }
    if (method === "resource") {
      const { ref, operation } = params;
      const key = `${ref.kind}:${operation.kind}`;
      const results = { "project:grants": [], "project:skills": [], "project:board": null, "project:schedules": [], "process:processes": [], "project:focus": null };
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
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
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
    await page.locator('.session[data-id="chat-a"]').waitFor();
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

async function drop(page) {
  await page.evaluate(() => {
    const dataTransfer = new DataTransfer(); dataTransfer.items.add(new File(["origin only"], "notes.txt"));
    document.querySelector("#thread").dispatchEvent(new DragEvent("drop", { dataTransfer, bubbles: true, cancelable: true }));
  });
}

for (const runtime of ["fixture", "window"]) for (const other of ["chat-b", "chat-c"]) {
  test(`upload completion preserves unrelated draft ${other} on ${runtime}`, async () => fixture(async (page, upload) => {
    const input = page.locator("#input");
    if (runtime === "window") assert.equal(await input.inputValue(), "Existing remote draft");
    await input.fill("Origin draft");
    await drop(page);
    await upload.waiting;
    if (other === "chat-c") await page.locator(".project-row", { hasText: "Writing" }).click();
    else await page.locator(`.session[data-id="${other}"]`).click();
    await page.waitForFunction(id => window.ark.summary().session.id === id, other);
    await input.fill("Unrelated draft");
    await input.evaluate(el => el.setSelectionRange(3, 6));
    await page.locator("#mode-chip").focus();
    upload.release();
    await page.locator(".adding").waitFor({ state: "detached" });
    assert.equal(await input.inputValue(), "Unrelated draft");
    assert.equal(await page.locator("#mode-chip").evaluate(el => document.activeElement === el), true);
    assert.deepEqual(await input.evaluate(el => [el.selectionStart, el.selectionEnd]), [3, 6]);
    assert.deepEqual(upload.files, [{ location: { runtime: "fixture", project: "a", session: "chat-a" }, path: "notes.txt" }]);
    if (other === "chat-c") await page.locator(".project-row", { hasText: "Notes" }).click();
    else await page.locator('.session[data-id="chat-a"]').click();
    await page.waitForFunction(() => window.ark.summary().session.id === "chat-a");
    assert.equal(await input.inputValue(), "Origin draft\nAdded to the project: notes.txt");
    await upload.settle();
    await page.reload();
    await page.locator('.session[data-id="chat-a"]').waitFor();
    assert.equal(await input.inputValue(), "Origin draft\nAdded to the project: notes.txt");
    if (runtime === "window") assert.equal(await page.evaluate(() => localStorage.getItem("ark-draft:fixture:chat-a")), null);
  }, "success", runtime));
}

for (const outcome of ["failure", "stop"]) {
  test(`upload ${outcome} preserves the draft`, async () => fixture(async (page, upload) => {
    await page.locator("#input").fill("Keep my draft");
    await drop(page);
    await upload.waiting;
    if (outcome === "stop") await page.locator('.adding [aria-label="Stop"]').click();
    upload.release();
    await page.locator(".adding").waitFor({ state: "detached" });
    assert.equal(await page.locator("#input").inputValue(), "Keep my draft");
    assert.deepEqual(upload.files, []);
    assert.ok(upload.calls.some(call => call.method === "resource" && call.params.operation.kind === "upload_cancel"));
    await page.getByText(outcome === "stop" ? "Stopped. Nothing was added." : "The files were not added: Upload refused", { exact: true }).waitFor();
  }, outcome));
}

test("same-chat upload keeps the cursor above its saved attachment line", async () => fixture(async (page, upload) => {
  const input = page.locator("#input");
  await input.fill("Compare them");
  await drop(page);
  await upload.waiting;
  upload.release();
  await page.locator(".adding").waitFor({ state: "detached" });
  assert.equal(await input.inputValue(), "Compare them\nAdded to the project: notes.txt");
  assert.equal(await input.evaluate(el => el.selectionStart), "Compare them".length);
  await upload.settle();
  await page.reload();
  await page.locator('.session[data-id="chat-a"]').waitFor();
  assert.equal(await input.inputValue(), "Compare them\nAdded to the project: notes.txt");
}));

for (const runtime of ["fixture", "window"]) for (const state of ["mounted", "disposed", "promoted"]) {
  test(`upload completion follows a ${state} secondary origin on ${runtime}`, async () => fixture(async (page, upload) => {
    await page.locator('.session[data-id="chat-b"]').click();
    await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
    await page.locator("#chat-split").click();
    const origin = page.locator(".secondary textarea");
    await origin.waitFor();
    if (runtime === "window") assert.equal(await origin.inputValue(), "Existing remote draft");
    await origin.fill("Initial origin draft");
    const chooser = page.waitForEvent("filechooser");
    await page.locator('.secondary [aria-label="Add files"]').click();
    await (await chooser).setFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("origin only") });
    await upload.waiting;
    await origin.fill("Latest origin draft");
    const input = page.locator("#input");
    if (state === "disposed") await page.locator('.chat-tab-item[data-session="chat-a"] .chat-tab-close').click();
    if (state === "promoted") {
      await page.locator('.chat-tab-item[data-session="chat-b"] .chat-tab-close').click();
      await page.waitForFunction(() => window.ark.summary().session.id === "chat-a");
      await page.locator(".secondary").waitFor({ state: "detached" });
    } else await input.fill("Unrelated primary draft");
    await input.evaluate(el => el.setSelectionRange(2, 5));
    await page.locator("#mode-chip").focus();
    upload.release();
    await page.locator(".adding").waitFor({ state: "detached" });
    const expected = "Latest origin draft\nAdded to the project: notes.txt";
    // A promoted origin's draft is restored, then completed, after the progress row goes.
    if (state === "promoted") await page.waitForFunction(v => document.querySelector("#input").value === v, expected, { timeout: 5000 }).catch(() => {});
    assert.equal(await input.inputValue(), state === "promoted" ? expected : "Unrelated primary draft");
    assert.deepEqual(await input.evaluate(el => [el.selectionStart, el.selectionEnd]), [2, 5]);
    assert.equal(await page.locator("#mode-chip").evaluate(el => document.activeElement === el), true);
    if (state === "mounted") assert.equal(await origin.inputValue(), expected);
    if (state === "disposed") {
      await page.locator('.session[data-id="chat-a"]').click();
      await page.waitForFunction(() => window.ark.summary().session.id === "chat-a");
      assert.equal(await input.inputValue(), expected);
    }
    assert.deepEqual(upload.files, [{ location: { runtime: "fixture", project: "a", session: "chat-a" }, path: "notes.txt" }]);
    await upload.settle();
    await page.reload();
    await page.locator('.session[data-id="chat-a"]').waitFor();
    if (state === "mounted") await page.locator(".secondary textarea").waitFor();
    assert.equal(await (state === "mounted" ? page.locator(".secondary textarea") : input).inputValue(), expected);
    if (runtime === "window") assert.equal(await page.evaluate(() => localStorage.getItem("ark-draft:fixture:chat-a")), null);
  }, "success", runtime));
}

for (const next of ["general", "other-project"]) {
  test(`main conversation drop keeps captured origin after navigating to ${next}`, async () => fixture(async (page, upload) => {
    await page.evaluate(() => {
      window.mainConversations = [];
      window.ark.view({ id: "project-chat", title: "New chat", resourceScoped: true, render(el, _params, api) {
        const record = { location: api.location, text: "", disposed: false, sent: [] };
        window.mainConversations.push(record);
        const view = api.chatView(el, { submit: text => record.sent.push(text) });
        view.render([], { empty: "What should we work on?" });
        view.input.addEventListener("input", () => { record.text = view.input.value; });
        return {
          title: () => "Project conversation",
          focusInput: () => view.focus(),
          cancel: () => { record.cancels = (record.cancels || 0) + 1; },
          captureUploadOrigin: () => ({ uploadApi: api, appendDraft(line) {
            record.text = `${record.text}\n${line}`;
            if (!record.disposed) view.set(record.text);
          } }),
          dispose() { record.disposed = true; view.dispose(); },
        };
      } });
      window.ark.panel({ id: "details-test", title: "Details", render(el) { el.textContent = "Details"; return {}; } });
      window.ark.openPanel("details-test");
      document.querySelector('#close-pane').click();
      window.ark.openView("project-chat");
    });
    const projected = page.locator('#stage textarea');
    assert.equal(await page.locator('textarea:visible').count(), 1);
    await projected.fill("Original draft");
    await page.locator('#session-title').focus();
    await page.keyboard.press('x');
    assert.equal(await projected.evaluate(el => el === document.activeElement), true);
    await page.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: "\\", metaKey: true, bubbles: true })));
    await page.waitForFunction(() => document.querySelector('.app').dataset.pane === 'open');
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => document.querySelector('.app').dataset.pane !== 'open');
    await page.keyboard.press('Escape');
    assert.equal(await page.evaluate(() => window.mainConversations[0].cancels), 1);
    await page.evaluate(() => { document.querySelector('#stage .palette').hidden = false; });
    await page.keyboard.press('Escape');
    assert.equal(await page.evaluate(() => window.mainConversations[0].cancels), 1, "dismissing slash suggestions does not cancel the agent");
    await page.evaluate(() => {
      const dataTransfer = new DataTransfer(); dataTransfer.items.add(new File(["origin only"], "notes.txt"));
      document.querySelector("#stage textarea").dispatchEvent(new DragEvent("drop", { dataTransfer, bubbles: true, cancelable: true }));
    });
    await upload.waiting;
    if (next === "other-project") {
      await page.locator('.project-row', { hasText: 'Writing' }).click();
      await page.waitForFunction(() => window.ark.summary().project.id === "b");
      assert.equal(await page.evaluate(() => window.mainConversations[0].disposed), true);
      await page.evaluate(() => window.ark.openView("project-chat"));
      await page.locator('#stage textarea').fill("Unrelated draft");
    } else {
      await page.evaluate(() => window.ark.showConversation());
      await page.locator('#input').fill("Unrelated draft");
    }
    upload.release();
    await page.waitForFunction(() => window.mainConversations[0].text.includes("notes.txt"));
    assert.equal(await page.locator('textarea:visible').inputValue(), "Unrelated draft");
    assert.equal(upload.files[0].location.project, "a");
    assert.equal(upload.files[0].location.session, "chat-a");
    if (next === "other-project") assert.deepEqual(await page.evaluate(() => window.mainConversations.map(item=>item.location.project)), ["a", "b"]);
  }));
}
