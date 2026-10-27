import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { ICONS } from '../../web/js/icons.js';
const { webkit } = createRequire(import.meta.url)('playwright-core');

test('all navigation icons render within the shared viewport', async () => {
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage({viewport:{width:1000,height:780}});
    await page.setContent(`<body style="margin:28px;background:#fafaf7;color:#0f1115;font:12px system-ui"><h1 style="font-size:18px">ARK icon family</h1><div style="display:grid;grid-template-columns:repeat(8,1fr);gap:24px">${Object.entries(ICONS).map(([name,svg]) => `<div style="display:grid;gap:8px;justify-items:center"><svg width="24" height="24" viewBox="0 0 256 256" fill="currentColor">${svg}</svg><span>${name}</span></div>`).join('')}</div></body>`);
    const bounds = await page.locator('svg').evaluateAll(icons => icons.map(svg => {
      const box = svg.querySelector('g').getBBox();
      return {width:box.width,height:box.height,x:box.x,y:box.y,stroke:getComputedStyle(svg.querySelector('g')).strokeWidth};
    }));
    assert.equal(bounds.length, 56);
    for (const box of bounds) {
      assert.ok(box.width + box.height > 0);
      assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= 24.1 && box.y + box.height <= 24.1, JSON.stringify(box));
      assert.equal(box.stroke, '1.65px');
    }
    if (process.env.ARK_UI_SHOTS) await page.screenshot({path:resolve(process.env.ARK_UI_SHOTS,'icons.png')});
  } finally { await browser.close(); }
});
