import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('webhook input preview is inert and scheduled required inputs disable activation only', async () => {
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
    const page = await browser.newPage({viewport:{width:390,height:844}});
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async()=>{
      const {registerAutomationEditor}=await import('/js/automation-editor.js');const views=new Map(),calls=[];
      const draft=kind=>({name:'Input acceptance',trigger:kind==='schedule'?{kind,when:{at:'09:00',tz:'UTC',days:[]}}:{kind},inputs:[{name:'message',port:'text',required:true}],authority:{actions:[],max_calls:8,max_mutations:0},steps:[{id:'result',name:'Result',action:{kind:'result',inputs:[{kind:'input',name:'message',port:'text'}]}}]});
      const api={location:{runtime:'local',project:'p'},view:view=>views.set(view.id,view),on:()=>()=>{},openView(){},async call(method,input){calls.push({method,input});if(['agents','artifacts','tools','project_models'].includes(method))return [];if(method==='automation_get')return {entry:{definition:{id:input.automation,revision:1,enabled:false},revision:{number:1,draft:draft(input.automation==='schedule'?'schedule':'webhook')}},layout:{version:0,positions:{}}};if(method==='automation_validate'){if(window.fixture?.holdValidation)await new Promise(done=>window.fixture.releaseValidation=done);return {can_save:true,issues:[],activation_issues:input.draft.trigger.kind==='schedule'?[{field:'Execution inputs',message:'Schedules cannot supply required inputs'}]:[]};}if(method==='automation_history')return {runs:[],next:null};if(method==='automation_preview')return {next:[]};if(method==='webhook_get')return {connection:{available:false,bases:[]}};if(method==='automation_preview_inputs')return {revision:1,inputs:input.inputs,bindings:[{step:'result',input:'message',port:'text'}]};throw Error(method);}};
      registerAutomationEditor(api);window.fixture={calls,controller:views.get('automation-editor').render(document.querySelector('#stage'),{automation:'webhook'},api)};
    });
    await page.getByRole('button',{name:'Select Start',exact:true}).click();
    await page.waitForFunction(()=>!!document.querySelector('[data-preview-inputs]') && !document.querySelector('[data-preview-inputs]').disabled);
    await page.getByRole('button',{name:'Test inputs',exact:true}).click();
    await page.getByLabel('message',{exact:true}).fill('Synthetic owned event');
    await page.getByRole('button',{name:'Preview inputs',exact:true}).click();
    await page.getByText('Inputs valid · no execution started',{exact:true}).waitFor();
    const payload=JSON.parse(await page.getByLabel('Delivery payload',{exact:true}).inputValue());
    assert.deepEqual(payload,{inputs:{message:{kind:'text',value:'Synthetic owned event'}}});
    await page.getByText('Result: message (text)',{exact:true}).waitFor();
    assert.equal(await page.locator('html').evaluate(el=>el.scrollWidth<=innerWidth),true);
    assert.equal(await page.evaluate(()=>window.fixture.calls.some(c=>['automation_run','automation_begin_start','automation_activate','work_admit'].includes(c.method))),false);
    await page.getByRole('button',{name:'Close',exact:true}).click();
    await page.evaluate(()=>window.fixture.holdValidation=true);
    await page.getByRole('textbox',{name:'Name',exact:true}).fill('Compact header focus');
    await page.waitForFunction(()=>typeof window.fixture.releaseValidation==='function');
    await page.getByLabel('More automation actions').click();
    await page.getByLabel('More automation actions').focus();
    await page.evaluate(()=>{window.fixture.focusBefore=document.activeElement;window.fixture.holdValidation=false;window.fixture.releaseValidation();});
    await page.waitForFunction(()=>document.activeElement!==window.fixture.focusBefore && document.activeElement?.getAttribute('aria-label')==='More automation actions' && document.querySelector('.auto-head > .auto-more')?.open);
    await page.getByLabel('More automation actions').press('Escape');
    assert.equal(await page.locator('.auto-head > .auto-more').evaluate(element=>element.open),false);

    await page.evaluate(()=>window.fixture.controller.update({automation:'schedule'}));
    await page.getByRole('button',{name:'Select Start',exact:true}).click();
    await page.getByText('Schedules cannot supply required inputs',{exact:true}).waitFor();
    await page.getByLabel('More automation actions').click();
    assert.equal(await page.getByRole('button',{name:'Activate',exact:true}).isDisabled(),true);
    assert.equal(await page.getByRole('button',{name:'Run now',exact:true}).isEnabled(),true);
    assert.deepEqual(errors,[]);await page.evaluate(()=>window.fixture.controller.dispose());
  } finally { await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done)); }
});
