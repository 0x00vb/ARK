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

for (const other of ["chat-b", "chat-c"]) {
  test(`creative draft preserves original input across ${other}`, async () => fixture(async (page, host) => {
    const input = page.locator("#input");
    const original = "Keep my exact draft  \nwith its spacing";
    await input.fill(original);
    await page.evaluate(() => window.ark.compose("Create a local asset", { append: true }));
    const combined = `${original}\n\nCreate a local asset`;
    assert.equal(await input.inputValue(), combined);
    assert.equal(await input.evaluate(el => document.activeElement === el), true);
    assert.equal(host.calls.some(call => ["send", "submit", "ask"].includes(call.method)), false);
    if (other === "chat-c") await page.locator(".project-row", { hasText: "Writing" }).click();
    else await page.locator('.session[data-id="chat-b"]').click();
    await page.waitForFunction(id => window.ark.summary().session.id === id, other);
    assert.equal(await input.inputValue(), "");
    if (other === "chat-c") await page.locator(".project-row", { hasText: "Notes" }).click();
    else await page.locator('.session[data-id="chat-a"]').click();
    await page.waitForFunction(() => window.ark.summary().session.id === "chat-a");
    assert.equal(await input.inputValue(), combined);
    await page.evaluate(() => window.ark.compose("Replace explicitly"));
    assert.equal(await input.inputValue(), "Replace explicitly");
  }));
}

test("primary, split and app conversations share the same canvas and composer", async () => fixture(async (page) => {
  await page.locator('.session[data-id="chat-b"]').click();
  await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
  await page.locator("#chat-split").click();
  await page.locator(".secondary textarea").waitFor();
  await page.evaluate(() => {
    const mount = document.createElement("div");
    document.querySelector(".pane-body").append(mount);
    const view = window.ark.chatView(mount, {
      submit: async () => {},
      model: { open: () => { mount.dataset.modelOpened = "true"; } },
      files: { captureOrigin: () => { throw Error("No upload expected"); } },
    });
    view.render([{ key: "fixture", input: "A question", answer: "An answer" }]);
    view.setComposer({ modelLabel: "mock" });
  });
  for (const theme of ["light", "dark"]) {
    await page.evaluate(theme => document.documentElement.dataset.colorScheme = theme, theme);
    const styles = await page.locator(".chat-surface").evaluateAll(surfaces => surfaces.map(surface => {
      const composer = surface.querySelector(".composer");
      return {
        paper: getComputedStyle(surface).backgroundColor,
        font: getComputedStyle(surface.querySelector("textarea")).font,
        padding: getComputedStyle(composer).padding,
        files: !!composer.querySelector('[aria-label="Add files"]'),
        model: !!composer.querySelector(".model-chip"),
        mic: !!composer.querySelector(".dictate"),
      };
    }));
    assert.equal(styles.length, 3);
    assert.deepEqual(styles[1], styles[0]);
    assert.deepEqual(styles[2], styles[0]);
  }
  await page.locator(".chat-view .model-chip").dispatchEvent("click");
  assert.equal(await page.locator(".pane-body > div").getAttribute("data-model-opened"), "true");
}));
