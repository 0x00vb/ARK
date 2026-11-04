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
      const api={location:{runtime:'local',project:'p'},view:view=>views.set(view.id,view),on:()=>()=>{},openView(...args){window.opened.push(args);},async call(method,input){if(method==='automations')return input.cursor ? {entries:entries.slice(1),next:null} : {entries:entries.slice(0,1),next:"page2"};if(['agents','artifacts','tools','project_models'].includes(method))return [];if(method==='automation_history')return {runs:[],next:null};if(method==='automation_validate')return {can_save:true,issues:[],activation_issues:[]};if(method==='automation_get')return {legacy:{draft:{name:'Existing routine',trigger:{kind:'manual'},inputs:[],authority:{actions:[],max_calls:8,max_mutations:0},steps:[]},schedule:'legacy',version:1,policies:{source_digest:'legacy',rules:[],after:[],max_attempts:1,retry_delay_ms:0}}};if(method==='schedules')return [{id:'s',target:{automation:'a'},next_ms:null},{id:'legacy',objective:'Existing routine',says:'Monday09:00',state:'scheduled',status:'Active'}];throw Error(method);}};
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
    await page.getByRole('region', {name:'Automation configuration'}).waitFor();
    await page.getByLabel('Automation name',{exact:true}).waitFor();
    assert.equal(await page.getByLabel('Automation name',{exact:true}).inputValue(),'Existing routine');
    assert.deepEqual(await page.evaluate(()=>window.opened),[]);
    assert.equal(await page.locator('html').evaluate(el=>el.scrollWidth<=innerWidth),true);
    assert.deepEqual(errors,[]);await page.evaluate(()=>window.controller.dispose());
  } finally { await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done)); }
});

test('canvas creates in place, separates drawing from execution, and preserves pending saves and scoped layout', async () => {
  const root = resolve(import.meta.dirname, '../../web');
  const server = createServer(async (request, response) => {
    const path = new URL(request.url, 'http://fixture').pathname;
    if (path === '/') { response.setHeader('Content-Type','text/html'); return response.end('<link rel="stylesheet" href="/app.css"><style>html,body{height:100%;margin:0}#stage{height:100%}</style><div id="stage"></div>'); }
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) { response.writeHead(404); return response.end(); }
    try { response.setHeader('Content-Type', extname(file) === '.css' ? 'text/css' : 'text/javascript'); response.end(await readFile(file)); }
    catch { response.writeHead(404); response.end(); }
  });
  await new Promise(done => server.listen(0,'127.0.0.1',done));
  const browser = await webkit.launch();
  try {
    const page = await browser.newPage({viewport:{width:1100,height:800}});
    page.setDefaultTimeout(5000);
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async()=>{
      const {registerAutomations}=await import('/js/automations.js');const views=new Map();
      const entries=[], calls=[];
      const api={location:{runtime:'local',project:'canvas-a'},view:v=>views.set(v.id,v),on:()=>()=>{},openView(){throw Error('Unexpected navigation');},async call(method,input){
        calls.push({method,input:structuredClone(input)});
        if(method==='automations')return {entries,next:null};
        if(['schedules','agents','artifacts','tools','project_models'].includes(method))return [];
        if(method==='automation_history')return {runs:[],next:null};
        if(method==='automation_validate')return {can_save:true,issues:[],activation_issues:[]};
        if(method==='automation_save') {
          if(input.cancel)return {saved:window.fixture.commitReceipt || null};
          if(window.fixture.holdSave) await new Promise(resolve=>(window.fixture.saves ||= []).push(resolve));
          if(window.fixture.loseSave){ window.fixture.loseSave=false; throw Error('Save response lost'); }
          const entry={definition:{id:'saved',revision:1,enabled:false},revision:{number:1,draft:input.draft}};
          entries.splice(0,entries.length,entry);
          if(window.fixture.commitThenLose){window.fixture.commitThenLose=false;window.fixture.commitReceipt={automation:'saved',revision:1};throw Error('Committed receipt lost');}
          return {automation:'saved',revision:1};
        }
        if(method==='automation_get')return {entry:entries[0],layout:{version:0,positions:{}}};
        throw Error(method);
      }};
      registerAutomations(api);
      window.fixture={api,views,calls,loseSave:true,mount(project='canvas-a') {this.controller?.dispose();const stage=document.querySelector('#stage');stage.replaceChildren();api.location.project=project;this.controller=views.get('schedules').render(stage,{},api);}};
      window.fixture.mount();
    });
    const canvas=page.getByLabel('Automations canvas',{exact:true});
    await canvas.waitFor();
    assert.equal(await page.getByText('No automations yet').count(),0);
    assert.equal(await page.getByRole('button',{name:'Canvas',exact:true}).count(),0);
    await page.getByRole('button',{name:'Draw',exact:true}).click();
    await page.mouse.move(100,120);await page.mouse.down();await page.mouse.move(220,190,{steps:12});await page.mouse.up();
    assert.equal(await page.locator('.canvas-marks path').count(),1);
    assert.equal(await page.locator('.auto-inline-editor').count(),0);
    await page.getByRole('button',{name:'Rectangle',exact:true}).click();
    await page.mouse.move(260,120);await page.mouse.down();await page.mouse.move(400,230,{steps:5});await page.mouse.up();
    assert.equal(await page.locator('.canvas-marks path').count(),2);
    await page.getByRole('button',{name:'Undo drawing',exact:true}).click();
    assert.equal(await page.locator('.canvas-marks path').count(),1);
    await page.getByRole('button',{name:'Select',exact:true}).click();
    await page.getByRole('button',{name:'Zoom in',exact:true}).click();
    await canvas.click({position:{x:550,y:330}});
    assert.equal(await page.getByLabel('Automation name', {exact:true}).count(), 0);
    await page.getByRole('button', {name:'Add automation',exact:true}).click();
    if (process.env.ARK_UI_SHOTS) await page.screenshot({ path: `${process.env.ARK_UI_SHOTS}/automation-name-header.png` });
    await page.getByLabel('Automation name',{exact:true}).fill('Weekly review');
    await page.getByRole('button',{name:'Save',exact:true}).click();
    await page.getByText('Save response lost',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Back',exact:true}).click();
    const pending=page.getByRole('button',{name:/Weekly review/});
    const initialPosition = await pending.evaluate(el=>({x:parseFloat(el.parentElement.style.left),y:parseFloat(el.parentElement.style.top)}));
    await pending.focus();await pending.press('Alt+ArrowRight');
    assert.ok(Math.abs(await pending.evaluate(el=>parseFloat(el.parentElement.style.left))-initialPosition.x-10)<.01);
    assert.equal(await pending.evaluate(el=>el===document.activeElement),true);
    await pending.click();
    await page.getByRole('button',{name:'Retry saving',exact:true}).click();
    await page.getByRole('button',{name:'Back',exact:true}).click();
    await page.getByRole('button',{name:/Weekly review/}).waitFor();
    const attempts = await page.evaluate(()=>window.fixture.calls.filter(call=>call.method==='automation_save'));
    assert.equal(attempts.length,2);
    assert.deepEqual(attempts[0].input,attempts[1].input);
    const position = await page.evaluate(()=>JSON.parse(localStorage.getItem('ark.canvas:overview:local:canvas-a')).positions.saved);
    assert.ok(Math.abs(position.x-initialPosition.x-10)<1);
    assert.ok(Math.abs(position.y-initialPosition.y)<1);
    await page.evaluate(()=>window.fixture.mount());
    assert.equal(await page.locator('.canvas-marks path').count(),1);
    await page.evaluate(()=>window.fixture.holdSave=true);
    await page.getByRole('button',{name:'Add automation',exact:true}).click();
    await page.getByLabel('Automation name',{exact:true}).fill('Delayed save');
    await page.getByRole('button',{name:'Save',exact:true}).click();
    await page.waitForFunction(()=>window.fixture.saves?.length===1,null,{timeout:5000});
    await page.getByRole('button',{name:'Back',exact:true}).click();
    await page.getByRole('button',{name:/Delayed save/}).click();
    await page.getByRole('button',{name:'Retry saving',exact:true}).click();
    await page.waitForFunction(()=>window.fixture.saves?.length===2,null,{timeout:5000});
    await page.evaluate(()=>window.fixture.saves[0]());
    await page.waitForFunction(()=>document.querySelector('.auto-revision')?.textContent.includes('Save pending'),null,{timeout:5000});
    await page.evaluate(()=>window.fixture.saves[1]());
    await page.waitForFunction(()=>document.querySelector('.auto-revision')?.textContent.includes('Saved revision'),null,{timeout:5000});
    assert.equal(await page.locator('.auto-status').innerText(),'');
    const overlapping=await page.evaluate(()=>window.fixture.calls.filter(call=>call.method==='automation_save').slice(-2));
    assert.deepEqual(overlapping[0].input,overlapping[1].input);
    await page.getByRole('button',{name:'Back',exact:true}).click();
    await page.evaluate(()=>{window.fixture.holdSave=false;window.fixture.commitThenLose=true;});
    await page.getByRole('button',{name:'Add automation',exact:true}).click();
    await page.getByLabel('Automation name',{exact:true}).fill('Recovered draft');
    await page.getByRole('button',{name:'Save',exact:true}).click();
    await page.getByText('Committed receipt lost',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Back',exact:true}).click();
    const recovery=page.locator('[data-automation^="draft:"]',{hasText:'Recovered draft'});
    await recovery.focus();await recovery.press('Alt+ArrowRight');await recovery.click();
    await page.getByRole('button',{name:'Recover draft',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('.auto-revision')?.textContent.includes('Saved revision'),null,{timeout:5000});
    await page.getByRole('button',{name:'Back',exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll('.auto-gallery-card').length===1,null,{timeout:5000});
    assert.equal(await page.locator('[data-automation^="draft:"]').count(),0);
    const recovered=await page.evaluate(()=>JSON.parse(localStorage.getItem('ark.canvas:overview:local:canvas-a')).positions.saved);
    assert.deepEqual(recovered,{x:90,y:80});
    await page.evaluate(()=>window.fixture.mount('canvas-b'));
    assert.equal(await page.locator('.canvas-marks path').count(),0);
    assert.deepEqual(errors,[]);
    await page.evaluate(async()=>{
      const {canvasScene}=await import('/js/canvas-tools.js');
      const original=Storage.prototype.setItem;
      Storage.prototype.setItem=function(){throw Error('Quota denied');};
      const first=[], second=[];
      canvasScene('storage-check',value=>first.push(value));
      const scene=canvasScene('storage-check',value=>second.push(value));
      scene.save();
      if(!second.at(-1).includes('Quota denied') || first.some(value=>value.includes('Quota denied')))throw Error('Storage feedback targeted disposed canvas');
      Storage.prototype.setItem=original;
      window.fixture.controller.dispose();
    });
  } finally {await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done));}
});
