import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(process.env.ARK_UI_WEB_ROOT || fileURLToPath(new URL("../../web/", import.meta.url)));
const shots = process.env.ARK_UI_SHOTS || "";
const types = { ".js": "text/javascript", ".css": "text/css", ".html": "text/html" };
let browser;
let server;
let base;

const page_html = `<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/app.css">
<style>
  html, body { height: 100%; margin: 0; }
  #host { height: 100%; display: flex; }
  #host > * { flex: 1; min-width: 0; }
  .xname[data-variant="blue"] { --pet-ink: #2f6fed; }
  .xname[data-variant="ink"] { --pet-ink: #1d1d1b; }
  .xname[data-variant="green"] { --pet-ink: #1f8a4c; }
  [data-theme="dark"] .xname[data-variant="blue"] { --pet-ink: #8fb4ff; }
  [data-theme="dark"] .xname[data-variant="ink"] { --pet-ink: #d8d8d4; }
  [data-theme="dark"] .xname[data-variant="green"] { --pet-ink: #78d6a1; }
</style>
<div id="host"></div>
<script type="module">
import { chatView } from "/js/chat-view.js";
import { setAgentIdentity } from "/js/dom.js";
setAgentIdentity((name, look) => look || "ink");
const now = Date.now();
const MIN = 60000, HOUR = 60 * MIN, DAY = 24 * HOUR;
// Calendar days, not hours back from now: the stamps say Yesterday and Today at any hour.
const startOfToday = new Date(now).setHours(0, 0, 0, 0);
const yesterday = startOfToday - 9 * HOUR;
const earlierToday = Math.max(startOfToday + MIN, now - 2 * HOUR);
const person = (agent, name, look) => ({ agent, name, look });
const ceo = person("ceo", "CEO", "ink");
const cmo = person("cmo", "CMO", "blue");
const cso = person("cso", "CSO", "green");
const state = {
  delay: 0, delays: [], fail: false, orch: [], reads: [],
  lines: [
    { turn: "t1", with: [cmo], count: 2, last_ms: now, failed: false, label: "2 messages with CMO", parts: [{ text: "2 messages with " }, { with: 0 }] },
    { turn: "t3", with: [cso, cmo], count: 4, last_ms: now, failed: false, label: "Messaged CSO and CMO", parts: [{ text: "Messaged " }, { with: 0 }, { text: " and " }, { with: 1 }] },
  ],
  talks: {
    cmo: [
      { author: "ceo", text: "Draft the launch post for **Friday**.", at_ms: yesterday, work: "w1" },
      { author: "cmo", text: "Draft ready: three options, the second is my pick.", at_ms: yesterday + 4 * MIN, work: "w1" },
      { author: "ceo", text: "Go with the second.", at_ms: earlierToday },
      { author: "cmo", text: "", at_ms: earlierToday + MIN, work: "w2", failed: true },
    ],
    cso: [{ author: "ceo", text: "Who are our first ten buyers?", at_ms: now - 30 * MIN, work: "w3", pending: true }],
  },
};
window.state = state;
const turns = [
  { key: "task:t1", input: "Brief the board", answer: "I asked the CMO for the launch post." },
  { key: "task:t2", input: "Thanks", answer: "You are welcome." },
  { key: "task:t3", input: "Ask both", answer: "Both are working on it." },
];
const base = {
  call: async (method, params) => {
    state.reads.push([method, params]);
    // What a read returns is what the data was when it started.
    const answer = method === "exchange_lines" ? structuredClone(state.lines)
      : method === "exchange_talk" ? structuredClone({ with: [ceo, params.b === "cso" ? cso : cmo], messages: state.talks[params.b] || [] })
      : {};
    const wait = method === "exchange_talk" && state.delays.length ? state.delays.shift() : state.delay;
    if (wait) await new Promise((ok) => setTimeout(ok, wait));
    if (method === "exchange_talk" && state.fail) throw new Error("offline");
    return answer;
  },
  toast() {},
  on(name, fn) { (state.orch[name] ||= []).push(fn); return () => {}; },
  openFile() {},
  dictationRoute: () => ({ place: "", session: "s", runtime: "fixture" }),
  summary: () => ({ session: { id: "s" }, location: { runtime: "fixture", project: "p", session: "s" } }),
  pendingPlace: () => null,
  popover() {},
  ready() {},
  hereName: () => "This computer",
};
state.orch = {};
window.emitOrch = () => (state.orch["ark:orch"] || []).forEach((fn) => fn({}));
window.mount = (options = {}) => {
  const host = document.getElementById("host");
  host.replaceChildren();
  const view = chatView(host, base, { submit: async () => {}, exchanges: { project: "p", agent: "ceo", name: "CEO", look: "ink" }, ...options });
  window.view = view;
  view.render(options.turns || turns);
};
window.target = { project: "p", agent: "ceo", name: "CEO", look: "ink" };
window.mountWith = (fn) => window.mount({ exchanges: fn });
window.ready = true;
</script>`;

before(async () => {
  browser = await webkit.launch({ headless: true });
  server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    if (url.pathname === "/t.html") { res.setHeader("content-type", "text/html"); res.end(page_html); return; }
    const file = resolve(root, "." + url.pathname);
    if (!file.startsWith(root + sep)) { res.writeHead(403).end(); return; }
    try {
      const ext = file.slice(file.lastIndexOf("."));
      res.setHeader("content-type", types[ext] || "application/octet-stream");
      res.end(await readFile(file));
    } catch { res.writeHead(404).end(); }
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  base = `http://127.0.0.1:${server.address().port}`;
});
after(async () => { await browser?.close(); server?.close(); });

async function open(options = {}) {
  const { width = 900, height = 700, theme = "", touch = false, reduced = false, setup = "", mount = true, wait = true } = options;
  const context = await browser.newContext({ viewport: { width, height }, hasTouch: touch, isMobile: touch, reducedMotion: reduced ? "reduce" : "no-preference" });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(`${base}/t.html`);
  await page.waitForFunction(() => window.ready);
  if (theme) await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  if (setup) await page.evaluate(setup);
  if (mount) await page.evaluate(() => window.mount());
  if (wait) await page.waitForSelector(".xline");
  page.errors = errors;
  page.close = async () => { await context.close(); };
  return page;
}

/// The words of an element without the pets' initials.
const words = (locator) => locator.evaluate((el) => {
  const copy = el.cloneNode(true);
  for (const face of copy.querySelectorAll(".chat-face")) face.remove();
  return copy.textContent.replace(/\s+/g, " ").trim();
});

async function shot(page, name) {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png` });
}

test("a turn that reached other agents has one quiet line, the others have none", async () => {
  const page = await open();
  const lines = page.locator(".xline");
  assert.equal(await lines.count(), 2);
  assert.equal(await page.locator('.turn:nth-of-type(2) .xline').count(), 0, "a turn with no exchange draws nothing");
  const first = lines.first();
  assert.equal(await first.evaluate((el) => el.tagName), "BUTTON");
  assert.equal(await first.getAttribute("aria-label"), "2 messages with CMO");
  assert.equal(await words(first), "2 messages with CMO");
  assert.equal(await page.locator(".turn").first().locator(".xline").count(), 1, "the line sits inside its turn");
  assert.equal(await words(lines.nth(1)), "Messaged CSO and CMO");
  const pets = await first.locator(".chat-face").evaluateAll((els) => els.map((e) => [e.getBoundingClientRect().width, e.getBoundingClientRect().height]));
  assert.deepEqual(pets, [[16, 16]]);
  assert.equal(await lines.nth(1).locator(".chat-face").count(), 2);
  const style = await first.evaluate((el) => ({ color: getComputedStyle(el).color, text3: getComputedStyle(document.documentElement).getPropertyValue("--text-3").trim(), t: getComputedStyle(el).transitionDuration, cursor: getComputedStyle(el).cursor }));
  assert.equal(style.color, "rgb(93, 93, 91)", "text-2 at rest: readable, 4.5:1 or more");
  assert.match(style.t, /0\.12s/);
  assert.equal(style.cursor, "pointer");
  await shot(page, "line-light");
  assert.deepEqual(page.errors, []);
  await page.close();
});

test("hover raises the line to text and a press scales it to 0.98", async () => {
  const page = await open();
  const line = page.locator(".xline").first();
  await line.hover();
  await page.waitForTimeout(250);
  assert.equal(await line.evaluate((el) => getComputedStyle(el).color), await line.evaluate(() => { const p = document.createElement("p"); p.style.color = "var(--text)"; document.body.append(p); const c = getComputedStyle(p).color; p.remove(); return c; }), "text on hover");
  await page.mouse.down();
  await page.waitForTimeout(250);
  assert.equal(await line.evaluate((el) => getComputedStyle(el).scale), "0.98");
  await page.mouse.up();
  await page.evaluate(() => document.querySelector(".xview")?.querySelector(".xback")?.click());
  await page.close();
});

test("the line is reached with Tab, Enter opens the view over the chat, Esc returns to the same scroll and the line", async () => {
  const page = await open({ height: 360 });
  await page.evaluate(() => { const s = document.querySelector(".chat-surface .scroller"); s.scrollTop = 90; });
  const before = await page.evaluate(() => document.querySelector(".chat-surface .scroller").scrollTop);
  await page.locator(".xline").first().focus();
  assert.equal(await page.evaluate(() => document.activeElement.className.includes("xline")), true);
  await page.keyboard.press("Enter");
  await page.waitForSelector(".xview");
  const geometry = await page.evaluate(() => {
    const surface = document.querySelector(".chat-surface").getBoundingClientRect();
    const view = document.querySelector(".xview").getBoundingClientRect();
    return { surface: [surface.left, surface.top, surface.width, surface.height], view: [view.left, view.top, view.width, view.height] };
  });
  await page.waitForTimeout(300);
  const covered = await page.evaluate(() => {
    const surface = document.querySelector(".chat-surface").getBoundingClientRect();
    const view = document.querySelector(".xview").getBoundingClientRect();
    return [view.left - surface.left, view.top - surface.top, view.width, view.height].map(Math.round);
  });
  assert.deepEqual(covered, [0, 0, Math.round(geometry.surface[2]), Math.round(geometry.surface[3])], "the view replaces the chat surface");
  assert.equal(await page.evaluate(() => document.querySelector(".chat-surface .scroller").inert), true, "the chat is not reachable behind it");
  assert.equal(await page.evaluate(() => document.activeElement.classList.contains("xback")), true, "focus moves to Back");
  assert.equal(await page.locator(".xdialog, [aria-modal], .scrim").count(), 0, "no modal");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".xview").count(), 0, "Esc returns at once");
  assert.equal(await page.evaluate(() => document.querySelector(".chat-surface .scroller").scrollTop), before, "the same scroll");
  assert.equal(await page.evaluate(() => document.querySelector(".chat-surface .scroller").inert), false);
  assert.equal(await page.evaluate(() => document.activeElement.className.includes("xline")), true, "focus is back on the line");
  assert.deepEqual(page.errors, []);
  await page.close();
});

test("the view names both agents in the header and each author in its pet colour with the day and time", async () => {
  const page = await open();
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  assert.deepEqual(await page.locator(".xchip").locator(".xname, .xarrows").allTextContents(), ["CEO", "⇄", "CMO"], "CEO ⇄ CMO");
  assert.equal(await page.locator(".xchip .chat-face").count(), 2);
  const authors = await page.locator(".xmsg .xname").evaluateAll((els) => els.map((e) => [e.textContent, getComputedStyle(e).color]));
  assert.deepEqual(authors.map((a) => a[0]), ["CEO", "CMO", "CEO", "CMO"]);
  assert.equal(authors[1][1], "rgb(47, 111, 237)", "CMO's pet blue");
  assert.equal(authors[0][1], "rgb(29, 29, 27)", "CEO's pet ink");
  assert.equal(await page.locator(".xmsg .turn-author .chat-face").count(), 4, "each author has its pet");
  assert.equal(await page.locator(".xmsg .bubble").first().evaluate((el) => el.querySelector("strong")?.textContent), "Friday", "the text is the chat's markdown");
  const stamps = await page.locator(".xstamp").allInnerTexts();
  assert.equal(stamps.length, 2, "one stamp per gap of an hour or a day");
  assert.match(stamps[0], /^Yesterday \d/);
  assert.match(stamps[1], /^Today \d/);
  assert.equal(await page.locator(".xstamp").first().evaluate((el) => getComputedStyle(el).textAlign), "center");
  assert.equal(await page.locator(".xview textarea, .xview input").count(), 0, "read only");
  assert.equal(await page.locator(".xmsg.xnew").count(), 0, "nothing animates on the first open");
  await page.waitForTimeout(300);
  await shot(page, "view-light");
  await page.close();
});

test("a reply that never came says so, and a brief still waiting shows the typing mark", async () => {
  const page = await open();
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  assert.equal(await page.locator(".xmsg .xfail").innerText(), "Did not arrive");
  assert.equal(await page.locator(".xmsg .xfail").locator("xpath=ancestor::*[contains(@class,'xmsg')]").getAttribute("data-author"), "cmo");
  await page.keyboard.press("Escape");
  await page.locator(".xline").nth(1).click();
  await page.waitForSelector(".xwaiting");
  assert.equal(await page.locator(".xwaiting").getAttribute("data-author"), "cso", "the one asked is the one writing");
  assert.deepEqual(await page.locator(".xchip").locator(".xname, .xarrows").allTextContents(), ["CEO", "⇄", "CSO", "CMO"]);
  await page.close();
});

test("a brief the chat's agent was sent shows the chat's agent writing, not the sender", async () => {
  const page = await open({ setup: `window.state.talks.cmo.push({ author: "cmo", text: "Can you approve the budget?", at_ms: Date.now() - 1000, work: "w9", pending: true });` });
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xwaiting");
  assert.equal(await page.locator(".xwaiting").getAttribute("data-author"), "ceo", "the CEO has to answer");
  await page.close();
});

test("a read that fails while messages are shown keeps them", async () => {
  const page = await open();
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  const count = await page.locator(".xmsg").count();
  await page.evaluate(() => { window.state.fail = true; window.emitOrch(); });
  await page.waitForFunction(() => window.state.reads.filter((r) => r[0] === "exchange_talk").length >= 2, null, { timeout: 4000 });
  await page.waitForTimeout(150);
  assert.equal(await page.locator(".xmsg").count(), count, "the thread stays");
  assert.equal(await page.getByText("Could not load the exchange.").count(), 0);
  await page.evaluate(() => { window.state.fail = false; });
  await page.close();
});

test("a failed reply reads as a line that did not arrive, with no runtime words", async () => {
  const page = await open({ setup: `window.state.lines[0] = { turn: "t1", with: [{ agent: "cmo", name: "CMO", look: "blue" }], count: 2, last_ms: 1, failed: true, label: "Reply from CMO did not arrive", parts: [{ text: "Reply from " }, { with: 0 }, { text: " did not arrive" }] };` });
  const text = await words(page.locator(".xline").first());
  assert.equal(text, "Reply from CMO did not arrive");
  for (const word of ["task", "tool", "session", "agents.", "delegate"]) assert.ok(!text.toLowerCase().includes(word));
  await page.close();
});

test("the open animation slides 8px with opacity in 200ms, and reduced motion keeps only the opacity", async () => {
  const page = await open();
  const play = async () => {
    await page.locator(".xline").first().click();
    return page.evaluate(() => {
      const a = document.querySelector(".xview").getAnimations()[0];
      const cs = getComputedStyle(document.querySelector(".xview"));
      return { name: a?.animationName, duration: cs.animationDuration, easing: cs.animationTimingFunction };
    });
  };
  const full = await play();
  assert.equal(full.name, "xin");
  assert.equal(full.duration, "0.2s");
  assert.equal(full.easing, "cubic-bezier(0.23, 1, 0.32, 1)");
  const keyframes = await page.evaluate(() => [...document.styleSheets].flatMap((s) => [...s.cssRules]).filter((r) => r.name === "xin").map((r) => r.cssText)[0]);
  assert.match(keyframes, /translateX\(8px\)/);
  await page.close();
  const quiet = await open({ reduced: true });
  await quiet.locator(".xline").first().click();
  const name = await quiet.evaluate(() => document.querySelector(".xview").getAnimations()[0]?.animationName);
  assert.equal(name, "xfade", "opacity only");
  await quiet.close();
});

test("a new message in the open view rises in with opacity and 4px, the old ones stay", async () => {
  const page = await open();
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  const count = await page.locator(".xmsg").count();
  await page.evaluate(() => {
    window.state.talks.cmo.push({ author: "cmo", text: "Posted it.", at_ms: Date.now(), work: "w4" });
    window.emitOrch();
  });
  await page.waitForFunction((n) => document.querySelectorAll(".xmsg").length === n + 1, count, { timeout: 4000 });
  const fresh = await page.locator(".xmsg.xnew").evaluateAll((els) => els.map((e) => [e.textContent.includes("Posted it."), getComputedStyle(e).animationName, getComputedStyle(e).animationDuration]));
  assert.deepEqual(fresh, [[true, "xrise", "0.18s"]]);
  const rule = await page.evaluate(() => [...document.styleSheets].flatMap((s) => [...s.cssRules]).filter((r) => r.name === "xrise").map((r) => r.cssText)[0]);
  assert.match(rule, /translateY\(4px\)/);
  await page.close();
});

test("the header shows at once while the messages load, a failed load offers Retry, and nothing is a modal", async () => {
  const page = await open({ setup: "window.state.delay = 0;" });
  await page.evaluate(() => { window.state.delay = 600; });
  await page.locator(".xline").first().click();
  assert.equal(await page.locator(".xchip").isVisible(), true, "the header is there before any message");
  assert.equal(await page.locator(".xview").getAttribute("data-state"), "loading");
  assert.equal(await page.locator(".xmsg").count(), 0);
  await page.waitForSelector(".xmsg");
  await page.keyboard.press("Escape");
  await page.evaluate(() => { window.state.delay = 0; window.state.fail = true; });
  await page.locator(".xline").first().click();
  await page.getByText("Could not load the exchange.").waitFor();
  await page.evaluate(() => { window.state.fail = false; });
  await page.getByRole("button", { name: "Retry" }).click();
  await page.waitForSelector(".xmsg");
  assert.equal(await page.getByText("Could not load the exchange.").count(), 0);
  await page.getByRole("button", { name: "Back" }).click();
  assert.equal(await page.locator(".xview").count(), 0, "Back returns to the chat");
  await page.close();
});

test("no exchange: no line, and an empty exchange says so", async () => {
  const page = await open({ mount: false, wait: false });
  await page.evaluate(() => { window.state.lines = []; window.state.reads.length = 0; window.mount(); });
  await page.waitForSelector(".turn");
  await page.waitForFunction(() => window.state.reads.some((r) => r[0] === "exchange_lines"));
  await page.waitForTimeout(100);
  assert.equal(await page.locator(".xline").count(), 0);
  await page.close();
  const empty = await open({ setup: "window.state.talks.cmo = [];" });
  await empty.locator(".xline").first().click();
  await empty.getByText("No messages yet.").waitFor();
  await empty.close();
});

test("a turn that gets its line later fades it in, the first read does not", async () => {
  const page = await open({ mount: false, wait: false });
  await page.evaluate(() => { window.state.lines = [window.state.lines[0]]; window.mount(); });
  await page.waitForSelector(".xline");
  assert.equal(await page.locator(".xline.xnew").count(), 0, "first read: no motion");
  await page.evaluate(() => {
    window.state.lines.push({ turn: "t3", with: [{ agent: "cso", name: "CSO", look: "green" }], count: 1, last_ms: 1, failed: false, label: "Messaged CSO", parts: [{ text: "Messaged " }, { with: 0 }] });
    window.emitOrch();
  });
  await page.waitForFunction(() => document.querySelectorAll(".xline").length === 2, null, { timeout: 4000 });
  assert.equal(await page.locator(".xline.xnew").count(), 1);
  assert.equal(await page.locator(".xline.xnew").evaluate((el) => getComputedStyle(el).animationName), "xfade");
  await page.close();
});

test("320px: nothing scrolls sideways, the line wraps, and a touch screen gets 44px targets", async () => {
  const page = await open({ width: 320, height: 640, touch: true });
  const line = await page.locator(".xline").nth(1).evaluate((el) => ({ h: el.getBoundingClientRect().height, right: el.getBoundingClientRect().right, doc: document.documentElement.scrollWidth }));
  assert.ok(line.h >= 44, `touch height ${line.h}`);
  assert.ok(line.right <= 320 && line.doc <= 320);
  await shot(page, "line-320");
  await page.locator(".xline").nth(1).tap();
  await page.waitForSelector(".xmsg");
  await page.waitForTimeout(300);
  const back = await page.locator(".xback").evaluate((el) => { const r = el.getBoundingClientRect(); return [r.width, r.height]; });
  assert.ok(back[0] >= 44 && back[1] >= 44, `back ${back}`);
  const wide = await page.evaluate(() => [document.querySelector(".xscroll").scrollWidth, document.querySelector(".xscroll").clientWidth, document.documentElement.scrollWidth]);
  assert.ok(wide[0] <= wide[1] && wide[2] <= 320, `no sideways scroll ${wide}`);
  await page.waitForTimeout(300);
  await shot(page, "view-320");
  await page.close();
});

test("dark: the line and the view take their colours from the tokens", async () => {
  const page = await open({ theme: "dark" });
  const text2 = await page.locator(".xline").first().evaluate((el) => getComputedStyle(el).color);
  assert.equal(text2, "rgb(168, 168, 165)", "text-2 at rest in dark");
  await shot(page, "line-dark");
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  const colours = await page.evaluate(() => ({ view: getComputedStyle(document.querySelector(".xview")).backgroundColor, stamp: getComputedStyle(document.querySelector(".xstamp")).color, pet: getComputedStyle(document.querySelectorAll(".xmsg .xname")[1]).color }));
  assert.equal(colours.view, "rgb(27, 27, 27)");
  assert.equal(colours.stamp, "rgb(111, 111, 108)");
  assert.equal(colours.pet, "rgb(143, 180, 255)");
  await page.waitForTimeout(300);
  await shot(page, "view-dark");
  await page.close();
});

test("two reads in flight: the one started last wins, an older one landing later changes nothing", async () => {
  const page = await open();
  await page.evaluate(() => { window.state.delays = [1800]; });
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xchip");
  await page.evaluate(() => {
    window.state.delays = [];
    window.state.talks.cmo.push({ author: "cmo", text: "Newest word.", at_ms: Date.now(), work: "w7" });
    window.emitOrch();
  });
  await page.waitForFunction(() => document.body.textContent.includes("Newest word."), null, { timeout: 4000 });
  await page.waitForTimeout(1200);
  assert.equal(await page.getByText("Newest word.").count(), 1, "the old read, landing last, did not take it away");
  assert.deepEqual(page.errors, []);
  await page.close();
});

test("two identical messages in one millisecond are two rows", async () => {
  const page = await open({ setup: `const same = { author: "cmo", text: "Noted.", at_ms: Date.now() - 5000, work: "w8" }; window.state.talks.cmo.push(same, { ...same });` });
  const errors = [];
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  assert.equal(await page.locator(".xmsg").filter({ hasText: "Noted." }).count(), 2);
  assert.deepEqual(errors, []);
  await page.close();
});

test("reduced motion: a press does not scale the line or Retry", async () => {
  const page = await open({ reduced: true });
  const line = page.locator(".xline").first();
  await line.hover();
  await page.mouse.down();
  await page.waitForTimeout(250);
  assert.ok(["1", "none"].includes(await line.evaluate((el) => getComputedStyle(el).scale)), "line");
  await page.mouse.up();
  await page.keyboard.press("Escape");
  await page.evaluate(() => { window.state.fail = true; });
  await line.click();
  await page.getByRole("button", { name: "Retry" }).waitFor();
  const retry = page.getByRole("button", { name: "Retry" });
  await retry.hover();
  await page.mouse.down();
  await page.waitForTimeout(250);
  assert.ok(["1", "none"].includes(await retry.evaluate((el) => getComputedStyle(el).scale)), "retry");
  await page.mouse.up();
  await page.close();
});

test("the option as a function: read at each use, null makes no call, refreshExchanges follows a switch", async () => {
  const page = await open({ mount: false, wait: false });
  const calls = () => page.evaluate(() => window.state.reads.filter((r) => r[0].startsWith("exchange_")).map((r) => [r[0], r[1].agent]));
  // Null: no lines and no window call, also when events come.
  await page.evaluate(() => { window.saved = structuredClone(window.state.lines); window.target = null; window.mountWith(() => window.target); });
  await page.waitForSelector(".turn");
  await page.evaluate(() => window.emitOrch());
  await page.waitForTimeout(1300);
  assert.equal(await page.locator(".xline").count(), 0);
  assert.deepEqual(await calls(), [], "no call for a null descriptor");
  // The app gives an agent and tells the view: lines come, for that agent.
  await page.evaluate(() => { window.target = { project: "p", agent: "ceo", name: "CEO", look: "ink" }; return window.view.refreshExchanges(); });
  await page.waitForSelector(".xline");
  assert.deepEqual((await calls()).at(-1), ["exchange_lines", "ceo"]);
  assert.equal(await page.locator(".xline.xnew").count(), 0, "the first read after a switch has no motion");
  // Another agent: the old lines go and the new agent is asked; an open view of the old one closes.
  await page.locator(".xline").first().click();
  await page.waitForSelector(".xmsg");
  await page.evaluate(() => { window.state.lines = []; window.target = { project: "p", agent: "cmo", name: "CMO", look: "blue" }; return window.view.refreshExchanges(); });
  assert.equal(await page.locator(".xview").count(), 0, "the view of the other chat closed");
  await page.waitForFunction(() => document.querySelectorAll(".xline").length === 0);
  assert.deepEqual((await calls()).at(-1), ["exchange_lines", "cmo"]);
  // None again: lines gone, no further call.
  const before = (await calls()).length;
  await page.evaluate(() => { window.target = null; return window.view.refreshExchanges(); });
  await page.evaluate(() => window.emitOrch());
  await page.waitForTimeout(1300);
  assert.equal((await calls()).length, before);
  // The object form still works.
  await page.evaluate(() => { window.state.lines = window.saved; window.mount(); });
  await page.waitForSelector(".xline");
  assert.deepEqual(page.errors, []);
  await page.close();
});
