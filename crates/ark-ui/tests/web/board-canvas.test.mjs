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
      const views = new Map(), calls = [], opened = [], listeners = new Set();
      let saved, workState = "in_progress";
      const draft = { name: 'Drawing', trigger: { kind: 'manual' }, inputs: [], authority: { actions: [], max_calls: 8, max_mutations: 0 }, steps: ['Source', 'Target', 'Third', 'Fourth', 'Fifth', 'Sixth', 'Late source', 'Finish'].map(name => ({ id: name.toLowerCase().replaceAll(' ', '-'), name, action: { kind: 'result', inputs: [] } })) };
      const api = { location: { runtime: 'local', project: 'project-a' }, view: view => views.set(view.id, view), on: (_event, listener) => { listeners.add(listener); return () => listeners.delete(listener); }, openAgentChat: name => opened.push(['agent', name]), openSession: id => opened.push(['session', id]), openView: (...args) => opened.push(args), reportFocus() {}, async call(method, input) {
        calls.push({ method, input: structuredClone(input) });
        if (method === 'board') return { boards: [{ id: 'board-a', goal: 'Release' }], items: [{ id: 'owned-ticket', board: 'board-a', title: 'Review the proposal', state: 'doing', owner: window.fixture?.unassigned ? '' : window.fixture?.reassigned ? 'Writer' : 'Editor', conversation: window.fixture?.manual ? null : 'chat-a', tasks: [] }], tasks: {}, agents: [] };
        if (method === 'create_agent') { if (!/^[a-zA-Z0-9_.-]+$/.test(input.spec.name)) throw Error('Invalid agent name'); if (calls.filter(call => call.method === method).length === 1) throw Error('Creation response lost'); return { name: input.spec.name }; }
        if (method === 'board_change') { if (!input.change?.owner) throw Error('missing owner change'); return {}; }
        if (method === 'work_get') return window.fixture.manual ? { active_receipt: null, item: { target: null } } : { active_receipt: 'source-receipt', item: { target: { kind: 'agent', name: 'Editor' } } };
        if (method === 'work_admit') { workState = 'in_progress'; if (calls.filter(call => call.method === method).length === 1) throw Error('Admission response lost'); return { id: 'continued-receipt', conversation: 'agent-chat' }; }
        if (method === 'work_execute') { if (calls.filter(call => call.method === method).length === 1) throw Error('Execution response lost'); return {}; }
        if (method === 'work_list') { if (window.fixture?.defer) await new Promise(resolve => window.fixture.release = resolve); return { items: [{ id: 'owned-ticket', state: workState, status: workState, title: 'Review the proposal', target: { kind: 'agent', name: 'Editor' }, origin: 'manual' }], next: null }; }
        if (['agents', 'artifacts', 'tools', 'project_models'].includes(method)) return [];
        if (method === 'automation_validate') return { can_save: true, issues: [], activation_issues: [] };
        if (method === 'automation_save') { saved = structuredClone(input.draft); return { automation: 'drawing', revision: 1 }; }
        if (method === 'automation_get') return { entry: { definition: { id: input.automation, revision: 1, enabled: false }, revision: { number: 1, draft: saved } }, layout: { version: 0, positions: input.automation === 'legacy-layout' ? { source: { x: 40, y: 40 } } : {} } };
        if (method === 'automation_history') return { runs: [], next: null };
        throw Error(method);
      } };
      registerWork(api); registerAutomationEditor(api);
      window.fixture = { api, views, calls, opened, draft, listeners, setWorkState: value => { workState = value; }, controller: views.get('work').render(document.querySelector('#stage'), {}, api) };
    });
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
    await page.evaluate(() => { const f = window.fixture; f.controller.dispose(); const stage = document.querySelector('#stage'); stage.replaceChildren(); f.controller = f.views.get('automation-editor').render(stage, { draft_key: 'drawing', draft: f.draft }, f.api); });
    assert.equal(await page.getByRole("button", { name: "Canvas", exact: true }).count(), 0);
    assert.equal(await page.getByRole("button", { name: "Steps", exact: true }).count(), 0);
    await page.getByRole('button', { name: 'Connect text from Source', exact: true }).click();
    await page.getByRole('button', { name: 'Connect input to Finish', exact: true }).click();
    await page.getByText('Connect to the next step', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Connect text from Source', exact: true }).press('Enter');
    await page.getByRole('button', { name: 'Connect input to Target', exact: true }).press('Enter');
    await page.getByRole('button', { name: 'Select Target', exact: true }).press('Shift+F10');
    await page.getByRole('menu').waitFor();
    await page.keyboard.press('Escape');
    assert.equal(await page.getByRole('menu').count(), 0);
    assert.equal(await page.getByRole('button', { name: 'Select Target', exact: true }).evaluate(el => el === document.activeElement), true);
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
    await page.getByRole('button', { name: 'Collapse card', exact: true }).click();
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
