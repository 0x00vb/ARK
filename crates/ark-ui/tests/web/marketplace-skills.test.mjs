import test, { before, after } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, sep } from "node:path";
const { webkit } = createRequire(import.meta.url)("playwright-core");
const root = resolve(import.meta.dirname, "../../web");
let browser;
before(async () => { browser = await webkit.launch(); });
after(async () => { await browser?.close(); });
async function fixture(run, full = false) {
  const server = createServer(async (req, res) => {
    if (req.url === "/") { res.setHeader("content-type", "text/html"); res.end('<link rel="stylesheet" href="/app.css"><main id="stage" class="marketplace"></main>'); return; }
    const file = resolve(root, req.url.slice(1));
    if (!file.startsWith(root + sep)) { res.writeHead(403).end(); return; }
    try { res.setHeader("content-type", file.endsWith(".css") ? "text/css" : "text/javascript"); res.end(await readFile(file)); }
    catch { res.writeHead(404).end(); }
  });
  await new Promise(done => server.listen(0, "127.0.0.1", done));
  const page = await browser.newPage({ viewport: { width: 1180, height: 800 } });
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(async full => {
      window.runtime = "mac"; window.calls = []; window.failCatalog = false; window.holdInstall = false; window.callbacks = new Map();
      window.packages = Object.fromEntries(["mac", "server"].map(id => [id, [{ id: "example.notes", name: "Release notes", summary: "Write verified release notes", publisher: "Example", version: "1", digest: "pinned", skills: ["release-notes"], installed: false, available: true, error: null }]]));
      const call = async (at, method, params = {}) => {
        window.calls.push({ at, method, params });
        if (method === "capabilities") return [];
        if (method === "skill_packages") {
          if (window.failCatalog) throw Error("Catalog offline");
          return structuredClone(window.packages[at]);
        }
        if (method === "skills") return [{ name: "data-analysis", description: "Analyze editable data", scope: "builtin", chat: true }];
        if (method === "skill_package_install") {
          if (window.holdInstall) await new Promise(done => window.releaseInstall = done);
          window.packages[at][0].installed = true; return structuredClone(window.packages[at][0]);
        }
        if (method === "skill_package_remove") { window.packages[at][0].installed = false; return {}; }
        throw Error(`Unexpected ${method}`);
      };
      window.api = { summary: () => ({ location: { runtime: window.runtime, project: "project", session: "chat" }, place: { name: window.runtime === "mac" ? "Office Mac" : "Project server" } }),
        at: location => ({ call: (method, params) => call(location.runtime, method, params) }),
        call: (method, params) => call(window.runtime, method, params),
        on: (event, fn) => { window.callbacks.set(event, fn); return () => window.callbacks.delete(event); },
        ext: async name => name === "desktop.account" ? { state: "signed_out" } : {},
        view: view => { window.view = view; }, project: () => ({ id: "project" }),
      };
      if (full) {
        const { registerMarketplace } = await import("/js/marketplace.js"); registerMarketplace(window.api); window.control = window.view.render(document.querySelector("#stage"));
      } else {
        const { skillsMarketplace } = await import("/js/marketplace-skills.js"); window.control = skillsMarketplace(document.querySelector("#stage"), window.api);
      }
    }, full);
    await run(page);
    assert.deepEqual(errors, []);
  } finally {
    await page.evaluate(() => window.control?.dispose());
    await page.close(); server.closeAllConnections(); await new Promise(done => server.close(done));
  }
}

test("install and removal keep builtins, pinned identity, search and installed scope truthful", async () => fixture(async page => {
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor();
  assert.match(await page.locator(".market-install-scope").innerText(), /Office Mac/);
  assert.equal(await page.getByText("Included", { exact: true }).count(), 1);
  await page.getByRole("searchbox").fill("release");
  await page.getByRole("button", { name: "Install Release notes", exact: true }).click();
  await page.getByText("Release notes installed.").waitFor();
  assert.equal(await page.getByRole("searchbox").inputValue(), "release");
  await page.getByRole("button", { name: "Installed", exact: true }).click();
  await page.getByRole("button", { name: "View Release notes", exact: true }).click();
  assert.match(await page.locator(".market-includes").innerText(), /release-notes/);
  await page.getByRole("button", { name: "Remove", exact: true }).click();
  await page.getByText("Release notes removed. Local files were kept.").waitFor();
  await page.getByRole("button", { name: "Back", exact: true }).click();
  assert.equal(await page.getByRole("button", { name: "View Release notes", exact: true }).count(), 0);
  const install = await page.evaluate(() => window.calls.find(call => call.method === "skill_package_install"));
  assert.equal(install.at, "mac"); assert.deepEqual(install.params, { id: "example.notes", digest: "pinned" });
  assert.equal(await page.evaluate(() => window.calls.filter(call => call.method === "send").length), 0);
}));

test("catalog failure keeps visible data and prevents stale installs", async () => fixture(async page => {
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor();
  await page.evaluate(async () => { window.failCatalog = true; await window.control.refresh(); });
  assert.equal(await page.getByRole("button", { name: "View Release notes", exact: true }).count(), 1);
  assert.equal(await page.getByRole("button", { name: "Install Release notes", exact: true }).isDisabled(), true);
  assert.match(await page.locator("#stage").innerText(), /Skill packages are unavailable/);
  await page.evaluate(() => window.failCatalog = false);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.getByRole("button", { name: "Install Release notes", exact: true }).isEnabled(), true);
}));

test("an in-flight install stays bound to its original computer", async () => fixture(async page => {
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor();
  await page.evaluate(() => window.holdInstall = true);
  await page.getByRole("button", { name: "Install Release notes", exact: true }).click();
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Install Release notes", exact: true }).isDisabled(), true);
  await page.evaluate(() => { window.runtime = "server"; window.callbacks.get("ark:summary")(); });
  await page.waitForFunction(() => document.querySelector(".market-install-scope")?.textContent.includes("Project server"));
  assert.equal(await page.getByRole("button", { name: "Install Release notes", exact: true }).isEnabled(), true);
  await page.evaluate(() => window.releaseInstall());
  await page.waitForFunction(() => window.packages.mac[0].installed);
  assert.equal(await page.evaluate(() => window.packages.server[0].installed), false);
  assert.equal(await page.evaluate(() => window.calls.filter(call => call.method === "skill_package_install").length), 1);
}));

test("Plugins and Skills selector supports keyboard, dark and narrow layouts", async () => fixture(async page => {
  await page.getByRole("tab", { name: "Plugins", exact: true }).waitFor();
  await page.getByRole("tab", { name: "Plugins", exact: true }).focus(); await page.keyboard.press("ArrowRight");
  await page.getByRole("button", { name: "Install Release notes", exact: true }).waitFor();
  assert.equal(await page.getByRole("tab", { name: "Skills", exact: true }).getAttribute("aria-selected"), "true");
  const shots = process.env.ARK_UI_SHOTS;
  if (shots) { await mkdir(shots, { recursive: true }); await page.screenshot({ path: resolve(shots, "skills-light.png") }); }
  await page.evaluate(() => document.documentElement.dataset.theme = "dark");
  if (shots) await page.screenshot({ path: resolve(shots, "skills-dark.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  if (shots) await page.screenshot({ path: resolve(shots, "skills-narrow.png") });
  await page.getByRole("tab", { name: "Skills", exact: true }).focus(); await page.keyboard.press("ArrowLeft");
  assert.equal(await page.getByRole("tab", { name: "Plugins", exact: true }).getAttribute("aria-selected"), "true");
}, true));

test('private file preview is inert, shows all files as text, cancels and installs only on its captured computer', () => fixture(async page => {
  await page.getByRole('button',{name:'Import skill package'}).waitFor();
  await page.evaluate(()=>{
    const oldAt=window.api.at;
    window.sourcePackage={id:'private.notes',name:'Private notes',summary:'Private workflow',publisher:'Local author',version:'1',skills:[{name:'private-notes',files:{'SKILL.md':'<script>bad()</script>','reference.txt':'Keep evidence'}}]};
    window.api.at=location=>({call:async(method,input={})=>{
      if(method==='skill_packages'&&input.source){window.importPreview={location,input};return {package:window.sourcePackage,digest:'frozen'};}
      if(method==='skill_package_install'&&input.source){window.importInstalls=(window.importInstalls||0)+1;window.installLocation=location;return {};}
      return oldAt(location).call(method,input);
    }});
    window.control.refresh();
  });
  const source=await page.evaluate(()=>JSON.stringify(window.sourcePackage));
  await page.getByLabel('Skill package file').setInputFiles({name:'private.skills.json',mimeType:'application/json',buffer:Buffer.from(source)});
  await page.getByRole('heading',{name:'Review skill package'}).waitFor();
  assert.equal(await page.evaluate(()=>window.importInstalls||0),0);
  await page.getByText('SKILL.md',{exact:true}).click();
  assert.equal(await page.getByText('<script>bad()</script>',{exact:true}).count(),1);
  assert.equal(await page.locator('.market-skills script').count(),0);
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  assert.equal(await page.evaluate(()=>window.importInstalls||0),0);
  await page.getByLabel('Skill package file').setInputFiles({name:'private.skills.json',mimeType:'application/json',buffer:Buffer.from(source)});
  await page.getByRole('heading',{name:'Review skill package'}).waitFor();
  await page.getByRole('button',{name:'Install package',exact:true}).click();
  await page.waitForFunction(()=>window.importInstalls===1);
  assert.equal(await page.evaluate(()=>window.installLocation.runtime),'mac');
  await page.getByRole('button',{name:'Import skill package'}).waitFor();
  await page.getByLabel('Skill package file').setInputFiles({name:'large.json',mimeType:'application/json',buffer:Buffer.alloc(1048577)});
  await page.getByText('Skill package exceeds the 1 MiB import limit',{exact:true}).waitFor();
}));
