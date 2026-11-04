import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import test from 'node:test';
import { webkit } from 'playwright-core';

test('product control tokens give close buttons compact geometry without competing hit areas', async () => {
  const css=await readFile(resolve(import.meta.dirname,'../../web/app.css'),'utf8');
  const browser=await webkit.launch();
  try {
    for(const touch of [false,true]){
      const context=await browser.newContext({viewport:{width:1100,height:760},hasTouch:touch,isMobile:touch});
      const page=await context.newPage();
      await page.setContent(`<meta name="viewport" content="width=device-width,initial-scale=1"><style>${css}</style><div style="padding:24px;display:flex;gap:16px;align-items:center"><div class="chat-tab-item" data-selected="true"><button class="chat-tab">Research notes</button><button class="chat-tab-close" aria-label="Close chat"><span class="ico">×</span></button></div><div class="pane-tab-item" data-selected="true"><button class="pane-tab">Report.md</button><button class="pane-tab-close" aria-label="Close file"><span class="ico">×</span></button></div><button class="icon-btn small" aria-label="Compact icon">×</button><button class="auto-icon" aria-label="Canvas icon">+</button><button class="btn">Save</button></div>`);
      const sizes=await page.locator('.chat-tab-close,.pane-tab-close,.icon-btn.small,.auto-icon,.btn').evaluateAll(nodes=>nodes.map(el=>{const r=el.getBoundingClientRect(),s=getComputedStyle(el);return {width:r.width,height:r.height,padding:s.padding};}));
      for(const size of sizes.slice(0,3)){assert.equal(size.width,touch?44:24);assert.equal(size.height,touch?44:24);assert.equal(size.padding,'0px');}
      assert.equal(sizes[3].width,touch?44:32);assert.equal(sizes[3].height,touch?44:32);
      for(const size of sizes.slice(4))assert.equal(size.height,touch?44:32);
      const before=await page.locator('.icon-btn.small').evaluate(el=>getComputedStyle(el,'::before').content);assert.equal(before,'none');
      await page.evaluate(()=>window.clicked=[]);
      await page.locator('.chat-tab').evaluate(el=>el.onclick=()=>window.clicked.push('select'));
      await page.locator('.chat-tab-close').evaluate(el=>el.onclick=()=>window.clicked.push('close'));
      await page.locator('.chat-tab-item').hover();
      await page.getByRole('button',{name:'Close chat',exact:true}).click();
      assert.deepEqual(await page.evaluate(()=>window.clicked),['close']);
      await page.locator('.chat-tab').click();assert.deepEqual(await page.evaluate(()=>window.clicked),['close','select']);
      if(process.env.ARK_UI_SHOTS){await mkdir(process.env.ARK_UI_SHOTS,{recursive:true});await page.screenshot({path:`${process.env.ARK_UI_SHOTS}/controls-${touch?'touch':'desktop'}.png`});}
      await context.close();
    }
  }finally{await browser.close();}
});
