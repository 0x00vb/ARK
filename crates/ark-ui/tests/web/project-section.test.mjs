import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { chromium, webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
let browser;
before(async () => { browser = await (process.env.ARK_UI_BROWSER === "chromium" ? chromium : webkit).launch({ headless: true }); });
after(async () => { await browser?.close(); });

async function fixture(run, { boot = false, searchProjects = false } = {}) {
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
  const page = await context.newPage();
  page.setDefaultTimeout(5000);
  const errors = [];
  const calls = [], unexpected = [];
  let pending = 0;
  let events;
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
  const rpc = {
    hello: () => ({ summary, ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture" }),
    summary: () => summary,
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
        "project:schedules": [], "process:processes": [], "project:agents": [
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
      setTimeout(() => void events.fulfill({ contentType: "text/event-stream", body: `data: ${JSON.stringify({ kind: "summary", summary })}\n\n` }), 180);
      return null;
    },
    open_session: async params => {
      assert.equal(params.id, "chat-b");
      summary.session = { ...sessions[1] };
      summary.location.session = "chat-b";
      await events.fulfill({ contentType: "text/event-stream", body: `data: ${JSON.stringify({ kind: "summary", summary })}\n\n` });
      return null;
    },
  };
  page.on("pageerror", error => errors.push(error.message));
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
    if (boot && url.pathname === "/events") { events = route; return; }
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
    await page.goto("http://ark.test/index.html");
    if (!boot) await page.evaluate(async () => {
      const { Sidebar } = await import("./js/sidebar.js");
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
          await f.sidebar.refresh();
        },
        newSession() {}, openSettings() {}, toast() {},
      };
      f.api = installApi(api);
      f.sidebar = new Sidebar(api);
      f.register = (id = "items", label = "Saved items") => f.api.projectSection({ id, label, render(el, { project }, api) {
        f.events.push(["render", id, project.id, el.isConnected, api === f.api]);
        const button = document.createElement("button");
        button.className = "nav-row";
        button.textContent = `Saved item in ${project.name}`;
        button.onclick = () => api.openSession(`item-${project.id}`);
        el.append(button);
        return { dispose() { f.events.push(["dispose", id, project.id, el.isConnected]); } };
      } });
      await f.sidebar.refresh();
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
    await run(page, { calls, unexpected, errors, settle });
    assert.deepEqual(errors, [], "uncaught page errors");
    assert.deepEqual(unexpected, [], "unexpected fixture requests");
  } catch (error) {
    if (boot) console.error(JSON.stringify({ rpc: calls, unexpected, errors }, null, 2));
    throw error;
  } finally {
    await context.close();
  }
}

test("registration validates before mutation and signals a late project-tree refresh", () => fixture(async page => {
  const result = await page.evaluate(() => {
    const f = window.fixture, invalid = [null, {}, { id: "", label: "Items", render() {} },
      { id: 3, label: "Items", render() {} }, { id: "bad", label: " ", render() {} },
      { id: "bad", label: "Items", render: true }];
    const rejected = invalid.map(value => { try { f.api.projectSection(value); return false; } catch { return true; } });
    f.register();
    try { f.register(); rejected.push(false); } catch { rejected.push(true); }
    return { rejected, size: f.registry.projectSections.size, changed: f.changed };
  });
  assert.deepEqual(result, { rejected: Array(7).fill(true), size: 1, changed: [true] });
}));

test("actual frontend batches late sections without redrawing for unrelated extensions", () => fixture(async (page, { calls, unexpected, errors, settle }) => {
  await page.locator("#app.ready").waitFor();
  await page.locator('.project[data-id="a"] .project-row').waitFor();
  await settle();
  assert.deepEqual(unexpected, [], "frontend startup used only known fixture RPCs");
  assert.deepEqual(errors, [], "frontend startup completed without page errors");
  const projectReads = () => calls.filter(call => call.method === "projects").length;
  const initial = projectReads();
  await page.evaluate(() => {
    window.__sectionEvents = [];
    for (const id of ["first", "second"]) {
      window.ark.projectSection({ id, label: id, render(el, { project }, api) {
        window.__sectionEvents.push(["render", id, project.id, el.isConnected]);
        const button = api.h("button.nav-row", { type: "button", onclick: () => api.openSession("chat-b") }, "Open second draft");
        el.append(button);
        return { dispose() { window.__sectionEvents.push(["dispose", id, project.id, el.isConnected]); } };
      } });
      if (id === "first") window.ark.status({ id: "unrelated", render() {} });
    }
  });
  try {
    await page.getByRole("group", { name: "second", exact: true }).waitFor();
  } catch (error) {
    assert.fail(`Late project sections did not render: ${error.message}`);
  }
  await settle();
  assert.equal(projectReads() - initial, 1, "late sections must cause one project refresh");
  assert.deepEqual(await page.locator(".project-section").evaluateAll(nodes => nodes.map(node => node.dataset.id)), ["first", "second"]);
  if (process.env.ARK_UI_SHOTS) {
    await mkdir(process.env.ARK_UI_SHOTS, { recursive: true });
    await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, "frontend-project-section-1100.png"), animations: "disabled" });
  }
  await page.setViewportSize({ width: 390, height: 760 });
  await page.locator("#reveal-sidebar").click();
  await page.getByRole("group", { name: "second", exact: true }).waitFor({ state: "visible" });
  await page.waitForFunction(() => {
    const box = document.querySelector('.project-section[data-id="second"]').getBoundingClientRect();
    return box.left >= 0 && box.right <= innerWidth;
  });
  const narrow = await page.getByRole("group", { name: "second", exact: true }).boundingBox();
  assert.ok(narrow && narrow.x >= 0 && narrow.x + narrow.width <= 390, "the actual frontend drawer shows the section");
  if (process.env.ARK_UI_SHOTS) await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, "frontend-project-section-390.png"), animations: "disabled" });
  await page.setViewportSize({ width: 1100, height: 760 });
  const mounted = await page.evaluate(() => window.__sectionEvents);
  const beforeUnrelated = projectReads();
  await page.evaluate(() => window.ark.status({ id: "later-unrelated", render() {} }));
  await settle();
  assert.equal(projectReads(), beforeUnrelated, "unrelated registration must not refresh projects");
  assert.deepEqual(await page.evaluate(() => window.__sectionEvents), mounted);
  await page.locator('.project-row[aria-current="true"] .project-chev').click();
  await page.waitForFunction(() => !document.querySelector(".project-section"));
  await page.locator('.project-row[aria-current="true"] .project-chev').click();
  await page.getByRole("group", { name: "second", exact: true }).waitFor();
  const events = await page.evaluate(() => window.__sectionEvents);
  assert.deepEqual(events.map(event => event.slice(0, 2)), [
    ["render", "first"], ["render", "second"], ["dispose", "first"], ["dispose", "second"], ["render", "first"], ["render", "second"],
  ]);
  assert.ok(events.every(event => event[3]));
  await page.getByRole("button", { name: "Open second draft", exact: true }).first().press("Enter");
  await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
  assert.ok(calls.some(call => call.method === "open_session" && call.params.id === "chat-b"));
}, { boot: true }));

test("connected current-project sections precede chats, including empty chats", () => fixture(async page => {
  await page.evaluate(async () => { const f = window.fixture; f.register(); f.register("more", "More items"); f.sidebar.folds.b = true; await f.sidebar.refresh(); });
  assert.deepEqual(await page.locator('.project[data-id="a"] > *').evaluateAll(nodes => nodes.map(node => node.className)),
    ["project-row", "project-section", "project-section", "chats"]);
  assert.equal(await page.locator('.project[data-id="b"] .project-section').count(), 0);
  assert.equal(await page.getByRole("group", { name: "Saved items", exact: true }).count(), 1);
  assert.deepEqual(await page.evaluate(() => window.fixture.events), [["render", "items", "a", true, true], ["render", "more", "a", true, true]]);
  await page.evaluate(async () => { window.fixture.sessions.a = []; await window.fixture.sidebar.refresh(); });
  assert.equal(await page.locator('.project[data-id="a"] .project-section').count(), 2);
  assert.equal(await page.locator('.project[data-id="a"] .chats').count(), 0);
}));

test("fold, unfold, project switch and hide release attached mounts exactly once", () => fixture(async page => {
  await page.evaluate(async () => { window.fixture.register(); await window.fixture.sidebar.refresh(); });
  await page.locator('.project[data-id="a"] .project-chev').click();
  await page.waitForFunction(() => !document.querySelector(".project-section"));
  await page.locator('.project[data-id="a"] .project-chev').click();
  await page.waitForFunction(() => document.querySelector(".project-section"));
  await page.locator('.project[data-id="b"] .project-row').press("Enter");
  await page.waitForFunction(() => document.querySelector('.project[data-id="b"] .project-section'));
  assert.equal(await page.locator('.project[data-id="a"] .project-section').count(), 0);
  await page.evaluate(() => window.fixture.sidebar.hide(window.fixture.projects[1]));
  await page.waitForFunction(() => !document.querySelector('.project[data-id="b"]'));
  const events = await page.evaluate(() => window.fixture.events);
  assert.deepEqual(events.map(event => event.slice(0, 4)), [
    ["render", "items", "a", true], ["dispose", "items", "a", true],
    ["render", "items", "a", true], ["dispose", "items", "a", true],
    ["render", "items", "b", true], ["dispose", "items", "b", true],
  ]);
}));

test("repeated refresh disposes only the previous mount and preserves keyboard navigation", () => fixture(async page => {
  await page.evaluate(async () => { const f = window.fixture; f.register(); await f.sidebar.refresh(); await f.sidebar.refresh(); await f.sidebar.refresh(); });
  await page.getByRole("button", { name: "Saved item in Notes", exact: true }).press("Enter");
  await page.locator('.session[data-id="chat-a"]').press("Enter");
  assert.deepEqual(await page.evaluate(() => window.fixture.opened), ["item-a", "chat-a"]);
  const events = await page.evaluate(() => window.fixture.events);
  assert.equal(events.filter(event => event[0] === "render").length, 3);
  assert.equal(events.filter(event => event[0] === "dispose").length, 2);
  assert.ok(events.every(event => event[3]));
}));

test("renderer and disposer failures do not block other sections or chats", () => fixture(async page => {
  await page.evaluate(async () => {
    const f = window.fixture;
    f.api.projectSection({ id: "throw-render", label: "Failed", render() { throw Error("render failed"); } });
    f.api.projectSection({ id: "throw-dispose", label: "Temporary", render() { return { dispose() { throw Error("dispose failed"); } }; } });
    f.register();
    await f.sidebar.refresh(); await f.sidebar.refresh();
  });
  assert.equal(await page.getByRole("group", { name: "Failed", exact: true }).count(), 0);
  assert.equal(await page.getByRole("group", { name: "Saved items", exact: true }).count(), 1);
  await page.locator('.session[data-id="chat-a"]').click();
  assert.deepEqual(await page.evaluate(() => ({ errors: window.fixture.errors, opened: window.fixture.opened })),
    { errors: ["Error: render failed", "Error: dispose failed", "Error: render failed"], opened: ["chat-a"] });
}));

test("an old projects response cannot change current, folds or hidden labels", () => fixture(async page => {
  const result = await page.evaluate(async () => {
    const f = window.fixture;
    f.register(); await f.sidebar.refresh();
    f.sidebar.hidden.set("hidden", "Current hidden label");
    let release;
    f.sidebar.api.call = method => {
      if (method !== "projects") throw Error("stale refresh requested sessions");
      return new Promise(resolve => { release = resolve; });
    };
    const old = f.sidebar.refresh();
    f.sidebar.api.call = async (method, params) => method === "projects"
      ? [{ id: "b", name: "Writing", current: true }, { id: "hidden", name: "Current hidden label", current: false }]
      : f.sessions[params.project] || [];
    await f.sidebar.refresh();
    const before = JSON.stringify({ current: f.sidebar.current, folds: f.sidebar.folds, hidden: [...f.sidebar.hidden], events: f.events });
    release([{ id: "a", name: "Old project", current: true }, { id: "hidden", name: "Stale label", current: false }]);
    await old;
    return { before, after: JSON.stringify({ current: f.sidebar.current, folds: f.sidebar.folds, hidden: [...f.sidebar.hidden], events: f.events }) };
  });
  assert.equal(result.after, result.before);
}));

test("an old sessions response cannot dispose or replace the newer mounts", () => fixture(async page => {
  const result = await page.evaluate(async () => {
    const f = window.fixture;
    f.register(); await f.sidebar.refresh();
    let release;
    f.sidebar.api.call = async method => method === "projects" ? structuredClone(f.projects)
      : new Promise(resolve => { release = resolve; });
    const old = f.sidebar.refresh();
    await new Promise(resolve => queueMicrotask(resolve));
    f.sidebar.api.call = async (method, params) => method === "projects" ? structuredClone(f.projects) : f.sessions[params.project];
    await f.sidebar.refresh();
    const before = JSON.stringify(f.events);
    release([]); await old;
    return { before, after: JSON.stringify(f.events), chats: document.querySelectorAll(".chats .session").length };
  });
  assert.equal(result.after, result.before);
  assert.equal(result.chats, 1);
}));

test("failed project reads preserve the attached working section", () => fixture(async page => {
  const result = await page.evaluate(async () => {
    const f = window.fixture;
    f.register(); await f.sidebar.refresh();
    const before = JSON.stringify(f.events), el = document.querySelector(".project-section");
    f.sidebar.api.call = async () => { throw Error("Offline"); };
    await f.sidebar.refresh();
    return { before, after: JSON.stringify(f.events), same: document.querySelector(".project-section") === el, attached: el.isConnected };
  });
  assert.equal(result.before, result.after);
  assert.equal(result.same, true);
  assert.equal(result.attached, true);
}));

test("project content fits light, dark and narrow sidebar layouts", () => fixture(async page => {
  await page.evaluate(async () => { window.fixture.register(); await window.fixture.sidebar.refresh(); });
  for (const [theme, width] of [["light", 1100], ["dark", 1100], ["light", 390]]) {
    await page.setViewportSize({ width, height: 760 });
    await page.evaluate(({ theme, width }) => {
      document.documentElement.dataset.theme = theme;
      const app = document.querySelector("#app");
      app.dataset.compact = String(width <= 760);
      app.dataset.drawer = "open";
    }, { theme, width });
    await page.getByRole("button", { name: "Saved item in Notes", exact: true }).click();
    const boxes = await page.locator(".project-section").evaluateAll(nodes => nodes.map(node => {
      const rect = node.getBoundingClientRect();
      return { width: node.clientWidth, scroll: node.scrollWidth, connected: node.isConnected,
        inView: rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight,
        visible: getComputedStyle(node).visibility === "visible" };
    }));
    assert.ok(boxes.every(box => box.connected && box.visible && box.inView && box.width > 0 && box.scroll <= box.width));
    if (process.env.ARK_UI_SHOTS) {
      await mkdir(process.env.ARK_UI_SHOTS, { recursive: true });
      await page.screenshot({ path: resolve(process.env.ARK_UI_SHOTS, `project-section-${theme}-${width}.png`), animations: "disabled" });
    }
  }
}));


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
