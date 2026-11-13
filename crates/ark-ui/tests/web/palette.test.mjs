import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import test from 'node:test';

function hsl(r, g, b) {
  [r, g, b] = [r, g, b].map(v => v / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (d === 0) return { h: 0, s: 0 };
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return { h: (h * 60 + 360) % 360, s };
}

test('no palette uses a cyan colour', async () => {
  const css = await readFile(resolve(import.meta.dirname, '../../web/app.css'), 'utf8');
  const cyan = [];
  for (const [, name, value] of css.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    const colours = [...value.matchAll(/#([0-9a-f]{6})\b/gi)].map(([, x]) => [0, 2, 4].map(i => parseInt(x.slice(i, i + 2), 16)));
    colours.push(...[...value.matchAll(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/g)].map(m => m.slice(1, 4).map(Number)));
    for (const rgb of colours) {
      const { h, s } = hsl(...rgb);
      if (h >= 160 && h <= 205 && s > 0.12) cyan.push(`${name}: ${value.trim()}`);
    }
  }
  assert.deepEqual(cyan, []);
});

test('selected text is visible on every palette and stays readable', async () => {
  const { webkit } = await import('playwright-core');
  const css = await readFile(resolve(import.meta.dirname, '../../web/app.css'), 'utf8');
  const lum = ([r, g, b]) => { const f = v => (v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
  const contrast = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage();
    const seen = {};
    for (const [theme, floor] of [['light', 1.5], ['dark', 2], ['monokai', 2], ['dracula', 2], ['github', 1.5], ['xcode', 1.5]]) {
      await page.setContent(`<html data-theme="${theme}"><style>${css}</style><body><p>Selected words</p></body></html>`);
      const { bg, selection, text } = await page.evaluate(() => {
        const ctx = document.createElement('canvas').getContext('2d', { willReadFrequently: true });
        const paint = (...colours) => { ctx.clearRect(0, 0, 1, 1); for (const c of colours) { ctx.fillStyle = c; ctx.fillRect(0, 0, 1, 1); } return [...ctx.getImageData(0, 0, 1, 1).data].slice(0, 3); };
        const bg = getComputedStyle(document.body).backgroundColor;
        const sel = getComputedStyle(document.querySelector('p'), '::selection').backgroundColor;
        return { bg: paint(bg), selection: paint(bg, sel), text: paint(getComputedStyle(document.body).color) };
      });
      seen[theme] = { selection: +contrast(selection, bg).toFixed(2), text: +contrast(text, selection).toFixed(1) };
      assert.ok(seen[theme].selection >= floor, `${theme}: ${JSON.stringify(seen[theme])}`);
      assert.ok(seen[theme].text >= 4.5, `${theme}: ${JSON.stringify(seen[theme])}`);
    }
  } finally { await browser.close(); }
});
