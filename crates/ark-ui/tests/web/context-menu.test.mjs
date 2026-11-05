import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, extname } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('shared context menu stays in the window and closes on scroll, resize, Tab and outside click', async () => {
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
    if(process.env.ARK_UI_SHOTS){await mkdir(process.env.ARK_UI_SHOTS,{recursive:true});await page.screenshot({path:`${process.env.ARK_UI_SHOTS}/context-menu-dark.png`});}
    await page.setViewportSize({width:390,height:844});
    assert.equal(await page.locator('html').evaluate(el=>el.scrollWidth<=innerWidth),true);
    assert.deepEqual(errors,[]);
  } finally { await browser.close();server.closeAllConnections();await new Promise(done=>server.close(done)); }
});
