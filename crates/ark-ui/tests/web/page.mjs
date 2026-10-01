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
      "work: Used 1 tool · Reading notes",
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
    await page.locator(".sidebar button", { hasText: "New session" }).click();
    await page.waitForTimeout(350);
    const closed = await state();
    assert.equal(closed.drawer, "closed");
    assert.ok(closed.sidebarLeft < 0, JSON.stringify(closed));
    await page.setViewportSize({ width: 1200, height: 800 });
    await page.waitForTimeout(100);
    assert.deepEqual(await state(), { compact: "false", drawer: "", sidebarLeft: 0, hscroll: false });
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
      "work: Used 1 tool · Reading notes",
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
