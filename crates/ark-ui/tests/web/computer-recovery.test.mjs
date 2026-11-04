import assert from "node:assert/strict";
import { before, after, test } from "node:test";
import { createRequire } from "node:module";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";

const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = fileURLToPath(new URL("../../web/", import.meta.url));
let browser;
before(async () => { browser = await webkit.launch({ headless: true }); });
after(async () => { await browser?.close(); });

async function fixture(run, { stale = false } = {}) {
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } });
  page.setDefaultTimeout(5000);
  const calls = [], errors = [];
  let acknowledge;
  let acknowledgementSeen;
  const pendingAcknowledgement = new Promise(resolve => { acknowledgementSeen = resolve; });
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*", async route => {
    const url = new URL(route.request().url());
    assert.equal(url.origin, "http://ark.test");
    if (url.pathname === "/fixture") return route.fulfill({ contentType: "text/html", body: '<link rel="stylesheet" href="/app.css"><button id="review">Review</button><script type="module" src="/fixture.js"></script>' });
    if (url.pathname === "/fixture.js") return route.fulfill({ contentType: "text/javascript", body: `import { reviewComputerMessage } from "/js/computer-recovery.js";
      const call = async (method, params = {}) => { const response = await fetch("/rpc", {method:"POST",body:JSON.stringify({method,params})}); const value = await response.json(); if (!value.ok) throw new Error(value.error); return value.result; };
      document.querySelector("#review").onclick = () => reviewComputerMessage(call, result => { window.completed = result; }, document.querySelector("#review"));` });
    if (url.pathname === "/rpc") {
      const request = route.request().postDataJSON();
      calls.push(request);
      if (request.method === "computer_review") return route.fulfill({ json: { ok: true, result: { app: "Fixture Messages", kind: "submitted_unconfirmed", token: "exact-block-token" } } });
      assert.equal(request.method, "computer_acknowledge");
      assert.deepEqual(request.params, { token: "exact-block-token", accepted_unverified: true });
      acknowledge = () => route.fulfill({ json: stale ? { ok: false, error: "The message block changed; review it again." } : { ok: true, result: { status: "acknowledged", verified: false, retry_allowed: false } } });
      acknowledgementSeen();
      return;
    }
    const path = resolve(root, `.${url.pathname}`);
    assert(path.startsWith(resolve(root) + sep));
    return route.fulfill({ contentType: path.endsWith(".css") ? "text/css" : "text/javascript", body: await readFile(path) });
  });
  try {
    await page.goto("http://ark.test/fixture");
    await run({ page, calls, release: async () => { await pendingAcknowledgement; await acknowledge(); } });
    assert.deepEqual(errors, []);
  } finally { await page.close(); }
}

test("human recovery requires an explicit choice, holds focus and never claims verification", async () => {
  await fixture(async ({ page, calls, release }) => {
    await page.getByRole("button", { name: "Review", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Allow new drafts?" });
    await dialog.waitFor();
    assert.match(await dialog.innerText(), /may already have been sent/);
    assert.equal(await page.evaluate(() => document.activeElement.textContent), "Keep blocked");
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.equal(calls.filter(call => call.method === "computer_acknowledge").length, 0);
    assert.equal(await page.evaluate(() => document.activeElement.id), "review");
    await page.getByRole("button", { name: "Review", exact: true }).click();
    await dialog.waitFor();
    if (process.env.ARK_COMPUTER_RECOVERY_EVIDENCE) {
      await mkdir(process.env.ARK_COMPUTER_RECOVERY_EVIDENCE, { recursive: true });
      await page.screenshot({ path: resolve(process.env.ARK_COMPUTER_RECOVERY_EVIDENCE, "human-recovery.png") });
      await page.setViewportSize({ width: 360, height: 700 });
      const box = await dialog.boundingBox();
      assert(box.x >= 0 && box.x + box.width <= 360);
      await page.screenshot({ path: resolve(process.env.ARK_COMPUTER_RECOVERY_EVIDENCE, "human-recovery-narrow.png") });
    }
    await dialog.getByRole("button", { name: "I checked the app" }).click();
    await page.waitForFunction(() => document.querySelector("dialog button:last-child").disabled);
    await page.keyboard.press("Escape");
    assert.equal(await dialog.isVisible(), true);
    await release();
    await dialog.waitFor({ state: "detached" });
    assert.deepEqual(await page.evaluate(() => window.completed), { status: "acknowledged", verified: false, retry_allowed: false });
    assert.equal(calls.filter(call => call.method === "computer_acknowledge").length, 1);
  });
});

test("a changed draft block stays open and cannot be acknowledged with the stale review", async () => {
  await fixture(async ({ page, calls, release }) => {
    await page.getByRole("button", { name: "Review", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("button", { name: "I checked the app" }).click();
    await page.waitForFunction(() => document.querySelector("dialog button:last-child").disabled);
    await release();
    await dialog.getByRole("alert").waitFor();
    assert.equal(await dialog.getByRole("button", { name: "I checked the app" }).isDisabled(), true);
    assert.equal(await page.evaluate(() => window.completed), undefined);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.equal(calls.filter(call => call.method === "computer_acknowledge").length, 1);
  }, { stale: true });
});
