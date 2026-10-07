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

// A picture of the page for a person to check, when `ARK_UI_SHOTS` names a folder.
async function shot(page, name) {
  const dir = process.env.ARK_UI_SHOTS;
  if (dir) await page.screenshot({ path: `${dir}/${name}.png` });
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

  // A model call that breaks off after some text is tried again: its text leaves the page, the
  // retry's shows once, and the turn replays as it was shown (ADR 0049).
  async withdrawn(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    const all = live.join("\n");
    assert.ok(!all.includes("then I wi"), all);
    assert.equal(live.filter((s) => s === "text: Reading the notes now.").length, 1, all);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, all);
    assert.ok(all.includes("retrying"), all);
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    assert.deepEqual(await shape(page), live);
  },

  // A reply the model could not use is taken back and redone: one quiet row, no failure, the
  // answer once (ADR 0050).
  async repair(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    const all = live.join("\n");
    assert.ok(!all.includes("Writing the edit now."), all);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, all);
    const text = await page.locator(".turn").last().innerText();
    assert.ok(text.includes("Redoing its last step"), text);
    assert.ok(!/not JSON|malformed|retrying/i.test(text), text);
    await shot(page, "repair");
  },

  // One it never gets right ends the turn in plain words, with no runtime terms, and Continue.
  async unusable(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const text = await page.locator(".turn").last().innerText();
    assert.ok(text.includes("The model's reply could not be used."), text);
    assert.ok(!/not JSON|malformed|provider|column 1573/i.test(text), text);
    assert.equal((await page.locator(".turn .resume").innerText()).trim(), "Continue");
    await shot(page, "unusable");
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    const stored = await page.locator(".turn").last().innerText();
    assert.ok(stored.includes("The model's reply could not be used."), stored);
    assert.ok(!/not JSON|malformed|column 1573/i.test(stored), stored);
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

  // A turn a crash cut short (ADR 0048): reopened, it shows what it did, says it was
  // interrupted, and offers one Continue on the chat's last turn. Continue is the person's next
  // message; once it is sent the offer is gone.
  async interrupted(page) {
    await page.locator(".session", { hasText: "Read the notes, then summarize them" }).first().click();
    const resume = page.locator(".turn .resume");
    await resume.waitFor();
    assert.equal(await resume.count(), 1);
    assert.equal((await resume.innerText()).trim(), "Continue");
    const notice = await page.locator(".turn .notice").last().innerText();
    assert.equal(notice.trim(), "Interrupted before it finished.");
    assert.equal(await page.locator(".turn .tone-error").count(), 0, "not shown as a failure");
    assert.equal(await page.locator('.turn .act[data-status="ok"]').count(), 1, "the read is done");
    assert.equal(await page.locator(".turn .turn-foot").count(), 0, "no counts for a turn that kept none");
    await finished(page, () => resume.click());
    assert.equal(await page.locator(".turn .resume").count(), 0);
    const turns = page.locator(".turn");
    assert.equal(await turns.count(), 2);
    assert.match(await turns.last().innerText(), /Continue[\s\S]*Picked up where it stopped/);
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

// The project's board: the person adds, drags and completes cards and opens one; an agent
// given a card moves it by itself; what waits for the person is marked; delete has an undo.
async function board(page) {
  await page.locator('.view-tab[data-view="board"]').click();
  const server = () => page.evaluate(async () => (await window.ark.call("board")).items.map((w) => `${w.state}:${w.title}`));
  const column = (state) => page.locator(`.bd-list[data-state="${state}"] .bd-card .bd-title`).allInnerTexts();
  // An empty board offers the input at once.
  const input = page.locator(".bd-input");
  await input.click();
  for (const t of ["Draft chapter 3", "Edit chapter 2", "Pick a title"]) {
    // Typed, not filled: the box keeps the focus from one item to the next.
    await page.keyboard.type(t);
    await page.keyboard.press("Enter");
    await page.locator(".bd-card", { hasText: t }).waitFor();
    assert.ok(await page.evaluate(() => document.activeElement?.classList.contains("bd-input")), `the box kept the focus after "${t}"`);
  }
  await page.keyboard.press("Escape");
  assert.deepEqual(await column("todo"), ["Draft chapter 3", "Edit chapter 2", "Pick a title"]);

  // Drag "Pick a title" above "Draft chapter 3", then "Draft chapter 3" into In progress.
  const drag = async (title, to) => {
    const from = await page.locator(".bd-card", { hasText: title }).boundingBox();
    await page.mouse.move(from.x + 40, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(from.x + 50, from.y + from.height / 2 + 8, { steps: 3 });
    await page.mouse.move(to.x, to.y, { steps: 8 });
    assert.equal(await page.locator(".bd-line").count(), 1, "a line shows where it lands");
    await page.mouse.up();
  };
  const first = await page.locator(".bd-card", { hasText: "Draft chapter 3" }).boundingBox();
  await drag("Pick a title", { x: first.x + 60, y: first.y + 4 });
  assert.deepEqual(await column("todo"), ["Pick a title", "Draft chapter 3", "Edit chapter 2"]);
  const doing = await page.locator('.bd-list[data-state="doing"]').boundingBox();
  await drag("Draft chapter 3", { x: doing.x + 60, y: doing.y + 30 });
  assert.deepEqual(await column("doing"), ["Draft chapter 3"]);
  await page.waitForFunction(async () => (await window.ark.call("board")).items.some((w) => w.title === "Draft chapter 3" && w.state === "doing"));
  assert.deepEqual(await server(), ["todo:Pick a title", "todo:Edit chapter 2", "doing:Draft chapter 3"]);

  // The circle completes a card.
  await page.locator(".bd-card", { hasText: "Edit chapter 2" }).locator(".bd-check").click();
  assert.deepEqual(await column("done"), ["Edit chapter 2"]);

  // A click opens the card beside the board; a note typed there is kept.
  await page.locator(".bd-card", { hasText: "Draft chapter 3" }).click();
  const panel = page.locator(".bd-panel");
  await panel.waitFor();
  assert.equal(await panel.locator(".bd-p-title").inputValue(), "Draft chapter 3");
  await panel.locator(".bd-p-note").fill("Under 2,000 words.");
  await panel.locator(".bd-p-title").click();
  await page.waitForFunction(async () => (await window.ark.call("board")).items.some((w) => w.note === "Under 2,000 words."));

  // Given to an agent, the card moves to Done on its own, and the panel shows the answer.
  await page.evaluate(() => window.ark.call("spawn_agent", { spec: { name: "writer", role: "drafts chapters" } }));
  await page.locator(".bd-p-give").waitFor();
  await page.locator(".bd-p-give").click();
  await page.locator(".popover .menu-item", { hasText: "writer" }).click();
  await page.locator('.bd-list[data-state="done"] .bd-card', { hasText: "Draft chapter 3" }).waitFor();
  await panel.locator(".bd-p-said", { hasText: "Chapter 3 is drafted." }).waitFor();
  assert.match(await panel.locator(".bd-p-history summary").innerText(), /History · \d/);

  // A card another client adds appears without a reload; one blocked on the person is marked.
  await page.evaluate(() => window.ark.call("board_add", { title: "Choose the cover", owner: "user", blocked: "needs your photo" }));
  const cover = page.locator(".bd-card", { hasText: "Choose the cover" });
  await cover.waitFor();
  assert.equal(await cover.locator(".bd-tag.you").innerText(), "Needs you");
  const chip = page.locator(".bd-needs");
  assert.equal(await chip.innerText(), "Needs you · 1");
  await chip.click();
  assert.equal(await page.locator(".bd-card").count(), 1);
  await chip.click();

  // Delete from the keyboard, then undo.
  await page.locator(".bd-card", { hasText: "Pick a title" }).focus();
  await page.keyboard.press("Backspace");
  assert.equal(await page.locator(".bd-card", { hasText: "Pick a title" }).count(), 0);
  await page.locator(".bd-undo-btn").click();
  await page.locator(".bd-card", { hasText: "Pick a title" }).waitFor();
  assert.ok((await server()).includes("todo:Pick a title"));
}
scenarios.board = board;

// Files the agent writes are cards that open beside the conversation; the person edits them in
// place and the file changes: a cell (the total follows), a document's lines.
async function files(page) {
  const ask = async (text) => {
    await page.locator("#input").fill(text);
    await page.keyboard.press("Enter");
    await page.locator(".approval .btn.primary").click();
  };
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }), path);
  // Until the file (as the host reads it) passes `test`.
  const until = async (path, test) => {
    for (let i = 0; ; i++) {
      const f = await read(path);
      if (test(f)) return f;
      if (i > 50) assert.fail(`${path} did not change as expected: ${JSON.stringify(f).slice(0, 400)}`);
      await page.waitForTimeout(100);
    }
  };
  const total = (f) => f.sheets[0].rows[3][1].t;

  await finished(page, () => ask("Make the sales workbook."));
  await page.locator(".file-card", { hasText: "sales.xlsx" }).click();
  const cell = (r, c) => page.locator(`td[data-r="${r}"][data-c="${c}"]`);
  await page.waitForFunction(() => document.querySelector('td[data-r="3"][data-c="1"]')?.textContent === "200");
  await cell(1, 1).click();
  await page.keyboard.type("150");
  await page.keyboard.press("Enter");
  await until("sales.xlsx", (f) => total(f) === "230");
  await page.waitForFunction(() => document.querySelector('td[data-r="3"][data-c="1"]').textContent === "230");
  assert.equal(await page.locator(".sh-ref").inputValue(), "B3");
  // The formula bar shows the selected cell's formula.
  await cell(3, 1).click();
  assert.equal(await page.locator(".sh-fx").inputValue(), "=SUM(B2:B3)");
  // Clearing a cell clears it in the file.
  await cell(2, 1).click();
  await page.keyboard.press("Backspace");
  await until("sales.xlsx", (f) => total(f) === "150");

  await finished(page, () => ask("Write the report."));
  await page.locator(".file-card", { hasText: "report.docx" }).click();
  await page.locator(".dv-page").waitFor();
  assert.deepEqual(await page.locator(".dv-b").allInnerTexts(), ["Sales", "North led.", "Check the south"]);
  assert.equal(await page.locator(".dv-b").nth(2).getAttribute("data-kind"), "bullet");
  await page.evaluate(() => {
    const b = document.querySelectorAll(".dv-b")[1];
    const r = document.createRange();
    r.selectNodeContents(b);
    r.collapse(false);
    document.querySelector(".dv-page").focus();
    getSelection().removeAllRanges();
    getSelection().addRange(r);
  });
  await page.keyboard.press("Enter");
  await page.keyboard.type("- South fell");
  // The agent's next read is the person's version.
  await until("report.docx", (f) => f.blocks.map((b) => `${b.kind}:${b.text}`).join("|") === "heading:Sales|paragraph:North led.|bullet:South fell|bullet:Check the south");
}
scenarios.files = files;

// A program the agent runs writes a chart, a table, a report that shows the chart, and a PDF.
// Each is a card in the turn and opens in the view for its kind; nothing names the program.
async function analysis(page) {
  await page.locator("#input").fill("Analyze the sales and make me a chart and a short report.");
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.keyboard.press("Enter");
  const ask = page.locator(".approval");
  await ask.waitFor();
  // The approval leads with what the run is for, then the program and what it may do.
  const words = await ask.innerText();
  assert.match(words, /ARK wants to:\nChart the sales and write a report\nWith a Sh program in the project folder \(it can change files there; no network\)/);
  assert.match(words, /cat > chart\.svg/);
  await page.locator(".approval .btn.primary").click();
  await done;
  await page.waitForTimeout(150);

  const cards = await page.locator(".file-card").evaluateAll((els) => els.map((e) => `${e.dataset.kind}:${e.innerText.replace(/\s+/g, " ").trim()}`));
  assert.deepEqual(cards.sort(), ["image:chart.svg Image", "markdown:report.md Markdown document", "pdf:summary.pdf PDF", "sheet:table.csv Spreadsheet"]);

  // The chart: the file's own bytes, drawn.
  await page.locator(".file-card", { hasText: "chart.svg" }).click();
  await page.waitForFunction(() => document.querySelector("img.media-image")?.naturalWidth > 0);

  // The report renders as a page and shows the chart it links to.
  await page.locator(".file-card", { hasText: "report.md" }).click();
  await page.locator(".md-page h2").waitFor();
  assert.equal(await page.locator(".md-page h2").innerText(), "Sales");
  await page.waitForFunction(() => document.querySelector(".md-page img.md-img")?.naturalWidth > 0);

  // The table opens as a sheet.
  await page.locator(".file-card", { hasText: "table.csv" }).click();
  await page.waitForFunction(() => document.querySelector('td[data-r="1"][data-c="1"]')?.textContent === "120");
  // It is longer than the grid shows, and says so; no rows to type into past the part shown.
  assert.equal(await page.locator(".sh-part").innerText(), "Showing the first 5,000 of 6,003 rows. Ask ARK to work with all of them.");

  // The PDF opens in the browser's own viewer (headless WebKit does not paint it, so only the
  // frame and its document are checked).
  await page.locator(".file-card", { hasText: "summary.pdf" }).click();
  await page.waitForFunction(() => document.querySelector("iframe.media-pdf")?.src.startsWith("blob:"));
  // The window reads a PDF as itself; the agent reads its text (files.read).
  const read = await page.evaluate(() => window.ark.call("file_read", { path: "summary.pdf" }));
  assert.equal(read.kind, "pdf");
  assert.equal(read.media_type, "application/pdf");
}
scenarios.analysis = analysis;

// ADR 0034: a run changes one file and removes another. Each card of that turn puts the file
// back as it was before the turn, and the changed one can take the turn's version back.
async function restore(page) {
  const run = async (text) => {
    const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
    await page.locator("#input").fill(text);
    await page.keyboard.press("Enter");
    await page.locator(".approval .btn.primary").click();
    await done;
    await page.waitForTimeout(150);
  };
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }).then((f) => f.text, (e) => `error: ${e.message}`), path);
  await run("Write the notes.");
  // A new file has nothing to put back.
  assert.equal(await page.locator(".turn").last().locator(".file-restore").count(), 0);

  await run("Rewrite the notes and drop the old file.");
  const turn = page.locator(".turn").last();
  const notes = turn.locator(".file-item", { hasText: "notes.md" });
  const old = turn.locator(".file-item", { hasText: "old.md" });
  assert.equal(await old.locator(".file-card-sub").innerText(), "Removed");
  assert.equal(await old.locator(".file-card").isDisabled(), true);
  assert.equal(await read("notes.md"), "two\n");

  await notes.locator(".file-restore").click();
  await notes.locator(".file-restore", { hasText: "Undo" }).waitFor();
  assert.equal(await read("notes.md"), "one\n");
  await notes.locator(".file-restore").click();
  await notes.locator(".file-restore", { hasText: "Restore" }).waitFor();
  assert.equal(await read("notes.md"), "two\n");

  // The removed file comes back and opens; there was nothing to undo.
  await old.locator(".file-restore").click();
  await old.locator(".file-restore").waitFor({ state: "hidden" });
  assert.equal(await read("old.md"), "old\n");
  await old.locator(".file-card").click();
  await page.locator(".md-page").waitFor();

  // A view of the file open beside the conversation follows a restore.
  await notes.locator(".file-card").click();
  await page.locator(".md-page h1, .md-page p", { hasText: "two" }).first().waitFor();
  await notes.locator(".file-restore").click();
  await page.locator(".md-page p", { hasText: "one" }).first().waitFor();
}
scenarios.restore = restore;

// ADR 0035: files dropped on the conversation go into the project's folder. A name in use gets
// a number, a file larger than one call arrives whole, and the composer names what was added.
async function drop(page) {
  // A drop as the browser makes it, with files built in the page.
  const dropFiles = (files) => page.evaluate((files) => {
    const dt = new DataTransfer();
    for (const f of files) {
      const text = f.lines ? Array.from({ length: f.lines }, (_, i) => `row ${i}`).join("\n") : f.text;
      dt.items.add(new File([text], f.name, { type: "text/plain" }));
    }
    const at = document.querySelector("#thread");
    for (const type of ["dragenter", "dragover", "drop"]) at.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true }));
  }, files);
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }).then((f) => f.text), path);

  // Dragging over shows where to drop; leaving hides it.
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "x.txt"));
    document.querySelector("#thread").dispatchEvent(new DragEvent("dragenter", { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
  assert.equal(await page.locator(".drop-hint").isVisible(), true);
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "x.txt"));
    document.querySelector("#thread").dispatchEvent(new DragEvent("dragleave", { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
  assert.equal(await page.locator(".drop-hint").isVisible(), false);

  await page.locator("#input").fill("Compare them");
  await dropFiles([{ name: "notes.md", text: "# Mine\n" }]);
  await page.waitForFunction(() => document.querySelector("#input").value.includes("Added to the project"));
  assert.equal(await page.locator("#input").inputValue(), "Compare them\nAdded to the project: notes.md");
  // The cursor stays after the person's words, above the line.
  assert.equal(await page.evaluate(() => document.querySelector("#input").selectionStart), "Compare them".length);
  assert.equal(await read("notes.md"), "# Mine\n");

  // About 5 MB: three calls. The second notes.md keeps the first.
  await page.locator("#input").fill("");
  await dropFiles([{ name: "notes.md", text: "# Theirs\n" }, { name: "rows.txt", lines: 500000 }]);
  await page.waitForFunction(() => document.querySelector("#input").value.includes("Added to the project"));
  assert.equal(await page.locator("#input").inputValue(), "\nAdded to the project: notes 2.md, rows.txt");
  assert.equal(await read("notes.md"), "# Mine\n");
  assert.equal(await read("notes 2.md"), "# Theirs\n");
  const rows = await read("rows.txt");
  assert.equal(rows.length, Array.from({ length: 500000 }, (_, i) => `row ${i}`).join("\n").length);
  assert.ok(rows.startsWith("row 0\nrow 1\n") && rows.endsWith("\nrow 499999"));
  assert.equal(await page.locator(".adding").count(), 0);
  // Nothing is left of the uploads.
  const left = await page.evaluate(() => window.ark.call("files", { path: "" }).then((r) => r.entries.map((e) => e.name).sort()));
  assert.deepEqual(left, ["notes 2.md", "notes.md", "rows.txt"]);
}
scenarios.drop = drop;

// The + menu opens the built-in panels. Files edits and saves a file in the project folder;
// the terminal runs a shell there. A page on this machine's loopback address says its runtime
// is local.
async function workbench(page) {
  assert.equal(await page.evaluate(() => window.ark.isLocal()), true);
  // A browser has no window to go back to.
  assert.equal(await page.evaluate(() => typeof window.ark.openLocal), "undefined");
  assert.equal(await page.locator("#app").getAttribute("data-shell"), "browser");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.runtime), "local");
  await page.evaluate(() => window.ark.call("file_write", { path: "draft.txt", text: "one" }));
  await page.locator("#open-menu").click();
  const items = page.locator(".popover .open-item");
  assert.deepEqual(await items.allInnerTexts().then((t) => t.map((x) => x.split("\n")[0].trim())), ["Terminal", "Browser", "Files"]);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.locator(".wb-entry", { hasText: "draft.txt" }).click();
  const editor = page.locator(".wb-editor");
  assert.equal(await editor.inputValue(), "one");
  assert.equal(await page.locator(".wb-save").isDisabled(), true);
  await editor.fill("two");
  await page.locator(".wb-save").click();
  for (let i = 0; (await page.evaluate(() => window.ark.call("file_read", { path: "draft.txt" }))).text !== "two"; i++) {
    assert.ok(i < 50, "draft.txt was not saved");
    await page.waitForTimeout(100);
  }
  await assert.rejects(page.evaluate(() => window.ark.call("file_read", { path: "../outside" })));

  // Markdown opens as a page; Edit shows its text, and Done the page as saved.
  await page.evaluate(() => window.ark.call("file_write", { path: "notes.md", text: "# Plan\n\nShip it." }));
  await page.evaluate(() => window.ark.openFile("notes.md"));
  await page.locator(".md-page h2", { hasText: "Plan" }).waitFor();
  await page.locator(".btn", { hasText: "Edit" }).click();
  const md = page.locator(".fv").filter({ has: page.locator('textarea[aria-label="notes.md"]') });
  await md.locator(".wb-editor").fill("# Plan\n\nShip it today.");
  await md.locator(".wb-save").click();
  await md.locator(".btn", { hasText: "Done" }).click();
  await page.locator(".md-page p", { hasText: "Ship it today." }).waitFor();

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

// The window's Browser view (ADR 0044) is drawn above the page, so the panel tells it where to
// be and when to step aside. A stub in place of the window records what the page asks.
async function browserview(page) {
  await page.evaluate(() => {
    window.calls = [];
    window.ark.browserView = {
      show: (r, url) => {
        window.calls.push(["show", r, url || null]);
        return url?.startsWith("file:") ? Promise.reject(new Error("the Browser does not open that address")) : Promise.resolve({ shown: true });
      },
      act: (a) => (window.calls.push([a]), Promise.resolve()),
      reach: () => Promise.resolve({ reached: true }),
      listen: (fn) => ((window.emitBrowser = fn), Promise.resolve()),
    };
  });
  const last = () => page.evaluate(() => window.calls.at(-1));
  const until = (what) => page.waitForFunction((w) => window.calls.at(-1)?.[0] === w, what);

  // A local address typed without a scheme opens over http, at the panel's place in the pane.
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "localhost:3000" }));
  await page.waitForFunction(() => window.calls.some((c) => c[0] === "show" && c[2] === "http://localhost:3000"));
  // Wait for the pane to finish opening, then compare the last place with the pane.
  await page.waitForTimeout(400);
  const [, r] = await last();
  const pane = await page.locator("#pane").boundingBox();
  const bar = await page.locator(".wb-bar").boundingBox();
  // Inside the pane, clear of its resize grip.
  assert.ok(r.x >= Math.floor(pane.x) + 6 && r.x + r.width <= Math.ceil(pane.x + pane.width), JSON.stringify({ r, pane }));
  assert.ok(r.y >= Math.floor(bar.y + bar.height) - 1, JSON.stringify({ r, bar }));
  assert.ok(r.width > 200 && r.height > 200, JSON.stringify(r));
  // Nothing more is asked while nothing moves.
  const n = await page.evaluate(() => window.calls.length);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(() => window.calls.length), n);

  // A page that loads inside the view reaches the address bar and the tab.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: false }));
  assert.equal(await page.locator(".wb-input").inputValue(), "http://localhost:3000/home");

  // An address the view refuses says why, and the bar goes back to the page shown.
  await page.locator(".wb-input").fill("file:///etc/passwd");
  await page.locator(".wb-input").press("Enter");
  await page.locator(".toast", { hasText: "does not open that address" }).waitFor();
  assert.equal(await page.locator(".wb-input").inputValue(), "http://localhost:3000/home");
  await page.locator("#input").focus();

  // The palette covers the pane: the view steps aside, and comes back where it was.
  await page.keyboard.press("Meta+k");
  await until("hide");
  await page.keyboard.press("Escape");
  await until("show");
  assert.deepEqual((await last())[1], r);
  assert.equal((await last())[2], null, "coming back does not load the page again");

  // Another tab in front, then the Browser again.
  await page.evaluate(() => window.ark.openPanel("files"));
  await until("hide");
  await page.locator(".pane-tab", { hasText: "localhost" }).click();
  await until("show");

  // Closing the pane hides the view; the toggle brings it back.
  await page.locator("#close-pane").click();
  await until("hide");
  await page.evaluate(() => window.ark.openPanel("browser"));
  await until("show");

  await page.locator('.wb-bar button[aria-label="Back"]').click();
  await until("back");
  await page.locator('.wb-bar button[aria-label="Reload"]').click();
  await until("reload");

  // Closing the tab ends the view.
  await page.locator(".pane-tab", { hasText: "localhost" }).locator(".pane-tab-x").click();
  await until("close");
}
scenarios.browserview = browserview;

// The window's view shows nothing when an address does not answer, so the panel asks first and
// says so; the bar shows a load until the page finishes.
async function browserdown(page) {
  await page.evaluate(() => {
    window.calls = [];
    window.reached = [];
    window.up = new Set(["http://localhost:3000", "http://localhost:3000/x"]);
    window.ark.browserView = {
      show: (r, url) => (window.calls.push(["show", r, url || null]), Promise.resolve({ shown: true })),
      act: (a) => (window.calls.push([a]), Promise.resolve()),
      reach: (url) => {
        window.reached.push(url);
        const answer = { reached: window.up.has(url) };
        // A slow check, overtaken by the next address.
        return url.includes(":4001") ? new Promise((ok) => setTimeout(() => ok(answer), 400)) : Promise.resolve(answer);
      },
      listen: (fn) => ((window.emitBrowser = fn), Promise.resolve()),
    };
  });
  const bar = page.locator(".wb-bar");
  const input = page.locator(".wb-input");
  const down = page.locator(".wb-refused");
  const shows = (u) => page.waitForFunction((u) => window.calls.some((c) => c[0] === "show" && c[2] === u), u);
  const enter = async (u) => {
    await input.fill(u);
    await input.press("Enter");
  };

  await page.evaluate(() => window.ark.openPanel("browser", { locator: "localhost:3000" }));
  await shows("http://localhost:3000");
  assert.equal(await bar.getAttribute("data-loading"), "", "the bar shows the load");
  assert.equal(await page.locator(".wb-frame-host").getAttribute("data-page"), null, "the pane shows through until a page arrives");
  // The page arrives at another address (a redirect): white behind it, then done.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: true }));
  assert.equal(await page.locator(".wb-frame-host").getAttribute("data-page"), "");
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: false }));
  await page.waitForFunction(() => !document.querySelector(".wb-bar").hasAttribute("data-loading"));

  // Nothing runs on the port: the pane says so, the view steps aside, the bar keeps the address.
  await enter("localhost:3999");
  await down.waitFor();
  assert.equal(await down.locator("p").innerText(), "localhost:3999 can't be reached");
  assert.equal(await down.locator("button").innerText(), "Try again");
  assert.equal(await input.inputValue(), "http://localhost:3999");
  assert.ok(await input.evaluate((el) => el === document.activeElement), "focus stays in the bar");
  await page.waitForFunction(() => window.calls.at(-1)?.[0] === "hide");
  assert.ok(!(await page.evaluate(() => window.calls.some((c) => c[2] === "http://localhost:3999"))));
  await page.waitForFunction(() => !document.querySelector(".wb-bar").hasAttribute("data-loading"));
  // The view's late news does not replace what was typed.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/other", loading: false }));
  assert.equal(await input.inputValue(), "http://localhost:3999");
  const box = await down.boundingBox();
  const pane = await page.locator("#pane").boundingBox();
  assert.ok(box.x >= pane.x && box.x + box.width <= pane.x + pane.width + 1, "the message fits the pane");

  // Try again asks again; still down, then up.
  const asked = await page.evaluate(() => window.reached.length);
  await down.locator("button").click();
  await page.waitForFunction((n) => window.reached.length > n, asked);
  assert.ok(await down.isVisible());
  await page.evaluate(() => window.up.add("http://localhost:3999"));
  await down.locator("button").click();
  await shows("http://localhost:3999");
  assert.ok(await down.isHidden());

  // Reload while down tries again; Back returns to the page before.
  await page.evaluate(() => window.up.delete("http://localhost:3999"));
  await enter("localhost:3999");
  await down.waitFor();
  const before = await page.evaluate(() => window.reached.length);
  await page.locator('.wb-bar button[aria-label="Reload"]').click();
  await page.waitForFunction((n) => window.reached.length > n, before);
  await page.locator('.wb-bar button[aria-label="Back"]').click();
  assert.ok(await down.isHidden());
  assert.equal(await input.inputValue(), "http://localhost:3000/home", "the bar names the page come back to");
  await page.waitForFunction(() => window.calls.at(-1)?.[0] === "show");
  assert.ok(!(await page.evaluate(() => window.calls.some((c) => c[0] === "back"))), "Back from the message does not move the page");

  // A slow check overtaken by the next address counts for nothing.
  await enter("localhost:4001");
  await enter("localhost:3000/x");
  await shows("http://localhost:3000/x");
  await page.waitForTimeout(600);
  assert.ok(await down.isHidden(), "the overtaken check does not say can't be reached");

  // The page before finishes while the next address is checked: the bar, the line and Try again
  // stay with the address typed.
  await enter("localhost:4001");
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/x", loading: false }));
  assert.equal(await bar.getAttribute("data-loading"), "", "the line goes on while the address is checked");
  await down.waitFor();
  assert.equal(await down.locator("p").innerText(), "localhost:4001 can't be reached");
  assert.equal(await input.inputValue(), "http://localhost:4001");
  await down.locator("button").focus();
  await page.evaluate(() => window.up.add("http://localhost:4001"));
  await down.locator("button").press("Enter");
  await shows("http://localhost:4001");
  assert.equal(await page.evaluate(() => window.reached.at(-1)), "http://localhost:4001", "Try again asks for the address typed");
  assert.ok(await input.evaluate((el) => el === document.activeElement), "focus goes back to the bar");

  // An address the view refuses is not asked about.
  const n = await page.evaluate(() => window.reached.length);
  await enter("file:///tmp/a.html");
  await shows("file:///tmp/a.html");
  assert.equal(await page.evaluate(() => window.reached.length), n);
}
scenarios.browserdown = browserdown;

// In a browser the panel is a frame: a page that refuses frames says so, with a way out.
async function framerefused(page) {
  await page.evaluate(() => {
    const call = window.ark.call;
    window.ark.call = (m, p) => (m === "frame_check" ? Promise.resolve({ framable: false, reason: "X-Frame-Options: DENY" }) : call(m, p));
  });
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "https://example.com/a" }));
  const refused = page.locator(".wb-refused");
  await refused.waitFor();
  assert.equal(await refused.locator("p").innerText(), "example.com can't be shown here");
  assert.equal(await refused.locator("button").innerText(), "Open in your browser");
  assert.equal(await page.locator("iframe.wb-frame").getAttribute("src"), "https://example.com/a");
}
scenarios.framerefused = framerefused;

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
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Appearance", "Capabilities", "Devices", "Permissions", "Skills", "Tool servers"]);

  await page.evaluate(() => window.ark.setting({ id: "look", title: "Look", render: (el) => el.append("the look") }));
  await page.evaluate(() => window.ark.setting({ id: "debug", title: "Debug", advanced: true, render: (el) => el.append("the debug") }));
  await page.evaluate(() => window.ark.openSettings("keys"));
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Look", "Appearance", "Capabilities", "Devices", "Permissions", "Debug", "Skills", "Tool servers"]);
  assert.deepEqual(await page.locator(".settings-list > *").evaluateAll((els) => els.map((e) => e.textContent).slice(5, 7)), ["Permissions", "Advanced"]);

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
  // The window on a runtime it visits (ADR 0029): Tauri defines `__TAURI__` there too.
  visit: () => {
    window.__TAURI__ = {};
  },
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

// A chat keeps working when the person opens another: its question waits in the tray with the
// chat's name, the sidebar marks it, the new chat runs its own turn, and the first one's answer
// is there when the person goes back.
async function background(page) {
  await page.locator("#input").fill("Save the notes for later, please");
  await page.keyboard.press("Enter");
  const question = page.locator("#thread .approval");
  await question.waitFor();
  const first = await page.evaluate(() => window.ark.summary().session.id);

  await page.locator("#new-session").click();
  await page.waitForFunction((id) => window.ark.summary().session.id !== id, first);
  const tray = page.locator("#asks .approval");
  await tray.waitFor();
  assert.match(await tray.innerText(), /^In /, "the tray says which chat asks");
  assert.equal(await page.locator("#thread .approval").count(), 0);
  await page.locator(`.session[data-id="${first}"] .working-dot`).waitFor();
  assert.equal(await page.locator("#app").getAttribute("data-running"), "false", "the new chat is free");

  await finished(page, () => page.locator("#input").fill("And a second question?").then(() => page.keyboard.press("Enter")));
  assert.deepEqual(await shape(page), ["answer: Second answer."]);

  await tray.locator("button", { hasText: "Allow once" }).click();
  await page.locator(`.session[data-id="${first}"] .session-time`).waitFor();
  assert.equal(await page.locator("#asks .approval").count(), 0);
  assert.deepEqual(await shape(page), ["answer: Second answer."], "the other chat's answer stays out");

  await page.locator(`.session[data-id="${first}"]`).click();
  await page.waitForFunction((id) => window.ark.summary().session.id === id, first);
  await page.locator(".turn.stored .answer", { hasText: "Saved the notes." }).waitFor();
}
scenarios.background = background;

// An agent using the computer shows who, and Stop takes it back: the click it then asks for
// fails, and the pill goes away.
async function computer(page) {
  const pill = page.locator("#computer-use");
  assert.equal(await pill.isVisible(), false, "nothing shows while nobody uses it");
  await page.locator("#input").fill("Save my note in Notes, please");
  await page.keyboard.press("Enter");
  const look = page.locator("#thread .approval:not(.decided)");
  await look.waitFor();
  assert.match(await look.innerText(), /Looking at Notes/);
  await look.locator("button", { hasText: "Allow once" }).click();
  const click = page.locator("#thread .approval:not(.decided)", { hasText: "Clicking “Save” in Notes" });
  await click.waitFor();
  // Using the computer is asked each time: no answer covers later calls.
  assert.deepEqual(await click.locator(".approval-actions button").allInnerTexts().then((t) => t.map((x) => x.split("\n")[0].trim())), ["Allow once", "Deny"]);
  await pill.waitFor();
  assert.match(await pill.innerText(), /ARK is using your computer/);
  await pill.locator("button", { hasText: "Stop" }).click();
  await pill.waitFor({ state: "hidden" });
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await click.locator("button", { hasText: "Allow once" }).click();
  await done;
  await page.locator(".answer", { hasText: "took the computer back" }).waitFor();
  assert.equal(await pill.isVisible(), false);
}
scenarios.computer = computer;

// ADR 0042: the picture the agent took shows under its step; a click in it is asked with the
// picture and a ring on the spot, so the person judges the spot itself.
async function computerPicture(page) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.locator("#input").fill("Press Save in Notes");
  await page.keyboard.press("Enter");
  const see = page.locator("#thread .approval:not(.decided)");
  await see.waitFor();
  assert.match(await see.innerText(), /Taking a picture of Notes's window/);
  await see.locator("button", { hasText: "Allow once" }).click();
  const point = page.locator("#thread .approval:not(.decided)", { hasText: "Clicking the Save button in Notes" });
  await point.waitFor();
  const shot = point.locator(".approval-shot");
  await shot.waitFor();
  assert.match(await shot.locator("img").getAttribute("src"), /^data:image\/png;base64,/);
  // The fake picture is 400 by 300: (100, 150) is a quarter across and half down.
  const ring = await shot.locator(".approval-spot").evaluate((r) => [r.style.left, r.style.top]);
  assert.deepEqual(ring, ["25%", "50%"]);
  assert.equal(await point.locator(".approval-class, .approval-summary").count(), 0, "no caption");
  await point.locator("button", { hasText: "Allow once" }).click();
  await done;
  // What the agent saw, under its step.
  await page.locator(".act[data-action='computer.see'] .act-shot img").waitFor({ state: "attached" });
}
scenarios["computer-picture"] = computerPicture;

// The window on a runtime it visits (ADR 0029): the page talks HTTP but leaves room for the
// window's controls, and `ark.openLocal()` loads the address the window catches to go back to
// its own page. An app's row sits above the projects.
async function visit(page) {
  assert.equal(await page.locator("#app").getAttribute("data-shell"), "desktop");
  assert.equal(await page.evaluate(() => typeof window.__TAURI__.core), "undefined");
  assert.ok(await page.evaluate(() => window.ark.call("hello")), "the page talks to its runtime over HTTP");
  await page.evaluate(() => window.ark.sidebar({ id: "place", render: (el) => el.append("This Mac · Cloud") }));
  const row = page.locator("#sidebar-ext .sidebar-item[data-id=place]");
  await row.waitFor();
  assert.equal(await row.innerText(), "This Mac · Cloud");
  // Above the projects.
  const [r, p] = await Promise.all([row.boundingBox(), page.locator("#projects").boundingBox()]);
  assert.ok(r.y < p.y, "the row is above the projects");
  let asked = null;
  await page.route("https://ark-local.invalid/**", (route) => {
    asked = route.request().url();
    route.abort();
  });
  await page.evaluate(() => window.ark.openLocal());
  for (let i = 0; !asked; i++) {
    assert.ok(i < 50, "ark.openLocal() did not ask for the window's own page");
    await page.waitForTimeout(50);
  }
  assert.equal(asked, "https://ark-local.invalid/");
}
scenarios.visit = visit;

// An app's action on a project: an item in that project's menu, given the project.
async function projectAction(page) {
  await page.evaluate(() => {
    window.ark.action({
      id: "copy", label: "Copy elsewhere", where: "project",
      when: ({ project }) => project.current,
      run: ({ project }) => { window.copied = project.name; },
    });
  });
  const current = page.locator(".project-row[aria-current=true]").first();
  await current.waitFor();
  const more = current.locator(".project-more");
  await more.click();
  const item = page.locator(".popover .menu-item", { hasText: "Copy elsewhere" });
  await item.waitFor();
  await item.click();
  const name = await current.locator(".project-name").innerText();
  assert.equal(await page.evaluate(() => window.copied), name);
  assert.equal(await page.locator(".popover[open]").count(), 0, "the menu closes");
}
scenarios["project-action"] = projectAction;

// ADR 0041: a process start asks with its command and where, then runs beside the chat: the
// Processes panel shows its output, its page opens in the Browser once ready, Stop ends it.
async function processes(page) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.locator("#input").fill("Run the app and show me the home page");
  await page.keyboard.press("Enter");
  const ask = page.locator(".approval");
  await ask.waitFor();
  const words = await ask.innerText();
  assert.match(words, /Keep this running in the project folder, then open \/home in the browser\. Not contained: it runs as you\.\n\s*python3 -u -m http\.server 0 --bind 127\.0\.0\.1/);
  const buttons = (await ask.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
  assert.deepEqual(buttons, ["Allow once", "Allow this command for this session", "Always allow this command", "Deny"]);
  await ask.locator('button[data-decision="allow_once"]').click();

  // The panel opens with the process and its output; the row turns ready (by then the Browser
  // tab is in front, so it is checked in the page, not for visibility).
  await page.locator(".proc-row").first().waitFor();
  await page.waitForFunction(() => document.querySelector(".proc-out .xterm-rows")?.textContent.includes("Serving HTTP"), null, { timeout: 20000 });
  await page.waitForFunction(() => document.querySelector('.proc-row[data-state="ready"]'), null, { timeout: 20000 });
  // The page the agent asked for, in the Browser panel, in front.
  await page.waitForFunction(() => /^http:\/\/localhost:\d+\/home$/.test(document.querySelector("iframe.wb-frame")?.src || ""));
  assert.equal(await page.locator(".pane-tab[aria-selected=true]").innerText(), "localhost");
  await done;

  // Stop ends it, and it stays listed as stopped.
  await page.locator('.pane-tab', { hasText: "Processes" }).click();
  await page.locator(".proc-stop").click();
  await page.locator('.proc-row[data-state="stopped"]').waitFor();
  const list = await page.evaluate(() => window.ark.call("procs"));
  assert.equal(list[0].state, "stopped");
}
scenarios.processes = processes;

// Work that waits for the person (ADR 0043): rows in the tray, newest first, above any
// question. Opening a result goes to its chat; opening a failure with no chat shows why under
// the row; the x and Delete clear. Cleared stays cleared after a reload.
async function waiting(page) {
  const rows = page.locator(".asks .waiting .wait-row");
  await rows.first().waitFor();
  assert.equal(await rows.count(), 3, "three shown, the fourth behind More");
  assert.equal(await rows.nth(0).getAttribute("data-kind"), "finished");
  assert.match(await rows.nth(0).innerText(), /File the notes/);
  await page.locator(".wait-more").click();
  assert.equal(await rows.count(), 4);

  // A failure with no chat: why it failed, under the row.
  const summarize = rows.filter({ hasText: "Summarize the notes" });
  await summarize.focus();
  await page.keyboard.press("Enter");
  await summarize.locator(".wait-detail").waitFor();
  assert.equal(await summarize.locator(".wait-detail").innerText(), "The notes folder is gone.");
  await summarize.click();
  await summarize.waitFor({ state: "detached" });

  // The x clears one; Delete clears another.
  const tidy = rows.filter({ hasText: "Tidy the drafts" });
  await tidy.hover();
  await tidy.locator(".wait-clear").click();
  await tidy.waitFor({ state: "detached" });
  const rename = rows.filter({ hasText: "Rename the chapters" });
  await rename.focus();
  await page.keyboard.press("Delete");
  await rename.waitFor({ state: "detached" });

  // A result posted into a chat: opening it opens the chat, where the post is.
  await rows.first().click();
  await page.locator("#session-title", { hasText: "hi" }).waitFor();
  await page.locator("#thread", { hasText: "Filed." }).waitFor();
  await page.locator(".asks .waiting").waitFor({ state: "detached" });

  await page.reload();
  await page.locator("#input").waitFor();
  await page.waitForTimeout(300);
  assert.equal(await page.locator(".asks .waiting").count(), 0, "cleared stays cleared");
}
scenarios.waiting = waiting;

// What waits on another computer (ADR 0043 step 4): its question is a card in this window's
// tray, saying where; answering it here decides it there. Its page, opened with a token and an
// item, signs in and keeps the item to open.
async function linked(page) {
  const card = page.locator(".asks .waiting-cards .approval");
  await card.waitFor();
  assert.match(await card.innerText(), /In My machine · novel/);
  // At phone width the card still fits and its choices show.
  await page.setViewportSize({ width: 390, height: 800 });
  const allow = card.getByRole("button", { name: "Allow once" });
  await allow.waitFor();
  const box = await card.boundingBox();
  assert.ok(box.x >= 0 && box.x + box.width <= 390, `the card fits: ${JSON.stringify(box)}`);

  // The other computer's page, as a window opens it: the token leaves the address, the item stays.
  const there = await page.context().browser().newPage();
  await there.addInitScript(() => {
    window.__addresses = [];
    const replace = history.replaceState.bind(history);
    history.replaceState = (s, t, u) => { window.__addresses.push(String(u)); replace(s, t, u); };
  });
  await there.goto(process.env.ARK_TEST_ELSEWHERE);
  await there.locator("#input").waitFor();
  await there.locator("#asks .approval").waitFor();
  const addresses = await there.evaluate(() => window.__addresses);
  assert.ok(addresses.every((a) => !a.includes("token=")), `the token left the address: ${addresses}`);
  assert.ok(addresses[0].includes("#item="), `the item stayed for the page: ${addresses}`);
  assert.equal(new URL(there.url()).hash, "", "and was opened");
  await there.close();

  await allow.click();
  await card.waitFor({ state: "detached" });
}
scenarios.linked = linked;

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
