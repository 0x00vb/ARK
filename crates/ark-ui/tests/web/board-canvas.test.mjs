import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('work board opens scoped tickets without changing execution states; canvas connections save actual bindings', async () => {
  const root = resolve(import.meta.dirname, '../../web');
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, 'http://fixture').pathname;
    if (path === '/') { response.setHeader('Content-Type', 'text/html'); return response.end('<link rel="stylesheet" href="/app.css"><div id="stage"></div>'); }
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) { response.writeHead(404); return response.end(); }
    try { response.setHeader('Content-Type', extname(file) === '.css' ? 'text/css' : 'text/javascript'); response.end(await readFile(file)); }
    catch { response.writeHead(404); response.end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 900, height: 844 } });
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async () => {
      const { registerWork } = await import('/js/work.js');
      const { registerAutomationEditor } = await import('/js/automation-editor.js');
      const views = new Map(), calls = [], opened = [];
      let saved;
      const draft = { name: 'Drawing', trigger: { kind: 'manual' }, inputs: [], authority: { actions: [], max_calls: 8, max_mutations: 0 }, steps: ['Source', 'Target', 'Third', 'Fourth', 'Fifth', 'Sixth', 'Late source', 'Finish'].map(name => ({ id: name.toLowerCase().replaceAll(' ', '-'), name, action: { kind: 'result', inputs: [] } })) };
      const api = { location: { runtime: 'local', project: 'project-a' }, view: view => views.set(view.id, view), on: () => () => {}, openView: (...args) => opened.push(args), async call(method, input) {
        calls.push({ method, input: structuredClone(input) });
        if (method === 'work_list') return { items: [{ id: 'owned-ticket', state: 'attention', title: 'Review the proposal', target: { kind: 'agent', name: 'Editor' }, origin: 'manual' }], next: null };
        if (['agents', 'artifacts', 'tools', 'project_models'].includes(method)) return [];
        if (method === 'automation_validate') return { can_save: true, issues: [], activation_issues: [] };
        if (method === 'automation_save') { saved = structuredClone(input.draft); return { automation: 'drawing', revision: 1 }; }
        if (method === 'automation_get') return { entry: { definition: { id: input.automation, revision: 1, enabled: false }, revision: { number: 1, draft: saved } }, layout: { version: 0, positions: input.automation === 'legacy-layout' ? { source: { x: 40, y: 40 } } : {} } };
        if (method === 'automation_history') return { runs: [], next: null };
        throw Error(method);
      } };
      registerWork(api); registerAutomationEditor(api);
      window.fixture = { api, views, calls, opened, draft, controller: views.get('work').render(document.querySelector('#stage'), {}, api) };
    });
    const card = page.getByRole('button', { name: 'Review the proposal', exact: true });
    await card.waitFor();
    const restoredPan = await page.locator('.work-columns').evaluate(el => { el.scrollLeft = 300; return el.scrollLeft; });
    assert.ok(restoredPan > 0);
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('work').render(stage, {}, f.api); });
    await page.waitForFunction(value => document.querySelector('.work-columns').scrollLeft === value, restoredPan);
    await page.locator('.work-columns').evaluate(el => { el.scrollLeft = 0; });
    await page.evaluate(() => window.fixture.controller.refresh());
    assert.equal(await page.locator('.work-columns').evaluate(el => el.scrollLeft), 0);
    assert.match(await card.innerText(), /Editor/);
    await card.press('Space');
    assert.deepEqual(await page.evaluate(() => window.fixture.opened), [['work-chat', { work: 'owned-ticket', archived: false }]]);
    assert.equal(await page.evaluate(() => window.fixture.calls.some(call => call.method === 'board_change')), false);
    assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('automation-editor').render(stage, { draft_key: 'drawing', draft: f.draft }, f.api); });
    assert.equal(await page.getByRole('button', { name: 'Canvas', exact: true }).getAttribute('aria-pressed'), 'true');
    await page.getByRole('button', { name: 'Connect text from Source', exact: true }).click();
    await page.getByRole('button', { name: 'Connect input to Finish', exact: true }).click();
    await page.getByText('Connect to the next step', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Connect text from Source', exact: true }).press('Enter');
    await page.getByRole('button', { name: 'Connect input to Target', exact: true }).press('Enter');
    await page.getByText('Connected', { exact: true }).waitFor();
    await page.locator('.auto-canvas-viewport').evaluate(el => { el.scrollLeft = 2100; });
    await page.getByRole('button', { name: 'Select Finish', exact: true }).click();
    assert.ok(await page.locator('.auto-canvas-viewport').evaluate(el => el.scrollLeft) > 2000);
    assert.equal(await page.evaluate(() => document.activeElement.getAttribute('aria-label')), 'Select Finish');
    await page.getByRole('button', { name: 'Connect text from Late source', exact: true }).click();
    const pan = await page.locator('.auto-canvas-viewport').evaluate(el => el.scrollLeft);
    await page.getByRole('button', { name: 'Connect input to Finish', exact: true }).click();
    assert.equal(await page.locator('.auto-canvas-viewport').evaluate(el => el.scrollLeft), pan);
    assert.equal(await page.getByRole('button', { name: 'Select Finish', exact: true }).getAttribute('aria-pressed'), 'true');
    assert.equal(await page.getByLabel('Add step', { exact: true }).getAttribute('aria-disabled'), 'true');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    const saved = await page.evaluate(() => window.fixture.calls.find(call => call.method === 'automation_save').input.draft);
    assert.deepEqual(saved.steps[1].action.inputs, [{ kind: 'step', step: 'source', port: 'text' }]);
    assert.deepEqual(saved.steps[2].action.inputs, []);
    assert.deepEqual(saved.steps[7].action.inputs, [{ kind: 'step', step: 'late-source', port: 'text' }]);
    await page.getByRole('button', { name: 'Fit', exact: true }).click();
    const fitted = await page.locator('.auto-canvas-plane').evaluate(el => Number(el.style.transform.match(/scale\(([^)]+)\)/)[1]));
    assert.ok(fitted < .25);
    await page.getByRole('button', { name: 'Zoom out', exact: true }).click();
    const zoomed = await page.locator('.auto-canvas-plane').evaluate(el => Number(el.style.transform.match(/scale\(([^)]+)\)/)[1]));
    assert.ok(zoomed < fitted);
    await page.evaluate(() => window.fixture.controller.update({ automation: 'drawing' }));
    assert.equal(await page.locator('.auto-canvas-plane').evaluate(el => Number(el.style.transform.match(/scale\(([^)]+)\)/)[1])), zoomed);
    await page.setViewportSize({ width: 1278, height: 844 });
    await page.getByRole('button', { name: 'Close inspector', exact: true }).click();
    await page.getByRole('button', { name: 'Fit', exact: true }).click();
    await page.getByRole('button', { name: 'Select Finish', exact: true }).click();
    assert.equal(await page.locator('.auto-canvas-viewport').evaluate(view => {
      const selected = view.querySelector('.auto-node[data-selected=true]').getBoundingClientRect(), bounds = view.getBoundingClientRect();
      return selected.left >= bounds.left - 1 && selected.right <= bounds.right + 1;
    }), true);
    await page.evaluate(() => window.fixture.controller.update({ automation: 'legacy-layout' }));
    await page.getByRole('button', { name: 'Select Source', exact: true }).waitFor();
    assert.equal(await page.locator('.auto-canvas-viewport').evaluate(view => {
      const start = view.querySelector('.auto-start').getBoundingClientRect(), source = view.querySelector('[data-step="source"]').getBoundingClientRect();
      return start.right <= source.left || start.left >= source.right || start.bottom <= source.top || start.top >= source.bottom;
    }), true);
    assert.equal(await page.evaluate(() => window.fixture.calls.some(call => call.method === 'automation_layout')), false);
    assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
    assert.deepEqual(errors, []);
    await page.evaluate(() => window.fixture.controller.dispose());
  } finally { await browser.close(); server.closeAllConnections(); await new Promise(done => server.close(done)); }
});
