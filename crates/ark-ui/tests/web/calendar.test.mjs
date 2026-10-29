import test,{before,after} from 'node:test';
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {resolve,sep} from 'node:path';
const {webkit}=createRequire(import.meta.url)('playwright-core');
const root=resolve(import.meta.dirname,'../../web');let browser;
before(async()=>{browser=await webkit.launch();});after(async()=>{await browser?.close();});
async function fixture(run,{coarse=false,artifact=false}={}){
 const server=createServer(async(req,res)=>{const path=new URL(req.url,'http://fixture').pathname;if(path==='/'){res.setHeader('content-type','text/html');res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/app.css"><main id="stage" style="height:100dvh"></main>');return;}const file=resolve(root,path.slice(1));if(!file.startsWith(root+sep)){res.writeHead(403).end();return;}try{res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await readFile(file));}catch{res.writeHead(404).end();}});
 await new Promise(done=>server.listen(0,'127.0.0.1',done));const page=await browser.newPage({viewport:{width:coarse?390:1180,height:800},hasTouch:coarse});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 try{
  await page.addInitScript(()=>{Date.now=()=>Date.parse('2026-10-29T12:00:00Z');});await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.evaluate(async artifact=>{
   window.calls=[];window.version='one';window.callbacks=new Map();
   window.data={type:'ark.calendar',version:1,title:'Project calendar',timezone:'America/New_York',generated_at:'2026-10-29T12:00:00Z',events:[
    {id:'draft',title:'Launch draft',start:'2026-10-29T10:00:00-04:00',end:'2026-10-29T10:30:00-04:00',all_day:false,state:'draft',delivery:'manual_reminder',account:'Example account',next_action:'You publish at10:00 after reviewing the draft',draft:'drafts/launch.md',source_record:'post-1',source_revision:'r1'},
    {id:'task',title:'Inventory review',start:'2026-10-29',end:'2026-10-30',all_day:true,state:'planned',delivery:'agent_run'},
    {id:'deadline',title:'<img src=x onerror=alert(1)>',start:'2026-10-30',end:'2026-10-31',all_day:true,state:'cancelled',delivery:'deadline'}]};
   const file=()=>({path:'work.calendar.json',kind:'text',text:JSON.stringify(window.data,null,2),version:window.version});
   window.api={isLocal:()=>false,resourceRef:()=>({}),resource:async()=>{},reportFocus(){},call:async(method,params)=>{window.calls.push({method,params});if(method==='artifact'){const bytes=new TextEncoder().encode(file().text),summary={id:'a'.repeat(64),name:'work.calendar.json',kind:'data',size:bytes.length};let result={artifact:summary,text:file().text,total:bytes.length,length:bytes.length,offset:0,hex:Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join('')};if(params.hex){if(window.holdArtifact)await new Promise(done=>window.releaseArtifact=done);const bad=window.badArtifact;if(bad==='id')result.artifact.id='b'.repeat(64);if(bad==='kind')result.artifact.kind='image';if(bad==='offset')result.offset=1;if(bad==='length')result.length--;if(bad==='total')result.total--;if(bad==='size')result.artifact.size--;if(bad==='hex')result.hex='gg'+result.hex.slice(2);if(bad==='utf8')result.hex='ff'+result.hex.slice(2);}return result;}if(method==='file_read')return file();if(method==='file_version')return {version:window.version};throw Error('Unexpected mutation '+method);},on:(name,fn)=>{window.callbacks.set(name,fn);return()=>window.callbacks.delete(name);},toast(){}};
   if(artifact){const {registry}=await import('/js/ext.js');window.api.artifactViewer=v=>registry.viewers.push(v);window.api.setFocus=()=>{};const {registerCalendar}=await import('/js/calendar.js');registerCalendar(window.api);const {artifactView}=await import('/js/pane.js');window.reopen=()=>{window.control?.dispose();document.querySelector('#stage').replaceChildren();window.control=artifactView(document.querySelector('#stage'),window.api,'a'.repeat(64));};window.reopen();}else{const {fileView}=await import('/js/file.js');window.control=fileView(document.querySelector('#stage'),'work.calendar.json',window.api,{});}
  },artifact);
  await page.getByRole('heading',{name:'Project calendar',exact:true}).waitFor();await run(page);assert.deepEqual(errors,[]);
  const methods=await page.evaluate(()=>window.calls.map(c=>c.method));assert.ok(methods.every(m=>['file_read','file_version','artifact'].includes(m)),methods.join(','));
 }finally{try{await page.evaluate(()=>window.control?.dispose());}finally{await page.close();server.closeAllConnections();await new Promise(done=>server.close(done));}}
}
test('actual file view supports month/week/day, keyboard details and editable source without creating work',()=>fixture(async page=>{
 assert.equal(await page.locator('.cal-days.month .cal-date-group').count(),42);
 await page.getByRole('button',{name:'Week',exact:true}).click();assert.equal(await page.locator('.cal-days.week .cal-date-group').count(),7);
 await page.getByRole('button',{name:'Day',exact:true}).click();assert.equal(await page.locator('.cal-days.day .cal-date-group').count(),1);
 const draft=page.getByRole('button',{name:/Launch draft/});await draft.focus();await page.keyboard.press('Enter');await page.getByRole('heading',{name:'Launch draft',exact:true}).waitFor();
 assert.match(await page.getByRole('region',{name:'Item details'}).innerText(),/Manual reminder/);assert.match(await page.locator('.cal-facts').innerText(),/You publish at10:00/);assert.equal(await page.locator('.cal-facts').getByText('Draft',{exact:true}).count(),2);
 await page.getByRole('button',{name:'Back to calendar',exact:true}).click();assert.equal(await draft.evaluate(el=>el===document.activeElement),true);
 await page.getByRole('button',{name:'Source',exact:true}).click();await page.locator('textarea').waitFor();await page.getByRole('button',{name:'Calendar',exact:true}).click();await page.getByRole('heading',{name:'Project calendar',exact:true}).waitFor();
 assert.equal(await page.locator('.cal img').count(),0);assert.match(await page.locator('.cal-snapshot').innerText(),/Snapshot.*America\/New_York/);
}));
test('calendar consumes current file revisions, removes vanished selection and recovers invalid snapshots',()=>fixture(async page=>{
 await page.getByRole('button',{name:/Launch draft/}).click();
 await page.evaluate(()=>{window.data.events=window.data.events.filter(e=>e.id!=='draft');window.version='two';window.callbacks.get('ark:files_changed')({paths:['work.calendar.json']});});
 await page.getByRole('button',{name:/Inventory review/}).waitFor();assert.equal(await page.getByRole('heading',{name:'Launch draft',exact:true}).count(),0);
 await page.evaluate(()=>{window.data.version=2;window.version='three';window.callbacks.get('ark:files_changed')({paths:['work.calendar.json']});});await page.getByRole('heading',{name:'Calendar is unavailable'}).waitFor();
 await page.evaluate(()=>{window.data.version=1;window.data.title='Recovered snapshot';window.version='four';window.callbacks.get('ark:files_changed')({paths:['work.calendar.json']});});await page.getByRole('heading',{name:'Recovered snapshot'}).waitFor();
}));
test('narrow coarse-pointer calendar uses an agenda, safe text and usable targets in dark mode',()=>fixture(async page=>{
 await page.evaluate(()=>{document.documentElement.dataset.theme='dark';document.documentElement.style.setProperty('--bg','#1b1b1b');document.documentElement.style.setProperty('--text','#ececeb');});
 const geometry=await page.locator('.cal').evaluate(el=>({width:el.clientWidth,scroll:el.scrollWidth,columns:getComputedStyle(el.querySelector('.cal-days')).gridTemplateColumns}));assert.ok(geometry.scroll<=geometry.width);assert.ok(!geometry.columns.includes(' '));
 for(const button of await page.locator('.cal button:visible').all())assert.ok(await button.evaluate(el=>el.getBoundingClientRect().height>=44));
 await page.getByRole('button',{name:/Launch draft/}).click();assert.equal(await page.locator('.cal-detail h2').evaluate(el=>el===document.activeElement),true);assert.ok(await page.locator('.cal').evaluate(el=>el.scrollWidth<=el.clientWidth));
 const shots=process.env.ARK_CALENDAR_SHOTS;if(shots){await mkdir(shots,{recursive:true});await page.screenshot({path:resolve(shots,'calendar-narrow-details.png')});await page.getByRole('button',{name:'Back to calendar'}).click();await page.screenshot({path:resolve(shots,'calendar-narrow.png')});}
},{coarse:true}));

test('artifact calendar binds complete bytes to the scoped immutable snapshot and rejects changed replies',()=>fixture(async page=>{
 assert.equal(await page.getByRole('button',{name:'Raw',exact:true}).count(),1);
 for(const bad of ['id','kind','offset','length','total','size','hex','utf8']){
  await page.evaluate(bad=>{window.badArtifact=bad;window.reopen();},bad);
  await page.getByRole('status').filter({hasText:bad==='utf8'?/encoded|UTF|decode|valid/i:/incomplete or changed/}).waitFor();assert.equal(await page.locator('.cal').count(),0);
 }
 await page.evaluate(()=>{window.badArtifact=null;window.reopen();});await page.getByRole('heading',{name:'Project calendar',exact:true}).waitFor();
 const requests=await page.evaluate(()=>window.calls.filter(c=>c.params?.hex));assert.ok(requests.every(c=>c.params.length===1048576&&c.params.offset===0));
 await page.evaluate(()=>{window.holdArtifact=true;window.reopen();});await page.getByText('Loading calendar',{exact:true}).waitFor();
 await page.evaluate(()=>{window.control.dispose();window.releaseArtifact();});await page.getByText('Loading calendar',{exact:true}).waitFor({state:'detached'});assert.equal(await page.locator('.cal').count(),0);
},{artifact:true}));
test('dense month stays bounded and more items opens the full day without hiding work',()=>fixture(async page=>{
 await page.evaluate(()=>{const item=window.data.events[0];window.data.events=Array.from({length:1000},(_,i)=>({...item,id:String(i),title:'Draft '+i}));window.version='dense';window.callbacks.get('ark:files_changed')({paths:['work.calendar.json']});});
 await page.getByRole('button',{name:/997 more items/}).waitFor();assert.equal(await page.locator('.cal-event').count(),3);
 await page.getByRole('button',{name:/997 more items/}).click();assert.equal(await page.locator('.cal-event').count(),1000);assert.equal(await page.locator('.cal-days.day').count(),1);
}));
test('snapshot clock uses its labelled zone rather than the computer clock',()=>fixture(async page=>{
 const expected=await page.evaluate(()=>`Snapshot ${new Intl.DateTimeFormat(undefined,{timeZone:window.data.timezone,dateStyle:'medium',timeStyle:'short'}).format(new Date(window.data.generated_at))} · ${window.data.timezone}`);
 assert.equal(await page.locator('.cal-snapshot').innerText(),expected);
}));

test('invalid artifact schema points to its actual Raw recovery control',()=>fixture(async page=>{
 await page.evaluate(()=>{window.data.version=2;window.reopen();});
 await page.getByRole('heading',{name:'Calendar is unavailable'}).waitFor();
 assert.equal(await page.getByText('Open Raw to inspect this file.',{exact:true}).count(),1);
 await page.getByRole('button',{name:'Raw',exact:true}).click();
 assert.match(await page.locator('.artifact-view pre').innerText(),/"version": 2/);
} ,{artifact:true}));
