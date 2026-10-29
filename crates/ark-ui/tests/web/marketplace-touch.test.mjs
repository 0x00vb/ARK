import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
const { webkit } = createRequire(import.meta.url)("playwright-core");
const css = await readFile(new URL("../../web/app.css", import.meta.url), "utf8");
const html = await readFile(new URL("../../web/index.html", import.meta.url), "utf8");

for (const touch of [false, true]) {
  test(`marketplace search keeps focus, readable input and zoom with ${touch ? "coarse" : "fine"} input`, async () => {
    const browser = await webkit.launch();
    try {
      const page = await browser.newPage({ viewport: { width: touch ? 390 : 1180, height: 800 }, hasTouch: touch, isMobile: touch });
      const viewport = html.match(/<meta name="viewport"[^>]*>/)[0];
      assert.doesNotMatch(viewport, /user-scalable\s*=\s*no|maximum-scale\s*=\s*1(?:[,."\s]|$)/);
      await page.setContent(`${viewport}<main class="marketplace"><input type="search" class="market-search" aria-label="Search skills"></main>`);
      await page.addStyleTag({ content: css });
      const search = page.getByRole("searchbox", { name: "Search skills" });
      await search.fill("content");
      assert.equal(await search.inputValue(), "content");
      const observed = await search.evaluate(el => ({ font: getComputedStyle(el).fontSize, coarse: matchMedia("(pointer: coarse)").matches, focused: el === document.activeElement }));
      assert.equal(observed.coarse, touch);
      assert.equal(observed.font, touch ? "16px" : "13px");
      assert.equal(observed.focused, true);
    } finally { await browser.close(); }
  });
}
