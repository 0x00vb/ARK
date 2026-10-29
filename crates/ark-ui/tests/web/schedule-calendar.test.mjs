import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {resolve,sep} from 'node:path';
import {webkit} from 'playwright-core';
const root=resolve(import.meta.dirname,'../../web');
test('Schedules calendar loads bounded real-record projections, follows navigation and hides stale data on errors',async()=>{
 const server=createServer(async(req,res)=>{const path=new URL(req.url,'http://fixture').pathname;if(path==='/'){res.setHeader('content-type','text/html');return res.end('<link rel="stylesheet" href="/app.css"><main id="stage"></main>');}const file=resolve(root,path.slice(1));if(!file.startsWith(root+sep)){res.writeHead(403).end();return;}try{res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await readFile(file));}catch{res.writeHead(404).end();}});
 await new Promise(done=>server.listen(0,'127.0.0.1',done));const browser=await webkit.launch();
 try{
  const page=await browser.newPage({viewport:{width:1180,height:850}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port);
  await page.evaluate(async()=>{
   window.calls=[];window.fail=false;window.callbacks=new Map();window.project='project';
   window.api={summary:()=>({session:{id:'chat'}}),project:()=>({id:window.project}),panel:()=>{},view:v=>window.view=v,on:(name,fn)=>{window.callbacks.set(name,fn);return()=>window.callbacks.delete(name);},call:async(method,input={})=>{
    window.calls.push({method,input});if(!input.calendar)return [];
    if(window.fail)throw Error('Disconnected');
    const start=new Date(input.calendar.from_ms+5*86400000).toISOString();
    return {type:'ark.calendar',version:1,title:'Scheduled work',timezone:input.calendar.timezone,generated_at:new Date().toISOString(),events:[{id:'schedule-1',title:'Review actual draft',start,end:new Date(Date.parse(start)+60000).toISOString(),all_day:false,state:'planned',delivery:'agent_run',source_record:'s-real',source_revision:'revision-1'}]};
   }};
   const {registerSchedules}=await import('/js/schedules.js');registerSchedules(window.api);window.control=window.view.render(document.querySelector('#stage'),{project:{id:'project'}},window.api);
  });
  await page.getByRole('button',{name:'Calendar',exact:true}).click();
  await page.getByRole('button',{name:/Review actual draft/}).first().waitFor();
  await page.getByRole('button',{name:/Review actual draft/}).first().click();assert.match(await page.locator('.cal-facts').innerText(),/s-real/);
  await page.getByRole('button',{name:'Back to calendar'}).click();
  const before=await page.evaluate(()=>window.calls.filter(c=>c.input.calendar).at(-1).input.calendar.from_ms);
  await page.getByRole('button',{name:'Next period'}).click();await page.waitForFunction(before=>window.calls.filter(c=>c.input.calendar).at(-1).input.calendar.from_ms>before,before);
  await page.evaluate(()=>window.fail=true);await page.getByRole('button',{name:'Next period'}).click();await page.getByText('Disconnected',{exact:true}).waitFor();assert.equal(await page.getByRole('button',{name:/Review actual draft/}).count(),0);assert.equal(await page.getByRole('button',{name:'Day',exact:true}).isVisible(),true);
  await page.evaluate(()=>window.fail=false);await page.getByRole('button',{name:'Retry',exact:true}).click();await page.getByRole('button',{name:/Review actual draft/}).first().waitFor();
  const shots=process.env.ARK_FOLLOWUP_SHOTS;if(shots){await mkdir(shots,{recursive:true});await page.screenshot({path:resolve(shots,'schedule-calendar.png')});}
  assert.ok((await page.evaluate(()=>window.calls)).every(c=>c.method==='schedules'));
  assert.ok((await page.evaluate(()=>window.calls.filter(c=>c.input.calendar))).every(c=>c.input.calendar.to_ms-c.input.calendar.from_ms<=62*86400000));
  await page.evaluate(()=>window.control.dispose());assert.deepEqual(errors,[]);
 }finally{await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done));}
});
