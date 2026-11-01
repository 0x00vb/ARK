import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('library separates the current draft from its active immutable trigger', async () => {
  const root = resolve(import.meta.dirname, '../../web');
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, 'http://fixture').pathname;
    if (path === '/') { response.setHeader('Content-Type','text/html'); return response.end('<link rel="stylesheet" href="/app.css"><div id="stage"></div>'); }
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) { response.writeHead(404); return response.end(); }
    try { response.setHeader('Content-Type', extname(file) === '.css' ? 'text/css' : 'text/javascript'); response.end(await readFile(file)); }
    catch { response.writeHead(404); response.end(); }
  });
  await new Promise(done => server.listen(0,'127.0.0.1',done));
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage({ viewport:{width:390,height:844} });
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async()=>{
      const {registerAutomations}=await import('/js/automations.js');const views=new Map();
      const draft=(name,kind)=>({name,trigger:{kind},steps:[{},{}]});
      const entries=[
        {definition:{id:'a',enabled:true,revision:2,active_revision:1,schedule:'s'},revision:{draft:draft('New schedule draft','webhook')},active:{draft:draft('Published schedule','schedule')}},
        {definition:{id:'b',enabled:true,revision:2,active_revision:1},revision:{draft:draft('New webhook draft','manual')},active:{draft:draft('Published hook','webhook')},webhook_availability:'local'},
      ];
      window.opened=[];
      const api={location:{runtime:'local',project:'p'},view:view=>views.set(view.id,view),on:()=>()=>{},openView(...args){window.opened.push(args);},async call(method){if(method==='automations')return {entries,next:null};if(method==='schedules')return [{id:'s',target:{automation:'a'},next_ms:null},{id:'legacy',objective:'Existing routine',says:'Monday09:00',state:'scheduled',status:'Active'}];throw Error(method);}};
      registerAutomations(api);window.controller=views.get('schedules').render(document.querySelector('#stage'),{},api);
    });
    const schedule=page.getByRole('button',{name:/New schedule draft/});await schedule.waitFor();
    assert.match(await schedule.innerText(),/Draft revision 2: Webhook/);
    assert.match(await schedule.innerText(),/Active revision 1: Scheduled/);
    assert.match(await schedule.innerText(),/No future starts/);
    const hook=page.getByRole('button',{name:/New webhook draft/});
    assert.match(await hook.innerText(),/Draft revision 2: Manual/);
    assert.match(await hook.innerText(),/Active revision 1: Webhook/);
    assert.match(await hook.innerText(),/Local events enabled/);
    await page.getByRole('button',{name:/Existing routine/}).click();
    assert.deepEqual(await page.evaluate(()=>window.opened),[['automation-editor',{legacy_schedule:'legacy'}]]);
    assert.equal(await page.locator('html').evaluate(el=>el.scrollWidth<=innerWidth),true);
    assert.deepEqual(errors,[]);await page.evaluate(()=>window.controller.dispose());
  } finally { await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done)); }
});
