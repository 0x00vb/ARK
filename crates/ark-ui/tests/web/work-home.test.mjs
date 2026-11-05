import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('Work is a composer over the open work; View all brings the board; rows open their chat', async () => {
  const root = resolve(import.meta.dirname, '../../web');
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, 'http://fixture').pathname;
    if (path === '/') { response.setHeader('Content-Type', 'text/html'); return response.end('<link rel="stylesheet" href="/app.css"><style>html,body{height:100%;margin:0}#stage{height:100%}</style><div id="stage"></div>'); }
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) { response.writeHead(404); return response.end(); }
    try { response.setHeader('Content-Type', extname(file) === '.css' ? 'text/css' : 'text/javascript'); response.end(await readFile(file)); }
    catch { response.writeHead(404); response.end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const browser = await webkit.launch();
  const shots = process.env.ARK_UI_SHOTS;
  if (shots) await mkdir(shots, { recursive: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
    page.setDefaultTimeout(5000);
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async () => {
      const { registerWork } = await import('/js/work.js');
      const views = new Map(), calls = [], opened = [], listeners = new Set();
      const items = [
        { id: 'ask', board: 'b', title: 'Choose the venue for the offsite', state: 'doing', owner: 'Planner', tasks: [] },
        { id: 'run', board: 'b', title: 'Read the quarterly report and list the risks', state: 'doing', owner: 'Analyst', tasks: [] },
        { id: 'todo', board: 'b', title: 'Clean the contact export', state: 'todo', owner: '', tasks: [] },
        { id: 'done', board: 'b', title: 'Send the invoice', state: 'done', owner: '', tasks: [] },
      ];
      const work = [
        { id: 'ask', state: 'attention', status: 'Answered', title: items[0].title, conversation: 'chat-ask', conversation_missing: false },
        { id: 'run', state: 'in_progress', status: 'Reading report.pdf', title: items[1].title, conversation: 'chat-run', conversation_missing: false },
        { id: 'queued', state: 'waiting', status: 'Waiting', title: 'Summarize the support tickets', conversation: null, conversation_missing: false },
        { id: 'broke', state: 'failed', status: 'Failed', title: 'Translate the press kit', conversation: 'chat-broke', conversation_missing: false, failures: [] },
      ];
      let executions = 0;
      const api = {
        location: { runtime: 'local', project: 'p' }, view: view => views.set(view.id, view), on: (_e, fn) => { listeners.add(fn); return () => listeners.delete(fn); },
        dictationRoute: () => ({ place: '', session: '' }), summary: () => ({}), toast: message => { window.fixture.toasts.push(message); }, popover: () => ({ close() {} }),
        openSession(id) { opened.push(id); }, openView() {}, openAgentChat() {}, reportFocus() {}, setPanelTitle() {},
        async call(method, input) {
          calls.push({ method, input: structuredClone(input) });
          if (method === 'board') return { boards: [{ id: 'b', goal: 'Work' }], items, tasks: {}, agents: [] };
          if (method === 'work_list') return { items: work, next: null };
          if (method === 'work_admit') return { id: 'receipt', work: 'new', conversation: 'chat-new' };
          if (method === 'work_archive') return {};
          if (method === 'work_execute') { if (++executions === 1) throw Error('Execution reply lost'); work.push({ id: 'new', state: 'in_progress', status: 'Starting', title: 'Compare the three hosting offers', conversation: 'chat-new', conversation_missing: false }); return {}; }
          throw Error(method);
        },
      };
      registerWork(api);
      window.fixture = { calls, opened, toasts: [], controller: views.get('work').render(document.querySelector('#stage'), {}, api) };
    });
    const input = page.getByRole('textbox', { name: 'New work' });
    await page.getByRole('region', { name: 'Needs you' }).waitFor();
    assert.deepEqual(await page.locator('.work-group-title').evaluateAll(nodes => nodes.map(node => node.firstChild.textContent)), ['Needs you', 'Working', 'Waiting', 'To do']);
    assert.equal(await page.getByText('Send the invoice').isVisible(), false);
    const box = await input.boundingBox(), rows = await page.locator('.work-list').boundingBox();
    assert.ok(box.y > 120 && box.y < rows.y, JSON.stringify({ box, rows }));
    if (shots) await page.screenshot({ path: `${shots}/work-home-light.png` });

    const failed = page.getByRole('button', { name: /Translate the press kit/ });
    assert.equal(await failed.evaluate(el => el.closest('section').getAttribute('aria-label')), 'Needs you');
    await failed.click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Archive', exact: true }).click();
    assert.deepEqual(await page.evaluate(() => window.fixture.calls.find(call => call.method === 'work_archive').input), { work: 'broke', archived: true });

    await input.fill('Compare the three hosting offers');
    await input.press('Enter');
    await page.waitForFunction(() => window.fixture.toasts.includes('Execution reply lost'));
    assert.equal(await input.inputValue(), 'Compare the three hosting offers');
    await input.press('Enter');
    await page.getByRole('button', { name: /Compare the three hosting offers/ }).waitFor();
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'work_admit').length), 1, 'a retry reuses the admitted work');
    const admitted = await page.evaluate(() => window.fixture.calls.find(call => call.method === 'work_admit').input);
    assert.deepEqual(admitted.target, { kind: 'general', model: null });
    assert.equal(admitted.task.objective, 'Compare the three hosting offers');
    assert.equal(await input.inputValue(), '');

    await page.getByRole('button', { name: /Read the quarterly report/ }).click();
    assert.deepEqual(await page.evaluate(() => window.fixture.opened), ['chat-run']);

    await page.getByRole('button', { name: 'View all', exact: true }).click();
    await page.locator('.bd-cols').waitFor();
    await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || animation.effect?.getTiming().iterations === Infinity));
    assert.equal(await page.locator('.work-list').isVisible(), false);
    assert.ok((await input.boundingBox()).y < 40);
    assert.equal(await page.getByRole('button', { name: 'Send the invoice' }).isVisible(), true);
    if (shots) await page.screenshot({ path: `${shots}/work-board-light.png` });
    await page.getByRole('button', { name: 'Show less', exact: true }).press('Escape');
    await page.locator('.work-list').waitFor();
    await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || animation.effect?.getTiming().iterations === Infinity));
    assert.equal(await page.getByRole('button', { name: 'View all', exact: true }).evaluate(el => el === document.activeElement), true);

    await page.evaluate(() => document.documentElement.dataset.theme = 'dark');
    if (shots) await page.screenshot({ path: `${shots}/work-home-dark.png` });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
    if (shots) await page.screenshot({ path: `${shots}/work-home-phone.png` });
    assert.deepEqual(errors, []);
    await page.evaluate(() => window.fixture.controller.dispose());
  } finally { await browser.close(); server.closeAllConnections(); await new Promise(done => server.close(done)); }
});
