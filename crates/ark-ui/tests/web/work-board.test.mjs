import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('work board under View all opens scoped tickets without changing execution states', async () => {
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
      const views = new Map(), calls = [], opened = [], listeners = new Set();
      let saved, workState = "in_progress";
      const api = { dictationRoute: () => ({ place: '', session: '' }), summary: () => ({}), toast: message => { throw Error(message); }, popover: () => ({ close() {} }), location: { runtime: 'local', project: 'project-a' }, view: view => views.set(view.id, view), on: (_event, listener) => { listeners.add(listener); return () => listeners.delete(listener); }, openAgentChat: name => opened.push(['agent', name]), openSession: id => opened.push(['session', id]), openView: (...args) => opened.push(args), reportFocus() {}, async call(method, input) {
        calls.push({ method, input: structuredClone(input) });
        if (method === 'board') return { boards: [{ id: 'board-a', goal: 'Release' }], items: [{ id: 'owned-ticket', board: 'board-a', title: 'Review the proposal', state: 'doing', owner: window.fixture?.unassigned ? '' : window.fixture?.reassigned ? 'Writer' : 'Editor', conversation: window.fixture?.manual ? null : 'chat-a', tasks: [] }], tasks: {}, agents: [] };
        if (method === 'create_agent') { if (!/^[a-zA-Z0-9_.-]+$/.test(input.spec.name)) throw Error('Invalid agent name'); if (calls.filter(call => call.method === method).length === 1) throw Error('Creation response lost'); return { name: input.spec.name }; }
        if (method === 'board_change') { if (!input.change?.owner) throw Error('missing owner change'); return {}; }
        if (method === 'work_get') return window.fixture.manual ? { active_receipt: null, item: { target: null } } : { active_receipt: 'source-receipt', item: { target: { kind: 'agent', name: 'Editor' } } };
        if (method === 'work_admit') { workState = 'in_progress'; if (calls.filter(call => call.method === method).length === 1) throw Error('Admission response lost'); return { id: 'continued-receipt', conversation: 'agent-chat' }; }
        if (method === 'work_execute') { if (calls.filter(call => call.method === method).length === 1) throw Error('Execution response lost'); return {}; }
        if (method === 'work_list') { if (window.fixture?.defer) await new Promise(resolve => window.fixture.release = resolve); return { items: [{ id: 'owned-ticket', state: workState, status: workState, title: 'Review the proposal', target: { kind: 'agent', name: 'Editor' }, origin: 'manual' }], next: null }; }
        throw Error(method);
      } };
      registerWork(api);
      window.fixture = { api, views, calls, opened, listeners, setWorkState: value => { workState = value; }, controller: views.get('work').render(document.querySelector('#stage'), {}, api) };
    });
    await page.getByRole('button', { name: 'View all', exact: true }).click();
    const card = page.getByRole('button', { name: 'Review the proposal', exact: true });
    await card.waitFor();
    assert.equal(await card.locator(".bd-avatar").getAttribute("title"), "Editor");
    await card.press('Enter');
    await page.getByLabel('Title', { exact: true }).waitFor();
    assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), 'Review the proposal');
    assert.deepEqual(await page.evaluate(() => window.fixture.opened), []);
    assert.equal(await page.getByRole('button', { name: 'Send message', exact: true }).isDisabled(), true);
    await page.evaluate(() => { const f = window.fixture; f.setWorkState('completed'); f.defer = true; for (const listener of f.listeners) listener(); });
    await page.waitForFunction(() => !!window.fixture.release);
    await page.evaluate(() => { const f = window.fixture; f.defer = false; f.release(); delete f.release; });
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Continue this ticket');
    await page.waitForFunction(() => document.querySelector('.bd-p-chat-compose button')?.disabled === false, null, { timeout: 3000 });
    assert.equal(await page.locator('.ticket-property-label').count(), 0);
    const circles = await page.evaluate(() => {
      const circle = document.querySelector('.ticket-properties .bd-mark circle');
      return { status: (2 * circle.r.baseVal.value + 1.5) * circle.getScreenCTM().a, avatar: document.querySelector('.ticket-properties .bd-avatar').getBoundingClientRect().width };
    });
    assert.ok(Math.abs(circles.status - circles.avatar) < 1, JSON.stringify(circles));

    assert.equal(await page.locator('.bd-p-chat-open').innerText(), 'Conversation');

    await page.getByRole('button', { name: /Conversation/ }).click();
    assert.deepEqual(await page.evaluate(() => window.fixture.opened), [['agent', 'Editor']]);
    assert.equal(await page.evaluate(() => window.fixture.calls.some(call => call.method === 'board_change')), false);
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Continue this ticket');
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
    await page.getByText('Admission response lost', { exact: true }).waitFor();
    await page.locator('.bd-detail').getByRole('button', { name: 'Work', exact: true }).click();
    await page.evaluate(() => window.fixture.controller.refresh());
    await card.press('Enter');
    await page.getByRole('button', { name: 'Retry sending', exact: true }).click();
    await page.getByText('Execution response lost', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Retry sending', exact: true }).click();
    await page.waitForFunction(() => window.fixture.opened.some(entry => entry[0] === 'agent' && entry[1] === 'Editor'));
    const submissions = await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'work_admit'));
    assert.equal(submissions.length, 2);
    assert.deepEqual(submissions[0].input, submissions[1].input);
    assert.equal(submissions[0].input.receipt, 'source-receipt');
    assert.deepEqual(submissions[0].input.target, { kind: 'agent', name: 'Editor' });
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'work_execute').length), 2);
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); f.manual = true; const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('work').render(stage, {}, f.api); });
    await card.press('Enter');
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Start this assigned manual ticket');
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
    await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'work_execute').length === 3);
    const manual = await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'work_admit').at(-1).input);
    assert.deepEqual(manual.target, { kind: 'agent', name: 'Editor' });
    assert.equal(manual.receipt, null);
    assert.equal(manual.task.item, 'owned-ticket');
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); f.manual = false; f.reassigned = true; f.setWorkState('completed'); const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('work').render(stage, {}, f.api); });
    await card.press('Enter');
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Ask the new owner');
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
    await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'work_execute').length === 4);
    const reassigned = await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'work_admit').at(-1).input);
    assert.deepEqual(reassigned.target, { kind: 'agent', name: 'Writer' });
    assert.equal(reassigned.receipt, null);
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); f.manual = true; f.unassigned = true; const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('work').render(stage, {}, f.api); });
    await card.press('Enter');
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Create someone for this work');
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
    await page.getByText('Creation response lost', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Retry sending', exact: true }).click();
    await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'work_execute').length === 5);
    const creations = await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'create_agent'));
    assert.deepEqual(creations[0].input, creations[1].input);
    const assignment = await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'board_change').at(-1).input);
    assert.equal(assignment.change.owner, creations[0].input.spec.name);



    assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
    assert.deepEqual(errors, []);
    await page.evaluate(() => window.fixture.controller.dispose());
  } finally { await browser.close(); server.closeAllConnections(); await new Promise(done => server.close(done)); }
});
