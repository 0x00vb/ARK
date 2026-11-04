import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('card configuration preserves authority; contextual actions support keyboard, edges and text editing', async () => {
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
      const {editor}=await import('/js/automation-editor.js');
      const calls=[];
      const draft={name:'Morning review',trigger:{kind:'manual'},inputs:[],authority:{actions:[{action:'file.write',inputs:{},unattended:false,per_run:2,quota:{count:3,window_ms:3600000}}],max_calls:12,max_mutations:2},steps:[{id:'writer',name:'Write',action:{kind:'agent',target:{kind:'general',model:null},prompt:'Review the notes',inputs:[]}},{id:'result',name:'Result',action:{kind:'result',inputs:[]}}]};
      const api={location:{runtime:'local',project:'owned'},on:()=>()=>{},openView(){},async call(method,input){calls.push({method,input:structuredClone(input)});if(method==='artifacts')return [{id:'notes',name:'Notes'}];if(['agents','tools','project_models'].includes(method))return [];if(method==='automation_validate')return {can_save:true,issues:[],activation_issues:[]};if(method==='automation_preview')return {next:[]};if(method==='automation_save')return {automation:'owned',revision:1};if(method==='automation_history')return {runs:[],next:null};if(method==='automation_get')return {entry:{definition:{id:'owned',revision:1,enabled:false},revision:{number:1,draft:calls.find(c=>c.method==='automation_save').input.draft}},layout:{version:0,positions:{}}};throw Error(method);}};
      window.fixture={calls,draft,controller:editor(document.querySelector('#stage'),{draft_key:'owned',draft},api)};
    });
    assert.equal(await page.getByText('Permissions',{exact:true}).count(),0);
    assert.equal(await page.locator('aside.auto-inspector').count(),0);
    assert.equal(await page.locator('.auto-node').evaluateAll(nodes=>nodes.some(node=>[...node.childNodes].some(child=>child.nodeType===Node.TEXT_NODE && child.textContent==='null'))),false);
    await page.getByRole('button',{name:'Select Start',exact:true}).click();
    assert.equal(await page.getByLabel('Automation name',{exact:true}).evaluate(el=>!!el.closest('.auto-head')),true);
    await page.getByLabel('Automation name',{exact:true}).fill('Weekly review');
    await page.getByLabel('Trigger',{exact:true}).selectOption('schedule');
    assert.equal(await page.getByLabel('Time zone',{exact:true}).evaluate(el=>!!el.closest('.auto-start')),true);
    await page.getByLabel('Trigger',{exact:true}).selectOption('manual');
    await page.getByRole('button',{name:'Collapse card',exact:true}).click();
    assert.equal(await page.getByRole('button',{name:'Select Start',exact:true}).evaluate(el=>el===document.activeElement),true);
    await page.getByRole('button',{name:'Select Write',exact:true}).click();
    assert.equal(await page.getByLabel('Task',{exact:true}).evaluate(el=>el.closest('[data-step]').dataset.step),'writer');
    await page.getByLabel('Task',{exact:true}).fill('Summarize the notes');
    await page.getByRole('button',{name:'Add input',exact:true}).click();
    const source=page.getByLabel('Source',{exact:true});
    await source.scrollIntoViewIfNeeded();await source.focus();
    const card=page.locator('.auto-node-expanded');
    const scrollBefore=await card.evaluate(el=>el.scrollTop);
    assert.ok(scrollBefore>0);
    await source.selectOption('artifact');
    assert.equal(await card.evaluate(el=>el.scrollTop),scrollBefore);
    assert.equal(await source.evaluate(el=>{const r=el.getBoundingClientRect(),c=el.closest('.auto-node').getBoundingClientRect();return el===document.activeElement&&r.top>=c.top&&r.bottom<=c.bottom;}),true);

    await page.getByRole('button',{name:'Select Write',exact:true}).press('Shift+F10');
    assert.equal(await page.getByRole('menuitem',{name:'Move up',exact:true}).isDisabled(),true);
    await page.keyboard.press('End');
    assert.equal(await page.getByRole('menuitem',{name:'Remove step',exact:true}).evaluate(el=>el===document.activeElement),true);
    await page.keyboard.press('Home');
    assert.equal(await page.getByRole('menuitem',{name:'Collapse card',exact:true}).evaluate(el=>el===document.activeElement),true);
    await page.keyboard.press('ArrowDown');
    assert.equal(await page.getByRole('menuitem',{name:'Move down',exact:true}).evaluate(el=>el===document.activeElement),true);
    await page.keyboard.press('Escape');
    await page.getByLabel('Task',{exact:true}).click({button:'right'});
    assert.equal(await page.getByRole('menu').count(),0);
    await page.getByRole('button',{name:'Save',exact:true}).click();
    const saved=await page.evaluate(()=>window.fixture.calls.find(c=>c.method==='automation_save').input.draft);
    assert.equal(saved.steps[0].action.prompt,'Summarize the notes');
    assert.deepEqual(saved.steps[0].action.inputs,[{kind:'artifact',id:'notes'}]);
    assert.equal(saved.name,'Weekly review');
    assert.deepEqual(saved.authority,await page.evaluate(()=>window.fixture.draft.authority));
    // Exercise the shared menu at the lower-right edge with a disabled item.
    await page.evaluate(async()=>{
      const {bindContextMenu}=await import('/js/context-menu.js');
      const edge=document.createElement('button');edge.textContent='Edge';edge.style='position:fixed;bottom:0;right:0';document.body.append(edge);
      bindContextMenu(edge,()=>[{label:'Unavailable',disabled:true,run:()=>{throw Error('disabled action');}},{label:'Do it',run:()=>window.acted=true}]);
    });
    await page.getByRole('button',{name:'Edge',exact:true}).click({button:'right'});
    assert.equal(await page.getByRole('menu').evaluate(el=>{const r=el.getBoundingClientRect();return r.left>=0&&r.top>=0&&r.right<=innerWidth&&r.bottom<=innerHeight;}),true);
    await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>window.acted),true);
    await page.evaluate(async()=>{
      const {bindContextMenu}=await import('/js/context-menu.js');
      const scroll=document.createElement('div');scroll.id='scroll-fixture';scroll.style='position:fixed;top:0;right:0;width:100px;height:80px;overflow:auto';
      const anchor=document.createElement('button');anchor.textContent='Scroll anchor';
      const space=document.createElement('div');space.style.height='400px';scroll.append(anchor,space);document.body.append(scroll);
      bindContextMenu(anchor,()=>[{label:'Scroll action',run(){}}]);
      scroll.scrollTop=10;
      anchor.dispatchEvent(new KeyboardEvent('keydown',{key:'F10',shiftKey:true,bubbles:true,cancelable:true}));
    });
    await page.evaluate(()=>new Promise(done=>requestAnimationFrame(()=>requestAnimationFrame(done))));
    assert.equal(await page.getByRole('menuitem',{name:'Scroll action',exact:true}).count(),1);
    await page.evaluate(()=>document.querySelector('#scroll-fixture').scrollTop=20);
    await page.getByRole('menu').waitFor({state:'detached'});

    await page.getByRole('button',{name:'Edge',exact:true}).click({button:'right'});
    await page.setViewportSize({width:900,height:700});await page.getByRole('menu').waitFor({state:'detached'});assert.equal(await page.getByRole('menu').count(),0);
    await page.getByRole('button',{name:'Edge',exact:true}).press('Shift+F10');await page.keyboard.press('Tab');assert.equal(await page.getByRole('menu').count(),0);
    await page.getByRole('button',{name:'Edge',exact:true}).click({button:'right'});await page.mouse.click(1,1);assert.equal(await page.getByRole('menu').count(),0);
    await page.evaluate(()=>document.documentElement.dataset.theme='dark');
    if(process.env.ARK_UI_SHOTS){await mkdir(process.env.ARK_UI_SHOTS,{recursive:true});await page.screenshot({path:`${process.env.ARK_UI_SHOTS}/inline-card-dark.png`});}
    await page.setViewportSize({width:390,height:844});
    assert.equal(await page.locator('html').evaluate(el=>el.scrollWidth<=innerWidth),true);
    assert.deepEqual(errors,[]);await page.evaluate(()=>window.fixture.controller.dispose());
  } finally { await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done)); }
});
