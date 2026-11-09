// The agent-first sidebar, the title's recent chats and /history (ADR 0110), on the real page
// with a mocked host that answers with the shapes of the shared contract.

import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { readFile, mkdir } from "node:fs/promises";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
const shots = process.env.ARK_UI_SHOTS;
let browser;
before(async () => { browser = await webkit.launch({ headless: true }); });
after(async () => { await browser?.close(); });

const HOUR = 3600_000;
const now = Date.now();
const iso = (ago) => new Date(now - ago).toISOString();

export function agentRows(count, { projects = ["a", "b", "c", "d", "e", "f"], twin = true, asking = [], working = [], program = [] } = {}) {
  const names = ["Mia", "Writer", "Editor", "Scout", "Planner", "Archivist", "Builder", "Critic", "Lead", "Reader", "Tester", "Ops", "Finance", "Legal", "Design", "Support",
    "Research", "Data", "Sales", "Ads", "Docs", "Translate", "Review", "Release", "Triage", "Notes", "Budget", "Hiring", "Travel", "Garden"];
  const rows = Array.from({ length: count }, (_, i) => {
    const project = projects[i % projects.length];
    const name = names[i];
    return { project, project_name: `Folder ${project.toUpperCase()}`, name, title: "", role: "", state: working.includes(name) ? "running" : "idle", asking: asking.includes(name),
      running: working.includes(name) ? [`task-${name}`] : [], updated_ms: now - i * HOUR, preview: `Latest with ${name}`, program: program.includes(name) };
  });
  if (twin && count > 3) rows.push({ ...rows[0], project: projects[1], project_name: `Folder ${projects[1].toUpperCase()}`, updated_ms: now - 30 * HOUR, preview: "Other Mia" });
  return rows;
}

async function fixture(run, { agentView = true, agents = agentRows(4), general = null, conversations = {}, pins = null, hidden = null, failAgents = false, holdAgents = false, width = 1180, coarse = false, theme = null, board = null } = {}) {
  const context = await browser.newContext({ viewport: { width, height: 800 }, hasTouch: coarse });
  if (pins || hidden) await context.addInitScript(([pins, hidden]) => {
    if (pins) localStorage.setItem("ark-pins:fixture", JSON.stringify(pins));
    if (hidden) localStorage.setItem("ark-hidden-projects", JSON.stringify(hidden));
  }, [pins, hidden]);
  if (theme) await context.addInitScript(mode => localStorage.setItem("ark-theme", mode), theme);
  const page = await context.newPage();
  page.setDefaultTimeout(5000);
  const errors = [], calls = [], unexpected = [];
  let stream = null;
  const emit = async (message) => { stream.write(`data: ${JSON.stringify(message)}\n\n`); };
  const projects = [["a", "Notes"], ["b", "Writing"], ["c", "Garden"], ["d", "Studio"], ["e", "Travel"], ["f", "Home"]].map(([id, name], i) => ({ id, name, workspace: `/work/${name.toLowerCase()}`, current: i === 0 }));
  const summary = {
    name: "ARK", tagline: "", model: "mock", grants: [],
    project: { id: "a", name: "Notes", workspace: "/work/notes" },
    session: { id: "chat-a", title: "First draft", turns: 2 },
    location: { runtime: "fixture", project: "a", session: "chat-a" },
    execution_mode: "auto", execution_mode_revision: 0,
  };
  const generalChats = general || [
    { id: "chat-a", title: "First draft", turns: 2, updated_at: iso(2 * HOUR), project: "a", project_name: "Notes" },
    { id: "chat-b", title: "Garden plan", turns: 3, updated_at: iso(HOUR), project: "b", project_name: "Writing" },
  ];
  let releaseAgents = null;
  const control = { holdConversations: null, failConversations: false, agentsCalls: 0, failAgents, holdAgents: holdAgents ? new Promise((done) => { releaseAgents = done; }) : null };
  control.release = () => { control.holdAgents = null; releaseAgents?.(); };
  const switchTo = (project, session) => {
    const p = projects.find((entry) => entry.id === project);
    projects.forEach((entry) => { entry.current = entry.id === project; });
    summary.project = { id: p.id, name: p.name, workspace: p.workspace };
    summary.session = { id: session, title: generalChats.find((chat) => chat.id === session)?.title || "", turns: 1 };
    summary.location = { runtime: "fixture", project, session };
    setTimeout(() => void emit({ kind: "summary", summary: structuredClone(summary) }), 30);
  };
  const resource = async ({ ref, operation }) => {
    const key = `${ref.kind}:${operation.kind}`;
    const input = operation.input || {};
    if (key === "project:agent_conversations") {
      if (control.holdConversations) await control.holdConversations;
      if (control.failConversations) throw Error("store busy");
      return structuredClone(conversations[`${input.project || ref.location.project}:${input.agent}`] || [{ id: `default-${input.agent}`, title: input.agent, updated_at: iso(HOUR), turns: 0, running: null, asking: false, default: true }]);
    }
    if (key === "project:agent_conversation_start") return { conversation: `new-${input.agent}` };
    if (key === "project:board") return board;
    const results = { "project:grants": [], "project:skills": [], "project:schedules": [], "process:processes": [], "project:focus": null };
    if (Object.hasOwn(results, key)) return results[key];
    throw Error(`Unknown fixture resource ${key}`);
  };
  const rpc = {
    hello: () => ({ summary: structuredClone(summary), ui: {}, commands: [], seq: 0, working: [], approvals: [], runtime: "fixture" }),
    summary: () => structuredClone(summary),
    projects: () => structuredClone(projects),
    sessions: (params) => params.general ? structuredClone(generalChats) : structuredClone(generalChats.filter((chat) => chat.project === (params.project || summary.project.id))),
    agents: async (params) => {
      assert.deepEqual(params, { all: true });
      control.agentsCalls++;
      if (control.holdAgents) await control.holdAgents;
      if (control.failAgents) throw Error("store busy");
      return structuredClone(agents);
    },
    transcript: () => [],
    places: () => ({ here: "This computer", places: [] }),
    attention_all: () => ({ items: [], notes: [], away: [] }),
    plugins: () => [], computers: () => [],
    context: () => ({ total: 0, budget: 64000 }),
    dictation_status: () => ({ status: "unavailable" }),
    resource,
    open_project: (params) => { switchTo(params.id, generalChats.find((chat) => chat.project === params.id)?.id || `empty-${params.id}`); return null; },
    open_session: (params) => { switchTo(generalChats.find((chat) => chat.id === params.id)?.project || summary.project.id, params.id); return null; },
    new_session: (params) => { switchTo(params?.project || summary.project.id, `new-${calls.filter((c) => c.method === "new_session").length}`); return null; },
    rename_session: () => null,
  };
  page.on("pageerror", (error) => errors.push(error.message));
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://fixture");
      if (url.pathname === "/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); stream = res; return; }
      if (url.pathname === "/session") { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ signed_in: true, pairing: false })); return; }
      if (url.pathname === "/manifest.webmanifest") { res.setHeader("content-type", "application/manifest+json"); res.end(JSON.stringify({ name: "fixture", start_url: "/index.html" })); return; }
      if (url.pathname === "/rpc") {
        let raw = ""; for await (const chunk of req) raw += chunk;
        const { method, params } = JSON.parse(raw);
        calls.push({ method, params });
        res.setHeader("content-type", "application/json");
        if (!Object.hasOwn(rpc, method)) { unexpected.push(`RPC ${method}`); res.end(JSON.stringify({ ok: false, error: `Unknown fixture RPC ${method}` })); return; }
        try { res.end(JSON.stringify({ ok: true, result: await rpc[method](params || {}) })); }
        catch (error) { res.end(JSON.stringify({ ok: false, error: error.message })); }
        return;
      }
      const path = resolve(root, `.${decodeURIComponent(url.pathname)}`);
      if (!path.startsWith(root + sep)) { unexpected.push(req.url); res.writeHead(404); res.end(); return; }
      res.setHeader("content-type", path.endsWith(".css") ? "text/css" : path.endsWith(".html") ? "text/html" : "text/javascript");
      res.end(await readFile(path));
    } catch (error) { res.writeHead(500); res.end(error.message); }
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator("#agents .agent-btn").first().waitFor({ state: "attached" });
    if (agentView) await page.evaluate(() => {
      window.opened = [];
      window.ark.agentIdentity((name) => ["white", "blue", "pearl", "ink"][name.length % 4]);
      window.ark.view({ id: "agent-chat", title: "Chat", agentConversation: true, resourceScoped: true, render(el, params) {
        let current = params;
        const draw = () => { el.textContent = `Chat with ${current.name} ${current.conversation || "first"}`; window.opened.push({ ...current }); };
        draw();
        return {
          update(next) { current = next; draw(); },
          identity: () => current.name ? { name: current.name, title: current.conversation === "new" ? "New chat" : `${current.name} chat`, avatar: (size) => window.ark.h("span", { style: { width: `${size}px`, height: `${size}px`, display: "inline-block" } }),
            agent: { name: current.name, project: current.project, conversation: current.conversation === "new" ? "new" : current.conversation || null, program: false } } : null,
          title: () => `${current.name} chat`,
        };
      } });
    });
    if (agentView && !failAgents && !holdAgents) await page.waitForFunction((n) => document.querySelectorAll("#agents .agent-item:not(.placeholder)").length === n, 1 + agents.filter((a) => !(hidden || []).some((h) => h.id === a.project)).length);
    await run(page, { calls, emit, control, summary });
    assert.deepEqual(errors, [], "uncaught page errors");
    assert.deepEqual(unexpected, [], "unexpected fixture requests");
  } finally {
    await context.close(); stream?.end(); server.closeAllConnections();
    await new Promise((done) => server.close(done));
  }
}

const keys = (page) => page.locator("#agents .agent-item:not(.placeholder)").evaluateAll((items) => items.map((item) => [item.dataset.kind, item.dataset.key.replace("/", "/")]));
const agent = (page, project, name) => page.locator(`#agents .agent-btn[data-key="${project}/${name}"]`);
const shot = async (page, name) => {
  if (!shots) return;
  await mkdir(shots, { recursive: true });
  await page.waitForTimeout(250);
  await page.screenshot({ path: resolve(shots, `${name}.png`), animations: "disabled" });
};

test("with no app that draws agents' chats, the sidebar is ARK alone and never asks for agents", () => fixture(async (page, { calls }) => {
  await page.waitForTimeout(300);
  assert.deepEqual(await keys(page), [["tile", "ark"]]);
  assert.equal(await page.locator("#agents .agent-btn").getAttribute("aria-label"), "ARK");
  assert.equal(calls.filter((call) => call.method === "agents").length, 0);
  assert.equal(await page.locator("#projects, #add-project, #rail-pins, .section-label").count(), 0);
  assert.equal(await page.locator("#agents-note").isHidden(), true);
  await shot(page, "sidebar-ark-only-light");
}, { agentView: false }));

test("tiles: ARK, pins in pin order, then latest activity; rows after; a pin past six makes more tiles", async () => {
  await fixture(async (page) => {
    const order = await keys(page);
    assert.deepEqual(order.slice(0, 6), [["tile", "ark"], ["tile", "c/Editor"], ["tile", "a/Mia"], ["tile", "b/Writer"], ["tile", "d/Scout"], ["tile", "e/Planner"]]);
    assert.deepEqual(order.slice(6).map(([kind]) => kind), Array(order.length - 6).fill("row"));
    assert.equal(order.at(-1)[1], "b/Mia", "the oldest last");
    // Pin from the tile menu: it moves after the pins, the menu offers Unpin next time.
    await agent(page, "f", "Archivist").click({ button: "right" });
    await page.getByRole("menuitem", { name: "Pin", exact: true }).click();
    assert.deepEqual((await keys(page)).slice(0, 3).map(([, key]) => key), ["ark", "c/Editor", "f/Archivist"]);
    assert.deepEqual(await page.evaluate(() => JSON.parse(localStorage.getItem("ark-pins:fixture"))), [["c", "Editor"], ["f", "Archivist"]]);
    await agent(page, "f", "Archivist").click({ button: "right" });
    assert.equal(await page.getByRole("menuitem", { name: "Unpin", exact: true }).count(), 1);
    await page.keyboard.press("Escape");
  }, { agents: agentRows(8), pins: [["c", "Editor"]] });
  const many = agentRows(12).slice(0, 12);
  await fixture(async (page) => {
    const order = await keys(page);
    assert.equal(order.filter(([kind]) => kind === "tile").length, 9, "eight pins and ARK");
  }, { agents: many, pins: many.slice(0, 8).map((row) => [row.project, row.name]) });
});

test("same name in two folders shows the folder; rows show the latest chat; hidden folders and offline", () => fixture(async (page, { emit }) => {
  const twins = page.locator('#agents .agent-btn[aria-label^="Mia,"]');
  assert.equal(await twins.count(), 2);
  assert.deepEqual(await twins.evaluateAll((nodes) => nodes.map((node) => node.querySelector(".agent-sub")?.textContent)), ["Folder A", "Folder B"]);
  assert.equal(await agent(page, "a", "Mia").getAttribute("aria-label"), "Mia, Folder A");
  const row = agent(page, "b", "Builder");
  assert.equal(await row.locator("xpath=..").getAttribute("data-kind"), "row");
  assert.equal(await row.locator(".agent-sub").textContent(), "Latest with Builder");
  assert.equal(await page.locator('#agents .agent-btn[data-key^="c/"]').count(), 0, "a hidden folder's agents get no tile");
  assert.equal(await page.locator('#agents [aria-label="Writer, needs you"] .agent-mark[data-mark="asking"]').count(), 1);
  assert.equal(await page.locator('#agents [aria-label="Scout, working"] .agent-mark[data-mark="working"]').count(), 1);
  await emit({ kind: "reconnecting", delay: 1000 });
  await page.locator('#agents[data-offline="true"]').waitFor();
  assert.equal(await page.locator("#agents .agent-mark").evaluateAll((marks) => marks.filter((mark) => getComputedStyle(mark).display !== "none").length), 0);
  const before = await page.evaluate(() => window.opened.length);
  await agent(page, "a", "Mia").dispatchEvent("click");
  await page.waitForTimeout(200);
  assert.equal(await page.evaluate(() => window.opened.length), before, "clicks wait for the connection");
  // The palette offers the hidden folder back.
  await page.keyboard.press("Meta+k");
  await page.getByRole("option", { name: /Show Garden again/ }).click();
}, { agents: agentRows(8, { projects: ["a", "b", "c", "d", "e"], asking: ["Writer"], working: ["Scout"] }), hidden: [{ id: "c", name: "Garden" }] }));

test("an agent in another folder opens its latest chat there; latest click wins", () => fixture(async (page, { calls }) => {
  await agent(page, "b", "Writer").click();
  await page.getByText("Chat with Writer conv-2", { exact: true }).waitFor();
  const conversationsCall = calls.find((call) => call.method === "resource" && call.params.operation.kind === "agent_conversations");
  assert.deepEqual(conversationsCall.params.operation.input, { agent: "Writer", project: "b" });
  assert.deepEqual(calls.filter((call) => call.method === "open_project").map((call) => call.params.id), ["b"]);
  assert.deepEqual(await page.evaluate(() => window.opened.at(-1)), { name: "Writer", project: "b", conversation: "conv-2" });
  assert.equal(await agent(page, "b", "Writer").getAttribute("aria-current"), "true");
  // An agent with no chat yet: a new one.
  await agent(page, "a", "Mia").click();
  await page.getByText("Chat with Mia first", { exact: true }).waitFor();
}, { conversations: { "b:Writer": [
  { id: "conv-2", title: "Chapter two", updated_at: iso(HOUR), turns: 4, running: null, asking: false, default: false },
  { id: "default-Writer", title: "Writer", updated_at: iso(9 * HOUR), turns: 2, running: null, asking: false, default: true },
] } }));

test("an agent with no chat yet opens its first chat; until it has one, New chat opens that one too", () => fixture(async (page, { calls }) => {
  await agent(page, "a", "Mia").click();
  await page.getByText("Chat with Mia first", { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.opened.at(-1)), { name: "Mia", project: "a" });
  await page.keyboard.press("Meta+n");
  await page.waitForFunction(() => window.opened.length === 2);
  assert.deepEqual(await page.evaluate(() => window.opened.at(-1)), { name: "Mia", project: "a" }, "no second chat beside a default one that does not exist");
  assert.equal(calls.filter((call) => call.method === "resource" && call.params.operation.kind === "agent_conversation_start").length, 0);
  // Once it has its default chat, New chat is a new one.
  await agent(page, "b", "Writer").click();
  await page.getByText("Chat with Writer first", { exact: true }).waitFor();
  await page.keyboard.press("Meta+n");
  await page.getByText("Chat with Writer new", { exact: true }).waitFor();
}, { conversations: { "a:Mia": [] } }));

test("⌘-click opens ARK's newest chat as a chat tab and an agent as a click does", () => fixture(async (page) => {
  await agent(page, "b", "Writer").click({ modifiers: ["Meta"] });
  await page.getByText("Chat with Writer first", { exact: true }).waitFor();
  await page.locator('#agents .agent-btn[data-key="ark"]').click({ modifiers: ["Meta"] });
  await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
  assert.equal(await page.locator('#agents .agent-btn[data-key="ark"]').getAttribute("aria-current"), "true");
}));

test("an app opening an agent's chat in another folder marks that folder's agent", () => fixture(async (page) => {
  await page.evaluate(() => window.ark.openView("agent-chat", { name: "Mia", project: "b" }));
  await page.getByText("Chat with Mia first", { exact: true }).waitFor();
  await page.waitForFunction(() => document.querySelector('#agents .agent-btn[data-key="b/Mia"]')?.getAttribute("aria-current") === "true");
  assert.equal(await agent(page, "a", "Mia").getAttribute("aria-current"), null);
}));

test("ARK opens its newest chat with turns, in its own folder", () => fixture(async (page, { calls }) => {
  await page.locator('#agents .agent-btn[data-key="ark"]').click();
  await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
  assert.deepEqual(calls.filter((call) => call.method === "open_session").map((call) => call.params.id), ["chat-b"]);
  assert.equal(await page.evaluate(() => window.ark.summary().project.id), "b");
}, { general: [
  { id: "chat-new", title: "", turns: 0, updated_at: iso(1000), project: "a", project_name: "Notes" },
  { id: "chat-b", title: "Garden plan", turns: 3, updated_at: iso(HOUR), project: "b", project_name: "Writing" },
  { id: "chat-a", title: "First draft", turns: 2, updated_at: iso(2 * HOUR), project: "a", project_name: "Notes" },
] }));

test("⌘N, the tile menu and /new start a chat with the agent in view", () => fixture(async (page, { calls }) => {
  await page.keyboard.press("Meta+n");
  await page.waitForFunction(() => window.ark.summary().session.id.startsWith("new-"));
  assert.equal(calls.filter((call) => call.method === "new_session").length, 1);
  await agent(page, "c", "Editor").click();
  await page.getByText(/Chat with Editor/).waitFor();
  await agent(page, "c", "Editor").click({ button: "right" });
  await page.getByRole("menuitem", { name: "New chat", exact: true }).click();
  await page.getByText("Chat with Editor new", { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.opened.at(-1)), { name: "Editor", project: "c", conversation: "new" });
  await page.keyboard.press("Meta+n");
  await page.waitForFunction(() => window.opened.filter((open) => open.conversation === "new").length === 2);
  // An agent on an agent program has one chat: New chat opens that one, its menu has no New chat.
  await agent(page, "d", "Scout").click();
  await page.getByText(/Chat with Scout/).waitFor();
  await page.keyboard.press("Meta+n");
  await page.waitForFunction(() => window.opened.at(-1).name === "Scout" && !window.opened.at(-1).conversation);
  await agent(page, "d", "Scout").click({ button: "right" });
  assert.deepEqual(await page.locator(".context-menu .menu-item").allTextContents(), ["Chats", "Pin"]);
  await page.keyboard.press("Escape");
  // Back to ARK: /new is a new ARK chat.
  await page.locator('#agents .agent-btn[data-key="ark"]').click();
  await page.locator("#input").waitFor();
  await page.locator("#input").fill("/new");
  await page.locator("#input").press("Enter");
  await page.waitForFunction(() => window.ark.summary().session.id === "new-2");
  assert.equal(calls.filter((call) => call.method === "new_session").length, 2);
}, { agents: agentRows(4, { program: ["Scout"] }) }));

test("the title opens Recent chats: placeholders, list, current checked, All chats, error, empty", () => fixture(async (page, { control }) => {
  const title = page.locator(".identity-title-btn");
  await title.waitFor();
  assert.match(await title.getAttribute("aria-label"), /First draft: recent chats/);
  await title.click();
  const menu = page.locator(".recent-menu");
  await menu.locator(".recent-row").first().waitFor();
  assert.deepEqual(await menu.locator(".recent-title").allTextContents(), [...Array(8)].map((_, i) => `Chat ${i}`));
  assert.deepEqual(await menu.locator(".recent-folder").evaluateAll((nodes) => nodes.slice(0, 2).map((node) => node.textContent)), ["Writing", "Notes"]);
  assert.equal(await menu.locator('[aria-checked="true"] .recent-title').textContent(), "Chat 1");
  assert.equal(await menu.getByRole("menuitem", { name: "All chats" }).count(), 1);
  await shot(page, "recent-chats-light");
  await page.keyboard.press("Escape");
  await menu.waitFor({ state: "detached" });
  assert.equal(await title.evaluate((node) => document.activeElement === node), true, "Escape returns focus to the title");
  // An agent's chats: first time placeholders, then the rows; a failure says so and retries.
  await agent(page, "b", "Writer").click();
  await page.getByText(/Chat with Writer/).waitFor();
  let release;
  control.holdConversations = new Promise((resolve) => { release = resolve; });
  await page.locator(".identity-title-btn").press("Enter");
  await page.locator(".recent-menu .recent-ghost").first().waitFor();
  assert.equal(await page.locator(".recent-menu .recent-ghost").count(), 3);
  release();
  control.holdConversations = null;
  await page.locator(".recent-menu .recent-row").first().waitFor();
  assert.equal(await page.locator('.recent-menu [aria-checked="true"]').evaluate((node) => document.activeElement === node), true, "opens on the current chat");
  await page.keyboard.press("Escape");
  control.failConversations = true;
  await page.locator(".identity-title-btn").click();
  await page.getByText("Could not load", { exact: true }).waitFor();
  assert.equal(await page.locator(".recent-menu .recent-row").count(), 2, "the last list stays");
  control.failConversations = false;
  await page.getByRole("menuitem", { name: "Try again" }).click();
  await page.locator(".recent-menu .recent-error").waitFor({ state: "detached" });
  await page.getByRole("menuitemradio", { name: /Draft two/ }).click();
  await page.getByText("Chat with Writer conv-x", { exact: true }).waitFor();
}, {
  general: [...Array(10)].map((_, i) => ({ id: i === 1 ? "chat-a" : `chat-${i}`, title: `Chat ${i}`, turns: 1, updated_at: iso(i * HOUR), project: i % 2 ? "a" : "b", project_name: i % 2 ? "Notes" : "Writing" })),
  conversations: { "b:Writer": [
    { id: "conv-x", title: "Draft two", updated_at: iso(HOUR), turns: 1, running: "task-1", asking: false, default: false },
    { id: "default-Writer", title: "Writer", updated_at: iso(3 * HOUR), turns: 3, running: null, asking: true, default: true },
  ] },
}));

test("an agent with no other chat has only New chat in its menu", () => fixture(async (page) => {
  await agent(page, "a", "Mia").click();
  await page.getByText(/Chat with Mia/).waitFor();
  await page.locator(".identity-title-btn").click();
  await page.locator(".recent-menu .recent-list[aria-busy=false]").waitFor({ state: "attached" });
  assert.deepEqual(await page.locator(".recent-menu .menu-item").allInnerTexts().then((texts) => texts.map((t) => t.split("\n")[0])), ["New chat"]);
}, { conversations: { "a:Mia": [] } }));

test("/history opens this agent's chats in the search overlay, filtered by the words, and Enter opens", () => fixture(async (page) => {
  await page.locator("#input").fill("/history garden");
  await page.locator("#input").press("Enter");
  const input = page.locator(".palette-input");
  await input.waitFor();
  assert.equal(await input.inputValue(), "garden");
  await page.locator(".palette-item").first().waitFor();
  assert.deepEqual(await page.locator(".palette-item .palette-item-main > span:first-child").allTextContents(), ["Garden plan"]);
  assert.match(await page.locator(".palette-item").first().textContent(), /Writing/);
  await shot(page, "history-overlay-light");
  await input.press("Enter");
  await page.waitForFunction(() => window.ark.summary().session.id === "chat-b");
  // From a tile's menu, an agent's chats.
  await agent(page, "b", "Writer").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Chats", exact: true }).click();
  await page.locator(".palette-item").first().waitFor();
  assert.equal(await page.locator(".palette-input").getAttribute("placeholder"), "Search chats with Writer");
  await page.locator(".palette-input").fill("two");
  assert.deepEqual(await page.locator(".palette-item .palette-item-main > span:first-child").allTextContents(), ["Chapter two"]);
  await page.keyboard.press("Enter");
  await page.getByText("Chat with Writer conv-2", { exact: true }).waitFor();
}, { conversations: { "b:Writer": [
  { id: "conv-2", title: "Chapter two", updated_at: iso(HOUR), turns: 4, running: null, asking: false, default: false },
  { id: "default-Writer", title: "Chapter one", updated_at: iso(9 * HOUR), turns: 2, running: null, asking: false, default: true },
] } }));

test("the list is one tab stop: arrows by tile row, Home, End, typeahead and the menu key", () => fixture(async (page) => {
  await page.locator("#new-agent").focus();
  await page.keyboard.press("Tab");
  const focused = () => page.evaluate(() => document.activeElement?.dataset.key?.replace("/", "/"));
  assert.equal(await focused(), "ark");
  await page.keyboard.press("ArrowRight");
  const second = await focused();
  await page.keyboard.press("ArrowDown");
  const order = (await keys(page)).map(([, key]) => key);
  assert.equal(await focused(), order[4], "down moves by a row of three tiles");
  await page.keyboard.press("ArrowDown");
  assert.equal(await focused(), order[6], "past the tiles: the first row");
  await page.keyboard.press("ArrowDown");
  assert.equal(await focused(), order[7]);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  assert.equal(await focused(), order[5]);
  await page.keyboard.press("End");
  assert.equal(await focused(), order.at(-1));
  await page.keyboard.press("Home");
  assert.equal(await focused(), "ark");
  await page.keyboard.press("p");
  assert.equal(await focused(), "e/Planner");
  assert.equal(await page.locator("#agents .agent-btn[tabindex='0']").count(), 1);
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  assert.equal(await focused(), "e/Planner", "the list remembers its stop");
  await page.keyboard.press("Shift+F10");
  await page.getByRole("menuitem", { name: "New chat", exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await page.keyboard.press("Enter");
  await page.getByText(/Chat with Planner/).waitFor();
  assert.notEqual(second, "ark");
}, { agents: agentRows(10) }));

test("an agents call that fails leaves ARK working with a way to try again", () => fixture(async (page, { control }) => {
  await page.getByText("Agents unavailable", { exact: true }).waitFor();
  assert.deepEqual(await keys(page), [["tile", "ark"]]);
  control.failAgents = false;
  await page.getByRole("button", { name: "Try again" }).click();
  await page.waitForFunction(() => document.querySelectorAll("#agents .agent-btn").length === 6);
  assert.equal(await page.locator("#agents-note").isHidden(), true);
}, { failAgents: true }));

test("while agents load, ARK is there at once and three placeholder tiles follow", () => fixture(async (page, { control }) => {
  await page.locator(".agent-item.placeholder").first().waitFor();
  assert.equal(await page.locator(".agent-item.placeholder").count(), 3);
  assert.deepEqual(await keys(page), [["tile", "ark"]]);
  control.release();
  await page.waitForFunction(() => document.querySelectorAll("#agents .agent-btn").length === 6);
  assert.equal(await page.locator(".agent-item.placeholder").count(), 0);
}, { holdAgents: true }));

test("a question in an ARK chat or a waiting board item marks ARK as needing the person", () => fixture(async (page, { emit }) => {
  const ark = page.locator('#agents .agent-btn[data-key="ark"]');
  await page.waitForFunction(() => document.querySelector('#agents .agent-btn[data-key="ark"]')?.getAttribute("aria-label") === "ARK, needs you");
  assert.equal(await ark.locator('.agent-mark[data-mark="asking"]').count(), 1, "board item waiting");
  await emit({ kind: "working", working: [{ session: "chat-b", project: "b" }] });
  assert.equal(await ark.getAttribute("aria-label"), "ARK, needs you", "needs you wins over working");
}, { board: { boards: [{ id: "bd", goal: "Launch" }], items: [{ id: "w1", board: "bd", title: "Sign the brief", state: "todo", owner: "user", blocked: "needs a yes", tasks: [] }], tasks: {}, agents: [] } }));

test("ark.rail and ark.projectSection are gone with a named error", () => fixture(async (page) => {
  const result = await page.evaluate(() => ["rail", "projectSection"].map((name) => {
    try { window.ark[name]({ id: "x", label: "x", render() {} }); return null; } catch (error) { return [error.name, error.message]; }
  }));
  for (const [name, message] of result) {
    assert.equal(name, "RemovedExtensionPoint");
    assert.match(message, /ADR 0110/);
  }
}, { agentView: false }));

test("narrow: the drawer lists agents and a pick closes it", () => fixture(async (page) => {
  await page.locator("#reveal-sidebar").click();
  await page.locator('.app[data-drawer="open"]').waitFor();
  const box = await agent(page, "b", "Writer").boundingBox();
  assert.ok(box.height >= 44 || (await agent(page, "b", "Writer").locator("xpath=..").getAttribute("data-kind")) === "tile");
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  assert.ok(width <= 390, "no sideways scroll");
  await agent(page, "b", "Writer").click();
  await page.locator('.app[data-drawer="closed"]').waitFor();
}, { width: 390, coarse: true }));

// Evidence only: `ARK_UI_SHOTS=<dir>` draws each state in light and dark. With
// `ARK_UI_BRAND_CSS=<path to a consumer's brand.css>`, faces wear that app's look.
const centers = (page) => page.evaluate(() => {
  const list = document.querySelector("#agents").getBoundingClientRect();
  const mid = list.left + list.width / 2;
  return [...document.querySelectorAll('#agents .agent-item[data-kind="tile"]')].map((item) => {
    const box = item.getBoundingClientRect();
    return [Math.round(box.top), Math.round(box.left + box.width / 2 - mid)];
  });
});

test("tiles sit centered: one alone, two side by side, a fourth alone on the next row", async () => {
  await fixture(async (page) => {
    assert.deepEqual((await centers(page)).map(([, dx]) => dx), [0], "ARK alone is centered");
  }, { agents: [] });
  await fixture(async (page) => {
    const tiles = await centers(page);
    assert.equal(tiles.length, 2);
    assert.equal(tiles[0][0], tiles[1][0], "one row");
    assert.equal(tiles[0][1], -tiles[1][1], "the pair is centered");
  }, { agents: agentRows(1) });
  await fixture(async (page) => {
    const tiles = await centers(page);
    assert.equal(tiles.length, 4);
    assert.equal(tiles[1][1], 0, "the middle of three is centered");
    assert.ok(tiles[3][0] > tiles[0][0], "the fourth starts a row");
    assert.equal(tiles[3][1], 0, "and is centered on it");
    await shot(page, "sidebar-centered-4");
  }, { agents: agentRows(3) });
});

test("+ makes an agent through the app; with no app it starts a chat", () => fixture(async (page, { calls }) => {
  const plus = page.locator("#new-agent");
  assert.equal(await plus.getAttribute("aria-label"), "New chat");
  await plus.click();
  await page.waitForFunction(() => window.ark.summary().session.id.startsWith("new-"));
  assert.equal(calls.filter((call) => call.method === "new_session").length, 1);
  await page.evaluate(() => window.ark.newAgent({ open: ({ location }) => { window.made = location; } }));
  await page.waitForFunction(() => document.querySelector("#new-agent").getAttribute("aria-label") === "New agent");
  await plus.click();
  await page.waitForFunction(() => window.made);
  assert.equal(await page.evaluate(() => window.made.runtime), "fixture");
  assert.equal(calls.filter((call) => call.method === "new_session").length, 1, "no chat besides");
  await page.evaluate(() => window.ark.newAgent({ open() {} })).then(() => assert.fail("a second handler"), (error) => assert.match(error.message, /duplicate New agent/));
  assert.equal(await page.locator("#destinations, #toggle-sidebar, #new-session").count(), 0, "no rail, no hide button, no New chat row");
}));

test("the foot holds the person, who opens Settings, and the app's views as buttons", () => fixture(async (page) => {
  await page.evaluate(() => {
    window.ark.profile(() => ({ name: "Ada Lovelace" }));
    window.ark.setting({ id: "account", title: "Account", render(el) { el.textContent = "Account section"; } });
    window.ark.view({ id: "board", title: "Board", nav: true, icon: "box", render(el) { el.textContent = "Board view"; } });
  });
  const profile = page.locator("#open-settings");
  await profile.waitFor();
  assert.equal(await profile.locator(".profile-name").textContent(), "Ada Lovelace");
  assert.equal(await profile.locator(".account-avatar").textContent(), "A");
  const board = page.locator('#sidebar-views [data-view="board"]');
  assert.equal(await board.getAttribute("aria-label"), "Board");
  const top = async (selector) => page.locator(selector).evaluate((el) => Math.round(el.getBoundingClientRect().top + el.getBoundingClientRect().height / 2));
  assert.ok(Math.abs(await top("#open-settings") - await top('#sidebar-views [data-view="board"]')) <= 1, "on one line");
  await board.click();
  await page.getByText("Board view").waitFor();
  assert.equal(await board.getAttribute("aria-current"), "page");
  await profile.click();
  await page.getByText("Account section").waitFor();
  assert.equal(await profile.getAttribute("aria-current"), "true");
  assert.equal(await board.getAttribute("aria-current"), null);
  await shot(page, "sidebar-foot");
}));

test("the sidebar edge resizes, remembers, hides past the minimum, and keys move it", () => fixture(async (page) => {
  const width = () => page.locator(".sidebar").evaluate((el) => Math.round(el.getBoundingClientRect().width));
  const grip = page.locator("#sidebar-grip");
  const start = await width();
  const box = await grip.boundingBox();
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + 3, y);
  await page.mouse.down();
  await page.mouse.move(box.x + 63, y, { steps: 4 });
  await page.mouse.up();
  await page.waitForTimeout(300);
  assert.equal(await width(), start + 60);
  assert.equal(await page.evaluate(() => localStorage.getItem("ark-sidebar-width")), String(start + 60));
  await page.mouse.move(box.x + 63, y);
  await page.mouse.down();
  await page.mouse.move(box.x + 63 + 400, y, { steps: 4 });
  await page.mouse.up();
  await page.waitForTimeout(300);
  assert.equal(await width(), 400, "no wider than the maximum");
  await grip.focus();
  await page.keyboard.press("ArrowLeft");
  await page.waitForTimeout(300);
  assert.equal(await width(), 384);
  const now = await grip.boundingBox();
  await page.mouse.move(now.x + 3, y);
  await page.mouse.down();
  await page.mouse.move(40, y, { steps: 4 });
  await page.mouse.up();
  await page.waitForFunction(() => document.querySelector("#app").dataset.sidebar === "closed");
  assert.equal(await page.evaluate(() => localStorage.getItem("ark-sidebar-width")), "384", "hiding keeps the width");
  await page.locator("#reveal-sidebar").click();
  await page.waitForFunction(() => document.querySelector("#app").dataset.sidebar === "open");
  await page.waitForTimeout(300);
  assert.equal(await width(), 384);
  // No handler of the hiding drag is left: a stray cancel later changes nothing.
  await grip.dispatchEvent("pointercancel");
  await page.mouse.click((await grip.boundingBox()).x + 3, y);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(() => document.querySelector("#app").dataset.sidebar), "open");
  assert.equal(await width(), 384);
  await grip.focus();
  await page.keyboard.press("Home");
  await page.waitForTimeout(300);
  assert.equal(await width(), 200, "Home: the narrowest");
  const inside = await page.evaluate(() => {
    const edge = document.querySelector(".sidebar").getBoundingClientRect().right;
    return ["#new-agent", "#search-chats", "#agents .agent-btn"].map((s) => Math.round(document.querySelector(s).getBoundingClientRect().right) <= Math.round(edge));
  });
  assert.deepEqual(inside, [true, true, true], "nothing is cut off at the narrowest width");
  await grip.dblclick();
  await page.waitForTimeout(300);
  assert.equal(await width(), start, "double click: the default width");
}));

test("an app's agent actions sit in an agent tile's menu, not ARK's, and a toast can offer Undo", () => fixture(async (page) => {
  await page.evaluate(() => {
    window.done = [];
    window.ark.action({ id: "delete", label: "Delete", where: "agent", when: ({ agent }) => agent.name !== "Scout", run: ({ agent }, api) => {
      window.done.push(agent);
      api.toast(`Deleted ${agent.title}`, { action: { label: "Undo", run: () => window.done.push("undo") } });
    } });
  });
  await page.locator('#agents .agent-btn[data-key="ark"]').click({ button: "right" });
  assert.equal(await page.getByRole("menuitem", { name: "Delete", exact: true }).count(), 0, "not on ARK");
  await page.keyboard.press("Escape");
  await agent(page, "d", "Scout").click({ button: "right" });
  assert.equal(await page.getByRole("menuitem", { name: "Delete", exact: true }).count(), 0, "when() hides it");
  await page.keyboard.press("Escape");
  await agent(page, "c", "Editor").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
  assert.deepEqual(await page.evaluate(() => window.done[0]), { name: "Editor", project: "c", title: "Editor" });
  const toast = page.locator("#toast");
  assert.match(await toast.innerText(), /Deleted Editor/);
  await toast.getByRole("button", { name: "Undo" }).click();
  assert.deepEqual(await page.evaluate(() => window.done.at(-1)), "undo");
  await page.waitForFunction(() => document.querySelector("#toast").dataset.show === "false");
  assert.equal(await toast.getByRole("button").count(), 0, "Undo runs once and leaves with the toast");
}));

test("screenshots of each sidebar state", { skip: !shots }, async () => {
  const brand = process.env.ARK_UI_BRAND_CSS;
  const both = async (page, name) => {
    if (brand) {
      let css = await readFile(brand, "utf8");
      for (const [match, file] of [...css.matchAll(/url\("(mascot\/[^"]+)"\)/g)]) {
        const bytes = await readFile(resolve(brand, "..", file)).catch(() => null);
        if (bytes) css = css.replaceAll(match, `url("data:image/${file.endsWith(".png") ? "png" : "webp"};base64,${bytes.toString("base64")}")`);
      }
      await page.addStyleTag({ content: css });
    }
    for (const theme of ["light", "dark"]) {
      await page.evaluate((mode) => window.arkTheme.setMode(mode), theme);
      await shot(page, `${name}-${theme}`);
    }
  };
  await fixture(async (page) => { await both(page, "sidebar-ark-only"); }, { agentView: false });
  await fixture(async (page) => {
    await agent(page, "b", "Writer").click();
    await page.getByText(/Chat with Writer/).waitFor();
    await both(page, "sidebar-3-agents");
  }, { agents: agentRows(3, { twin: false, working: ["Writer"] }) });
  const thirty = agentRows(29, { asking: ["Scout", "Legal"], working: ["Mia", "Docs", "Sales"] });
  await fixture(async (page) => {
    await both(page, "sidebar-30-agents");
    await page.locator(".agents-scroll").evaluate((el) => { el.scrollTop = el.scrollHeight; });
    await both(page, "sidebar-30-agents-scrolled");
  }, { agents: thirty, pins: [["c", "Editor"]] });
  await fixture(async (page) => {
    await page.locator(".identity-title-btn").click();
    await page.locator(".recent-menu .recent-row").first().waitFor();
    await both(page, "recent-chats");
  }, { agents: agentRows(4), general: [...Array(10)].map((_, i) => ({ id: i === 0 ? "chat-a" : `chat-${i}`, title: ["First draft", "Garden plan", "Budget for March", "Trip to Lisbon", "Reply to Ana", "Quarterly review", "Fix the invoice", "Notes from Monday", "Hiring plan", "Book list"][i], turns: 1, updated_at: iso(i * 5 * HOUR), project: i % 2 ? "b" : "a", project_name: i % 2 ? "Writing" : "Notes" })) });
  await fixture(async (page) => {
    await page.locator("#input").fill("/history plan");
    await page.locator("#input").press("Enter");
    await page.locator(".palette-item").first().waitFor();
    await both(page, "history-overlay");
  }, { general: [
    { id: "chat-a", title: "First draft", turns: 2, updated_at: iso(2 * HOUR), project: "a", project_name: "Notes" },
    { id: "chat-b", title: "Garden plan", turns: 3, updated_at: iso(HOUR), project: "b", project_name: "Writing" },
    { id: "chat-c", title: "Hiring plan", turns: 3, updated_at: iso(30 * HOUR), project: "c", project_name: "Garden" },
  ] });
  await fixture(async (page) => {
    await page.locator("#reveal-sidebar").click();
    await page.locator('.app[data-drawer="open"]').waitFor();
    await page.waitForTimeout(300);
    await both(page, "narrow-390-drawer");
  }, { width: 390, coarse: true, agents: agentRows(12, { asking: ["Scout"], working: ["Mia"] }) });
});
