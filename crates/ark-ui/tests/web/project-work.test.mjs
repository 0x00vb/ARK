import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { chromium, webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
let browser;
before(async () => { browser = await (process.env.ARK_UI_BROWSER === "chromium" ? chromium : webkit).launch({ headless: true, ...(process.env.ARK_UI_BROWSER_PATH ? { executablePath: process.env.ARK_UI_BROWSER_PATH } : {}) }); });
after(async () => { await browser?.close(); });

async function fixture(run, { boot = false, searchProjects = false, holdHello = false, rpcOverrides = {} } = {}) {
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
  const page = await context.newPage();
  page.setDefaultTimeout(5000);
  const errors = [];
  const calls = [], unexpected = [];
  let pending = 0;
  let events;
  const eventQueue = [];
  const eventTimers = new Set();
  const emit = async message => {
    const body = `data: ${JSON.stringify(message)}\n\n`;
    if (!events) { eventQueue.push(body); return; }
    const route = events; events = null;
    await route.fulfill({ contentType: 'text/event-stream', body });
  };
  const summary = {
    name: "Notes", tagline: "", model: "mock", grants: [], detail: false,
    project: { id: "a", name: "Notes", workspace: "/fixture/notes" },
    session: { id: "chat-a", title: "First draft", turns: 0 },
    location: { runtime: "fixture", project: "a", session: "chat-a" },
    execution_mode: "manual", execution_mode_revision: 0,
  };
  const projects = [{ ...summary.project, current: true }];
  if (searchProjects) projects.push({ id: "b", name: "Writing", current: false });
  const sessions = [{ ...summary.session, updated_at: Date.now() }, { id: "chat-b", title: "Second draft", turns: 1, updated_at: Date.now() }];
  let releaseHello;
  const helloReady = holdHello ? new Promise(resolve => { releaseHello = resolve; }) : Promise.resolve();
  const rpc = {
    hello: async () => { await helloReady; return { summary, ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture" }; },
    summary: () => summary,
    execution_mode: (params) => { summary.execution_mode = params.mode; summary.execution_mode_revision = 1; return summary; },
    chat_execution_mode: (params) => { summary.execution_mode = params.mode; summary.execution_mode_revision = 1; return summary; },
    projects: () => projects,
    sessions: () => sessions,
    agent_contacts: params => {
      assert.ok(projects.some(project => project.id === params.project));
      return [{ name: "Archivist", role: params.project === "b" ? "Separate project specialist" : "Keeps procurement decisions", state: "idle" },
        { name: "Removed", role: "Former reviewer", state: "terminated" }];
    },
    transcript: () => [],
    places: () => ({ here: "This computer", places: [] }),
    attention_all: () => ({ items: [], notes: [], away: [] }),
    plugins: () => [],
    computers: () => [],
    context: () => ({ total: 0, budget: 64000 }),
    resource: params => {
      const results = { "project:grants": [], "project:skills": [], "project:board": null,
        "project:schedules": [], "process:processes": [], "project:focus": null, "project:agents": [
          { name: "Archivist", role: "Keeps procurement decisions", state: "idle" },
          { name: "Removed", role: "Former reviewer", state: "terminated" },
        ] };
      const key = `${params.ref?.kind}:${params.operation?.kind}`;
      if (!Object.hasOwn(results, key)) throw Error(`Unknown fixture resource ${key}`);
      const location = params.ref?.location;
      if (location?.runtime !== "fixture" || !["a", ...(searchProjects ? ["b"] : [])].includes(location.project) || !["chat-a", "chat-b"].includes(location.session)) {
        throw Error(`Unexpected fixture resource location ${JSON.stringify(location)}`);
      }
      return results[key];
    },
    open_project: params => {
      assert.equal(params.id, "b");
      summary.project = { id: "b", name: "Writing" };
      summary.location.project = "b";
      summary.session = { ...sessions[1] };
      summary.location.session = "chat-b";
      projects.forEach(project => { project.current = project.id === "b"; });
      const timer = setTimeout(() => { eventTimers.delete(timer); void emit({ kind: 'summary', summary }); }, 180);
      eventTimers.add(timer);
      return null;
    },
    open_session: async params => {
      assert.equal(params.id, "chat-b");
      summary.session = { ...sessions[1] };
      summary.location.session = "chat-b";
      await emit({ kind: 'summary', summary });
      return null;
    },
  };
  page.on("pageerror", error => errors.push(error.message));
  Object.assign(rpc, rpcOverrides);
  await page.route("**/*", async route => {
    const url = new URL(route.request().url());
    const path = resolve(root, `.${decodeURIComponent(url.pathname)}`);
    if (url.origin !== "http://ark.test" || !path.startsWith(root + sep)) {
      unexpected.push(route.request().url());
      return route.abort();
    }
    if (boot && url.pathname === "/rpc") {
      pending++;
      try {
        const { method, params } = route.request().postDataJSON();
        calls.push({ method, params });
        if (!Object.hasOwn(rpc, method)) {
          unexpected.push(`RPC ${method}`);
          return await route.fulfill({ json: { ok: false, error: `Unknown fixture RPC ${method}` } });
        }
        let result;
        try {
          result = await rpc[method](params);
        } catch (error) {
          unexpected.push(`RPC ${method}: ${error.message}`);
          return await route.fulfill({ json: { ok: false, error: error.message } });
        }
        return await route.fulfill({ json: { ok: true, result } });
      } finally {
        pending--;
      }
    }
    if (boot && url.pathname === "/session") return route.fulfill({ json: { signed_in: true, pairing: false } });
    if (boot && url.pathname === "/events") {
      if (eventQueue.length) await route.fulfill({ contentType: 'text/event-stream', body: eventQueue.splice(0).join('') });
      else events = route;
      return;
    }
    if (url.pathname === "/index.html") {
      let html = await readFile(path, "utf8");
      if (!boot) html = html.replace('<script type="module" src="js/app.js"></script>', "");
      return route.fulfill({ contentType: "text/html", body: html });
    }
    if (url.pathname === "/manifest.webmanifest") return route.fulfill({ contentType: "application/manifest+json", json: { name: "Notes fixture", start_url: "/index.html", display: "standalone" } });
    if (!/^\/js\/[a-z-]+\.js$/.test(url.pathname) && url.pathname !== "/app.css") {
      unexpected.push(url.pathname);
      return route.abort();
    }
    await route.fulfill({ contentType: path.endsWith(".css") ? "text/css" : "text/javascript", body: await readFile(path) });
  });
  try {
    await page.goto("http://ark.test/index.html", { waitUntil: holdHello ? "commit" : "load" });
    if (!boot) await page.evaluate(async () => {
      const { installApi, registry } = await import("./js/ext.js");
      const f = window.fixture = {
        projects: [{ id: "a", name: "Notes", current: true }, { id: "b", name: "Writing", current: false }],
        sessions: { a: [{ id: "chat-a", title: "First draft", turns: 1, updated_at: Date.now() }], b: [] },
        events: [], opened: [], errors: [], changed: [], registry,
      };
      console.error = error => f.errors.push(String(error));
      const api = {
        call: async (method, params) => {
          if (method === "projects") return structuredClone(f.projects);
          if (method === "sessions") return structuredClone(f.sessions[params.project] || []);
          throw new Error(`Unexpected call ${method}`);
        },
        _changed: projects => f.changed.push(projects),
        openSession: id => f.opened.push(id),
        openProject: async id => {
          f.projects.forEach(project => { project.current = project.id === id; });
        },
        newSession() {}, openSettings() {}, toast() {},
      };
      f.api = installApi(api);
    });
    const settle = async () => {
      let previous = -1;
      for (let i = 0; i < 100; i++) {
        await page.waitForTimeout(10);
        if (pending === 0 && previous === calls.length) return;
        previous = calls.length;
      }
      throw Error("Fixture startup RPCs did not settle");
    };
    await run(page, { calls, unexpected, errors, settle, releaseHello, emit });
    assert.deepEqual(errors, [], "uncaught page errors");
    assert.deepEqual(unexpected, [], "unexpected fixture requests");
  } catch (error) {
    if (boot) console.error(JSON.stringify({ rpc: calls, unexpected, errors }, null, 2));
    throw error;
  } finally {
    for (const timer of eventTimers) clearTimeout(timer);
    await context.close();
  }
}

test("shared search discovers zero-turn agents by role, preserves chats, and opens the scoped identity", () => fixture(async page => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.openedSearch = [];
    window.ark.searchSource({ id: "agents", label: "Agent", async load(project, api) {
      return (await api.call("agent_contacts", { project: project.id })).filter(agent => agent.state !== "terminated").map(agent => ({ id: agent.name, title: agent.name, description: agent.role }));
    }, open(entry, scoped) { window.openedSearch.push({ id: entry.id, location: scoped.location }); } });
  });
  await page.locator("#input").fill("Keep my unsent draft");
  await page.locator("#search-chats").click();
  const input = page.getByRole("combobox", { name: "Search chats and agents" });
  await page.locator('#chat-search-results[aria-busy="false"]').waitFor();
  assert.match(await page.locator("#chat-search-results").innerText(), /Second draft/);
  assert.match(await page.locator("#chat-search-results").innerText(), /Archivist/);
  assert.doesNotMatch(await page.locator("#chat-search-results").innerText(), /Removed/);
  await input.fill("PROCUREMENT");
  assert.equal(await page.getByRole("option").count(), 1);
  await input.press("Enter");
  assert.deepEqual(await page.evaluate(() => window.openedSearch), [{ id: "Archivist", location: { runtime: "fixture", project: "a", session: "chat-a" } }]);
  assert.equal(await page.locator("#input").inputValue(), "Keep my unsent draft");
}, { boot: true }));

test("partial search failure retains successful results and exposes retry", () => fixture(async page => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.ark.searchSource({ id: "away", load() { throw Error("offline"); }, open() {} });
  });
  await page.locator("#search-chats").click();
  await page.locator('#chat-search-results[aria-busy="false"]').waitFor();
  assert.match(await page.locator("#chat-search-results").innerText(), /Second draft/);
  assert.match(await page.locator("#chat-search-results").innerText(), /Some results are not available/);
  assert.equal(await page.getByRole("button", { name: "Try again", exact: true }).count(), 1);
  await page.keyboard.press("Escape");
}, { boot: true }));

test("search rejects stale computer identity before opening an agent", () => fixture(async page => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.openedSearch = [];
    window.ark.searchSource({ id: "agents", load() { return [{ id: "Archivist", title: "Archivist" }]; }, open(entry) { window.openedSearch.push(entry.id); } });
  });
  await page.locator("#search-chats").click();
  const input = page.getByRole("combobox", { name: "Search chats and agents" });
  await page.locator('#chat-search-results[aria-busy="false"]').waitFor();
  await input.fill("Archivist");
  await page.evaluate(() => { window.ark.summary().location.runtime = "another-computer"; });
  await input.press("Enter");
  assert.deepEqual(await page.evaluate(() => window.openedSearch), []);
  await page.getByText("The computer changed. Search again.", { exact: true }).waitFor();
}, { boot: true }));


test("search waits for delayed project navigation before opening the exact agent", () => fixture(async page => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.openedSearch = [];
    window.ark.searchSource({ id: "agents", async load(project, api) {
      return (await api.call("agent_contacts", { project: project.id })).filter(agent => agent.state !== "terminated")
        .map(agent => ({ id: agent.name, title: agent.name, description: agent.role }));
    }, open(entry, scoped) { window.openedSearch.push({ id: entry.id, location: scoped.location }); } });
  });
  await page.locator("#search-chats").click();
  const input = page.getByRole("combobox", { name: "Search chats and agents" });
  await page.locator('#chat-search-results[aria-busy="false"]').waitFor();
  await input.fill("Separate project specialist");
  await input.press("Enter");
  await page.waitForFunction(() => window.openedSearch.length === 1);
  assert.deepEqual(await page.evaluate(() => window.openedSearch), [{ id: "Archivist", location: { runtime: "fixture", project: "b", session: "chat-b" } }]);
}, { boot: true, searchProjects: true }));

test("New chat interception is shared by the sidebar + (no New agent handler) and keyboard and creates nothing on open", () => fixture(async (page, { calls }) => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.destinations = [];
    window.ark.newChat({ open({ project, location, createChat }) { window.destinations.push({ project, location }); window.createGeneral = createChat; } });
    let rejected = false;
    try { window.ark.newChat({open() {}}); } catch { rejected = true; }
    if (!rejected) throw Error("duplicate New chat handler accepted");
  });
  await page.locator("#new-agent").click();
  await page.waitForFunction(() => window.destinations.length === 1);
  await page.keyboard.press(process.platform === "darwin" ? "Meta+n" : "Control+n");
  await page.waitForFunction(() => window.destinations.length === 2);
  const destinations = await page.evaluate(() => window.destinations);
  assert.equal(destinations[0].project, "a");
  assert.equal(destinations[1].project, "a");
  assert.equal(destinations[0].location.runtime, "fixture");
  await page.evaluate(() => window.ark.command("/new"));
  await page.waitForFunction(() => window.destinations.length === 3);
  await page.keyboard.press(process.platform === "darwin" ? "Meta+k" : "Control+k");
  await page.getByRole("option", { name: /New chat/ }).click();
  await page.waitForFunction(() => window.destinations.length === 4);
  assert.equal(calls.filter(call => call.method === "new_session").length, 0);
}, { boot: true }));


test("New chat waits for the requested project's observed origin", () => fixture(async page => {
  await page.locator("#app.ready").waitFor();
  await page.evaluate(() => {
    window.destinations = [];
    window.ark.newChat({ open({ project, location }) { window.destinations.push({ project, location }); } });
  });
  await page.evaluate(() => window.ark.newSession("b"));
  await page.waitForFunction(() => window.destinations.length === 1);
  assert.deepEqual(await page.evaluate(() => window.destinations), [{ project: "b", location: {runtime:"fixture", project:"b", session:"chat-b"} }]);
}, { boot: true, searchProjects: true }));


test("New chat waits for initial readiness before resolving its destination", () => fixture(async (page, { calls, releaseHello }) => {
  await page.waitForFunction(() => window.ark?.newChat);
  await page.evaluate(() => {
    window.destinations = [];
    window.ark.newChat({ open({ location }) { window.destinations.push(location); } });
    void window.ark.newSession();
  });
  assert.deepEqual(await page.evaluate(() => window.destinations), []);
  releaseHello();
  await page.waitForFunction(() => window.destinations.length === 1);
  assert.deepEqual(await page.evaluate(() => window.destinations), [{ runtime: "fixture", project: "a", session: "chat-a" }]);
  assert.equal(calls.filter(call => call.method === "new_session").length, 0);
}, { boot: true, holdHello: true }));


test("project panel keys do not remove the opening chat from resource authorization", () => fixture(async (page, { calls, settle }) => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  await page.evaluate(() => {
    window.ark.panel({ id: "scoped-test", title: "Project details", projectScoped: true, render(el, _params, api) {
      window.panelLocation = api.location;
      el.append(api.h("div", "Project details"));
      window.panelRead = api.call("agents");
      api.setPanelTitle("scoped-test", "Captured project");
      return {};
    } });
    window.ark.openPanel("scoped-test");
  });
  await page.evaluate(() => window.panelRead);
  assert.deepEqual(await page.evaluate(() => window.panelLocation), {runtime:"fixture",project:"a",session:""});
  await settle();
  const read = calls.find(call => call.method === "resource" && call.params.operation.kind === "agents");
  assert.equal(read.params.ref.location.session, "chat-a");
}, { boot: true }));

test("project focus follows only a chat on the same runtime and project", () => fixture(async (page, { calls, settle }) => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  const before = calls.filter(call => call.method === "resource" && call.params.operation.kind === "focus").length;
  await page.evaluate(async () => {
    const focus = { surface: { kind_tag: "custom", title: "Fixture" } };
    await window.ark.at({ runtime: "other-runtime", project: "a", session: "" }).setFocus(focus);
  });
  await settle();
  assert.equal(calls.filter(call => call.method === "resource" && call.params.operation.kind === "focus").length, before);
  await page.evaluate(async () => {
    const focus = { surface: { kind_tag: "custom", title: "Fixture" } };
    await window.ark.at({ runtime: "fixture", project: "a", session: "" }).setFocus(focus);
  });
  await settle();
  const focusCalls = calls.filter(call => call.method === "resource" && call.params.operation.kind === "focus");
  assert.equal(focusCalls.length, before + 1);
  assert.deepEqual(focusCalls.at(-1).params.ref.location, { runtime: "fixture", project: "a", session: "chat-a" });
}, { boot: true }));


test("focus writes for one chat are ordered despite a delayed response", () => fixture(async page => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  let releaseFirst, firstSeen;
  const firstStarted = new Promise(resolve => { firstSeen = resolve; });
  let secondStarted = false, applied = null;
  await page.route('**/rpc', async route => {
    const body = route.request().postDataJSON();
    const title = body?.params?.operation?.input?.focus?.surface?.title;
    if (body.method !== 'resource' || body.params?.operation?.kind !== 'focus' || !['First', 'Second'].includes(title)) return route.fallback();
    if (title === 'First') {
      firstSeen();
      await new Promise(resolve => { releaseFirst = resolve; });
    } else secondStarted = true;
    applied = title;
    await route.fulfill({ json: { ok: true, result: null } });
  });
  await page.evaluate(() => {
    const api = window.ark.at({ runtime: 'fixture', project: 'a', session: '' });
    window.__focusFirst = api.setFocus({ surface: { kind_tag: 'custom', title: 'First' } });
    const sameChat = window.ark.at({ session: 'chat-a', project: 'a', runtime: 'fixture' });
    window.__focusSecond = sameChat.setFocus({ surface: { kind_tag: 'custom', title: 'Second' } });
  });
  await firstStarted;
  await new Promise(resolve => setTimeout(resolve, 100));
  const ordered = !secondStarted;
  releaseFirst();
  await page.evaluate(() => Promise.all([window.__focusFirst, window.__focusSecond]));
  assert.equal(ordered, true, 'the newer focus was dispatched before the earlier write completed');
  assert.equal(applied, 'Second');
}, { boot: true }));

test("chat tab context actions reflect whether rename can run", () => fixture(async page => {
  await page.evaluate(() => window.ark.openSession("chat-b"));
  const active=page.locator('.chat-tab-item[data-session="chat-b"] .chat-tab');
  await active.click({button:'right'});
  assert.equal(await page.getByRole('menuitem',{name:'Rename chat',exact:true}).isEnabled(),true);
  await page.keyboard.press('Escape');
  await page.locator('.chat-tab-item[data-session="chat-a"] .chat-tab').click({button:'right'});
  assert.equal(await page.getByRole('menuitem',{name:'Rename chat',exact:true}).isDisabled(),true);
  await page.keyboard.press('Escape');
  assert.equal(await page.locator('.chat-tab-close').first().evaluate(el=>el.getBoundingClientRect().width),24);
}, {boot:true}));

test('a project panel can retain its draft before its tab is removed', () => fixture(async page => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  await page.evaluate(() => {
    window.__closeCalls = 0; window.__disposed = 0; window.__releaseClose = null;
    window.ark.panel({ id: 'draft-close-test', title: 'Draft close test', projectScoped: true, render(el) {
      el.textContent = 'Unsaved work';
      return { beforeRemove() {
        window.__closeCalls++;
        return new Promise(resolve => { window.__releaseClose = value => { window.__releaseClose = null; resolve(value); }; });
      }, dispose() { window.__disposed++; } };
    } });
    window.ark.openPanel('draft-close-test');
  });
  const tab = page.locator('.pane-tab-item', { hasText: 'Draft close test' });
  await tab.locator('.pane-tab-close').click();
  await page.waitForFunction(() => window.__closeCalls === 1);
  assert.equal(await tab.count(), 1);
  assert.equal(await page.evaluate(() => window.__disposed), 0);
  await page.evaluate(() => { void window.ark.closePanel('draft-close-test'); });
  assert.equal(await page.evaluate(() => window.__closeCalls), 1);
  await page.evaluate(() => window.__releaseClose(false));
  await page.waitForFunction(() => window.__releaseClose === null);
  assert.equal(await tab.count(), 1);
  await tab.locator('.pane-tab-close').click();
  await page.waitForFunction(() => window.__closeCalls === 2);
  await page.evaluate(() => window.__releaseClose(true));
  await tab.waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => window.__disposed), 1);
}, { boot: true }));

test('a failed draft handoff is hidden across project switches and remains recoverable', () => fixture(async page => {
  await page.evaluate(async () => {
    const { Pane } = await import('./js/pane.js');
    window.__pane = new Pane(document.getElementById('app'), { setFocus() {}, toast() {} });
    window.__disposed = 0; window.__resolve = null;
    window.__pane.reset('a');
    window.__pane.openTab('draft', 'Draft A', 'box', el => {
      el.textContent = 'Private project A draft';
      return { beforeRemove: () => new Promise(resolve => { window.__resolve = resolve; }), dispose: () => { window.__disposed++; } };
    }, {}, { projectScoped: true, project: 'a' });
    window.__pane.reset('b');
  });
  await page.waitForFunction(() => !!window.__resolve);
  assert.equal(await page.locator('.pane-tab-item', { hasText: 'Draft A' }).count(), 0);
  assert.equal(await page.locator('.pane-view[data-key="draft"]').evaluate(el => el.hidden && el.inert), true);
  await page.evaluate(() => window.__resolve(false));
  await page.waitForFunction(() => !window.__pane.tabs.get('draft').removing);
  assert.equal(await page.evaluate(() => window.__disposed), 0);
  await page.evaluate(() => { window.__pane.reset('a'); window.__pane.open(); });
  assert.equal(await page.locator('.pane-tab-item', { hasText: 'Draft A' }).count(), 1);
  assert.equal(await page.locator('.pane-view[data-key="draft"]').textContent(), 'Private project A draft');
}, { boot: true }));

test('real project navigation hides a pending project panel until its handoff resolves', () => fixture(async page => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  await page.evaluate(() => {
    window.__handoff = null; window.__disposed = 0;
    window.ark.panel({ id: 'project-handoff-test', title: 'Project draft', projectScoped: true, render(el) {
      el.textContent = 'Draft from A';
      return { beforeRemove: () => new Promise(resolve => { window.__handoff = resolve; }), dispose: () => { window.__disposed++; } };
    } });
    window.ark.at({ ...window.ark.summary().location }).openPanel('project-handoff-test');
    void window.ark.openProject('b');
  });
  await page.waitForFunction(() => window.ark.summary()?.location?.project === 'b' && !!window.__handoff);
  assert.equal(await page.locator('.pane-tab-item', { hasText: 'Project draft' }).count(), 0);
  assert.equal(await page.getByText('Draft from A', { exact: true }).isVisible(), false);
  await page.evaluate(() => window.__handoff(false));
  await page.waitForTimeout(20);
  assert.equal(await page.evaluate(() => window.__disposed), 0);
  assert.equal(await page.locator('.pane-tab-item', { hasText: 'Project draft' }).count(), 0);
}, { boot: true, searchProjects: true }));

test('switching back waits for an in-flight handoff before reopening the panel', () => fixture(async page => {
  await page.evaluate(async () => {
    const { Pane } = await import('./js/pane.js');
    window.__pane = new Pane(document.getElementById('app'), { setFocus() {}, toast() {} });
    window.__builds = 0; window.__disposed = 0; window.__resolve = null;
    window.__make = el => {
      window.__builds++; el.textContent = `Draft instance ${window.__builds}`;
      return { beforeRemove: () => new Promise(resolve => { window.__resolve = resolve; }), dispose: () => { window.__disposed++; } };
    };
    window.__pane.reset('a');
    window.__pane.openTab('draft', 'Draft A', 'box', window.__make, {}, { projectScoped: true, project: 'a' });
    window.__pane.reset('b');
  });
  await page.waitForFunction(() => !!window.__resolve);
  await page.evaluate(() => window.__pane.open());
  assert.equal(await page.evaluate(() => window.__pane.isOpen), false);
  await page.evaluate(() => {
    window.__pane.reset('a');
    window.__pane.openTab('draft', 'Draft A', 'box', window.__make, {}, { projectScoped: true, project: 'a' });
  });
  assert.equal(await page.evaluate(() => window.__builds), 1);
  assert.equal(await page.locator('.pane-tab-item', { hasText: 'Draft A' }).count(), 0);
  await page.evaluate(() => window.__resolve(true));
  await page.waitForFunction(() => window.__builds === 2);
  assert.equal(await page.evaluate(() => window.__disposed), 1);
  assert.equal(await page.locator('.pane-view[data-key="draft"]').textContent(), 'Draft instance 2');
}, { boot: true }));

test('a rejected panel handoff leaves the draft and reports the failure', () => fixture(async page => {
  await page.waitForFunction(() => window.ark?.summary()?.location);
  await page.evaluate(() => {
    window.__attempts = 0; window.__disposed = 0;
    window.ark.panel({ id: 'draft-error-test', title: 'Draft error', projectScoped: true, render(el) {
      el.textContent = 'Unsaved work';
      return { beforeRemove() { if (++window.__attempts === 1) throw new Error('Storage unavailable'); return true; },
        dispose() { window.__disposed++; } };
    } });
    window.ark.openPanel('draft-error-test');
  });
  const tab = page.locator('.pane-tab-item', { hasText: 'Draft error' });
  await tab.locator('.pane-tab-close').click();
  await page.getByText(/Could not close Draft error.*Storage unavailable/).waitFor();
  assert.equal(await tab.count(), 1);
  assert.equal(await page.evaluate(() => window.__disposed), 0);
  await tab.locator('.pane-tab-close').click();
  await tab.waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => window.__disposed), 1);
}, { boot: true }));

test("an expanded resource keeps the same composer and restores its draft and history", () => fixture(async (page, { calls, settle }) => {
  await settle();
  await page.evaluate(() => {
    window.__workspaceInput = document.getElementById('input');
    window.ark.panel({ id: 'shared-workspace', title: 'Shared work', projectScoped: true, expandable: true, fill: true,
      render(el, _params, api) {
        window.__workspaceApi = api;
        el.textContent = 'Project-owned work';
        return { focus: () => ({ surface: { kind_tag: 'document', title: 'Shared document' } }) };
      } });
    window.ark.openPanel('shared-workspace');
  });
  await page.locator('#input').fill('Keep this draft');
  await page.getByRole('button', { name: 'Expand workspace', exact: true }).click();
  const floating = page.locator('.workspace-conversation');
  assert.equal(await floating.locator('#input').inputValue(), 'Keep this draft');
  assert.equal(await page.evaluate(() => window.__workspaceInput === document.querySelector('.workspace-conversation #input')), true);
  assert.equal(await page.evaluate(() => window.__workspaceApi.location.session), '');
  assert.equal(await page.locator('.main').isVisible(), false);
  assert.equal(await floating.locator('.scroller').isVisible(), false);
  assert.equal(await floating.locator('#input').isVisible(), false);
  assert.ok((await floating.boundingBox()).height <= 45);
  const handle = page.getByRole('button', { name: 'Expand composer', exact: true });
  const beforeDrag = await floating.boundingBox();
  const grip = await handle.boundingBox();
  await page.mouse.move(grip.x + 40, grip.y + 15);
  await page.mouse.down();
  await page.mouse.move(grip.x - 100, grip.y - 85, { steps: 8 });
  await page.mouse.up();
  const afterDrag = await floating.boundingBox();
  assert.ok(afterDrag.x < beforeDrag.x - 80 && afterDrag.y < beforeDrag.y - 50);
  assert.equal(await floating.locator('#input').isVisible(), false);
  await handle.click();
  assert.equal(await floating.locator('#input').isVisible(), true);
  assert.equal(await floating.locator('#input').evaluate(el => el === document.activeElement), true);
  await page.getByRole('button', { name: 'Minimize composer', exact: true }).click();
  assert.equal(await floating.locator('#input').isVisible(), false);
  assert.equal(await floating.locator('#input').inputValue(), 'Keep this draft');
  await page.getByRole('button', { name: 'Expand composer', exact: true }).click();
  await page.getByRole('button', { name: 'Show conversation', exact: true }).focus();
  for (let step = 0; step < 25; step++) {
    await page.keyboard.press('Alt+ArrowRight');
    await page.keyboard.press('Alt+ArrowDown');
  }
  await page.setViewportSize({width: 1000, height: 700});
  await page.waitForFunction(() => {
    const dock = document.querySelector('.workspace-conversation').getBoundingClientRect();
    const pane = document.getElementById('pane').getBoundingClientRect();
    return dock.right <= pane.right && dock.bottom <= pane.bottom;
  });
  await page.setViewportSize({width: 900, height: 600});
  await page.waitForFunction(() => parseFloat(getComputedStyle(document.getElementById('pane-body')).paddingBottom) > 60);
  const clearance = await page.evaluate(() => {
    const body = document.getElementById('pane-body');
    const view = [...body.querySelectorAll('.pane-view')].find(view => !view.hidden);
    return {bottom: view.getBoundingClientRect().bottom, composer: document.querySelector('.workspace-conversation').getBoundingClientRect().top};
  });
  const bounded = await floating.boundingBox();
  assert.ok(bounded.x >= 0 && bounded.y >= 0 && bounded.x + bounded.width <= 900 && bounded.y + bounded.height <= 600, JSON.stringify({clearance, bounded}));
  await page.getByRole('button', { name: 'Show conversation', exact: true }).click();
  assert.equal(await floating.locator('.scroller').isVisible(), true);
  await page.getByRole('button', { name: 'Hide conversation', exact: true }).click();
  await floating.locator('#input').fill('Please edit the document');
  await floating.locator('#input').press('Enter');
  await settle();
  assert.equal(calls.filter(call => call.method === 'send').at(-1)?.params.text, 'Please edit the document');
  await floating.locator('#input').fill('Retained after restore');
  await page.getByRole('button', { name: 'Restore workspace', exact: true }).click();
  assert.equal(await floating.count(), 0);
  assert.equal(await page.locator('#pane-body').evaluate(body => body.style.getPropertyValue('--workspace-composer-inset')), '');
  assert.equal(await page.locator('.main #input').inputValue(), 'Retained after restore');
  assert.equal(await page.evaluate(() => window.__workspaceInput === document.querySelector('.main #input')), true);
}, { boot: true, rpcOverrides: { send: () => null } }));

test("expanded project work follows the active chat and restores on project change", () => fixture(async (page, { calls, settle }) => {
  await settle();
  await page.evaluate(() => {
    window.ark.panel({ id: 'project-workspace', title: 'Shared work', projectScoped: true, expandable: true,
      render(el) { el.textContent = 'Shared project document'; return { focus: () => ({ surface: { kind_tag: 'document', title: 'Project work' } }) }; } });
    window.ark.openPanel('project-workspace');
  });
  await page.getByRole('button', { name: 'Expand workspace', exact: true }).click();
  await page.evaluate(() => window.ark.openSession('chat-b'));
  await settle();
  await page.waitForFunction(() => document.querySelector('.workspace-conversation-title')?.textContent === 'Second draft');
  const before = calls.length;
  await page.evaluate(() => window.ark.reportFocus());
  await settle();
  const focus = calls.slice(before).filter(call => call.method === 'resource' && call.params.operation?.kind === 'focus');
  assert.ok(focus.length > 0);
  assert.ok(focus.every(call => call.params.ref.location.session === 'chat-b'));
  await page.evaluate(() => window.ark.openProject('b'));
  await settle();
  await page.waitForFunction(() => document.getElementById('app').dataset.paneExpanded === 'false');
  assert.equal(await page.locator('.workspace-conversation').count(), 0);
  assert.equal(await page.locator('.main #input').count(), 1);
}, { boot: true, searchProjects: true }));

test("switching cached conversations clears the outgoing resource selection", () => fixture(async (page, { settle }) => {
  await settle();
  await page.evaluate(() => {
    window.__recipientFocus = null;
    const setFocus = value => { window.__recipientFocus = value; };
    window.ark.view({id: 'recipient-fixture', title: 'Recipient', resourceScoped: true,
      render(el, _params, api) {
        const surface = document.createElement('div'); surface.className = 'chat-view';
        const input = document.createElement('textarea'); input.setAttribute('aria-label', 'Recipient message');
        surface.append(input); el.append(surface);
        return {conversation: () => ({surface, title: 'Selected agent', location: {...api.location, session: ''}, focus: setFocus})};
      }});
    window.ark.panel({id: 'selection-fixture', title: 'Document', projectScoped: true, expandable: true,
      render(el) { el.textContent = 'Document'; return {focus: () => ({surface: {kind_tag: 'document', title: 'Visible selection'}})}; }});
    window.ark.openView('recipient-fixture');
    window.ark.openPanel('selection-fixture');
  });
  await page.getByRole('button', {name: 'Expand workspace', exact: true}).click();
  assert.equal(await page.locator('.workspace-conversation').getByLabel('Recipient message').count(), 1);
  assert.equal(await page.evaluate(() => window.__recipientFocus?.surface?.title), 'Visible selection');
  await page.evaluate(() => window.ark.openView('ark:settings'));
  await settle();
  assert.equal(await page.evaluate(() => window.__recipientFocus), null);
  await page.getByRole('button', {name: 'Close', exact: true}).click();
  await settle();
  await page.evaluate(() => window.ark.openView('recipient-fixture'));
  await settle();
  assert.equal(await page.evaluate(() => window.__recipientFocus), null);
}, {boot: true}));

test('a main workspace floats the original composer and guards navigation', () => fixture(async (page, {settle, calls}) => {
  await page.evaluate(async () => {
    window.__workspaceOriginal = document.getElementById('input');
    window.__workspaceCanLeave = false;
    window.ark.view({id: 'main-workspace', title: 'Canvas', resourceScoped: true, workspace: true,
      render(el) { el.textContent = 'Project canvas'; return {focus: () => ({surface: {kind_tag: 'document', title: 'Selected content'}}),
        beforeRemove: async () => { if (!window.__workspaceCanLeave) throw Error('Retain the draft first'); return true; }}; }});
    await window.ark.openView('main-workspace');
  });
  await settle();
  const workspace = page.locator('.view[data-view="main-workspace"]');
  await workspace.getByRole('button', {name:'Expand composer', exact:true}).click();
  assert.equal(await workspace.locator('#input').evaluate(input => input === window.__workspaceOriginal), true);
  await workspace.locator('#input').fill('Keep this draft');
  await page.evaluate(() => window.ark.openView('ark:settings'));
  assert.equal(await page.locator('#app').getAttribute('data-view'), 'main-workspace');
  assert.equal(await workspace.locator('#input').inputValue(), 'Keep this draft');
  const before = calls.filter(call => call.method === 'open_session').length;
  await page.evaluate(() => window.ark.openSession('chat-b'));
  assert.equal(calls.filter(call => call.method === 'open_session').length, before);
  await page.evaluate(async () => {window.__workspaceCanLeave = true; await window.ark.showConversation();});
  await settle();
  assert.equal(await page.locator('.workspace-conversation').count(), 0);
  assert.equal(await page.locator('.main #input').inputValue(), 'Keep this draft');
  assert.equal(await page.locator('.main #input').evaluate(input => input === window.__workspaceOriginal), true);
}, {boot:true}));

test('the latest navigation wins after a delayed workspace handoff', () => fixture(async (page, {settle}) => {
  await page.evaluate(async () => {
    const handoff = new Promise(resolve => {window.__releaseWorkspace = resolve;});
    window.ark.view({id:'delayed-workspace',title:'Canvas',workspace:true,resourceScoped:true,
      render(el) {el.textContent='Canvas'; return {beforeRemove:()=>handoff};}});
    await window.ark.openView('delayed-workspace');
    window.__oldNavigation = window.ark.openView('ark:settings');
    window.__newNavigation = window.ark.showConversation();
    window.__releaseWorkspace(true);
    await Promise.all([window.__oldNavigation, window.__newNavigation]);
  });
  await settle();
  assert.equal(await page.locator('#app').getAttribute('data-view'), null);
  assert.equal(await page.locator('.workspace-conversation').count(), 0);
}, {boot:true}));

test('a forced project change retains a refused workspace until its project returns', () => fixture(async (page, {settle, emit}) => {
  await page.evaluate(async () => {
    window.__retiredDisposals=0;window.__allowRetire=false;
    window.ark.view({id:'retained-workspace',title:'Canvas',workspace:true,resourceScoped:true,
      render(el) { const input=document.createElement('textarea');input.setAttribute('aria-label','Unsaved style');input.value='Keep this style';el.append(input);
        return {beforeRemove:async()=>{if(!window.__allowRetire)throw Error('Style is unfinished');return true;},dispose(){window.__retiredDisposals++;}};}});
    await window.ark.openView('retained-workspace');
    window.__originalWorkspace=document.querySelector('.view[data-view="retained-workspace"]');
  });
  const original = await page.evaluate(()=>window.ark.summary());
  const next=structuredClone(original);next.project={id:'b',name:'Writing'};next.session={id:'chat-b',title:'Second draft',turns:1};next.location={runtime:'fixture',project:'b',session:'chat-b'};
  await emit({kind:'summary',summary:next});await settle();
  assert.equal(await page.evaluate(()=>window.__retiredDisposals),0);
  assert.equal(await page.evaluate(()=>window.__originalWorkspace.isConnected && window.__originalWorkspace.hidden && window.__originalWorkspace.inert),true);
  await emit({kind:'summary',summary:original});await settle();
  await page.evaluate(()=>window.ark.openView('retained-workspace'));
  assert.equal(await page.getByLabel('Unsaved style',{exact:true}).inputValue(),'Keep this style');
  assert.equal(await page.evaluate(()=>window.__originalWorkspace===document.querySelector('.view[data-view="retained-workspace"]:not([hidden])')),true);
  await page.evaluate(async()=>{window.__allowRetire=true;await window.ark.showConversation();});
}, {boot:true,searchProjects:true}));


test('a detached Work approval remains clickable over the transparent workspace', () => fixture(async page => {
  await page.evaluate(async () => {
    const {Pane}=await import('./js/pane.js');
    const surface=document.createElement('div');surface.innerHTML='<form class="composer"><textarea></textarea></form>';
    const decisions=document.createElement('div');decisions.className='work-questions';
    const question=document.createElement('div');question.className='work-question';
    const choice=document.createElement('button');choice.textContent='Allow this change';choice.onclick=()=>{window.__approved=true;question.remove();};question.append(choice);decisions.append(question);
    const home=document.createElement('section');home.append(surface,decisions);document.body.append(home);
    const canvas=document.createElement('section');canvas.style.cssText='position:fixed;inset:60px 0 0;background:white';document.body.append(canvas);
    window.__approvalPane=new Pane(document.getElementById('app'),{setFocus(){}},{conversation:()=>({surface,attention:decisions,title:'Work'})});
    window.__approvalPane.setWorkspace(canvas);
    window.__approvalHome=home;
  });
  const dock=page.locator('.workspace-conversation');
  assert.equal(await dock.getAttribute('data-composer'),'expanded');
  await dock.getByRole('button',{name:'Allow this change',exact:true}).click();
  assert.equal(await page.evaluate(()=>window.__approved),true);
  await page.evaluate(()=>window.__approvalPane.restoreConversation());
  assert.equal(await page.evaluate(()=>window.__approvalHome.querySelector('.work-questions').classList.contains('workspace-conversation-attention')),false);
}));
