import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

async function fixture(run, viewport = { width: 1000, height: 800 }) {
  const root = resolve(import.meta.dirname, '../../web');
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, 'http://fixture').pathname;
    if (path === '/') { response.setHeader('Content-Type', 'text/html'); return response.end('<link rel="stylesheet" href="/app.css"><style>html,body{height:100%;margin:0}#stage{height:100%;overflow:auto}</style><div id="stage"></div>'); }
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) { response.writeHead(404); return response.end(); }
    try { response.setHeader('Content-Type', extname(file) === '.css' ? 'text/css' : 'text/javascript'); response.end(await readFile(file)); }
    catch { response.writeHead(404); response.end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage({ viewport });
    page.setDefaultTimeout(5000);
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async () => {
      const { registerAutomations } = await import('/js/automations.js');
      const views = new Map(), calls = [], opened = [], panels = [];
      const listeners = new Set();
      const step = (id, prompt) => ({ id, name: prompt, action: { kind: 'agent', target: { kind: 'general', model: 'claude:sonnet' }, prompt, inputs: [] } });
      const result = (id, from) => ({ id: 'result', name: 'Result', action: { kind: 'result', inputs: [{ kind: 'step', step: from, port: 'text' }, { kind: 'step', step: from, port: 'files' }] } });
      const store = new Map([['daily', { definition: { id: 'daily', revision: 3, enabled: true, active_revision: 3 }, revision: { number: 3, draft: { name: 'Morning summary', trigger: { kind: 'schedule', when: { at: '08:30', tz: 'UTC', days: [], once: false } }, inputs: [{ name: 'topic', port: 'text', required: false }], authority: { actions: [], max_calls: 12, max_mutations: 2 }, steps: [step('read', 'Read my inbox'), result('result', 'read')] } }, activation: { selections: { read: { model: 'claude:sonnet', agent: null } }, agents: {}, descriptors: {} } }]]);
      let next = 1;
      const api = {
        location: { runtime: 'local', project: 'p' }, view: view => views.set(view.id, view), on: (_name, fn) => { listeners.add(fn); return () => listeners.delete(fn); },
        openSession(id) { opened.push(id); }, openPanel(id, params) { panels.push([id, params]); }, openView() { throw Error('unexpected navigation'); },
        async call(method, input) {
          calls.push({ method, input: structuredClone(input) });
          if (method === 'automations') return { entries: [...store.values()], next: null };
          if (method === 'schedules') return [{ id: 'old', objective: 'Recount the stock', says: 'every day at 02:00', state: 'scheduled', status: 'next tomorrow' }];
          if (method === 'automation_preview') return { next: [{ label: 'tomorrow at 08:30' }] };
          if (method === 'automation_save') {
            const id = input.automation || `new-${next++}`, prior = store.get(id);
            const revision = (prior?.definition.revision || 0) + 1;
            store.set(id, { ...prior, definition: { ...(prior?.definition || { enabled: false, active_revision: null }), id, revision }, revision: { number: revision, draft: input.draft } });
            return { automation: id, revision };
          }
          if (method === 'automation_get') { const entry = store.get(input.automation); return { entry: { ...entry, last_start: null, pending_start: null }, activation: entry.definition.enabled ? entry.activation || null : null }; }
          if (method === 'automation_pause') { store.get(input.automation).definition.enabled = false; return {}; }
          if (method === 'automation_prepare') return { id: 'act', digest: 'd', selections: { [input.automation === 'daily' ? 'read' : 'x']: { model: window.fixture.model || 'claude:sonnet', agent: null } }, agents: {}, descriptors: {} };
          if (method === 'automation_activate') { const d = store.get(input.automation).definition; d.enabled = true; d.active_revision = d.revision; return {}; }
          if (method === 'automation_begin_start') return { ...input };
          if (method === 'automation_run') return { id: 'run-1', work: 'work-1', conversation: 'chat-run-1' };
          if (method === 'automation_acknowledge_start') return { run: 'run-1' };
          throw Error(method);
        },
      };
      registerAutomations(api);
      window.fixture = { calls, opened, panels, store, emit: () => { for (const fn of listeners) fn({}); }, controller: views.get('schedules').render(document.querySelector('#stage'), {}, api) };
    });
    await run(page);
    assert.deepEqual(errors, []);
    await page.evaluate(() => window.fixture.controller.dispose());
  } finally { await browser.close(); server.closeAllConnections(); await new Promise(done => server.close(done)); }
}

const saves = page => page.evaluate(() => window.fixture.calls.filter(call => call.method === 'automation_save').map(call => call.input));

test('a card is a name, its steps in plain words and when; nothing else', () => fixture(async page => {
  const card = page.locator('.auto-card', { has: page.getByLabel('Automation name') }).first();
  await card.waitFor();
  assert.equal(await card.getByLabel('Automation name').inputValue(), 'Morning summary');
  assert.equal(await card.getByLabel('What it does').inputValue(), 'Read my inbox');
  assert.equal(await card.getByLabel('When').inputValue(), 'daily');
  assert.equal(await card.getByLabel('Time').inputValue(), '08:30');
  assert.equal(await card.getByRole('switch', { name: 'On' }).getAttribute('aria-checked'), 'true');
  await card.getByText('Next tomorrow at 08:30').waitFor();
  for (const gone of ['Execution inputs', 'Source', 'History', 'Revision', 'Canvas', 'Trigger', 'Responsible', 'Model']) assert.equal(await page.getByText(gone, { exact: false }).count(), 0, gone);
  await page.getByRole('button', { name: /Recount the stock/ }).click();
  assert.deepEqual(await page.evaluate(() => window.fixture.panels), [['schedule', { id: 'old' }]]);
  if (process.env.ARK_UI_SHOTS) { await mkdir(process.env.ARK_UI_SHOTS, { recursive: true }); await page.screenshot({ path: `${process.env.ARK_UI_SHOTS}/automations-light.png` }); await page.evaluate(() => document.documentElement.dataset.theme = 'dark'); await page.screenshot({ path: `${process.env.ARK_UI_SHOTS}/automations-dark.png` }); }
}));

test('typing saves; steps chain; Run opens the run as a normal chat', () => fixture(async page => {
  await page.getByRole('button', { name: 'New automation' }).click();
  const card = page.locator('.auto-card').first();
  assert.equal(await card.getByLabel('What it does').evaluate(el => el === document.activeElement), true);
  assert.equal(await card.getByRole('button', { name: 'Run' }).isDisabled(), true);
  await page.keyboard.type('Collect the open invoices');
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_save'));
  let saved = (await saves(page)).at(-1);
  assert.equal(saved.automation, null);
  assert.equal(saved.draft.name, 'Collect the open invoices');
  assert.deepEqual(saved.draft.steps.map(step => step.action.kind), ['agent', 'result']);
  assert.deepEqual(saved.draft.steps[1].action.inputs.map(binding => binding.step), [saved.draft.steps[0].id, saved.draft.steps[0].id]);

  await card.getByRole('button', { name: 'Step' }).click();
  assert.equal(await card.getByRole('textbox', { name: 'Step 2' }).evaluate(el => el === document.activeElement), true);
  await page.keyboard.type('Email me the total');
  await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'automation_save').length === 2);
  saved = (await saves(page)).at(-1);
  assert.equal(saved.automation, 'new-1');
  assert.equal(saved.expected, 1);
  const [first, second, last] = saved.draft.steps;
  assert.deepEqual(second.action.inputs, [{ kind: 'step', step: first.id, port: 'text' }, { kind: 'step', step: first.id, port: 'files' }]);
  assert.deepEqual(last.action.inputs.map(binding => binding.step), [second.id, second.id]);
  assert.equal(second.name, 'Email me the total');

  await card.getByRole('button', { name: 'Run' }).click();
  await page.waitForFunction(() => window.fixture.opened.length === 1);
  assert.deepEqual(await page.evaluate(() => window.fixture.opened), ['chat-run-1']);
  const order = await page.evaluate(() => window.fixture.calls.map(call => call.method).filter(method => method.startsWith('automation_') && /start|run/.test(method)));
  assert.deepEqual(order, ['automation_begin_start', 'automation_run', 'automation_acknowledge_start']);
}));

test('a schedule turns on and off from the card, and the page fits a phone', () => fixture(async page => {
  const card = page.locator('.auto-card').first();
  const toggle = card.getByRole('switch', { name: 'On' });
  await toggle.click();
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_pause'));
  await page.waitForFunction(() => document.querySelector('.auto-switch')?.getAttribute('aria-checked') === 'false');
  await card.getByLabel('When').selectOption('weekdays');
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_save'));
  const saved = (await saves(page)).at(-1);
  assert.deepEqual(saved.draft.trigger.when.days, ['mon', 'tue', 'wed', 'thu', 'fri']);
  assert.equal(saved.draft.trigger.when.at, '08:30');
  await page.locator('.auto-switch').click();
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_activate'));
  await page.waitForFunction(() => document.querySelector('.auto-switch')?.getAttribute('aria-checked') === 'true');
  await card.getByLabel('When').selectOption('manual');
  await page.waitForFunction(() => !document.querySelector('.auto-switch'));
  assert.equal(await page.locator('html').evaluate(el => el.scrollWidth <= innerWidth), true);
  if (process.env.ARK_UI_SHOTS) await page.screenshot({ path: `${process.env.ARK_UI_SHOTS}/automations-phone.png` });
}, { width: 390, height: 844 }));

test('reloads never drop typing; unshown fields stay; an active schedule follows only safe edits', () => fixture(async page => {
  const card = page.locator('.auto-card').first();
  const text = card.getByLabel('What it does');
  await card.getByText('Next tomorrow at 08:30').waitFor();
  await text.click(); await text.press('End');
  await page.evaluate(() => window.fixture.emit());
  await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'automations').length === 2);
  await page.keyboard.type(' and calendar');
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_save'));
  let saved = (await saves(page)).at(-1);
  assert.equal(saved.draft.steps[0].action.prompt, 'Read my inbox and calendar');
  assert.equal(saved.draft.steps[0].name, 'Read my inbox and calendar');
  assert.equal(saved.expected, 3);
  assert.deepEqual(saved.draft.inputs, [{ name: 'topic', port: 'text', required: false }]);
  assert.deepEqual(saved.draft.authority, { actions: [], max_calls: 12, max_mutations: 2 });
  assert.deepEqual(saved.draft.steps[0].action.target, { kind: 'general', model: 'claude:sonnet' });
  // Same model as the running revision: the edit goes live without asking.
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_activate'));

  // Another model would grant more: the change waits for the person.
  await page.evaluate(() => { window.fixture.model = 'other:model'; });
  await page.keyboard.type('\tnow');
  await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'automation_prepare').length === 2);
  await page.waitForFunction(() => document.querySelector('.auto-switch')?.getAttribute('aria-checked') === 'mixed');
  assert.equal(await page.evaluate(() => window.fixture.calls.filter(call => call.method === 'automation_activate').length), 1);
  saved = (await saves(page)).at(-1);
  assert.equal(saved.draft.steps[0].name, 'Read my inbox and calendar now');

  // An empty new step does not hold back the rest of the card.
  await card.getByRole('button', { name: 'Step' }).click();
  await card.getByLabel('Automation name').fill('Inbox');
  await page.waitForFunction(() => window.fixture.calls.filter(call => call.method === 'automation_save').at(-1).input.draft.name === 'Inbox');
  assert.deepEqual((await saves(page)).at(-1).draft.steps.map(step => step.action.kind), ['agent', 'result']);

  // When I run it: the old schedule stops.
  await card.getByLabel('When').selectOption('manual');
  await page.waitForFunction(() => window.fixture.calls.some(call => call.method === 'automation_pause'));
  await page.locator('body').click({ position: { x: 5, y: 700 } });
  await page.waitForFunction(() => !document.querySelector('.auto-switch'));
}));
