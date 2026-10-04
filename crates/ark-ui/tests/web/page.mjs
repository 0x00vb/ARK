// Drives the real ark-ui page in WebKit (the engine behind the macOS desktop window) against a
// host that `tests/page.rs` starts with a scripted mock model.
//
//   node page.mjs <scenario> <url>
//
// Each scenario guards a rendering bug found while dogfooding. Exit code 0 means it held.

import { webkit } from "playwright-core";
import assert from "node:assert/strict";

const [scenario, url] = process.argv.slice(2);

// What a turn shows, in order, as plain strings. Replayed and live turns must agree on this.
async function shape(page) {
  return page.evaluate(() => {
    const turn = [...document.querySelectorAll(".turn")].pop();
    const out = [];
    for (const el of turn.querySelector(".agent").children) {
      const cls = el.classList;
      if (cls.contains("working-line") || cls.contains("turn-foot")) continue;
      const text = el.innerText.trim().replace(/\s+/g, " ");
      if (cls.contains("steered")) out.push(`steer: ${text}`);
      else if (cls.contains("answer")) out.push(`answer: ${text}`);
      else if (cls.contains("prose")) out.push(`text: ${text}`);
      else if (cls.contains("work")) out.push(`work: ${el.querySelector(".work-summary").innerText.trim()}`);
      else out.push(`${el.className}: ${text}`);
    }
    return out;
  });
}

async function finished(page, send) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await send();
  await done;
  // The page redraws streamed text on the next frame.
  await page.waitForTimeout(150);
}

const scenarios = {
  // Narration written just before a tool call keeps its last chunk, and the answer shows once.
  async stream(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    assert.deepEqual(live, [
      "text: Reading the notes now.",
      "work: Reading notes",
      "answer: The notes mention a locket.",
    ]);
    // The head shows through an attribute, not :has(:empty), which the macOS web view does
    // not re-evaluate when the summary text arrives.
    assert.equal(await page.locator(".work[data-summary] > .work-head").isVisible(), true);
  },

  // A row that arrives after the streamed answer (a failed self-check) does not make the page
  // draw the answer a second time.
  async selfcheck(page) {
    // Long enough (over 14 words) that the runtime does not treat it as a trivial question.
    const ask = "Go through all of the notes carefully and tell me every object that they mention, in the order it appears.";
    await finished(page, () => page.locator("#input").fill(ask).then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, live.join("\n"));
    assert.ok(live.some((s) => s.startsWith("work: ") && s.includes("ungrounded")), live.join("\n"));
    const agent = await page.locator(".turn .agent").last().innerText();
    assert.equal(agent.split("a locket").length - 1, 1, agent);
  },

  // A stopped turn says so once, and replays as it was shown live.
  async cancel(page) {
    await finished(page, async () => {
      await page.locator("#input").fill("What do the notes say?");
      await page.keyboard.press("Enter");
      await page.locator(".prose.streaming").waitFor();
      await page.keyboard.press("Escape");
    });
    const live = await shape(page);
    assert.equal(live.filter((s) => s.includes("Stopped")).length, 1, live.join("\n"));
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    assert.deepEqual(await shape(page), live);
  },

  // A narrow window folds the sidebar into a drawer the reveal button opens; it closes on a
  // pick, and the width never scrolls sideways.
  async narrow(page) {
    await page.setViewportSize({ width: 720, height: 800 });
    // Wait for the layout to switch rather than a fixed time (flaky on a busy machine).
    await page.waitForFunction(() => document.getElementById("app").dataset.compact === "true");
    const state = () => page.evaluate(() => ({
      compact: document.getElementById("app").dataset.compact,
      drawer: document.getElementById("app").dataset.drawer || "",
      sidebarLeft: Math.round(document.querySelector(".sidebar").getBoundingClientRect().left),
      hscroll: document.documentElement.scrollWidth > innerWidth,
    }));
    assert.equal((await state()).compact, "true");
    assert.ok(await page.locator("#reveal-sidebar").isVisible(), "the sidebar can be reached");
    await page.click("#reveal-sidebar");
    await page.waitForTimeout(350);
    assert.deepEqual(await state(), { compact: "true", drawer: "open", sidebarLeft: 0, hscroll: false });
    await page.click("#new-session");
    await page.waitForTimeout(350);
    const closed = await state();
    assert.equal(closed.drawer, "closed");
    assert.ok(closed.sidebarLeft < 0, JSON.stringify(closed));
    await page.setViewportSize({ width: 1200, height: 800 });
    await page.waitForTimeout(100);
    assert.deepEqual(await state(), { compact: "false", drawer: "", sidebarLeft: 0, hscroll: false });
  },

  // The model chip names the model only. Its menu groups models by backend: a signed-in
  // subscription offers its models, a missing one shows the command that fixes it.
  async models(page) {
    const chip = page.locator("#model-chip");
    assert.ok(!(await chip.textContent()).includes("single tier"), await chip.textContent());
    await chip.click();
    await page.waitForSelector(".model-group .model-name");
    const menu = await page.locator(".popover").textContent();
    assert.ok(!menu.includes("Model for the next turn"), menu);
    const claude = page.locator(".model-group", { hasText: "Claude subscription" });
    assert.equal(await claude.locator(".model-dot").getAttribute("data-ok"), "true");
    const codex = page.locator(".model-group", { hasText: "ChatGPT subscription" });
    assert.equal(await codex.locator(".model-dot").getAttribute("data-ok"), "false");
    assert.ok((await codex.locator(".model-fix code").textContent()).includes("npm install"));
    await claude.locator(".model-item", { hasText: "opus" }).click();
    await page.waitForFunction(() => document.getElementById("model-chip").textContent === "claude opus");
  },

  // A message sent mid-turn waits above the composer, joins the thread where the agent read
  // it, and a reload replays the turn exactly as it was shown live.
  async steer(page) {
    await finished(page, async () => {
      await page.locator("#input").fill("What do the notes say?");
      await page.keyboard.press("Enter");
      await page.locator(".prose.streaming").waitFor();
      await page.locator("#input").fill("Also check for a key.");
      await page.keyboard.press("Meta+Enter");
      await page.locator(".queued.steer").waitFor();
      // Streaming continues in one piece while the message waits.
      assert.equal(await page.locator(".turn .agent .steered").count(), 0);
    });
    const live = await shape(page);
    assert.equal(await page.locator(".queued.steer").count(), 0, "the pending steer was cleared");
    const at = (prefix) => live.findIndex((s) => s.startsWith(prefix));
    assert.ok(at("text: Reading the notes first") === 0, live.join("\n"));
    assert.ok(at("steer: Also check for a key.") > 0, live.join("\n"));
    assert.ok(at("answer: ") > at("steer: "), live.join("\n"));
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, live.join("\n"));

    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    const stored = await shape(page);
    assert.deepEqual(stored, live);
  },

  // A window that reloads in the middle of a turn rebuilds the turn from its events so far and
  // then follows it: the narration shows once and whole, the answer once.
  async rejoin(page) {
    await page.locator("#input").fill("What do the notes say?");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(900);
    await page.reload();
    await page.locator("#input").waitFor();
    await page.locator(".answer", { hasText: "The notes mention a locket." }).waitFor({ timeout: 20000 });
    await page.waitForTimeout(250);
    const after = await shape(page);
    assert.deepEqual(after, [
      `text: ${NARRATION}`,
      "work: Reading notes",
      "answer: The notes mention a locket.",
    ]);
    assert.equal(await page.locator(".turn").count(), 1, "one turn, not a copy");
  },

  // A stream that keeps failing: reconnects back off (1 s doubling to 30 s), stop after ten,
  // and say so with a way to try again.
  async backoff(page) {
    await page.waitForFunction(() => window.__es.opened >= 11, null, { timeout: 10000 });
    await page.waitForTimeout(200);
    const { opened, delays } = await page.evaluate(() => window.__es);
    assert.equal(opened, 11, "the first try and ten reconnects");
    assert.equal(delays.length, 10, delays.join(","));
    delays.forEach((d, i) => {
      const cap = Math.min(30000, 1000 * 2 ** i);
      assert.ok(d >= cap / 2 && d <= cap, `delay ${i} = ${d}, cap ${cap}`);
    });
    const toast = page.locator("#toast");
    assert.match(await toast.innerText(), /Disconnected/);
    await toast.locator("button", { hasText: "Retry" }).click();
    await page.waitForFunction(() => window.__es.opened >= 12, null, { timeout: 5000 });
  },
};

// An app view marked `nav` is a tab in the top bar: it replaces the conversation, and the "Chat"
// tab gives it back. An orchestrated agent's question waits in the tray outside any turn, and
// esc does not deny it.
async function agents(page) {
  await page.evaluate(() => window.ark.view({ id: "board", title: "Board", nav: true, render: (el) => el.append("the board") }));
  const tab = page.locator('.view-tab[data-view="board"]');
  await tab.click();
  assert.equal(await page.locator("#stage").isVisible(), true);
  assert.equal(await page.locator("#scroller").isVisible(), false);
  // The tab names the view, so the title names the project.
  const project = await page.evaluate(async () => (await window.ark.call("projects")).find((p) => p.current).name);
  assert.equal(await page.locator("#session-title").innerText(), project);
  assert.equal(await tab.getAttribute("aria-selected"), "true");

  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:orch", (m) => m.event.type === "task_finished" && ok(m.event.state))));
  await page.evaluate(async () => {
    await window.ark.call("spawn_agent", { spec: { name: "scribe", tools: ["notes.*"] } });
    await window.ark.call("submit_task", { agent: "scribe", task: { objective: "save the notes" } });
  });
  const card = page.locator(".asks .approval");
  await card.waitFor();
  assert.match(await card.innerText(), /scribe wants to:/);
  await page.keyboard.press("Escape");
  await page.waitForTimeout(200);
  assert.equal(await card.isVisible(), true, "esc leaves an agent's question alone");
  await card.locator("button", { hasText: "Allow once" }).click();
  assert.equal(await done, "done");
  await card.waitFor({ state: "detached" });

  await page.locator('.view-tab[data-view=""]').click();
  assert.equal(await page.locator("#stage").isVisible(), false);
  assert.equal(await page.locator("#scroller").isVisible(), true);
  assert.equal(await tab.getAttribute("aria-selected"), "false");
}
scenarios.agents = agents;

// The + menu opens the built-in panels. Files edits and saves a file in the project folder;
// the terminal runs a shell there.
async function workbench(page) {
  await page.evaluate(() => window.ark.call("file_write", { path: "draft.md", text: "one" }));
  await page.locator("#open-menu").click();
  const items = page.locator(".popover .open-item");
  assert.deepEqual(await items.allInnerTexts().then((t) => t.map((x) => x.split("\n")[0].trim())), ["Terminal", "Browser", "Files"]);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.locator(".wb-entry", { hasText: "draft.md" }).click();
  const editor = page.locator(".wb-editor");
  assert.equal(await editor.inputValue(), "one");
  assert.equal(await page.locator(".wb-save").isDisabled(), true);
  await editor.fill("two");
  await page.locator(".wb-save").click();
  await page.waitForFunction(async () => (await window.ark.call("file_read", { path: "draft.md" })).text === "two");
  await assert.rejects(page.evaluate(() => window.ark.call("file_read", { path: "../outside" })));

  await page.evaluate(() => window.ark.openPanel("terminal"));
  // A shell under load can miss a line typed before its prompt: type it again until it answers.
  const term = page.locator(".xterm-helper-textarea");
  await term.waitFor({ state: "attached" });
  const answer = page.locator(".xterm-rows", { hasText: "ark-42" });
  for (let i = 0; ; i++) {
    await term.type("echo ark-$((40+2))\n");
    try {
      await answer.waitFor({ timeout: 5000 });
      break;
    } catch (e) {
      if (i === 3) throw e;
    }
  }
}
scenarios.workbench = workbench;

// An app's Settings sections come first, ARK's own after; advanced ones (the app's, then
// ARK's Skills and Tool servers) under Advanced. A section added while Settings is open joins
// the list. Appearance › Dark darkens the page and is remembered. `ark.compose` fills the
// composer from any view and sends nothing.
async function settings(page) {
  // ARK's own sections register once the page is connected: wait for the Settings entry.
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.setting({ id: "keys", title: "Keys", render: (el) => el.append("the keys") }));
  await page.locator("#open-settings").click();
  assert.equal(await page.locator(".settings-title").innerText(), "Keys", "the app's section opens first");
  assert.equal(await page.locator(".settings-section").innerText(), "the keys");
  const rows = page.locator(".settings-list .nav-row");
  await rows.nth(2).waitFor();
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Appearance", "Capabilities", "Permissions", "Skills", "Tool servers"]);

  await page.evaluate(() => window.ark.setting({ id: "look", title: "Look", render: (el) => el.append("the look") }));
  await page.evaluate(() => window.ark.setting({ id: "debug", title: "Debug", advanced: true, render: (el) => el.append("the debug") }));
  await page.evaluate(() => window.ark.openSettings("keys"));
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Look", "Appearance", "Capabilities", "Permissions", "Debug", "Skills", "Tool servers"]);
  assert.deepEqual(await page.locator(".settings-list > *").evaluateAll((els) => els.map((e) => e.textContent).slice(4, 6)), ["Permissions", "Advanced"]);

  // The test page opens light (no app default, a light system); Dark is kept across a reload.
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "light");
  await page.locator(".settings-list .nav-row", { hasText: "Appearance" }).click();
  await page.locator(".settings-section .seg", { hasText: "Dark" }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark");
  const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  assert.equal(await bg(), "rgb(27, 27, 27)");
  await page.reload();
  await page.locator("#open-settings:not([hidden])").waitFor();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark");
  await page.evaluate(() => window.arkTheme.set("system"));
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "light");
  await page.evaluate(() => window.ark.setting({ id: "look", title: "Look", render: (el) => el.append("the look") }));
  await page.evaluate(() => window.ark.openSettings("look"));
  await page.locator(".settings-list .nav-row", { hasText: "Look" }).click();
  assert.equal(await page.locator(".settings-title").innerText(), "Look");
  assert.equal(await page.locator(".settings-section").innerText(), "the look");

  await page.evaluate(() => window.ark.compose("Set up a reviewer"));
  assert.equal(await page.locator("#scroller").isVisible(), true, "the conversation is in view");
  assert.equal(await page.locator("#input").inputValue(), "Set up a reviewer");
  await page.waitForTimeout(200);
  assert.equal(await page.locator(".user").count(), 0, "nothing was sent");
}
scenarios.settings = settings;

// An app that draws the model picker: until a model is chosen the button asks for one, and
// sending opens the menu with the text kept. `ark.onSubmit` sees each message first and can
// take it; one it lets through is sent.
async function picker(page) {
  await page.evaluate(() => {
    window.picked = null;
    window.taken = [];
    window.ark.modelPicker({
      label: (model) => (window.picked ? `Picked ${window.picked}` : { text: "Choose model", missing: true }),
      render: (el, { close }) => el.append(window.ark.h("button.pick-m", { onclick: () => { window.picked = "m"; close(); window.ark.refreshModel(); } }, "m")),
    });
    window.ark.onSubmit((text) => {
      if (!text.startsWith("@")) return false;
      window.taken.push(text);
      return true;
    });
  });
  const chip = page.locator("#model-chip");
  await page.waitForFunction(() => document.getElementById("model-chip").textContent === "Choose model");
  assert.equal(await chip.getAttribute("data-missing"), "true");

  await page.locator("#input").fill("What do the notes say?");
  await page.keyboard.press("Enter");
  await page.locator(".popover .pick-m").waitFor();
  assert.equal(await page.locator("#input").inputValue(), "What do the notes say?", "the text waits");
  assert.equal(await page.locator(".user").count(), 0, "nothing was sent");

  await page.locator(".popover .pick-m").click();
  assert.equal(await chip.textContent(), "Picked m");
  assert.equal(await chip.getAttribute("data-missing"), "false");

  await page.locator("#input").fill("@writer draft it");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.taken.length === 1);
  assert.equal(await page.locator(".user").count(), 0, "the app took it");

  await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
  assert.equal(await page.locator(".user").count(), 1);
}
scenarios.picker = picker;

// Settings › Tool servers: an MCP server is added from a typed command, shows its tools,
// joins the app's tools, and is removed with a second click. One that cannot start says why.
async function plugins(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings("plugins"));
  await page.locator(".settings-title", { hasText: "Tool servers" }).waitFor();
  assert.equal(await page.locator(".plugin-list").innerText(), "No servers yet.");

  const input = page.locator(".plugin-input");
  await input.fill(`echo python3 "${process.env.ARK_TEST_MCP}"`);
  await page.locator(".plugin-add .btn.primary").click();
  const row = page.locator('.plugin-row[data-name="echo"]');
  await row.waitFor();
  assert.equal(await row.locator(".plugin-detail").innerText(), "2 tools: echo, stamp");
  assert.equal(await input.inputValue(), "", "the field empties once added");
  const tools = await page.evaluate(async () => (await window.ark.call("tools")).map((t) => t.name));
  assert.ok(tools.includes("echo.echo"), tools.join(","));

  await input.fill("bad /no/such/server");
  await page.locator(".plugin-add .btn.primary").click();
  await page.locator(".plugin-why", { hasText: "did not start" }).waitFor();
  assert.equal(await page.locator(".plugin-row").count(), 1, "a server that failed is not listed");

  const remove = row.locator(".plugin-remove");
  await remove.click();
  assert.equal(await remove.innerText(), "Remove?");
  await remove.click();
  await row.waitFor({ state: "detached" });
  assert.equal(await page.locator(".plugin-list").innerText(), "No servers yet.");
}
scenarios.plugins = plugins;

// Settings › Capabilities: Web is ready, Mail and Calendar are one sign-in away. Connect opens
// the provider's page (here it approves at once), the row turns ready with the account, and
// Disconnect (two clicks) makes it connectable again. `tools` then lists the web too.
async function capabilities(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings("capabilities"));
  await page.locator(".settings-title", { hasText: "Capabilities" }).waitFor();
  const row = (id) => page.locator(`.capability-row[data-id="${id}"]`);
  await row("mail").waitFor();
  assert.deepEqual(await page.locator(".capability-row .plugin-name").allInnerTexts(), ["Web", "Mail", "Calendar"]);
  assert.equal(await row("web").getAttribute("class"), "plugin-row capability-row");
  assert.match(await row("mail").getAttribute("class"), /off/);

  await row("mail").locator(".btn.primary").click();
  await page.waitForFunction(() => !document.querySelector('.capability-row[data-id="mail"]')?.classList.contains("off"), null, { timeout: 15000 });
  assert.match(await row("mail").locator(".plugin-detail").innerText(), /ada@example\.com/);
  assert.match(await row("calendar").getAttribute("class"), /off/, "Calendar needs its own scope");

  const off = row("mail").locator(".plugin-remove");
  await off.click();
  assert.equal(await off.innerText(), "Disconnect Google?");
  await off.click();
  await page.waitForFunction(() => document.querySelector('.capability-row[data-id="mail"]')?.classList.contains("off"));

  const tools = await page.evaluate(async () => (await window.ark.call("tools")).map((t) => t.name));
  assert.ok(tools.includes("web.search"), tools.join(","));
}
scenarios.capabilities = capabilities;

// An agent asks for Mail mid-task: the card names the need and offers Connect or Not now;
// connecting signs in (the test plays the browser) and the turn goes on to the send, which
// asks as usual, then answers.
async function connectMidTask(page) {
  await page.locator("#input").fill("Email Bob that the report is ready");
  await page.keyboard.press("Enter");
  const card = page.locator(".approval").first();
  await card.waitFor();
  assert.equal(await card.locator(".approval-title").innerText(), "ARK needs this to finish:");
  assert.match(await card.locator(".approval-summary").innerText(), /^Connect Google so ARK can send email from your account/);
  const buttons = (await card.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
  assert.deepEqual(buttons, ["Connect", "Not now"]);
  await card.locator('button[data-decision="allow_once"]').click();
  const send = page.locator(".approval:not(.decided)");
  await send.waitFor({ timeout: 15000 });
  assert.match(await send.locator(".approval-line").innerText(), /mail\.send|Send|bob@example\.com/i);
  await send.locator('button[data-decision="allow_once"]').click();
  await page.locator(".agent", { hasText: "Sent the report to Bob." }).waitFor({ timeout: 15000 });
}
scenarios["connect-mid-task"] = connectMidTask;

// An approval offers "Always in this project". After it, the same action runs without a card,
// Settings › Permissions lists the grant, and removing it brings the card back.
async function grants(page) {
  const ask = (text) => page.locator("#input").fill(text).then(() => page.keyboard.press("Enter"));
  await finished(page, async () => {
    await ask("Save the notes");
    const card = page.locator(".approval").first();
    await card.waitFor();
    const buttons = (await card.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
    assert.deepEqual(buttons, ["Allow once", "Allow for this session", "Always in this project", "Deny"]);
    await card.locator('button[data-decision="allow_always"]').click();
  });
  const cards = await page.locator(".approval").count();
  await finished(page, () => ask("Save them again"));
  assert.equal(await page.locator(".approval").count(), cards, "the second save asked nothing");
  await page.locator(".agent", { hasText: "Saved again." }).waitFor();

  await page.evaluate(() => window.ark.openSettings("permissions"));
  const row = page.locator('.grant-row[data-class="notes.save"]');
  await row.waitFor();
  await row.locator(".plugin-remove").click();
  await row.waitFor({ state: "detached" });
  await page.evaluate(() => window.ark.openSettings("permissions"));

  await page.locator("#open-settings").waitFor();
  await page.keyboard.press("Escape");
  await page.evaluate(() => window.ark.compose(""));
  await ask("And once more");
  await page.locator(".approval:not(.decided)").waitFor({ timeout: 15000 });
  await page.locator('.approval:not(.decided) button[data-decision="allow_once"]').click();
}
scenarios.grants = grants;

// Settings › Skills lists the store's skill, and Add opens the Terminal in the project folder
// with the install command typed, unsent.
async function skills(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings("skills"));
  const row = page.locator('.skill-row[data-name="release-notes"]');
  await row.waitFor();
  assert.match(await row.innerText(), /All projects/);
  assert.match(await row.innerText(), /Write release notes/);
  await page.locator(".skill-pkg").fill("vercel-labs/agent-skills");
  await page.locator(".skill-add .btn.primary").click();
  await page.locator(".xterm-rows", { hasText: "npx skills add vercel-labs/agent-skills" }).waitFor();
}
scenarios.skills = skills;

const NARRATION = "Reading the notes slowly, one line at a time, so that a window can reload in between. ".repeat(3).trim();

// Scripts that run before the page, for scenarios that replace a browser API.
const inits = {
  // An EventSource that always fails, and timers that fire at once (recording how long the
  // page asked to wait) so ten backoffs take milliseconds.
  backoff: () => {
    window.__es = { opened: 0, delays: [] };
    const realTimeout = window.setTimeout.bind(window);
    window.setTimeout = (fn, ms, ...a) => {
      // Only the transport's reconnect waits are compressed and recorded.
      const caller = (new Error().stack || "").split("\n")[1] || "";
      if (ms >= 500 && caller.includes("transport.js")) {
        window.__es.delays.push(ms);
        return realTimeout(fn, 1, ...a);
      }
      return realTimeout(fn, ms, ...a);
    };
    window.EventSource = class {
      constructor() {
        window.__es.opened += 1;
        realTimeout(() => this.onerror && this.onerror(new Event("error")), 1);
      }
      close() {}
    };
  },
};

const run = scenarios[scenario];
if (!run) {
  console.error(`unknown scenario ${scenario}`);
  process.exit(2);
}
const browser = await webkit.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  if (inits[scenario]) await page.addInitScript(inits[scenario]);
  await page.goto(url);
  await page.locator("#input").waitFor();
  await run(page);
  assert.deepEqual(errors, [], "the page threw");
  console.log(`${scenario}: ok`);
} catch (e) {
  console.error(`${scenario}: ${e.stack || e}`);
  process.exitCode = 1;
} finally {
  await browser.close();
}
