// Drives the real ark-ui page in WebKit (the engine behind the macOS desktop window) against a
// host that `tests/page.rs` starts with a scripted mock model. `ARK_UI_BROWSER=chromium` uses
// Chromium instead, the engine behind WebView2 on Windows.
//
//   node page.mjs <scenario> <url>
//
// Each scenario guards a rendering bug found while dogfooding. Exit code 0 means it held.

import { chromium, webkit } from "playwright-core";
import assert from "node:assert/strict";

const [scenario, url] = process.argv.slice(2);

// What a turn shows, in order, as plain strings. Replayed and live turns must agree on this.
async function shape(page) {
  return page.evaluate(() => {
    const turn = [...document.querySelectorAll(".turn")].pop();
    const out = [];
    for (const el of turn.querySelector(".agent").children) {
      const cls = el.classList;
      if (cls.contains("working-line") || cls.contains("turn-foot")) continue;
      const text = el.innerText.trim().replace(/\s+/g, " ");
      if (cls.contains("steered")) out.push(`steer: ${text}`);
      else if (cls.contains("answer")) out.push(`answer: ${text}`);
      else if (cls.contains("prose")) out.push(`text: ${text}`);
      else if (cls.contains("work")) out.push(`work: ${el.querySelector(".work-summary").innerText.trim()}`);
      else out.push(`${el.className}: ${text}`);
    }
    return out;
  });
}

// A picture of the page for a person to check, when `ARK_UI_SHOTS` names a folder.
async function shot(page, name) {
  const dir = process.env.ARK_UI_SHOTS;
  if (dir) await page.screenshot({ path: `${dir}/${name}.png` });
}

async function finished(page, send) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await send();
  await done;
  // The page redraws streamed text on the next frame.
  await page.waitForTimeout(150);
}

const scenarios = {
  async "project-sections"(page) {
    await page.evaluate(() => {
      window.__projectSectionEvents = [];
      window.ark.projectSection({ id: "project-notes", label: "Project notes", render(el, { project }) {
        window.__projectSectionEvents.push(["render", project.id, el.isConnected]);
        el.textContent = `Notes for ${project.name}`;
        return { dispose() { window.__projectSectionEvents.push(["dispose", project.id, el.isConnected]); } };
      } });
    });
    const section = page.getByRole("group", { name: "Project notes", exact: true });
    await section.waitFor({ state: "visible" });
    assert.equal(await section.evaluate(el => el.parentElement.querySelector(".project-row").getAttribute("aria-current")), "true");
    await page.locator('.project-row[aria-current="true"] .project-chev').click();
    await section.waitFor({ state: "detached" });
    await page.locator('.project-row[aria-current="true"] .project-chev').click();
    await section.waitFor({ state: "visible" });
    const events = await page.evaluate(() => window.__projectSectionEvents);
    assert.ok(events.every(event => event[2]), "render and disposal happen before detachment");
    assert.ok(events.some(event => event[0] === "dispose"));
  },
  async "devices-startup"(page) {
    const result = await page.evaluate(async () => {
      const { registerDevices } = await import('./js/devices.js');
      const originalFetch = window.fetch, originalTauri = window.__TAURI__;
      const originalSecure = Object.getOwnPropertyDescriptor(window, 'isSecureContext');
      const sections = [], ark = { setting: section => sections.push(section.id) };
      let requests = 0;
      try {
        // An insecure page cannot subscribe, but Devices still lists and signs out devices,
        // so a bad session reply keeps the section without pairing. No native bridge is used.
        Object.defineProperty(window, 'isSecureContext', { configurable: true, value: false });
        window.fetch = async () => { requests++; throw Error('Unexpected native session request'); };
        window.__TAURI__ = { core: {} };
        await registerDevices(ark);
        const native = { requests, sections: sections.splice(0) };
        delete window.__TAURI__;
        window.fetch = async () => new Response('<!doctype html>', { status: 200, headers: { 'content-type': 'text/html' } });
        let malformedError = null;
        try { await registerDevices(ark); } catch (error) { malformedError = error.message; }
        const malformed = { error: malformedError, sections: sections.splice(0) };
        window.fetch = async () => { throw Error('Offline'); };
        let networkError = null;
        try { await registerDevices(ark); } catch (error) { networkError = error.message; }
        const network = { error: networkError, sections: sections.splice(0) };
        window.fetch = async () => new Response(JSON.stringify({ pairing: true }), { status: 200, headers: { 'content-type': 'application/json' } });
        await registerDevices(ark);
        return { native, malformed, network, pairing: sections.splice(0) };
      } finally {
        window.fetch = originalFetch;
        if (originalTauri === undefined) delete window.__TAURI__; else window.__TAURI__ = originalTauri;
        if (originalSecure) Object.defineProperty(window, 'isSecureContext', originalSecure); else delete window.isSecureContext;
      }
    });
    assert.deepEqual(result, { native: { requests: 0, sections: [] }, malformed: { error: null, sections: ['devices'] },
      network: { error: null, sections: ['devices'] }, pairing: ['devices'] });
  },
  async "draft-isolation"(page) {
    const input = page.locator("#input");
    const a = await page.evaluate(() => window.ark.summary().session.id);
    const first = "Earlier draft 👋 reunión.";
    const second = "Separate draft 🐈.";
    const open = async (id) => {
      await page.evaluate(id => window.ark.openSession(id), id);
      await page.evaluate(id => window.ark.shown(id), id);
    };
    await input.fill(first);
    await page.evaluate(() => window.ark.newSession());
    await page.waitForFunction(a => window.ark.summary().session.id !== a, a);
    const b = await page.evaluate(() => window.ark.summary().session.id);
    await page.evaluate(id => window.ark.shown(id), b);
    assert.equal(await input.inputValue(), "", "a new chat starts with its own empty draft");
    await input.fill(second);
    await open(a);
    assert.equal(await input.inputValue(), first);
    await open(b);
    assert.equal(await input.inputValue(), second);
    await input.fill("");
    await open(a);
    await open(b);
    assert.equal(await input.inputValue(), "", "an intentionally cleared draft stays empty");
    await input.fill(second);
    await input.evaluate(el => { el.focus(); el.setSelectionRange(2, 2); });
    await page.evaluate(() => window.ark.call("rename_session", { title: "Draft B" }));
    await page.waitForFunction(() => window.ark.summary().session.title === "Draft B");
    assert.equal(await input.inputValue(), second, "same-chat summaries preserve text");
    assert.equal(await input.evaluate(el => el.selectionStart), 2, "same-chat summaries preserve selection");
    await page.locator("#chat-split").click();
    const secondary = page.locator(".secondary textarea");
    await secondary.waitFor();
    const promoted = "Latest split draft 👋.";
    await secondary.fill(promoted);
    await input.click();
    await page.locator(`.chat-tab-item[data-session="${b}"] .chat-tab-close`).click();
    await page.waitForFunction(a => window.ark.summary().session.id === a, a);
    await page.evaluate(id => window.ark.shown(id), a);
    assert.equal(await input.inputValue(), promoted, "promotion restores the secondary's latest draft");
    assert.equal(await page.locator(".secondary").count(), 0);
    assert.equal((await page.evaluate(() => window.ark.call("transcript"))).length, 0);
    await shot(page, "draft-isolation");
  },
  async "file-viewer-consumer"(page) {
    await page.evaluate(() => window.ark.openFile('sites.geojson'));
    const frame = page.frameLocator('iframe[title="Map"]');
    await frame.getByRole('button', { name: 'Fit', exact: true }).waitFor({ timeout: 5000 });
    await frame.getByText('Features', { exact: true }).click();
    await frame.getByRole('button', { name: 'Site one', exact: true }).click();
    await frame.locator('#selection').filter({ hasText: 'Site one' }).waitFor();
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    await page.locator('textarea.wb-editor').waitFor();
    await page.getByRole('button', { name: 'Map', exact: true }).click();
    await frame.getByRole('button', { name: 'Fit', exact: true }).waitFor();
    await page.evaluate(() => window.ark.openFile('part.stl'));
    const model = page.frameLocator('iframe[title="3D model"]');
    await model.getByRole('button', { name: 'Fit', exact: true }).waitFor();
    await model.getByRole('button', { name: 'Wireframe', exact: true }).click();
    await shot(page, 'consumer-file-viewer');
    await page.reload();
    await page.getByRole('button', { name: 'Retry', exact: true }).waitFor();
    await page.bringToFront();
    await page.waitForTimeout(200);
    assert.equal(await page.locator('iframe[title="3D model"]').count(), 0, 'recovery must wait for Retry');
    const bookmark = await page.evaluate(() => JSON.parse(sessionStorage.getItem('ark-file-recovery')));
    assert.deepEqual(Object.keys(bookmark).sort(), ['location', 'path', 'version']);
    assert.equal(bookmark.path, 'part.stl');
    await page.getByRole('button', { name: 'Retry', exact: true }).click();
    await model.getByRole('button', { name: 'Fit', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Close part.stl', exact: true }).click();
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ark-file-recovery')), null);
    await page.reload();
    await page.locator('#input').waitFor();
    assert.equal(await page.getByRole('button', { name: 'Retry', exact: true }).count(), 0);
    assert.equal(await page.evaluate(async () => {
      const { fileRecovery } = await import('/js/file.js');
      const here = window.ark.summary().location;
      for (const key of ['runtime', 'project', 'session']) {
        fileRecovery(here, 'sites.geojson', 'remember');
        if (fileRecovery({ ...here, [key]: 'another' }) !== null || sessionStorage.getItem('ark-file-recovery') !== null) return false;
        fileRecovery({ ...here, [key]: 'x'.repeat(513) }, 'sites.geojson', 'remember');
        if (sessionStorage.getItem('ark-file-recovery') !== null) return false;
      }
      sessionStorage.setItem('ark-file-recovery', '{broken');
      return fileRecovery(here) === null && sessionStorage.getItem('ark-file-recovery') === null;
    }), true);
  },
  async "advanced-files"(page) {
    const probe = await page.evaluate(async () => {
      const run = path => new Promise(resolve => {
        const worker = new Worker(path, { type: 'module' });
        worker.onmessage = ({ data }) => { worker.terminate(); resolve(data); };
        worker.onerror = () => { worker.terminate(); resolve('error'); };
      });
      return { wasm: await run('/ext/wasm-probe.js'), plain: await run('/ext/plain-probe.js') };
    });
    assert.equal(probe.wasm.compiled, true);
    assert.equal(probe.plain.fetched, true);
    assert.equal(typeof probe.wasm.fetched, 'string', 'the registered worker cannot fetch');
    assert.equal(typeof probe.plain.compiled, 'string', 'an ordinary asset worker cannot compile WASM');
    await page.evaluate(() => {
      window.__viewerCalls = 0;
      window.__viewerReads = 0;
      window.ark.fileViewer({ id: 'proof', kinds: ['map', 'model'], async render(el, file, api) {
        window.__viewerCalls++;
        if (file.format === 'stl') {
          const refused = label => api.confirm(label).then(() => 'opened', error => error.message);
          window.__confirmRefusals = await Promise.all(['', '  ', 'x'.repeat(81), 5].map(refused));
          const opening = api.confirm('Open model');
          window.__confirmRefusals.push(await refused('Again'));
          await opening;
        }
        window.__viewerReads++;
        const bytes = await api.read(0);
        el.append(api.h('button', { type: 'button', onclick: () => api.setFocus({ id: 'site-1', label: 'Site one', properties: { name: 'Site one' } }) }, 'Select site'));
        el.append(api.h('span.proof-model', `${file.kind}:${file.size}:${bytes.length}`));
        return { dispose() { window.__viewerDisposed = (window.__viewerDisposed || 0) + 1; } };
      } });
      window.ark.openFile('sites.geojson');
    });
    await page.getByRole('button', { name: 'Select site', exact: true }).click();
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    await page.locator('textarea.wb-editor').waitFor();
    assert.match(await page.locator('textarea.wb-editor').inputValue(), /FeatureCollection/);
    await page.getByRole('button', { name: 'Map', exact: true }).click();
    await page.getByRole('button', { name: 'Select site', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__viewerDisposed), 1);
    await page.evaluate(() => window.ark.openFile('bad.geojson'));
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    assert.equal(await page.locator('textarea.wb-editor').inputValue(), '{broken');
    await page.evaluate(() => window.ark.openFile('bad.ifc'));
    await page.getByText('This IFC file has an invalid header', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    const source = page.getByRole('textbox', { name: 'bad.ifc', exact: true });
    await source.waitFor();
    assert.equal(await source.inputValue(), 'invalid IFC source');
    assert.equal(await source.evaluate(el => el.readOnly), true);
    assert.equal(await page.getByRole('button', { name: 'Save', exact: true }).count(), 0);
    await page.getByRole('button', { name: 'Model', exact: true }).click();
    await page.getByRole('button', { name: 'Source', exact: true }).waitFor();
    await page.evaluate(() => window.ark.openFile('large.stl'));
    const open = page.getByRole('button', { name: 'Open model', exact: true });
    await open.waitFor();
    const reads = await page.evaluate(() => window.__viewerReads);
    await open.evaluate(el => { window.__oldFileAllow = el; });
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.evaluate(() => window.__oldFileAllow.click());
    assert.equal(await page.evaluate(() => window.__viewerReads), reads);
    await page.getByRole('button', { name: 'Retry', exact: true }).click();
    await open.click();
    assert.deepEqual(await page.evaluate(() => window.__confirmRefusals), Array(5).fill('Invalid file view confirmation'));
    await page.waitForFunction(() => [...document.querySelectorAll('.proof-model')].some(el => el.textContent === 'model:30000084:1048576'));
    await page.setViewportSize({ width: 420, height: 780 });
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await shot(page, 'advanced-files');
  },
  // Closing while the target runtime is still opening leaves no sheet or terminal behind. If
  // the late reply contains a terminal, the UI closes it immediately.
  async "terminal-close-race"(page) {
    await page.evaluate(async () => {
      const { terminalSheet } = await import("/js/places.js");
      window.__lateCloses = [];
      const api = {
        resource: async (ref, kind, input) => window.__lateCloses.push({ ref, kind, input }),
      };
      const opened = new Promise((resolve) => { window.__resolveLateTerminal = resolve; });
      window.__lateSheet = terminalSheet(
        api,
        "Install Grok",
        "on this Mac",
        () => opened,
      );
    });
    const sheet = page.locator(".program-sheet");
    await sheet.waitFor();
    await sheet.getByRole("button", { name: "Close", exact: true }).last().click();
    await sheet.waitFor({ state: "detached" });
    await page.evaluate(() => window.__resolveLateTerminal({
      session: "late-terminal",
      ref: { runtime: "remote", project: "project", session: "chat" },
    }));
    await page.evaluate(() => window.__lateSheet);
    await page.waitForFunction(() => window.__lateCloses.length === 1);
    const closes = await page.evaluate(() => window.__lateCloses);
    assert.deepEqual(closes, [{
      ref: { runtime: "remote", project: "project", session: "chat" },
      kind: "terminal_close",
      input: { session: "late-terminal" },
    }]);
    assert.equal(await page.locator(".program-overlay").count(), 0);
  },

  // The existing Terminal panel opens a project shell. No project action or main-column view is
  // added, and opening it does not create, select or read a chat.
  async "project-terminal"(page) {
    const before = await page.evaluate(() => ({
      session: window.ark.summary().session.id,
      project: window.ark.summary().project,
    }));
    await page.evaluate(() => {
      const original = window.ark.windowCall.bind(window.ark);
      window.__projectTerminalCalls = [];
      window.ark.windowCall = (method, params) => {
        window.__projectTerminalCalls.push({ method, params });
        return original(method, params);
      };
    });
    await page.locator(".project-row[aria-current='true'] .project-more").click();
    assert.equal(await page.getByRole("button", { name: "Open Terminal" }).count(), 0);
    await page.keyboard.press("Escape");
    assert.equal(await page.locator('[data-view="project-terminal"]').count(), 0);
    await page.evaluate(() => window.ark.openPanel("terminal"));
    const view = page.locator(".wb-terminal");
    await view.waitFor();
    await view.locator(".xterm-helper-textarea").waitFor();
    assert.equal(await page.evaluate(() => window.ark.summary().session.id), before.session);
    const calls = await page.evaluate(() => window.__projectTerminalCalls);
    const opened = calls.find((call) => call.method === "project_terminal_open");
    assert.equal(opened.params.project, before.project.id);
    assert.equal(opened.params.place, "");
    for (const forbidden of ["open_project", "open_session", "new_session", "transcript", "send"]) {
      assert.ok(!calls.some((call) => call.method === forbidden), `${forbidden} must not run`);
    }
    assert.match(await page.locator(".pane-tab[aria-selected='true']").getAttribute("title"), new RegExp(`Terminal · ${before.project.name}`));
    await page.evaluate(() => window.ark.openPanel("terminal"));
    assert.equal(await page.locator(".wb-terminal").count(), 1, "one Terminal pane per project");
    // A project panel reports its focus to the chat the window shows.
    const focused = [];
    page.on("request", (r) => {
      const body = r.url().endsWith("/rpc") ? r.postData() || "" : "";
      if (body.includes('"kind":"focus"')) focused.push(JSON.parse(body).params.ref.location.session);
    });
    await page.evaluate(() => window.ark.reportFocus());
    await new Promise((r) => setTimeout(r, 300));
    assert.deepEqual(focused, [before.session]);

    const textarea = view.locator(".xterm-helper-textarea");
    const answer = view.locator(".xterm-rows", { hasText: "CHATLESS_42" });
    for (let i = 0; ; i++) {
      await textarea.type("echo CHATLESS_$((40+2))\n");
      try {
        await answer.waitFor({ timeout: 5000 });
        break;
      } catch (e) {
        if (i === 3) throw e;
      }
    }
    await page.setViewportSize({ width: 390, height: 760 });
    // The layout settles over a few frames after a resize; a lasting overflow still fails here.
    await page.waitForFunction(() => document.documentElement.scrollWidth <= innerWidth, null, { timeout: 2000 });
    await shot(page, "project-terminal-narrow");

    await page.evaluate(() => {
      window.ark.closePanel("terminal");
      window.__projectTerminalCalls = [];
      window.ark.openPanel("terminal", { place: "Missing computer" });
    });
    await page.waitForFunction(() => window.__projectTerminalCalls.some((call) => call.method === "project_terminal_open"));
    const remoteCalls = await page.evaluate(() => window.__projectTerminalCalls.filter((call) => call.method === "project_terminal_open"));
    assert.deepEqual(remoteCalls.map((call) => call.params.place), ["Missing computer"], "a failed remote open must not fall back locally");

    await page.evaluate(() => window.ark.addProject({ name: "Terminal disposal check" }));
    await page.waitForFunction((id) => window.ark.summary().project.id !== id, before.project.id);
    await page.waitForFunction(() => !document.querySelector(".wb-terminal"));
    assert.equal(await page.locator(".wb-terminal").count(), 0, "switching projects disposes the old Terminal pane");
  },

  // Narration written just before a tool call keeps its last chunk, and the answer shows once.
  // An app's conversation drawn with ark.chatView is the chat: the same turn the chat drew
  // comes out with the same elements and the same computed styles, and its composer has none
  // of the chat's own controls (ADR 0076).
  async "chat-view"(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const result = await page.evaluate(async () => {
      const ark = window.ark;
      const read = await ark.call("chat_read", { session: ark.summary().session.id, limit: 5 });
      const turns = read.transcript.map((t) => ({ ...t, key: String(t.index) }));
      const sent = [];
      let view;
      ark.panel({ id: "parity", title: "Parity", fill: true, render(el) { view = ark.chatView(el, { submit: (text) => sent.push(text) }); view.render(turns); } });
      ark.openPanel("parity");
      await new Promise((ok) => setTimeout(ok, 400));
      document.activeElement?.blur();
      await new Promise((ok) => setTimeout(ok, 300));
      const main = document.querySelector("main");
      const pane = document.querySelector(".chat-view");
      const shape = (root) => [...root.querySelector(".turn .agent").children].map((el) => el.className.replace(/\s+/g, " ").trim());
      const props = ["font-family", "font-size", "line-height", "font-weight", "letter-spacing", "color", "background-color", "padding", "border-radius", "max-width", "box-shadow"];
      const style = (el) => el && Object.fromEntries(props.map((p) => [p, getComputedStyle(el).getPropertyValue(p)]));
      const drift = {};
      for (const sel of [".turn .bubble", ".prose.answer", ".prose pre.md-code", ".prose code", ".work-head", ".act", ".turn-foot", ".composer", ".composer textarea", ".composer .send"]) {
        const a = style(main.querySelector(sel)), b = style(pane.querySelector(sel));
        if (!a || !b) { drift[sel] = `missing in ${a ? "view" : "chat"}`; continue; }
        const d = props.filter((p) => a[p] !== b[p]).map((p) => `${p}: ${a[p]} / ${b[p]}`);
        if (d.length) drift[sel] = d;
      }
      const before = pane.querySelector(".turn");
      view.render(turns);
      const kept = pane.querySelector(".turn") === before;
      view.render([...turns, { key: "run", input: "Keep going", note: "From planner", activity: [{ kind: "tool", label: "Reading notes", call: "x", action: "notes.read" }], working: { label: "Working", since: Date.now() - 3000 } }]);
      await new Promise((ok) => setTimeout(ok, 700));
      const run = [...pane.querySelectorAll(".turn")].pop();
      const mode = document.querySelector("#mode-chip").textContent;
      const input = pane.querySelector("textarea");
      input.focus();
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true, cancelable: true }));
      input.value = "Next step";
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      const chatShape = shape(main), viewShape = shape(pane);
      // The states a run can be in, and a composer that cannot take a message now.
      view.reset();
      const many = Array.from({ length: 12 }, (_, i) => ({ key: `t${i}`, input: `Task ${i}`, activity: [], answer: `Answer ${i}\n\n${"Line. ".repeat(60)}` }));
      view.render([...many,
        { key: "failed", input: "Fail", activity: [], unfinished: "Provider unavailable" },
        { key: "cancelled", input: "Stop", activity: [], cancelled: true },
        { key: "waiting", input: "Wait", activity: [], status: "Paused" }]);
      const stored = pane.querySelector(".turn").classList.contains("stored");
      const notices = [...pane.querySelectorAll(".notice")].map((n) => `${n.className}: ${n.textContent}`);
      view.setComposer({ placeholder: "Writer is paused", disabled: true });
      view.actions.append(Object.assign(document.createElement("button"), { className: "btn primary", textContent: "Resume" }));
      const refused = { placeholder: input.placeholder, disabled: input.disabled, send: getComputedStyle(pane.querySelector(".composer .send")).display };
      // Scrolled up, a turn drawn again keeps its place.
      const scroller = pane.querySelector(".scroller");
      const target = pane.querySelectorAll(".turn")[3];
      scroller.scrollTop = target.offsetTop + 20;
      scroller.dispatchEvent(new Event("scroll"));
      const offset = target.offsetTop - scroller.scrollTop;
      many[2] = { ...many[2], answer: `${many[2].answer}\n\n${"Grown above. ".repeat(80)}` };
      many[3] = { ...many[3], answer: `${many[3].answer}\n\nMore.` };
      view.render([...many]);
      const again = pane.querySelectorAll(".turn")[3];
      const anchored = again !== target && Math.abs(again.offsetTop - scroller.scrollTop - offset) <= 1;
      return {
        stored, notices, refused, anchored,
        same: JSON.stringify(chatShape) === JSON.stringify(viewShape), chat: chatShape, view: viewShape, drift, kept,
        note: run.querySelector(".user-note")?.textContent, open: run.querySelector(".work")?.dataset.open,
        elapsed: run.querySelector(".working-line .elapsed")?.textContent, inspectable: !!run.querySelector(".act.inspectable"),
        controls: [...pane.querySelectorAll(".composer .model-chip, .composer .mode-chip, .composer .attach")].length,
        mode: document.querySelector("#mode-chip").textContent === mode, sent,
      };
    });
    assert.ok(result.same, `chat ${JSON.stringify(result.chat)} view ${JSON.stringify(result.view)}`);
    assert.deepEqual(result.drift, {});
    assert.equal(result.kept, true, "an unchanged turn is not drawn again");
    assert.equal(result.note, "From planner");
    assert.equal(result.open, "true", "work still going keeps its block open");
    assert.match(result.elapsed, /^\d+s$/, "elapsed time without an Esc hint");
    assert.equal(result.inspectable, false);
    assert.equal(result.controls, 0);
    assert.equal(result.mode, true, "Shift+Tab in the view leaves the chat's mode alone");
    assert.deepEqual(result.sent, ["Next step"]);
    assert.equal(result.stored, true, "after reset, turns do not rise in");
    assert.deepEqual(result.notices, ["notice tone-error: Provider unavailable", "notice tone-muted: Stopped.", "notice tone-muted: Paused"]);
    assert.deepEqual(result.refused, { placeholder: "Writer is paused", disabled: true, send: "none" });
    assert.equal(result.anchored, true, "a redrawn turn in view keeps its place");
    await shot(page, "chat-view");
  },

  async stream(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    assert.deepEqual(live, [
      "text: Reading the notes now.",
      "work: Reading notes",
      "answer: The notes mention a locket.",
    ]);
    // The head shows through an attribute, not :has(:empty), which the macOS web view does
    // not re-evaluate when the summary text arrives.
    assert.equal(await page.locator(".work[data-summary] > .work-head").isVisible(), true);
  },

  // A model call that breaks off after some text is tried again: its text leaves the page, the
  // retry's shows once, and the turn replays as it was shown (ADR 0049).
  async withdrawn(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    const all = live.join("\n");
    assert.ok(!all.includes("then I wi"), all);
    assert.equal(live.filter((s) => s === "text: Reading the notes now.").length, 1, all);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, all);
    assert.ok(all.includes("retrying"), all);
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    assert.deepEqual(await shape(page), live);
  },

  // A provider can stream whitespace after a false success claim. The trusted external-effect
  // guard withdraws the whole streamed claim before showing the unconfirmed result.
  async "guarded-effect"(page) {
    await finished(page, () => page.locator("#input").fill("Send the test message.").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    const all = live.join("\n");
    assert.ok(!all.includes("The message was sent."), all);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, all);
    assert.ok(all.includes("could not confirm"), all);
  },

  // A reply the model could not use is taken back and redone: one quiet row, no failure, the
  // answer once (ADR 0050).
  async repair(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    const all = live.join("\n");
    assert.ok(!all.includes("Writing the edit now."), all);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, all);
    const text = await page.locator(".turn").last().innerText();
    assert.ok(text.includes("Redoing its last step"), text);
    assert.ok(!/not JSON|malformed|retrying/i.test(text), text);
    await shot(page, "repair");
  },

  // One it never gets right ends the turn in plain words, with no runtime terms, and Continue.
  async unusable(page) {
    await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
    const text = await page.locator(".turn").last().innerText();
    assert.ok(text.includes("The model's reply could not be used."), text);
    assert.ok(!/not JSON|malformed|provider|column 1573/i.test(text), text);
    assert.equal((await page.locator(".turn .resume").innerText()).trim(), "Continue");
    await shot(page, "unusable");
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    const stored = await page.locator(".turn").last().innerText();
    assert.ok(stored.includes("The model's reply could not be used."), stored);
    assert.ok(!/not JSON|malformed|column 1573/i.test(stored), stored);
  },

  // A row that arrives after the streamed answer (a failed self-check) does not make the page
  // draw the answer a second time.
  async selfcheck(page) {
    // Long enough (over 14 words) that the runtime does not treat it as a trivial question.
    const ask = "Go through all of the notes carefully and tell me every object that they mention, in the order it appears.";
    await finished(page, () => page.locator("#input").fill(ask).then(() => page.keyboard.press("Enter")));
    const live = await shape(page);
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, live.join("\n"));
    assert.ok(live.some((s) => s.startsWith("work: ") && s.includes("ungrounded")), live.join("\n"));
    const agent = await page.locator(".turn .agent").last().innerText();
    assert.equal(agent.split("a locket").length - 1, 1, agent);
  },

  // A stopped turn says so once, and replays as it was shown live.
  async cancel(page) {
    await finished(page, async () => {
      await page.locator("#input").fill("What do the notes say?");
      await page.keyboard.press("Enter");
      await page.locator(".prose.streaming").waitFor();
      await page.keyboard.press("Escape");
    });
    const live = await shape(page);
    assert.equal(live.filter((s) => s.includes("Stopped")).length, 1, live.join("\n"));
    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    assert.deepEqual(await shape(page), live);
  },

  // A turn a crash cut short (ADR 0048): reopened, it shows what it did, says it was
  // interrupted, and offers one Continue on the chat's last turn. Continue is the person's next
  // message; once it is sent the offer is gone.
  async interrupted(page) {
    await page.locator(".session", { hasText: "Read the notes, then summarize them" }).first().click();
    const resume = page.locator(".turn .resume");
    await resume.waitFor();
    assert.equal(await resume.count(), 1);
    assert.equal((await resume.innerText()).trim(), "Continue");
    const notice = await page.locator(".turn .notice").last().innerText();
    assert.equal(notice.trim(), "Interrupted before it finished.");
    assert.equal(await page.locator(".turn .tone-error").count(), 0, "not shown as a failure");
    assert.equal(await page.locator('.turn .act[data-status="ok"]').count(), 1, "the read is done");
    assert.equal(await page.locator(".turn .turn-foot").count(), 0, "no counts for a turn that kept none");
    await finished(page, () => resume.click());
    assert.equal(await page.locator(".turn .resume").count(), 0);
    const turns = page.locator(".turn");
    assert.equal(await turns.count(), 2);
    assert.match(await turns.last().innerText(), /Continue[\s\S]*Picked up where it stopped/);
  },

  // A narrow window folds the sidebar into a drawer the reveal button opens; it closes on a
  // pick, and the width never scrolls sideways.
  async narrow(page) {
    const wide = () => page.evaluate(() => {
      const rail = document.querySelector(".destinations");
      const sidebar = document.querySelector(".sidebar");
      const main = document.querySelector(".main");
      const style = getComputedStyle(sidebar);
      return {
        railWidth: Math.round(rail.getBoundingClientRect().width),
        sidebarWidth: Math.round(sidebar.getBoundingClientRect().width),
        sidebarPadding: Math.round(parseFloat(style.paddingLeft)),
        mainLeft: Math.round(main.getBoundingClientRect().left),
        hscroll: document.documentElement.scrollWidth > innerWidth,
      };
    });
    assert.deepEqual(await wide(), { railWidth: 44, sidebarWidth: 236, sidebarPadding: 10, mainLeft: 280, hscroll: false });
    const rail = await page.locator(".destinations").evaluate((el) => ({
      divider: getComputedStyle(el, "::after").content,
      height: Math.round(el.getBoundingClientRect().height),
      sameSurface: getComputedStyle(el).backgroundColor === getComputedStyle(document.querySelector(".sidebar")).backgroundColor,
      labels: [...el.querySelectorAll(".destination-label")].map((label) => Math.round(label.getBoundingClientRect().width)),
    }));
    assert.equal(rail.divider, "none", "the fixed rail has no detached partial divider");
    assert.equal(rail.height, 800, "the fixed rail reaches the full window height");
    assert.equal(rail.sameSurface, false, "the fixed rail stays distinct from the project sidebar without a divider");
    assert.deepEqual(rail.labels, [1, 1, 1, 1], "wide rail labels stay available to assistive technology only");
    await page.evaluate(() => { document.documentElement.dataset.theme = "dark"; });
    assert.equal(await page.locator(".destinations").evaluate((el) =>
      getComputedStyle(el).backgroundColor === getComputedStyle(document.querySelector(".sidebar")).backgroundColor), false,
    "the fixed rail stays distinct in dark mode");
    await page.evaluate(() => { document.documentElement.dataset.theme = "light"; });
    await page.click("#toggle-sidebar");
    await page.waitForTimeout(350);
    assert.deepEqual(await wide(), { railWidth: 44, sidebarWidth: 0, sidebarPadding: 0, mainLeft: 44, hscroll: false });

    await page.setViewportSize({ width: 720, height: 800 });
    // Wait for the layout to switch rather than a fixed time (flaky on a busy machine).
    await page.waitForFunction(() => document.getElementById("app").dataset.compact === "true");
    const state = () => page.evaluate(() => ({
      compact: document.getElementById("app").dataset.compact,
      drawer: document.getElementById("app").dataset.drawer || "",
      sidebarLeft: Math.round(document.querySelector(".sidebar").getBoundingClientRect().left),
      sidebarWidth: Math.round(document.querySelector(".sidebar").getBoundingClientRect().width),
      sidebarPadding: Math.round(parseFloat(getComputedStyle(document.querySelector(".sidebar")).paddingLeft)),
      destinations: (() => { const r = document.querySelector(".destinations").getBoundingClientRect(); return { top: Math.round(r.top), bottom: Math.round(r.bottom), height: Math.round(r.height) }; })(),
      mainBottom: Math.round(document.querySelector(".main").getBoundingClientRect().bottom),
      hscroll: document.documentElement.scrollWidth > innerWidth,
    }));
    assert.equal((await state()).compact, "true");
    assert.ok(await page.locator("#reveal-sidebar").isVisible(), "the sidebar can be reached");
    await page.click("#reveal-sidebar");
    await page.waitForTimeout(350);
    const open = await state();
    assert.deepEqual({ ...open, destinations: undefined, mainBottom: undefined }, {
      compact: "true", drawer: "open", sidebarLeft: 0, sidebarWidth: 236, sidebarPadding: 10, destinations: undefined, mainBottom: undefined, hscroll: false,
    });
    assert.equal(open.destinations.height, 56, "compact destinations keep their own layout row");
    assert.equal(open.mainBottom, open.destinations.top, "the bar does not cover the workspace");
    assert.equal(await page.locator(".destination").count(), 4);
    for (const button of await page.locator(".destination").all()) {
      const box = await button.boundingBox();
      assert.ok(box.width >= 44 && box.height >= 44, JSON.stringify(box));
    }
    await page.click("#new-session");
    await page.waitForTimeout(350);
    const closed = await state();
    assert.equal(closed.drawer, "closed");
    assert.ok(closed.sidebarLeft < 0, JSON.stringify(closed));
    await page.setViewportSize({ width: 1200, height: 800 });
    await page.waitForTimeout(100);
    const restored = await state();
    assert.equal(restored.compact, "false");
    assert.equal(restored.drawer, "");
    assert.equal(restored.sidebarLeft, 44);
    assert.equal(restored.sidebarWidth, 0);
    assert.equal(restored.sidebarPadding, 0);
    assert.equal(restored.hscroll, false);
  },

  // The model chip names the model only. Its menu groups models by backend: a signed-in
  // subscription offers its models, a missing one shows the command that fixes it.
  async models(page) {
    const chip = page.locator("#model-chip");
    assert.ok(!(await chip.textContent()).includes("single tier"), await chip.textContent());
    await chip.click();
    await page.waitForSelector(".model-group .model-name");
    const menu = await page.locator(".popover").textContent();
    assert.ok(!menu.includes("Model for the next turn"), menu);
    const claude = page.locator(".model-group", { hasText: "Claude subscription" });
    assert.equal(await claude.locator(".model-dot").getAttribute("data-ok"), "true");
    const codex = page.locator(".model-group", { hasText: "ChatGPT subscription" });
    assert.equal(await codex.locator(".model-dot").getAttribute("data-ok"), "false");
    assert.ok((await codex.locator(".model-fix code").textContent()).includes("npm install"));
    await claude.locator(".model-item", { hasText: "Opus" }).click();
    await page.locator(".models-pop .model-item", { hasText: "Medium" }).click();
    await page.waitForFunction(() => document.getElementById("model-chip").textContent === "claude opus@medium");
  },

  // A message sent mid-turn waits above the composer, joins the thread where the agent read
  // it, and a reload replays the turn exactly as it was shown live.
  async steer(page) {
    await finished(page, async () => {
      await page.locator("#input").fill("What do the notes say?");
      await page.keyboard.press("Enter");
      await page.locator(".prose.streaming").waitFor();
      await page.locator("#input").fill("Also check for a key.");
      await page.keyboard.press("Meta+Enter");
      await page.locator(".queued.steer").waitFor();
      // Streaming continues in one piece while the message waits.
      assert.equal(await page.locator(".turn .agent .steered").count(), 0);
    });
    const live = await shape(page);
    assert.equal(await page.locator(".queued.steer").count(), 0, "the pending steer was cleared");
    const at = (prefix) => live.findIndex((s) => s.startsWith(prefix));
    assert.ok(at("text: Reading the notes first") === 0, live.join("\n"));
    assert.ok(at("steer: Also check for a key.") > 0, live.join("\n"));
    assert.ok(at("answer: ") > at("steer: "), live.join("\n"));
    assert.equal(live.filter((s) => s.startsWith("answer: ")).length, 1, live.join("\n"));

    await page.reload();
    await page.locator(".turn.stored").first().waitFor();
    const stored = await shape(page);
    assert.deepEqual(stored, live);
  },

  // A window that reloads in the middle of a turn rebuilds the turn from its events so far and
  // then follows it: the narration shows once and whole, the answer once.
  async rejoin(page) {
    await page.locator("#input").fill("What do the notes say?");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(900);
    await page.reload();
    await page.locator("#input").waitFor();
    await page.locator(".answer", { hasText: "The notes mention a locket." }).waitFor({ timeout: 20000 });
    await page.waitForTimeout(250);
    const after = await shape(page);
    assert.deepEqual(after, [
      `text: ${NARRATION}`,
      "work: Reading notes",
      "answer: The notes mention a locket.",
    ]);
    assert.equal(await page.locator(".turn").count(), 1, "one turn, not a copy");
  },

  // A stream that keeps failing: reconnects back off (1 s doubling to 30 s), stop after ten,
  // and say so with a way to try again.
  async backoff(page) {
    await page.waitForFunction(() => window.__es.opened >= 11, null, { timeout: 10000 });
    await page.waitForTimeout(200);
    const { opened, delays } = await page.evaluate(() => window.__es);
    assert.equal(opened, 11, "the first try and ten reconnects");
    assert.equal(delays.length, 10, delays.join(","));
    delays.forEach((d, i) => {
      const cap = Math.min(30000, 1000 * 2 ** i);
      assert.ok(d >= cap / 2 && d <= cap, `delay ${i} = ${d}, cap ${cap}`);
    });
    const toast = page.locator("#toast");
    assert.match(await toast.innerText(), /Disconnected/);
    await toast.locator("button", { hasText: "Retry" }).click();
    await page.waitForFunction(() => window.__es.opened >= 12, null, { timeout: 5000 });
  },
};

// An app view marked `nav` is a tab in the top bar: it replaces the conversation, and the "Chat"
// tab gives it back. An orchestrated agent's question waits in the tray outside any turn, and
// esc does not deny it.
async function agents(page) {
  await page.evaluate(() => window.ark.view({ id: "board", title: "Board", nav: true, render: (el) => el.append("the board") }));
  const tab = page.locator('.workspace-destination[data-view="board"]');
  await tab.click();
  assert.equal(await page.locator("#stage").isVisible(), true);
  assert.equal(await page.locator("#scroller").isVisible(), false);
  // The tab names the view, so the title names the project.
  const project = await page.evaluate(async () => (await window.ark.call("projects")).find((p) => p.current).name);
  assert.equal(await page.locator("#session-title").innerText(), project);
  assert.equal(await tab.getAttribute("aria-current"), "page");

  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:orch", (m) => m.event.type === "task_finished" && ok(m.event.state))));
  await page.evaluate(async () => {
    await window.ark.call("spawn_agent", { spec: { name: "scribe", tools: ["notes.*"] } });
    await window.ark.call("submit_task", { agent: "scribe", task: { objective: "save the notes" } });
  });
  const card = page.locator(".asks .approval");
  await card.waitFor();
  assert.match(await card.innerText(), /scribe wants to:/);
  await page.keyboard.press("Escape");
  await page.waitForTimeout(200);
  assert.equal(await card.isVisible(), true, "esc leaves an agent's question alone");
  await card.locator("button", { hasText: "Allow once" }).click();
  assert.equal(await done, "done");
  await card.waitFor({ state: "detached" });

  await page.locator('.destination[data-destination="home"]').click();
  assert.equal(await page.locator("#stage").isVisible(), false);
  assert.equal(await page.locator("#scroller").isVisible(), true);
  assert.equal(await tab.getAttribute("aria-current"), null);
}
scenarios.agents = agents;

// An app draws an agent's tasks with the chat's view, the way an app projects its runs (ADR
// 0076), and the view behaves as the chat: streamed text grows in place, the agent's question
// shows in its turn and in the tray, a tool row opens on `task_call`, a turn drawn from
// `task`'s feed keeps its work block, and Stop cancels the running task.
async function agentChat(page) {
  await page.evaluate(async () => {
    const ark = window.ark;
    const probe = (window.probe = { stops: 0 });
    const streams = new Map();
    const approvals = new Map(ark.approvals().map((m) => [m.id, m]));
    let turns = [];
    let view;
    const paint = () => {
      view.render(turns.map((t) => ({ ...t, streaming: t.live ? streams.get(t.key) || "" : "", approvals: [...approvals.values()].filter((m) => m.request?.task === t.key) })));
      view.setComposer({ running: turns.some((t) => t.live) });
    };
    let busy = null;
    const draw = async () => {
      if (busy) return busy.then(draw);
      busy = (async () => {
        const graph = await ark.call("graph");
        const mine = graph.tasks.filter((t) => t.owner === "scribe").sort((a, b) => a.created_ms - b.created_ms);
        const next = [];
        for (const t of mine) {
          const { feed } = await ark.call("task", { task: t.task });
          const turn = { key: t.task, input: t.objective, activity: feed.filter((m) => m.activity).map((m) => m.activity) };
          turn.live = t.state === "running" || t.state === "waiting";
          if (turn.live) turn.working = { label: "Working", since: t.started_ms || t.created_ms };
          else if (t.state === "done") turn.answer = t.result;
          else if (t.state === "cancelled") turn.cancelled = true;
          next.push(turn);
        }
        turns = next;
        paint();
      })();
      await busy;
      busy = null;
    };
    probe.draw = draw;
    ark.on("ark:orch_text", (m) => {
      streams.set(m.task, (streams.get(m.task) || "") + m.text);
      paint();
    });
    ark.on("ark:orch", (m) => {
      if (m.event?.type === "agent" && m.event.event?.type === "tool_call") streams.delete(m.event.task);
      draw();
    });
    ark.on("ark:approval", (m) => { approvals.set(m.id, m); paint(); });
    ark.on("ark:approval_closed", (m) => { approvals.delete(m.id); paint(); });
    ark.panel({ id: "agent-chat", title: "Scribe", fill: true, render(el) {
      view = ark.chatView(el, {
        submit: (text) => ark.call("submit_task", { agent: "scribe", task: { objective: text } }),
        stop: () => {
          probe.stops++;
          const t = turns.find((x) => x.live);
          if (t) ark.call("cancel_task", { task: t.key });
        },
        call: (key, id) => ark.call("task_call", { task: key, call: id }),
      });
      probe.view = view;
    } });
    ark.openPanel("agent-chat");
    await ark.call("spawn_agent", { spec: { name: "scribe", tools: ["notes.*"] } });
  });
  const pane = page.locator(".chat-view");
  await pane.locator("textarea").fill("Save the notes");
  await pane.locator("textarea").press("Enter");

  // The agent's question shows in its turn, and in the tray.
  const card = pane.locator(".turn .agent > .approval");
  await card.waitFor();
  const id = await card.getAttribute("data-id");
  assert.match(await card.innerText(), /scribe wants to:/);
  assert.equal(await page.locator(`#asks .approval[data-id="${id}"]`).count(), 1, "the tray keeps the question");
  await shot(page, "agent-chat-approval");
  await card.locator("button", { hasText: "Allow once" }).click();
  await page.locator(`.approval[data-id="${id}"]`).first().waitFor({ state: "detached" });
  assert.equal(await page.locator(`.approval[data-id="${id}"]`).count(), 0, "answered in the view, both cards go");

  // The answer streams, and the work block above it is not drawn again per chunk.
  await page.waitForFunction(() => document.querySelector(".chat-view .turn .prose.streaming")?.textContent.includes("Saved."));
  const kept = await page.evaluate(async () => {
    const work = document.querySelector(".chat-view .turn .work");
    const text = document.querySelector(".chat-view .turn .prose.streaming");
    const before = text.textContent.length;
    await new Promise((ok) => setTimeout(ok, 400));
    const now = document.querySelector(".chat-view .turn .prose.streaming");
    return { work: work === document.querySelector(".chat-view .turn .work"), text: text === now, grew: (now?.textContent.length || 0) > before };
  });
  assert.deepEqual(kept, { work: true, text: true, grew: true });
  await pane.locator(".turn .prose.answer").waitFor();

  // Drawn afresh from `task`, the finished task keeps its work block; a tool row opens on its call.
  await page.evaluate(async () => { window.probe.view.reset(); await window.probe.draw(); });
  const work = pane.locator(".turn .work").first();
  await work.locator(".work-head").click();
  const row = work.locator(".act.inspectable").first();
  await row.locator(".act-line").click();
  const detail = row.locator(".act-more .call-body");
  await detail.waitFor();
  await page.waitForFunction(() => {
    const row = document.querySelector(".chat-view .turn .work .act.inspectable");
    const text = row?.querySelector(".act-more .call-body")?.innerText || "";
    return row?.dataset.expanded === "true" && text.includes("notes.save")
      && text.includes("Arguments") && text.includes('"title": "Notes"');
  });
  const shown = await detail.innerText();
  assert.match(shown, /notes\.save/);
  assert.match(shown, /Arguments/);
  assert.match(shown, /"title": "Notes"/);
  await shot(page, "agent-chat-call");

  // Answered in the tray, the question leaves the view too.
  await pane.locator("textarea").fill("Write a long answer");
  await pane.locator("textarea").press("Enter");
  const again = pane.locator(".turn:last-child .agent > .approval");
  await again.waitFor();
  const second = await again.getAttribute("data-id");
  await page.locator(`#asks .approval[data-id="${second}"] button`, { hasText: "Allow once" }).click();
  await page.locator(`.approval[data-id="${second}"]`).first().waitFor({ state: "detached" });
  assert.equal(await page.locator(`.approval[data-id="${second}"]`).count(), 0, "answered in the tray, both cards go");

  // While a task runs and nothing is typed, the send button is Stop; typed text is a message.
  await page.waitForFunction(() => document.querySelectorAll(".chat-view .turn").length === 2 && document.querySelector(".chat-view .turn:last-child .prose.streaming"));
  const send = pane.locator(".composer .send");
  assert.equal(await send.getAttribute("aria-label"), "Stop");
  assert.equal(await pane.locator(".composer").getAttribute("data-running"), "true");
  await pane.locator("textarea").fill("one more thing");
  assert.equal(await send.getAttribute("aria-label"), "Send");
  await pane.locator("textarea").fill("");
  await shot(page, "agent-chat-stop");
  await send.click();
  await pane.locator(".turn:last-child .notice", { hasText: /Stopped/ }).waitFor();
  assert.equal(await page.evaluate(() => window.probe.stops), 1);
  assert.equal(await pane.locator(".composer").getAttribute("data-running"), "false");
}
scenarios["agent-chat"] = agentChat;

// A board per goal, beside the chat: the chat plans one and it opens by itself; the person adds,
// drags and completes cards and opens one in its place; an agent given a card moves it by itself;
// what waits for the person is marked; delete has an undo; the project lists its boards.
async function board(page) {
  // A window wide enough for the pane to show the three columns side by side.
  await page.setViewportSize({ width: 1440, height: 900 });
  // No fixed place for it: the rail has no Board and the project lists none yet.
  assert.equal(await page.locator('.workspace-destination[data-view="board"]').count(), 0);
  assert.equal(await page.locator('.project-section[data-id="boards"] .bd-row').count(), 0);
  await page.locator("#input").fill("plan chapter 3");
  await page.keyboard.press("Enter");
  const bd = page.locator(".pane-view.bd");
  await bd.locator(".bd-card", { hasText: "Edit chapter 2" }).waitFor();
  assert.equal(await bd.locator(".bd-goal").inputValue(), "Chapter 3");
  const row = page.locator('.project-section[data-id="boards"] .bd-row');
  await row.filter({ hasText: "0/2" }).waitFor();
  assert.match(await row.innerText(), /Chapter 3/);
  assert.equal(await row.getAttribute("aria-current"), "true");
  // The pane has finished opening before anything is dragged in it.
  await page.waitForFunction(() => !document.getAnimations().some((a) => a.playState === "running"));
  await shot(page, "board-beside-chat");

  const server = () => page.evaluate(async () => (await window.ark.call("board")).items.map((w) => `${w.state}:${w.title}`));
  const column = (state) => page.locator(`.bd-list[data-state="${state}"] .bd-card .bd-title`).allInnerTexts();
  await bd.locator(".bd-add").click();
  for (const t of ["Pick a title"]) {
    // Typed, not filled: the box keeps the focus from one item to the next.
    await page.keyboard.type(t);
    await page.keyboard.press("Enter");
    await page.locator(".bd-card", { hasText: t }).waitFor();
    assert.ok(await page.evaluate(() => document.activeElement?.classList.contains("bd-input")), `the box kept the focus after "${t}"`);
  }
  await page.keyboard.press("Escape");
  assert.deepEqual(await column("todo"), ["Draft chapter 3", "Edit chapter 2", "Pick a title"]);

  // Drag "Pick a title" above "Draft chapter 3", then "Draft chapter 3" into In progress.
  const drag = async (title, to) => {
    const from = await page.locator(".bd-card", { hasText: title }).boundingBox();
    await page.mouse.move(from.x + 40, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(from.x + 50, from.y + from.height / 2 + 8, { steps: 3 });
    await page.mouse.move(to.x, to.y, { steps: 8 });
    assert.equal(await page.locator(".bd-line").count(), 1, "a line shows where it lands");
    await page.mouse.up();
  };
  const first = await page.locator(".bd-card", { hasText: "Draft chapter 3" }).boundingBox();
  await drag("Pick a title", { x: first.x + 60, y: first.y + 4 });
  assert.deepEqual(await column("todo"), ["Pick a title", "Draft chapter 3", "Edit chapter 2"]);
  const doing = await page.locator('.bd-list[data-state="doing"]').boundingBox();
  await drag("Draft chapter 3", { x: doing.x + 60, y: doing.y + 30 });
  assert.deepEqual(await column("doing"), ["Draft chapter 3"]);
  await page.waitForFunction(async () => (await window.ark.call("board")).items.some((w) => w.title === "Draft chapter 3" && w.state === "doing"));
  assert.deepEqual(await server(), ["todo:Pick a title", "todo:Edit chapter 2", "doing:Draft chapter 3"]);

  // The circle completes a card.
  await page.locator(".bd-card", { hasText: "Edit chapter 2" }).locator(".bd-check").click();
  assert.deepEqual(await column("done"), ["Edit chapter 2"]);

  // A click opens the card in the board's place; a note typed there is kept.
  await page.locator(".bd-card", { hasText: "Draft chapter 3" }).click();
  const panel = page.locator(".bd-detail .bd-panel");
  await panel.waitFor();
  assert.equal(await panel.locator(".bd-p-title").inputValue(), "Draft chapter 3");
  await panel.locator(".bd-p-note").fill("Under 2,000 words.");
  await panel.locator(".bd-p-title").click();
  await page.waitForFunction(async () => (await window.ark.call("board")).items.some((w) => w.note === "Under 2,000 words."));

  // Given to an agent, the card moves to Done on its own, and the panel shows the answer.
  await page.evaluate(() => window.ark.call("spawn_agent", { spec: { name: "writer", role: "drafts chapters" } }));
  await page.locator(".bd-p-give").waitFor();
  await page.locator(".bd-p-give").click();
  await page.locator(".popover .menu-item", { hasText: "writer" }).click();
  await panel.locator(".bd-p-said", { hasText: "Chapter 3 is drafted." }).waitFor();
  assert.match(await panel.locator(".bd-p-history summary").innerText(), /History · \d/);
  // Back shows the board, the card in Done and the focus on it.
  await page.locator(".bd-back").click();
  await page.locator('.bd-list[data-state="done"] .bd-card', { hasText: "Draft chapter 3" }).waitFor();
  assert.equal(await page.evaluate(() => document.activeElement?.closest(".bd-card")?.innerText.includes("Draft chapter 3")), true);
  await row.filter({ hasText: "2/3" }).waitFor();

  // A card another client adds appears without a reload; one blocked on the person is marked.
  await page.evaluate(() => window.ark.call("board_add", { title: "Choose the cover", owner: "user", blocked: "needs your photo" }));
  const cover = page.locator(".bd-card", { hasText: "Choose the cover" });
  await cover.waitFor();
  assert.equal(await cover.locator(".bd-tag.you").innerText(), "Needs you");
  const chip = page.locator(".bd-needs");
  assert.equal(await chip.innerText(), "Needs you · 1");
  await chip.click();
  assert.equal(await page.locator(".bd-card").count(), 1);
  await chip.click();

  // Delete from the keyboard, then undo.
  await page.locator(".bd-card", { hasText: "Pick a title" }).focus();
  await page.keyboard.press("Backspace");
  assert.equal(await page.locator(".bd-card", { hasText: "Pick a title" }).count(), 0);
  await page.locator(".bd-undo-btn").click();
  await page.locator(".bd-card", { hasText: "Pick a title" }).waitFor();
  assert.ok((await server()).includes("todo:Pick a title"));

  // The goal is renamed in place; the project's list follows.
  await bd.locator(".bd-goal").fill("Chapter 3, first draft");
  await page.keyboard.press("Enter");
  await row.filter({ hasText: "Chapter 3, first draft" }).waitFor();
  assert.equal((await page.evaluate(async () => (await window.ark.call("board")).boards.map((b) => b.goal))).join(), "Chapter 3, first draft");

  // Closed, it comes back from the project; deleted, it leaves the pane and the list.
  await page.locator(".pane-tab-close").first().click();
  assert.equal(await row.getAttribute("aria-current"), null);
  await row.click();
  await bd.locator(".bd-card", { hasText: "Pick a title" }).waitFor();
  await bd.locator(".bd-menu").click();
  await page.locator(".popover .menu-item", { hasText: "Delete the board and its 4 items" }).click();
  await row.waitFor({ state: "detached" });
  assert.equal(await page.locator(".pane-view.bd").count(), 0);
  assert.deepEqual(await server(), []);
}
scenarios.board = board;

// Schedules (ADR 0053): asked in words, approved each time; a quiet line above the composer
// says when and where it is; the line opens it beside the chat: run now, pause, resume, stop.
async function schedules(page) {
  await page.locator("#input").fill("every day at 2 AM recount the stock and give me a summary");
  await page.keyboard.press("Enter");
  // Making standing work is asked, and no "always" answer is offered for it.
  const card = page.locator(".approval").first();
  await card.waitFor();
  assert.match(await card.innerText(), /Run on its own every day at 02:00/);
  assert.match(await card.innerText(), /Each run: Recount the stock in stock.xlsx/);
  assert.equal(await card.locator("button", { hasText: /Always/ }).count(), 0, "no always for standing work");
  await page.waitForTimeout(400);
  await shot(page, "schedule-approval");
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await card.locator("button", { hasText: "Allow once" }).click();
  await done;

  const line = page.locator("#plans .plan");
  await line.waitFor();
  assert.equal(await line.locator(".plan-when").innerText(), "every day at 02:00");
  assert.match(await line.locator(".plan-state").innerText(), /^next /);
  await shot(page, "schedule-line");

  const destinations = page.getByRole("navigation", { name: "Destinations" });
  assert.equal(await destinations.getByRole("button", { name: "Chat" }).getAttribute("aria-current"), "page");
  await destinations.getByRole("button", { name: "Schedules" }).click();
  await page.locator('.view[data-view="schedules"] .sv-row').waitFor();
  assert.equal(await destinations.getByRole("button", { name: "Schedules" }).getAttribute("aria-current"), "page");
  assert.equal(await page.locator('.view[data-view="schedules"] .sv-row').count(), 1);
  await destinations.getByRole("button", { name: "Chat" }).click();
  assert.equal(await page.locator("#stage").isHidden(), true);

  await line.click();
  const panel = page.locator(".sc-panel");
  await panel.locator(".sc-what", { hasText: "Recount the stock" }).waitFor();
  const ran = page.evaluate(() => new Promise((ok) => window.ark.on("ark:orch", (m) => m.event.type === "task_finished" && ok(m.event.state))));
  await panel.locator("button", { hasText: "Run now" }).click();
  assert.equal(await ran, "done");
  await panel.locator(".sc-run .sc-said", { hasText: "120 boxes" }).waitFor();
  // The line goes back to the next time; the panel says how the last run went.
  await panel.locator(".sc-last", { hasText: /^ran / }).waitFor();
  await page.waitForFunction(() => document.querySelector("#plans .plan-state")?.textContent.startsWith("next "));
  // The result is posted in the chat too.
  await page.locator(".post", { hasText: "120 boxes" }).first().waitFor();
  await shot(page, "schedule-panel");

  await panel.locator("button", { hasText: "Pause" }).click();
  await page.waitForFunction(() => document.querySelector("#plans .plan")?.dataset.tone === "muted");
  assert.equal(await line.locator(".plan-state").innerText(), "paused");
  await panel.locator("button", { hasText: "Resume" }).click();
  await page.waitForFunction(() => document.querySelector("#plans .plan-state")?.textContent.startsWith("next "));

  // Stop asks once more, then the line goes; the run and its post stay.
  const stop = panel.locator(".sc-stop");
  await stop.click();
  assert.equal(await stop.innerText(), "Stop for good?");
  assert.equal(await line.count(), 1);
  await page.locator(".sc-stop").click();
  await line.waitFor({ state: "detached" });
  assert.equal(await page.evaluate(async () => (await window.ark.call("schedules")).length), 0);
  assert.ok(await page.locator(".post", { hasText: "120 boxes" }).count() > 0);
}
scenarios.schedules = schedules;

// The fixed destinations have one hierarchy, every schedule state, useful offline recovery and
// no way for a late response from one project to appear in another.
async function scheduleDestinationStates(page) {
  await page.evaluate(() => {
    const original = window.ark.call.bind(window.ark);
    window.__scheduleReplies = [];
    window.__scheduleReply = (reply) => window.__scheduleReplies.push(reply);
    window.ark.call = (method, params = {}) => {
      if (method !== "schedules" || params.session) return original(method, params);
      const reply = window.__scheduleReplies.shift();
      if (!reply) return Promise.resolve([]);
      return typeof reply === "function" ? reply() : Promise.resolve(reply);
    };
    window.ark.view({
      id: "home-proof", title: "Home proof", nav: true,
      render(el) { el.innerHTML = '<h1 tabindex="-1" data-destination-heading>Home proof</h1>'; },
    });
    window.ark.openView("home-proof");
  });
  await page.locator('.view[data-view="home-proof"] h1').waitFor();

  await page.evaluate(() => {
    window.__scheduleReply(() => new Promise((resolve) => { window.__resolveSchedules = resolve; }));
  });
  const scheduleButton = page.getByRole("navigation", { name: "Destinations" }).getByRole("button", { name: "Schedules" });
  await scheduleButton.focus();
  await page.keyboard.press("Enter");
  const view = page.locator('.view[data-view="schedules"]');
  await view.locator(".sv-loading").waitFor();
  assert.equal(await view.getByRole("heading", { name: "Schedules" }).evaluate((el) => el === document.activeElement), true);

  const rows = [
    ["scheduled", "Upcoming", "next Fri 09:00"],
    ["running", "Running", "running now"],
    ["asking", "Waiting", "waits for you"],
    ["retrying", "Retrying", "trying again at 09:10"],
    ["succeeded", "Succeeded", "next Sat 09:00"],
    ["failed", "Failed", "did not finish Thu 09:00"],
    ["paused", "Paused", "paused"],
  ].map(([state, objective, status], i) => ({
    id: `s${i}`, agent: `agent-${i}`, objective, context: "", says: "every day at 09:00",
    tz: "UTC", next_ms: 1, next: "Fri 09:00", paused: state === "paused", state, status,
    last: state === "succeeded" ? "ran Thu 09:00" : null, chat: "chat", runs: i,
    missed: i === 0 ? 2 : 0, skipped: i === 0 ? 1 : 0, last_task: null,
  }));
  await page.evaluate((value) => window.__resolveSchedules(value), rows);
  await view.locator(".sv-row").first().waitFor();
  assert.equal(await view.locator(".sv-row").count(), 7);
  for (const state of ["scheduled", "running", "asking", "retrying", "succeeded", "failed", "paused"]) {
    assert.equal(await view.locator(`.sv-row[data-state="${state}"]`).count(), 1, state);
  }
  assert.equal((await view.locator('.sv-row[data-state="scheduled"] .sv-count').innerText()).trim(), "2 missed · 1 skipped");
  assert.match(await view.locator('.sv-row[data-state="succeeded"]').innerText(), /ran Thu 09:00/);
  assert.match(await view.locator('.sv-row[data-state="scheduled"]').getAttribute("aria-label"), /2 missed · 1 skipped/);
  assert.match(await view.locator('.sv-row[data-state="succeeded"]').getAttribute("aria-label"), /ran Thu 09:00/);
  await shot(page, "schedule-destination-states");

  // Same-project refresh failure retains useful rows and offers recovery.
  await page.evaluate(() => {
    window.__scheduleReply(() => Promise.reject(new Error("offline")));
    window.ark.openView("schedules");
  });
  await view.locator(".sv-stale").waitFor();
  assert.equal(await view.locator(".sv-row").count(), 7);
  assert.equal(await view.locator('.sv-stale[role="status"]').count(), 0, "one live region announces refresh failures");
  assert.ok(await view.getByRole("button", { name: "Try again" }).isVisible());

  // A project update clears old rows immediately and ignores the old request when it resolves.
  await page.evaluate(() => {
    window.__scheduleReply(() => new Promise((resolve) => { window.__resolveOldProject = resolve; }));
    window.ark.openView("schedules");
    window.__scheduleReply(() => new Promise((resolve) => { window.__resolveNewProject = resolve; }));
    window.ark.openView("schedules", { project: { id: "other-project", name: "Other" } });
  });
  assert.equal(await view.locator(".sv-row").count(), 0, "old project rows clear before the new load");
  await page.evaluate(() => window.__resolveOldProject([{ id: "old", agent: "old", objective: "Old project", says: "daily", state: "scheduled", status: "next tomorrow", runs: 0, missed: 0, skipped: 0 }]));
  await page.waitForTimeout(50);
  assert.equal(await view.getByText("Old project").count(), 0);
  await page.evaluate(() => window.__resolveNewProject([{ id: "new", agent: "new", objective: "New project", says: "daily", state: "scheduled", status: "next tomorrow", runs: 0, missed: 0, skipped: 0 }]));
  await view.getByText("New project").waitFor();

  // Empty and offline-without-data are distinct, and Retry can recover to the empty state.
  await page.evaluate(() => {
    window.__scheduleReply([]);
    window.ark.openView("schedules");
  });
  await view.getByRole("heading", { name: "No schedules" }).waitFor();
  await page.evaluate(() => {
    window.__scheduleReply(() => Promise.reject(new Error("offline")));
    window.ark.openView("schedules");
  });
  await view.getByRole("heading", { name: "Schedules are unavailable" }).waitFor();
  await shot(page, "schedule-destination-offline");
  await page.evaluate(() => window.__scheduleReply([]));
  await view.getByRole("button", { name: "Try again" }).click();
  await view.getByRole("heading", { name: "No schedules" }).waitFor();

  // Chat always returns to the conversation from another project destination.
  await page.getByRole("navigation", { name: "Destinations" }).getByRole("button", { name: "Chat" }).click();
  assert.equal(await page.locator("#scroller").isVisible(), true);
}
scenarios["schedule-destination-states"] = scheduleDestinationStates;

// Files the agent writes are cards that open beside the conversation; the person edits them in
// place and the file changes: a cell (the total follows), a document's lines.
async function files(page) {
  const ask = async (text) => {
    await page.locator("#input").fill(text);
    await page.keyboard.press("Enter");
    await page.locator(".approval .btn.primary").click();
  };
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }), path);
  // Until the file (as the host reads it) passes `test`.
  const until = async (path, test) => {
    for (let i = 0; ; i++) {
      const f = await read(path);
      if (test(f)) return f;
      if (i > 50) assert.fail(`${path} did not change as expected: ${JSON.stringify(f).slice(0, 400)}`);
      await page.waitForTimeout(100);
    }
  };
  const total = (f) => f.sheets[0].rows[3][1].t;

  // A second formatting blur during a slow remote save is queued, not overwritten by the
  // first response. The queued edit carries the complete inspector state.
  await page.evaluate(async () => {
    const { documentView } = await import("/js/document.js");
    const host = document.createElement("div");
    host.className = "layout-race";
    document.body.append(host);
    const base = { line_spacing: 1.15, paragraph_before: 0, paragraph_after: 8, character_spacing: 0, margin_top: 1, margin_right: 1, margin_bottom: 1, margin_left: 1, section_margins: [{ top: 1, right: 1, bottom: 1, left: 1 }] };
    let current = { path: "slow.docx", layout: base, blocks: [{ kind: "paragraph", text: "Slow", layout: {} }] };
    window.__layoutSaveCalls = [];
    window.__layoutSaveResolvers = [];
    documentView(host, current, {
      ark: { isLocal: () => false, on: () => () => {} },
      tools: document.createElement("div"), status: () => {}, focusChanged: () => {},
      save: (edits) => new Promise((resolve) => {
        window.__layoutSaveCalls.push(edits[0].layout);
        window.__layoutSaveResolvers.push(() => {
          current = { ...current, layout: { ...current.layout, ...edits[0].layout } };
          resolve(current);
        });
      }),
    });
  });
  const race = page.locator(".layout-race");
  await race.getByRole("button", { name: "Format document" }).click();
  await race.getByRole("spinbutton", { name: "Line" }).fill("1.4");
  await race.getByRole("spinbutton", { name: "Line" }).blur();
  await page.waitForFunction(() => window.__layoutSaveCalls.length === 1);
  await race.getByRole("spinbutton", { name: "After" }).fill("12");
  await race.getByRole("spinbutton", { name: "After" }).blur();
  assert.equal(await page.evaluate(() => window.__layoutSaveCalls.length), 1);
  await page.evaluate(() => window.__layoutSaveResolvers[0]());
  await page.waitForFunction(() => window.__layoutSaveCalls.length === 2);
  assert.deepEqual(await page.evaluate(() => window.__layoutSaveCalls[1]), {
    line_spacing: 1.4, paragraph_before: 0, paragraph_after: 12, character_spacing: 0,
    margin_top: 1, margin_right: 1, margin_bottom: 1, margin_left: 1,
  });
  await page.evaluate(() => window.__layoutSaveResolvers[1]());
  await page.waitForFunction(() => document.querySelector('.layout-race [aria-label="After"]')?.value === "12");
  await page.evaluate(() => document.querySelector(".layout-race").remove());

  await finished(page, () => ask("Make the sales workbook."));
  await page.locator(".file-card", { hasText: "sales.xlsx" }).click();
  // Drive the production sheet renderer with the same chart shape ark-files extracts from
  // native XLSX parts. Parser and byte-preservation behavior are covered in ark-files.
  await page.evaluate(async () => {
    const { sheetView } = await import("/js/sheet.js");
    const host = document.createElement("div");
    host.className = "fv chart-test-host";
    Object.assign(host.style, { position: "fixed", inset: "0", zIndex: "100", background: "var(--bg)" });
    document.body.append(host);
    sheetView(host, {
      path: "sales.xlsx", charts: [{ sheet: "Sheet1", title: "Quarterly sales", type: "column", sampled: false, series: [{ name: "Q1", categories: ["North", "South"], values: [120, 80], x_values: [] }] }],
      chart_warnings: [], sheets: [{ name: "Sheet1", rows: [], cols: 0, more_rows: 0, more_cols: 0 }],
    }, { ark: { isLocal: () => true, on: () => () => {} }, tools: document.createElement("div"), save: async () => null, status: () => {}, focusChanged: () => {} });
  });
  await page.getByRole("tab", { name: "Charts" }).click();
  await page.getByRole("heading", { name: "Quarterly sales" }).waitFor();
  assert.equal(await page.locator('.sh-chart svg[role="img"]').count(), 1);
  assert.equal(await page.locator('.sh-chart table[aria-label="Quarterly sales data"]').count(), 1);
  await shot(page, "office-charts-wide");
  await page.setViewportSize({ width: 430, height: 760 });
  await page.evaluate(() => document.documentElement.dataset.theme = "dark");
  await page.waitForFunction(() => {
    const app = document.querySelector(".app");
    return app.dataset.compact === "true" && app.style.getPropertyValue("--pw") === "430px"
      && !app.getAnimations().some((animation) => animation.playState === "running");
  });
  const chartLayout = await page.evaluate(() => {
    const bounds = (selector) => {
      const el = document.querySelector(selector);
      const rect = el.getBoundingClientRect();
      return { left: rect.left, right: rect.right, width: rect.width, client: el.clientWidth, scroll: el.scrollWidth };
    };
    return {
      viewport: innerWidth, document: document.documentElement.scrollWidth,
      app: bounds(".app"), host: bounds(".chart-test-host"),
      charts: bounds(".chart-test-host .sh-charts"), canvas: bounds(".chart-test-host .sh-chart-canvas"),
    };
  });
  assert.equal(chartLayout.document > chartLayout.viewport, false, JSON.stringify(chartLayout));
  assert.ok(chartLayout.canvas.scroll > chartLayout.canvas.client, JSON.stringify(chartLayout));
  await shot(page, "office-charts-narrow-dark");
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.evaluate(() => document.documentElement.dataset.theme = "light");
  await page.evaluate(() => document.querySelector(".chart-test-host").remove());
  const cell = (r, c) => page.locator(`td[data-r="${r}"][data-c="${c}"]`);
  await page.waitForFunction(() => document.querySelector('td[data-r="3"][data-c="1"]')?.textContent === "200");
  await cell(1, 1).click();
  await page.keyboard.type("150");
  await page.keyboard.press("Enter");
  await until("sales.xlsx", (f) => total(f) === "230");
  await page.waitForFunction(() => document.querySelector('td[data-r="3"][data-c="1"]').textContent === "230");
  assert.equal(await page.locator(".sh-ref").inputValue(), "B3");
  // The formula bar shows the selected cell's formula.
  await cell(3, 1).click();
  assert.equal(await page.locator(".sh-fx").inputValue(), "=SUM(B2:B3)");
  // Clearing a cell clears it in the file.
  await cell(2, 1).click();
  await page.keyboard.press("Backspace");
  await until("sales.xlsx", (f) => total(f) === "150");

  await finished(page, () => ask("Write the report."));
  await page.locator(".file-card", { hasText: "report.docx" }).click();
  await page.locator(".dv-page").waitFor();
  assert.deepEqual(await page.locator(".dv-b").allInnerTexts(), ["Sales", "North led.", "Check the south"]);
  assert.equal(await page.locator(".dv-b").nth(2).getAttribute("data-kind"), "bullet");
  await page.getByRole("button", { name: "Format document" }).click();
  const line = page.getByRole("spinbutton", { name: "Line" });
  await line.fill("1.5");
  await line.blur();
  await until("report.docx", (f) => f.layout?.line_spacing === 1.5);
  assert.equal(await page.locator(".dv-page").evaluate((el) => el.style.getPropertyValue("--dv-line")), "1.5");
  await shot(page, "office-document-format-wide");
  await page.locator(".dv").evaluate((el) => {
    const marker = document.createComment("document-view-home");
    el.before(marker);
    document.body.append(el);
    window.__documentViewHome = marker;
    Object.assign(el.style, { position: "fixed", inset: "0", zIndex: "100", background: "var(--bg)" });
  });
  await page.setViewportSize({ width: 430, height: 760 });
  await page.evaluate(() => document.documentElement.dataset.theme = "dark");
  await page.waitForTimeout(300);
  assert.ok((await page.locator(".dv-inspector").boundingBox()).y < 300, "mobile formatting sheet is inside the viewport");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await shot(page, "office-document-format-narrow-dark");
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.evaluate(() => document.documentElement.dataset.theme = "light");
  await page.locator(".dv").evaluate((el) => {
    window.__documentViewHome.replaceWith(el);
    delete window.__documentViewHome;
    el.removeAttribute("style");
  });
  await page.getByRole("button", { name: "Close formatting" }).click();
  await page.evaluate(() => {
    const b = document.querySelectorAll(".dv-b")[1];
    const r = document.createRange();
    r.selectNodeContents(b);
    r.collapse(false);
    document.querySelector(".dv-page").focus();
    getSelection().removeAllRanges();
    getSelection().addRange(r);
  });
  await page.keyboard.press("Enter");
  await page.keyboard.type("- South fell");
  // The agent's next read is the person's version.
  await until("report.docx", (f) => f.blocks.map((b) => `${b.kind}:${b.text}`).join("|") === "heading:Sales|paragraph:North led.|bullet:South fell|bullet:Check the south");

  // The Files tab belongs to the project it opened in: moving to another project closes it.
  await page.evaluate(() => window.ark.openPanel("files"));
  const files = page.locator(".pane-tab", { hasText: /^Files/ });
  await files.waitFor();
  const first = await page.evaluate(() => window.ark.summary().project.id);
  await page.evaluate(() => window.ark.call("add_project", { name: "Other project" }));
  await page.waitForFunction((first) => window.ark.summary().project.id !== first, first);
  await files.waitFor({ state: "detached" });
}
scenarios.files = files;

// A program the agent runs writes a chart, a table, a report that shows the chart, and a PDF.
// Each is a card in the turn and opens in the view for its kind; nothing names the program.
async function analysis(page) {
  await page.locator("#input").fill("Analyze the sales and make me a chart and a short report.");
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.keyboard.press("Enter");
  const ask = page.locator(".approval");
  await ask.waitFor();
  // The approval leads with what the run is for, then the program and what it may do.
  const words = await ask.innerText();
  assert.match(words, /ARK wants to:\nChart the sales and write a report\nWith a Sh program in the project folder \(it can change files there; no network\)/);
  assert.match(words, /cat > chart\.svg/);
  await page.locator(".approval .btn.primary").click();
  await done;
  await page.waitForTimeout(150);

  const cards = await page.locator(".file-card").evaluateAll((els) => els.map((e) => `${e.dataset.kind}:${e.innerText.replace(/\s+/g, " ").trim()}`));
  assert.deepEqual(cards.sort(), ["image:chart.svg Image", "markdown:report.md Markdown document", "pdf:summary.pdf PDF", "sheet:table.csv Spreadsheet"]);

  // The image file card is its inline preview. The same keyboard control opens the existing
  // image view beside the conversation.
  const chartCard = page.locator(".file-card", { hasText: "chart.svg" });
  await page.waitForFunction(() => document.querySelector(".file-card-image")?.naturalWidth > 0);
  assert.match(await chartCard.getAttribute("title"), /chart\.svg$/);
  await chartCard.focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector("img.media-image")?.naturalWidth > 0);

  // The report renders as a page and shows the chart it links to.
  await page.locator(".file-card", { hasText: "report.md" }).click();
  await page.locator(".md-page h2").waitFor();
  assert.equal(await page.locator(".md-page h2").innerText(), "Sales");
  await page.waitForFunction(() => document.querySelector(".md-page img.md-img")?.naturalWidth > 0);

  // The table opens as a sheet.
  await page.locator(".file-card", { hasText: "table.csv" }).click();
  await page.waitForFunction(() => document.querySelector('td[data-r="1"][data-c="1"]')?.textContent === "120");
  // It is longer than the grid shows, and says so; no rows to type into past the part shown.
  assert.equal(await page.locator(".sh-part").innerText(), "Showing the first 5,000 of 6,003 rows. Ask ARK to work with all of them.");

  // The PDF opens in the browser's own viewer (headless WebKit does not paint it, so only the
  // frame and its document are checked).
  await page.locator(".file-card", { hasText: "summary.pdf" }).click();
  await page.waitForFunction(() => document.querySelector("iframe.media-pdf")?.src.startsWith("blob:"));
  // The window reads a PDF as itself; the agent reads its text (files.read).
  const read = await page.evaluate(() => window.ark.call("file_read", { path: "summary.pdf" }));
  assert.equal(read.kind, "pdf");
  assert.equal(read.media_type, "application/pdf");
}
scenarios.analysis = analysis;

async function imageFailure(page) {
  await page.evaluate(() => {
    URL.createObjectURL = () => "blob:unreadable-image";
  });
  await page.locator("#input").fill("Make me an image.");
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.keyboard.press("Enter");
  await page.locator(".approval .btn.primary").click();
  await done;
  await page.waitForTimeout(150);
  const card = page.locator(".file-card", { hasText: "generated-image.png" });
  await card.waitFor();
  assert.equal(await card.locator(".file-card-image").isHidden(), true);
  assert.equal(await card.evaluate((el) => el.classList.contains("image-preview")), false);
  assert.match(await card.innerText(), /generated-image\.png\s+Image/);
}
scenarios["image-failure"] = imageFailure;

// ADR 0034: a run changes one file and removes another. Each card of that turn puts the file
// back as it was before the turn, and the changed one can take the turn's version back.
async function restore(page) {
  const run = async (text) => {
    const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
    await page.locator("#input").fill(text);
    await page.keyboard.press("Enter");
    await page.locator(".approval .btn.primary").click();
    await done;
    await page.waitForTimeout(150);
  };
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }).then((f) => f.text, (e) => `error: ${e.message}`), path);
  await run("Write the notes.");
  // A new file has nothing to put back.
  assert.equal(await page.locator(".turn").last().locator(".file-restore").count(), 0);

  await run("Rewrite the notes and drop the old file.");
  const turn = page.locator(".turn").last();
  const notes = turn.locator(".file-item", { hasText: "notes.md" });
  const old = turn.locator(".file-item", { hasText: "old.md" });
  assert.equal(await old.locator(".file-card-sub").innerText(), "Removed");
  assert.equal(await old.locator(".file-card").isDisabled(), true);
  assert.equal(await read("notes.md"), "two\n");

  await notes.locator(".file-restore").click();
  await notes.locator(".file-restore", { hasText: "Undo" }).waitFor();
  assert.equal(await read("notes.md"), "one\n");
  await notes.locator(".file-restore").click();
  await notes.locator(".file-restore", { hasText: "Restore" }).waitFor();
  assert.equal(await read("notes.md"), "two\n");

  // The removed file comes back and opens; there was nothing to undo.
  await old.locator(".file-restore").click();
  await old.locator(".file-restore").waitFor({ state: "hidden" });
  assert.equal(await read("old.md"), "old\n");
  await old.locator(".file-card").click();
  await page.locator(".md-page").waitFor();

  // A view of the file open beside the conversation follows a restore.
  await notes.locator(".file-card").click();
  await page.locator(".md-page h1, .md-page p", { hasText: "two" }).first().waitFor();
  await notes.locator(".file-restore").click();
  await page.locator(".md-page p", { hasText: "one" }).first().waitFor();
}
scenarios.restore = restore;

// ADR 0035: files dropped on the conversation go into the project's folder. A name in use gets
// a number, a file larger than one call arrives whole, and the composer names what was added.
async function drop(page) {
  // A drop as the browser makes it, with files built in the page.
  const dropFiles = (files) => page.evaluate((files) => {
    const dt = new DataTransfer();
    for (const f of files) {
      const text = f.lines ? Array.from({ length: f.lines }, (_, i) => `row ${i}`).join("\n") : f.text;
      dt.items.add(new File([text], f.name, { type: "text/plain" }));
    }
    const at = document.querySelector("#thread");
    for (const type of ["dragenter", "dragover", "drop"]) at.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true }));
  }, files);
  const read = (path) => page.evaluate((p) => window.ark.call("file_read", { path: p }).then((f) => f.text), path);

  // Dragging over shows where to drop; leaving hides it.
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "x.txt"));
    document.querySelector("#thread").dispatchEvent(new DragEvent("dragenter", { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
  assert.equal(await page.locator(".drop-hint").isVisible(), true);
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "x.txt"));
    document.querySelector("#thread").dispatchEvent(new DragEvent("dragleave", { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
  assert.equal(await page.locator(".drop-hint").isVisible(), false);

  await page.locator("#input").fill("Compare them");
  await dropFiles([{ name: "notes.md", text: "# Mine\n" }]);
  await page.waitForFunction(() => document.querySelector("#input").value.includes("Added to the project"));
  assert.equal(await page.locator("#input").inputValue(), "Compare them\nAdded to the project: notes.md");
  // The cursor stays after the person's words, above the line.
  assert.equal(await page.evaluate(() => document.querySelector("#input").selectionStart), "Compare them".length);
  assert.equal(await read("notes.md"), "# Mine\n");

  // About 5 MB: three calls. The second notes.md keeps the first.
  await page.locator("#input").fill("");
  await dropFiles([{ name: "notes.md", text: "# Theirs\n" }, { name: "rows.txt", lines: 500000 }]);
  await page.waitForFunction(() => document.querySelector("#input").value.includes("Added to the project"));
  assert.equal(await page.locator("#input").inputValue(), "\nAdded to the project: notes 2.md, rows.txt");
  assert.equal(await read("notes.md"), "# Mine\n");
  assert.equal(await read("notes 2.md"), "# Theirs\n");
  const rows = await read("rows.txt");
  assert.equal(rows.length, Array.from({ length: 500000 }, (_, i) => `row ${i}`).join("\n").length);
  assert.ok(rows.startsWith("row 0\nrow 1\n") && rows.endsWith("\nrow 499999"));
  assert.equal(await page.locator(".adding").count(), 0);
  // Nothing is left of the uploads.
  const left = await page.evaluate(() => window.ark.call("files", { path: "" }).then((r) => r.entries.map((e) => e.name).sort()));
  assert.deepEqual(left, ["notes 2.md", "notes.md", "rows.txt"]);
}
scenarios.drop = drop;

async function uploadDrafts(page) {
  const input = page.locator("#input");
  const origin = await page.evaluate(() => window.ark.summary().location);
  const open = async id => {
    await page.evaluate(id => window.ark.openSession(id), id);
    await page.evaluate(id => window.ark.shown(id), id);
  };
  for (const differentProject of [false, true]) {
    await input.fill("Origin draft");
    if (differentProject) await page.evaluate(() => window.ark.addProject({ name: "Upload destination check" }));
    else await page.evaluate(() => window.ark.newSession());
    await page.waitForFunction(origin => window.ark.summary().session.id !== origin.session, origin);
    const other = await page.evaluate(() => window.ark.summary().session.id);
    await page.evaluate(id => window.ark.shown(id), other);
    await open(origin.session);
    const name = differentProject ? "cross-project.txt" : "same-project.txt";
    let release, started;
    const waiting = new Promise(resolve => { started = resolve; });
    const handler = async route => {
      const request = route.request().postDataJSON();
      if (request.method !== "resource" || request.params.operation.kind !== "upload_add") return route.continue();
      assert.deepEqual(request.params.ref.location, origin);
      const response = await route.fetch();
      await new Promise(resolve => { release = resolve; started(); });
      await route.fulfill({ response });
    };
    await page.route("**/rpc", handler);
    try {
      await page.evaluate(name => {
        const dataTransfer = new DataTransfer();
        dataTransfer.items.add(new File(["Origin project only"], name));
        document.querySelector("#thread").dispatchEvent(new DragEvent("drop", { dataTransfer, bubbles: true, cancelable: true }));
      }, name);
      await waiting;
      await open(other);
      await input.fill("Unrelated draft");
      await input.evaluate(el => el.setSelectionRange(2, 5));
      await page.locator("#mode-chip").focus();
      release();
      await page.locator(".adding").waitFor({ state: "detached" });
      assert.equal(await input.inputValue(), "Unrelated draft");
      assert.deepEqual(await input.evaluate(el => [el.selectionStart, el.selectionEnd]), [2, 5]);
      assert.equal(await page.locator("#mode-chip").evaluate(el => document.activeElement === el), true);
      if (differentProject) await assert.rejects(page.evaluate(name => window.ark.call("file_read", { path: name }), name));
      await open(origin.session);
      assert.equal(await input.inputValue(), `Origin draft\nAdded to the project: ${name}`);
      assert.equal(await page.evaluate(name => window.ark.call("file_read", { path: name }).then(file => file.text), name), "Origin project only");
    } finally {
      release?.();
      await page.unroute("**/rpc", handler);
    }
  }
  await shot(page, "upload-drafts");
}
scenarios["upload-drafts"] = uploadDrafts;

// The + menu opens the built-in panels. Files edits and saves a file in the project folder;
// the terminal runs a shell there. A page on this machine's loopback address says its runtime
// is local.
async function workbench(page) {
  assert.equal(await page.evaluate(() => window.ark.isLocal()), true);
  // A browser has no window to go back to.
  assert.equal(await page.evaluate(() => typeof window.ark.openLocal), "undefined");
  assert.equal(await page.locator("#app").getAttribute("data-shell"), "browser");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.runtime), "local");
  await page.evaluate(() => window.ark.call("file_write", { path: "draft.txt", text: "one" }));
  await page.locator("#open-menu").click();
  const items = page.locator(".popover .open-item");
  assert.deepEqual(await items.allInnerTexts().then((t) => t.map((x) => x.split("\n")[0].trim())), ["Terminal", "Browser", "Files"]);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.locator(".wb-entry", { hasText: "draft.txt" }).click();
  const editor = page.locator(".wb-editor");
  assert.equal(await editor.inputValue(), "one");
  assert.equal(await page.locator(".wb-save").isDisabled(), true);
  await editor.fill("two");
  await page.locator(".wb-save").click();
  for (let i = 0; (await page.evaluate(() => window.ark.call("file_read", { path: "draft.txt" }))).text !== "two"; i++) {
    assert.ok(i < 50, "draft.txt was not saved");
    await page.waitForTimeout(100);
  }
  await assert.rejects(page.evaluate(() => window.ark.call("file_read", { path: "../outside" })));

  // Markdown opens as a page; Edit shows its text, and Done the page as saved.
  await page.evaluate(() => window.ark.call("file_write", { path: "notes.md", text: "# Plan\n\nShip it." }));
  await page.evaluate(() => window.ark.openFile("notes.md"));
  await page.locator(".md-page h2", { hasText: "Plan" }).waitFor();
  await page.locator(".btn", { hasText: "Edit" }).click();
  const md = page.locator(".fv").filter({ has: page.locator('textarea[aria-label="notes.md"]') });
  await md.locator(".wb-editor").fill("# Plan\n\nShip it today.");
  await md.locator(".wb-save").click();
  await md.locator(".btn", { hasText: "Done" }).click();
  await page.locator(".md-page p", { hasText: "Ship it today." }).waitFor();

  // Code is coloured behind the editor, with the same text, font and scroll; typing and Save
  // keep the exact text. Plain text has no colours and wraps as before.
  const source = "// sum\nconst total = items.reduce((a, b) => a + b, 0);\n" + "let n = 1;\n".repeat(80);
  await page.evaluate((text) => window.ark.call("file_write", { path: "app.js", text }), source);
  await page.evaluate(() => window.ark.openFile("app.js"));
  const js = page.locator(".fv").filter({ has: page.locator('textarea[aria-label="app.js"]') });
  await js.locator(".wb-hl .tk-key", { hasText: "const" }).first().waitFor();
  assert.equal(await js.locator(".wb-hl .tk-com").first().innerText(), "// sum");
  const layout = await js.evaluate((fv) => {
    const area = fv.querySelector(".wb-editor");
    const layer = fv.querySelector(".wb-hl");
    const a = getComputedStyle(area);
    const l = getComputedStyle(layer);
    return {
      same: ["fontFamily", "fontSize", "lineHeight", "paddingTop", "paddingLeft", "tabSize", "whiteSpace"].filter((k) => a[k] !== l[k]),
      text: layer.textContent === area.value + "\n",
      transparent: a.color,
    };
  });
  assert.deepEqual(layout.same, [], "the editor and its colours differ in layout");
  assert.ok(layout.text, "the colours show other text than the editor");
  assert.equal(layout.transparent, "rgba(0, 0, 0, 0)");
  await js.locator(".wb-editor").evaluate((area) => {
    area.scrollTop = 200;
    area.dispatchEvent(new Event("scroll"));
  });
  assert.match(await js.locator(".wb-hl code").evaluate((c) => c.style.transform), /translate\(0px, -\d+px\)/);
  await js.locator(".wb-editor").focus();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+End" : "Control+End");
  await page.keyboard.type("return 'done';");
  await js.locator(".wb-hl .tk-str", { hasText: "'done'" }).waitFor();
  await js.locator(".wb-save").click();
  for (let i = 0; (await page.evaluate(() => window.ark.call("file_read", { path: "app.js" }))).text !== source + "return 'done';"; i++) {
    assert.ok(i < 50, "app.js was not saved as typed");
    await page.waitForTimeout(100);
  }
  for (let i = 0; !(await js.locator(".wb-save").isDisabled()); i++) {
    assert.ok(i < 50, "Save stayed on after saving");
    await page.waitForTimeout(100);
  }
  // A selection stays visible over the colours.
  await js.locator(".wb-editor").evaluate((area) => {
    area.focus();
    area.setSelectionRange(0, 60);
    area.scrollTop = 0;
    area.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(400);
  await shot(page, "editor-code");
  await page.evaluate(() => window.arkTheme.set("dark"));
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark");
  // Colours ease into the new theme.
  await page.waitForTimeout(400);
  await shot(page, "editor-code-dark");
  await page.evaluate(() => window.arkTheme.setMode("system"));
  // A huge file is the plain editor: no layer to repaint, its own text shown.
  await page.evaluate((text) => window.ark.call("file_write", { path: "big.js", text }), "let x = 1;\n".repeat(16000));
  await page.evaluate(() => window.ark.openFile("big.js"));
  const big = page.locator(".fv").filter({ has: page.locator('textarea[aria-label="big.js"]') });
  await big.locator(".wb-code.wb-plain").waitFor();
  assert.equal(await big.locator(".wb-hl code").innerHTML(), "");
  assert.notEqual(await big.locator(".wb-editor").evaluate((a) => getComputedStyle(a).color), "rgba(0, 0, 0, 0)");
  await page.evaluate(() => window.ark.openFile("draft.txt"));
  const txt = page.locator(".fv").filter({ has: page.locator('textarea[aria-label="draft.txt"]') });
  await txt.locator(".wb-editor").waitFor();
  assert.equal(await txt.locator(".wb-hl").count(), 0);
  assert.equal(await txt.locator(".wb-editor").getAttribute("wrap"), null);

  await page.evaluate(() => window.ark.openPanel("terminal"));
  // A shell under load can miss a line typed before its prompt: type it again until it answers.
  const term = page.locator(".xterm-helper-textarea");
  await term.waitFor({ state: "attached" });
  const answer = page.locator(".xterm-rows", { hasText: "ark-42" });
  for (let i = 0; ; i++) {
    await term.type("echo ark-$((40+2))\n");
    try {
      await answer.waitFor({ timeout: 5000 });
      break;
    } catch (e) {
      if (i === 3) throw e;
    }
  }
}
scenarios.workbench = workbench;

// The window's Browser view (ADR 0044) is drawn above the page, so the panel tells it where to
// be and when to step aside. A stub in place of the window records what the page asks.
async function useNativeBrowserFixture(page) {
  await page.evaluate(async () => {
    const { BUILTINS } = await import("./js/workbench.js");
    const browser = BUILTINS.find(panel => panel.id === "browser");
    window.ark.panel({ ...browser, render: (el, params, scoped) => browser.render(el, params,
      Object.assign(Object.create(scoped), { destinationBrowser: false, browserView: window.ark.browserView })) });
  });
}

async function browserview(page) {
  await page.evaluate(() => {
    window.calls = [];
    window.ark.browserView = {
      show: (r, url) => {
        window.calls.push(["show", r, url || null]);
        return url?.startsWith("file:") ? Promise.reject(new Error("the Browser does not open that address")) : Promise.resolve({ shown: true });
      },
      act: (a) => (window.calls.push([a]), Promise.resolve()),
      reach: () => Promise.resolve({ reached: true }),
      listen: (fn) => ((window.emitBrowser = fn), Promise.resolve()),
    };
  });
  await useNativeBrowserFixture(page);
  const last = () => page.evaluate(() => window.calls.at(-1));
  const until = (what) => page.waitForFunction((w) => window.calls.at(-1)?.[0] === w, what);

  // A local address typed without a scheme opens over http, at the panel's place in the pane.
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "localhost:3000" }));
  await page.waitForFunction(() => window.calls.some((c) => c[0] === "show" && c[2] === "http://localhost:3000"));
  // Wait for the pane to finish opening, then compare the last place with the pane.
  await page.waitForTimeout(400);
  const [, r] = await last();
  const pane = await page.locator("#pane").boundingBox();
  const bar = await page.locator(".wb-bar").boundingBox();
  // Inside the pane, clear of its resize grip.
  assert.ok(r.x >= Math.floor(pane.x) + 6 && r.x + r.width <= Math.ceil(pane.x + pane.width), JSON.stringify({ r, pane }));
  assert.ok(r.y >= Math.floor(bar.y + bar.height) - 1, JSON.stringify({ r, bar }));
  assert.ok(r.width > 200 && r.height > 200, JSON.stringify(r));
  // Nothing more is asked while nothing moves.
  const n = await page.evaluate(() => window.calls.length);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(() => window.calls.length), n);

  // A page that loads inside the view reaches the address bar and the tab.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: false }));
  assert.equal(await page.locator(".wb-input").inputValue(), "http://localhost:3000/home");

  // An address the view refuses says why, and the bar goes back to the page shown.
  await page.locator(".wb-input").fill("file:///etc/passwd");
  await page.locator(".wb-input").press("Enter");
  await page.locator(".toast", { hasText: "does not open that address" }).waitFor();
  assert.equal(await page.locator(".wb-input").inputValue(), "http://localhost:3000/home");
  await page.locator("#input").focus();

  // The palette covers the pane: the view steps aside, and comes back where it was.
  await page.keyboard.press("Meta+k");
  await until("hide");
  await page.keyboard.press("Escape");
  await until("show");
  assert.deepEqual((await last())[1], r);
  assert.equal((await last())[2], null, "coming back does not load the page again");

  // While the view has the keyboard, ⌘K and ⌘N reach ARK through the window's menu.
  const menu = (key) => page.evaluate((key) => window.dispatchEvent(new CustomEvent("ark:menu-key", { detail: key })), key);
  await menu("k");
  await until("hide");
  assert.equal(await page.locator("#overlay").isVisible(), true, "the menu's ⌘K opens the palette");
  await page.keyboard.press("Escape");
  await until("show");
  // A plain k typed just before is not a ⌘K: the menu's key still opens the palette.
  await page.locator("#input").focus();
  await page.keyboard.press("k");
  await menu("k");
  await until("hide");
  assert.equal(await page.locator("#overlay").isVisible(), true, "a typed k does not swallow the menu's ⌘K");
  await page.keyboard.press("Escape");
  await until("show");
  // The page's own ⌘K acted already: the menu's echo of the same key does not close it again.
  await page.keyboard.press("Meta+k");
  await menu("k");
  await until("hide");
  await page.waitForTimeout(100);
  assert.equal(await page.locator("#overlay").isVisible(), true, "one key press, one palette");
  await page.keyboard.press("Escape");
  await until("show");

  // Another tab in front, then the Browser again.
  await page.evaluate(() => window.ark.openPanel("files"));
  await until("hide");
  await page.locator(".pane-tab", { hasText: "localhost" }).click();
  await until("show");

  // Closing the pane hides the view; the toggle brings it back.
  await page.locator("#close-pane").click();
  await until("hide");
  await page.evaluate(() => window.ark.openPanel("browser"));
  await until("show");

  await page.locator('.wb-bar button[aria-label="Back"]').click();
  await until("back");
  await page.locator('.wb-bar button[aria-label="Reload"]').click();
  await until("reload");

  // Closing the tab ends the view.
  await page.locator(".pane-tab-item", { hasText: "localhost" }).locator(".pane-tab-close").click();
  await until("close");

  // A Browser tab belongs to the project it opened in: moving to another project closes it.
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "localhost:3000" }));
  await page.locator(".pane-tab", { hasText: "localhost" }).waitFor();
  const first = await page.evaluate(() => window.ark.summary().project.id);
  const seen = await page.evaluate(() => window.calls.length);
  await page.evaluate(() => window.ark.call("add_project", { name: "Bun project" }));
  await page.waitForFunction((first) => window.ark.summary().project.id !== first, first);
  await page.locator(".pane-tab", { hasText: "localhost" }).waitFor({ state: "detached" });
  assert.ok((await page.evaluate((seen) => window.calls.slice(seen), seen)).some((c) => c[0] === "close"), "the view ends with its tab");

  // The menu's ⌘N starts a new chat.
  const before = await page.evaluate(() => window.ark.summary().session.id);
  await menu("n");
  await page.waitForFunction((before) => window.ark.summary().session.id !== before, before);
}
scenarios.browserview = browserview;

// The window's view shows nothing when an address does not answer, so the panel asks first and
// says so; the bar shows a load until the page finishes.
async function browserdown(page) {
  await page.evaluate(() => {
    window.calls = [];
    window.reached = [];
    window.up = new Set(["http://localhost:3000", "http://localhost:3000/x"]);
    window.ark.browserView = {
      show: (r, url) => (window.calls.push(["show", r, url || null]), Promise.resolve({ shown: true })),
      act: (a) => (window.calls.push([a]), Promise.resolve()),
      reach: (url) => {
        window.reached.push(url);
        const answer = { reached: window.up.has(url) };
        // A slow check, overtaken by the next address.
        return url.includes(":4001") ? new Promise((ok) => setTimeout(() => ok(answer), 400)) : Promise.resolve(answer);
      },
      listen: (fn) => ((window.emitBrowser = fn), Promise.resolve()),
    };
  });
  await useNativeBrowserFixture(page);
  const bar = page.locator(".wb-bar");
  const input = page.locator(".wb-input");
  const down = page.locator(".wb-refused");
  const shows = (u) => page.waitForFunction((u) => window.calls.some((c) => c[0] === "show" && c[2] === u), u);
  const enter = async (u) => {
    await input.fill(u);
    await input.press("Enter");
  };

  await page.evaluate(() => window.ark.openPanel("browser", { locator: "localhost:3000" }));
  await shows("http://localhost:3000");
  assert.equal(await bar.getAttribute("data-loading"), "", "the bar shows the load");
  assert.equal(await page.locator(".wb-frame-host").getAttribute("data-page"), null, "the pane shows through until a page arrives");
  // The page arrives at another address (a redirect): white behind it, then done.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: true }));
  assert.equal(await page.locator(".wb-frame-host").getAttribute("data-page"), "");
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/home", loading: false }));
  await page.waitForFunction(() => !document.querySelector(".wb-bar").hasAttribute("data-loading"));

  // Nothing runs on the port: the pane says so, the view steps aside, the bar keeps the address.
  await enter("localhost:3999");
  await down.waitFor();
  assert.equal(await down.locator("p").innerText(), "localhost:3999 can't be reached");
  assert.equal(await down.locator("button").innerText(), "Try again");
  assert.equal(await input.inputValue(), "http://localhost:3999");
  assert.ok(await input.evaluate((el) => el === document.activeElement), "focus stays in the bar");
  await page.waitForFunction(() => window.calls.at(-1)?.[0] === "hide");
  assert.ok(!(await page.evaluate(() => window.calls.some((c) => c[2] === "http://localhost:3999"))));
  await page.waitForFunction(() => !document.querySelector(".wb-bar").hasAttribute("data-loading"));
  // The view's late news does not replace what was typed.
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/other", loading: false }));
  assert.equal(await input.inputValue(), "http://localhost:3999");
  const box = await down.boundingBox();
  const pane = await page.locator("#pane").boundingBox();
  assert.ok(box.x >= pane.x && box.x + box.width <= pane.x + pane.width + 1, "the message fits the pane");

  // Try again asks again; still down, then up.
  const asked = await page.evaluate(() => window.reached.length);
  await down.locator("button").click();
  await page.waitForFunction((n) => window.reached.length > n, asked);
  assert.ok(await down.isVisible());
  await page.evaluate(() => window.up.add("http://localhost:3999"));
  await down.locator("button").click();
  await shows("http://localhost:3999");
  assert.ok(await down.isHidden());

  // Reload while down tries again; Back returns to the page before.
  await page.evaluate(() => window.up.delete("http://localhost:3999"));
  await enter("localhost:3999");
  await down.waitFor();
  const before = await page.evaluate(() => window.reached.length);
  await page.locator('.wb-bar button[aria-label="Reload"]').click();
  await page.waitForFunction((n) => window.reached.length > n, before);
  await page.locator('.wb-bar button[aria-label="Back"]').click();
  assert.ok(await down.isHidden());
  assert.equal(await input.inputValue(), "http://localhost:3000/home", "the bar names the page come back to");
  await page.waitForFunction(() => window.calls.at(-1)?.[0] === "show");
  assert.ok(!(await page.evaluate(() => window.calls.some((c) => c[0] === "back"))), "Back from the message does not move the page");

  // A slow check overtaken by the next address counts for nothing.
  await enter("localhost:4001");
  await enter("localhost:3000/x");
  await shows("http://localhost:3000/x");
  await page.waitForTimeout(600);
  assert.ok(await down.isHidden(), "the overtaken check does not say can't be reached");

  // The page before finishes while the next address is checked: the bar, the line and Try again
  // stay with the address typed.
  await enter("localhost:4001");
  await page.evaluate(() => window.emitBrowser({ url: "http://localhost:3000/x", loading: false }));
  assert.equal(await bar.getAttribute("data-loading"), "", "the line goes on while the address is checked");
  await down.waitFor();
  assert.equal(await down.locator("p").innerText(), "localhost:4001 can't be reached");
  assert.equal(await input.inputValue(), "http://localhost:4001");
  await down.locator("button").focus();
  await page.evaluate(() => window.up.add("http://localhost:4001"));
  await down.locator("button").press("Enter");
  await shows("http://localhost:4001");
  assert.equal(await page.evaluate(() => window.reached.at(-1)), "http://localhost:4001", "Try again asks for the address typed");
  assert.ok(await input.evaluate((el) => el === document.activeElement), "focus goes back to the bar");

  // An address the view refuses is not asked about.
  const n = await page.evaluate(() => window.reached.length);
  await enter("file:///tmp/a.html");
  await shows("file:///tmp/a.html");
  assert.equal(await page.evaluate(() => window.reached.length), n);
}
scenarios.browserdown = browserdown;

async function destinationBrowserFixture(page, unavailable = false) {
  const fixture = { calls: [], unavailable };
  await page.route("**/rpc", async route => {
    const { method, params } = route.request().postDataJSON();
    if (method !== "resource" || params.ref?.kind !== "browser") return route.fallback();
    fixture.calls.push(params);
    if (fixture.unavailable) return route.fulfill({ json: { ok: false, error: "Chromium is not installed on this computer" } });
    const result = params.operation.kind === "browser_snapshot"
      ? { image: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=",
        media_type: "image/png", width: 1, height: 1, url: "http://localhost:3000/home", title: "Destination fixture", loading: false }
      : {};
    await route.fulfill({ json: { ok: true, result } });
  });
  const flags = await page.evaluate(() => {
    const scoped = window.ark.at(window.ark.summary().location);
    return { remoteResource: scoped.remoteResource, destinationBrowser: scoped.destinationBrowser };
  });
  assert.deepEqual(flags, { remoteResource: false, destinationBrowser: true });
  return fixture;
}

// HTTP pages keep Browser rendering on the destination even when hello names that same
// runtime. A missing Chromium stays visible and cannot fall back to the viewer's network.
async function framerefused(page) {
  const fixture = await destinationBrowserFixture(page, true);
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "http://localhost:3000/home" }));
  const refused = page.locator(".wb-refused");
  await refused.waitFor();
  assert.equal(await refused.locator("p").innerText(), "Chromium is not installed on this computer");
  fixture.unavailable = false;
  await refused.getByRole("button", { name: "Try again" }).click();
  await page.locator("img.remote-browser[src^='data:image/png']").waitFor();
  await refused.waitFor({ state: "hidden" });
  assert.equal(fixture.calls.filter(call => call.operation.kind === "browser_navigate").length, 2);
  assert.equal(await page.locator("iframe.wb-frame").count(), 0);
  assert.equal(fixture.calls.some(call => call.operation.kind === "browser_reload"), false);
}
scenarios.framerefused = framerefused;

// An app's Settings sections come first, ARK's own after; advanced ones (the app's, then
// ARK's Skills and Tool servers) under Advanced. A section added while Settings is open joins
// the list. Appearance › Dark darkens the page and is remembered. `ark.compose` fills the
// composer from any view and sends nothing.
async function settings(page) {
  // ARK's own sections register once the page is connected: wait for the Settings entry.
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.setting({ id: "keys", title: "Keys", render: (el) => el.append("the keys") }));
  await page.locator("#open-settings").click();
  assert.equal(await page.locator(".settings-title").innerText(), "Keys", "the app's section opens first");
  assert.equal(await page.locator(".settings-section").innerText(), "the keys");
  const rows = page.locator(".settings-list .nav-row:not([hidden])");
  await rows.nth(2).waitFor();
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Appearance", "Devices", "Permissions", "Advanced"]);

  await page.evaluate(() => window.ark.setting({ id: "look", title: "Look", render: (el) => el.append("the look") }));
  await page.evaluate(() => window.ark.setting({ id: "debug", title: "Debug", advanced: true, render: (el) => el.append("the debug") }));
  await page.evaluate(() => window.ark.openSettings("keys"));
  await page.locator(".settings-advanced").click();
  assert.deepEqual(await rows.allInnerTexts(), ["Keys", "Look", "Appearance", "Devices", "Permissions", "Advanced", "Debug", "Skills", "Tool servers"]);
  assert.deepEqual(await page.locator(".settings-list > *").evaluateAll((els) => els.map((e) => e.textContent).slice(5, 7)), ["Advanced", "Debug"]);

  // The test page opens light (no app default, a light system); Dark is kept across a reload.
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "light");
  await page.locator(".app").evaluate((app) => {
    app.dataset.shell = "desktop";
    app.dataset.os = "macos";
    const root = document.documentElement.style;
    root.setProperty("--appearance-light-bg", "#fafaf7");
    root.setProperty("--appearance-light-accent", "#335eb7");
  });
  await page.locator(".settings-list .nav-row", { hasText: "Appearance" }).click();
  assert.equal(await page.locator(".appearance-palette-grid").isVisible(), false);
  await page.locator(".settings-options > summary").click();
  assert.equal(await page.locator(".settings-section .appearance-choice").count(), 8);
  assert.equal(await page.locator('.appearance-settings [role="radiogroup"]').count(), 3);
  assert.equal(await page.locator(".appearance-settings :is(.appearance-mode-grid, .appearance-palette-grid) input:checked").count(), 2);
  assert.equal(await page.locator(".appearance-mode-grid .appearance-preview").count(), 3);
  assert.equal(await page.locator(".appearance-palette-grid .appearance-preview").count(), 3);
  assert.equal(await page.locator(".appearance-material-grid .appearance-preview").count(), 2);
  const brandedLight = await page.locator('.appearance-palette-grid .appearance-choice[data-palette="light"]').evaluate((choice) => ({
    background: getComputedStyle(choice.querySelector(".appearance-preview")).backgroundColor,
    accent: getComputedStyle(choice.querySelector(".appearance-preview-page i")).backgroundColor,
  }));
  assert.deepEqual(brandedLight, { background: "rgb(250, 250, 247)", accent: "rgb(51, 94, 183)" });
  await page.locator(".appearance-material-grid .appearance-choice", { hasText: "Translucent" }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.material), "translucent");
  assert.equal(await page.locator(".appearance-settings :is(.appearance-mode-grid, .appearance-palette-grid) input:checked").count(), 2);
  await page.locator(".appearance-mode-grid .appearance-choice", { hasText: "Dark" }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.colorScheme), "dark");
  assert.equal(await page.evaluate(() => document.activeElement?.value), "dark", "redrawing appearance preserves keyboard focus");
  const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  assert.equal(await bg(), "rgb(27, 27, 27)");
  await page.locator(".settings-section .appearance-choice", { hasText: "Dracula" }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dracula");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.colorScheme), "dark");
  assert.equal(await bg(), "rgb(40, 42, 54)");
  await page.locator(".appearance-mode-grid .appearance-choice", { hasText: "Light" }).click();
  await page.locator(".settings-section .appearance-choice", { hasText: "GitHub" }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.colorScheme), "light");
  await page.setViewportSize({ width: 360, height: 720 });
  const themeLayout = await page.locator(".appearance-settings").evaluate((group) => ({
    fits: group.scrollWidth <= group.clientWidth,
    targets: [...group.querySelectorAll("label")].map((label) => label.getBoundingClientRect().height),
  }));
  assert.equal(themeLayout.fits, true, "theme choices do not overflow a 360px window");
  assert.equal(themeLayout.targets.every((height) => height >= 40), true, "theme targets stay at least 40px tall");
  await page.locator('.appearance-palette-grid input[value="github"]').focus();
  await page.keyboard.press("End");
  assert.equal(await page.evaluate(() => window.arkTheme.choice()), "xcode");
  await page.setViewportSize({ width: 1100, height: 800 });
  await page.locator(".appearance-mode-grid .appearance-choice", { hasText: "Dark" }).click();
  await page.reload();
  await page.locator("#open-settings:not([hidden])").waitFor();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dracula");
  await page.evaluate(() => window.arkTheme.setMode("system"));
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "xcode");
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForFunction(() => document.documentElement.dataset.theme === "dracula");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dracula", "system mode recalls the dark palette");
  await page.emulateMedia({ colorScheme: "light" });
  await page.waitForFunction(() => document.documentElement.dataset.theme === "xcode");
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "xcode", "system mode recalls the light palette");
  await page.evaluate(() => window.ark.setting({ id: "look", title: "Look", render: (el) => el.append("the look") }));
  await page.evaluate(() => window.ark.openSettings("look"));
  await page.locator(".settings-list .nav-row", { hasText: "Look" }).click();
  assert.equal(await page.locator(".settings-title").innerText(), "Look");
  assert.equal(await page.locator(".settings-section").innerText(), "the look");

  await page.evaluate(() => window.ark.compose("Set up a reviewer"));
  assert.equal(await page.locator("#scroller").isVisible(), true, "the conversation is in view");
  assert.equal(await page.locator("#input").inputValue(), "Set up a reviewer");
  await page.waitForTimeout(200);
  assert.equal(await page.locator(".user").count(), 0, "nothing was sent");
}
scenarios.settings = settings;

scenarios.search = async (page) => {
  const composer = page.locator("#input");
  await finished(page, () => composer.fill("A searchable chat").then(() => page.keyboard.press("Enter")));
  await composer.fill("Keep this draft");
  await composer.focus();
  await page.keyboard.press("Meta+Shift+f");
  const search = page.getByRole("combobox", { name: "Search chats" });
  await search.waitFor();
  assert.equal(await search.getAttribute("aria-controls"), "chat-search-results");
  await page.locator('#chat-search-results[aria-busy="false"]').waitFor();
  const results = await page.locator("#chat-search-results").innerText();
  assert.match(results, /A searchable chat/);
  assert.match(results, /now/);
  assert.doesNotMatch(results, /ago is not defined/);
  assert.equal(await composer.inputValue(), "Keep this draft");
  await page.keyboard.press("Escape");
  assert.equal(await composer.evaluate((el) => document.activeElement === el), true, "closing search restores focus");
  assert.equal(await composer.inputValue(), "Keep this draft");
};

scenarios["startup-extension-failure"] = async (page) => {
  const failure = page.locator(".startup-failure");
  await failure.waitFor({ timeout: 7000 });
  assert.match(await failure.innerText(), /ARK could not finish opening/);
  await failure.locator("summary").click();
  assert.match(await failure.locator("pre").innerText(), /broken/);
  assert.equal(await page.locator("#app.ready").count(), 0, "the incomplete workspace never becomes ready");
  assert.equal(await page.locator("#input").count(), 0, "the incomplete composer is removed");
  const duplicate = await page.evaluate(() => {
    window.ark.extensionReady("duplicate");
    try {
      window.ark.extensionReady("duplicate");
      return "accepted";
    } catch (error) {
      return error.message;
    }
  });
  assert.match(duplicate, /twice/);
};

scenarios["late-extension"] = async (page) => {
  await page.locator("#app.ready").waitFor();
  assert.equal(await page.locator(".startup-failure").count(), 0);
};

// An app that draws the model picker: until a model is chosen the button asks for one, and
// sending opens the menu with the text kept. `ark.onSubmit` sees each message first and can
// take it; one it lets through is sent.
async function picker(page) {
  await page.evaluate(() => {
    window.picked = null;
    window.taken = [];
    window.ark.modelPicker({
      label: (model) => (window.picked ? `Picked ${window.picked}` : { text: "Choose model", missing: true }),
      render: (el, { close }) => el.append(window.ark.h("button.pick-m", { onclick: () => { window.picked = "m"; close(); window.ark.refreshModel(); } }, "m")),
    });
    window.ark.onSubmit((text) => {
      if (!text.startsWith("@")) return false;
      window.taken.push(text);
      return true;
    });
  });
  const chip = page.locator("#model-chip");
  await page.waitForFunction(() => document.getElementById("model-chip").textContent === "Choose model");
  assert.equal(await chip.getAttribute("data-missing"), "true");

  await page.locator("#input").fill("What do the notes say?");
  await page.keyboard.press("Enter");
  await page.locator(".popover .pick-m").waitFor();
  assert.equal(await page.locator("#input").inputValue(), "What do the notes say?", "the text waits");
  assert.equal(await page.locator(".user").count(), 0, "nothing was sent");

  await page.locator(".popover .pick-m").click();
  assert.equal(await chip.textContent(), "Picked m");
  assert.equal(await chip.getAttribute("data-missing"), "false");

  await page.locator("#input").fill("@writer draft it");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.taken.length === 1);
  assert.equal(await page.locator(".user").count(), 0, "the app took it");

  await finished(page, () => page.locator("#input").fill("What do the notes say?").then(() => page.keyboard.press("Enter")));
  assert.equal(await page.locator(".user").count(), 1);
}
scenarios.picker = picker;

// An app module runs while the first hello is deliberately still in flight. `ark.ready()`
// wakes it once, after the applied summary has a complete resource location, and only then may
// its project-scoped call reach the host.
scenarios.ready = async (page) => {
  await page.waitForFunction(() => window.readyProbe?.graph);
  const probe = await page.evaluate(() => window.readyProbe);
  assert.equal(probe.before, null, "the app module did run before hello completed");
  assert.equal(probe.wakes, 1, "readiness wakes once");
  assert.equal(probe.calls, 1, "one scoped call runs after readiness");
  assert.ok(probe.after.location.runtime);
  assert.ok(probe.after.location.project);
  assert.ok(probe.after.location.session);
  assert.equal(probe.graph.project, probe.after.location.project);
};

// Settings › Tool servers: an MCP server is added from a typed command, shows its tools,
// joins the app's tools, and is removed with a second click. One that cannot start says why.
async function plugins(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings("plugins"));
  await page.locator(".settings-title", { hasText: "Tool servers" }).waitFor();
  assert.equal(await page.locator(".plugin-list").innerText(), "No servers yet.");

  const input = page.locator(".plugin-input");
  await input.fill(`echo python3 "${process.env.ARK_TEST_MCP}"`);
  await page.locator(".plugin-add .btn.primary").click();
  const row = page.locator('.plugin-row[data-name="echo"]');
  await row.waitFor();
  assert.equal(await row.locator(".plugin-detail").innerText(), "2 tools: echo, stamp");
  assert.equal(await input.inputValue(), "", "the field empties once added");
  const tools = await page.evaluate(async () => (await window.ark.call("tools")).map((t) => t.name));
  assert.ok(tools.includes("echo.echo"), tools.join(","));

  await input.fill("bad /no/such/server");
  await page.locator(".plugin-add .btn.primary").click();
  await page.locator(".plugin-why", { hasText: "did not start" }).waitFor();
  assert.equal(await page.locator(".plugin-row").count(), 1, "a server that failed is not listed");

  const remove = row.locator(".plugin-remove");
  await remove.click();
  assert.equal(await remove.innerText(), "Remove?");
  await remove.click();
  await row.waitFor({ state: "detached" });
  assert.equal(await page.locator(".plugin-list").innerText(), "No servers yet.");
}
scenarios.plugins = plugins;

// Plugins is a global destination. An unavailable broker is a visible account state, never an
// empty search result, and Settings has no duplicate Plugins or Connections sections.
async function marketplace(page) {
  const destination = page.locator('.workspace-destination[data-view="marketplace"]');
  await destination.waitFor();
  assert.equal(await destination.getAttribute("aria-label"), "Plugins");
  await destination.click();
  assert.equal(await destination.getAttribute("aria-current"), "page");
  assert.equal(await page.locator('.destination[data-destination="home"]').getAttribute("aria-current"), null);
  await page.locator('.market-row[data-id="capability:mail"]').waitFor();
  assert.deepEqual(await page.locator(".market-row .market-copy strong").allInnerTexts(), ["Mail", "Calendar"]);
  const size = page.viewportSize();
  for (const [name, width] of [["wide", 1280], ["narrow", 480]]) {
    await page.setViewportSize({ width, height: 800 });
    await shot(page, `plugins-${name}`);
    const box = await page.locator(".market-search").boundingBox();
    const body = await page.locator(".marketplace").boundingBox();
    const sidebar = await page.locator(".sidebar, #sidebar").first().boundingBox().catch(() => null);
    assert.ok(box.x + box.width <= width - 8, `${name}: the search box ends inside the window`);
    assert.ok(body.x >= (sidebar ? sidebar.x + sidebar.width : 0) + 16 || width < 700, `${name}: the content keeps a gutter from the sidebar`);
  }
  await page.setViewportSize(size);
  assert.equal(await page.locator('.market-row[data-id="capability:mail"] .market-add').innerText(), "Connect");
  const search = page.locator(".market-search");
  await search.pressSequentially("Calendar");
  assert.equal(await search.inputValue(), "Calendar");
  assert.equal(await search.evaluate((input) => input === document.activeElement), true, "search keeps focus while filtering");
  assert.deepEqual(await page.locator(".market-row .market-copy strong").allInnerTexts(), ["Calendar"]);
  await search.fill("");
  const mail = page.locator('.market-row[data-id="capability:mail"]');
  await page.evaluate(() => {
    const call = window.ark.call;
    let release;
    window.__pluginConnect = { calls: 0, release: () => release?.() };
    window.ark.call = (name, params) => {
      if (name !== "capability_connect") return call(name, params);
      window.__pluginConnect.calls += 1;
      return new Promise((resolve, reject) => {
        release = () => call(name, params).then(resolve, reject);
      });
    };
    window.__restoreArkCall = () => { window.ark.call = call; };
  });
  await mail.getByRole("button", { name: "Connect Mail" }).click();
  await mail.getByRole("button", { name: "Connect Mail" }).click({ force: true });
  assert.equal(await page.evaluate(() => window.__pluginConnect.calls), 1, "one connection starts while authorization is pending");
  assert.equal(await mail.locator(".market-add").innerText(), "Connecting…");
  assert.equal(await mail.locator(".market-add").isDisabled(), true);
  await page.evaluate(() => { window.__restoreArkCall(); window.__pluginConnect.release(); });
  await page.waitForFunction(() => document.querySelector('.market-row[data-id="capability:mail"]')?.textContent.includes("Connected"), null, { timeout: 15000 });
  await mail.getByRole("button", { name: "View Mail" }).click();
  await page.evaluate(() => {
    const call = window.ark.call;
    window.ark.call = (name, params) => name === "capabilities" ? Promise.reject(new Error("offline")) : call(name, params);
    window.__restoreArkCall = () => { window.ark.call = call; };
  });
  await destination.click();
  await page.getByText("Plugins on this computer could not be loaded.").waitFor();
  assert.equal(await page.getByRole("button", { name: "Retry" }).count() > 0, true);
  await page.evaluate(() => window.__restoreArkCall());
  await page.getByRole("button", { name: "Disconnect" }).click();
  await page.getByRole("button", { name: "Back" }).click();
  await page.locator('.market-row[data-id="capability:mail"] .market-add').waitFor();
  await page.evaluate(() => window.ark.openSettings());
  assert.equal(await page.locator('.settings-list .nav-row', { hasText: /^Plugins$/ }).count(), 0);
  assert.equal(await page.locator('.settings-list .nav-row', { hasText: /^Connections$/ }).count(), 0);
}
scenarios.marketplace = marketplace;

// Built-in capabilities remain enabled even though Connections is no longer a Settings page.
async function capabilities(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings());
  assert.equal(await page.locator('.settings-list .nav-row', { hasText: /^Connections$/ }).count(), 0);
  const tools = await page.evaluate(async () => (await window.ark.call("tools")).map((t) => t.name));
  assert.ok(tools.includes("web.search"), tools.join(","));
}
scenarios.capabilities = capabilities;

// An agent asks for Mail mid-task: the card names the need and offers Connect or Not now;
// connecting signs in (the test plays the browser) and the turn goes on to the send, which
// asks as usual, then answers.
async function connectMidTask(page) {
  await page.locator("#input").fill("Email Bob that the report is ready");
  await page.keyboard.press("Enter");
  const card = page.locator(".approval").first();
  await card.waitFor();
  assert.equal(await card.locator(".approval-title").innerText(), "ARK needs this to finish:");
  assert.match(await card.locator(".approval-summary").innerText(), /^Connect Google so ARK can send email from your account/);
  const buttons = (await card.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
  assert.deepEqual(buttons, ["Connect", "Not now"]);
  await card.locator('button[data-decision="allow_once"]').click();
  const send = page.locator(".approval:not(.decided)");
  try {
    await send.waitFor({ timeout: 15000 });
  } catch (error) {
    const thread = await page.locator("#thread").innerText();
    const approvals = await page.locator(".approval").evaluateAll((rows) =>
      rows.map((row) => ({ className: row.className, text: row.innerText })),
    );
    throw new Error(`${error.message}\nthread:\n${thread}\napprovals:\n${JSON.stringify(approvals, null, 2)}`);
  }
  assert.match(await send.locator(".approval-line").innerText(), /mail\.send|Send|bob@example\.com/i);
  await send.locator('button[data-decision="allow_once"]').click();
  await page.locator(".agent", { hasText: "Sent the report to Bob." }).waitFor({ timeout: 15000 });
}
scenarios["connect-mid-task"] = connectMidTask;

// An approval offers "Always in this project". After it, the same action runs without a card,
// Settings › Permissions lists the grant, and removing it brings the card back.
async function grants(page) {
  const ask = (text) => page.locator("#input").fill(text).then(() => page.keyboard.press("Enter"));
  await finished(page, async () => {
    await ask("Save the notes");
    const card = page.locator(".approval").first();
    await card.waitFor();
    const buttons = (await card.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
    assert.deepEqual(buttons, ["Allow once", "Allow for this session", "Always in this project", "Deny"]);
    await card.locator('button[data-decision="allow_always"]').click();
  });
  const cards = await page.locator(".approval").count();
  await finished(page, () => ask("Save them again"));
  assert.equal(await page.locator(".approval").count(), cards, "the second save asked nothing");
  await page.locator(".agent", { hasText: "Saved again." }).waitFor();

  await page.evaluate(() => window.ark.openSettings("permissions"));
  const row = page.locator('.grant-row[data-class="notes.save"]');
  await row.waitFor();
  await row.locator(".plugin-remove").click();
  await row.waitFor({ state: "detached" });
  await page.evaluate(() => window.ark.openSettings("permissions"));

  await page.locator("#open-settings").waitFor();
  await page.keyboard.press("Escape");
  await page.evaluate(() => window.ark.compose(""));
  await ask("And once more");
  await page.locator(".approval:not(.decided)").waitFor({ timeout: 15000 });
  await page.locator('.approval:not(.decided) button[data-decision="allow_once"]').click();
}
scenarios.grants = grants;

// Settings › Skills lists the store's skill, and Add opens the Terminal in the project folder
// with the install command typed, unsent.
async function skills(page) {
  await page.locator("#open-settings:not([hidden])").waitFor();
  await page.evaluate(() => window.ark.openSettings("skills"));
  const row = page.locator('.skill-row[data-name="release-notes"]');
  await row.waitFor();
  assert.match(await row.innerText(), /All projects/);
  assert.match(await row.innerText(), /Write release notes/);
  await page.locator(".skill-pkg").fill("vercel-labs/agent-skills");
  await page.locator(".skill-add .btn.primary").click();
  await page.locator(".xterm-rows", { hasText: "npx skills add vercel-labs/agent-skills" }).waitFor();
}
scenarios.skills = skills;

// On Windows the same page reads the Windows way: Ctrl in every shortcut label, and Ctrl (not
// the Windows key) runs them. The system is the one the browser reports.
async function windows(page) {
  await page.locator("#input").waitFor();
  assert.equal(await page.locator("#app").getAttribute("data-os"), "windows");
  assert.equal((await page.locator("kbd").first().innerText()).trim(), "Ctrl+N");
  assert.match(await page.locator("#toggle-sidebar").getAttribute("title"), /\(Ctrl\+B\)/);
  assert.equal(await page.locator("body").evaluate((b) => b.innerHTML.includes("⌘")), false, "no ⌘ left");
  await page.keyboard.press("Control+k");
  await page.locator(".palette-input").waitFor();
}
scenarios.windows = windows;

const NARRATION = "Reading the notes slowly, one line at a time, so that a window can reload in between. ".repeat(3).trim();

// Scripts that run before the page, for scenarios that replace a browser API.
const inits = {
  dictation: () => {
    const track = { stop() { window.__trackStopped = true; } };
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia: async () => ({ getTracks: () => [track] }) }, configurable: true });
    window.AudioContext = class {
      constructor() { this.sampleRate = 48000; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() {
        const node = { connect() {}, disconnect() {}, onaudioprocess: null };
        window.__dictationNode = node;
        return node;
      }
      close() { return Promise.resolve(); }
      get destination() { return {}; }
    };
    window.__TestAudio = window.AudioContext;
    window.__TestMedia = navigator.mediaDevices;
  },
  // The window on a runtime it visits (ADR 0029): Tauri defines `__TAURI__` there too.
  visit: () => {
    window.__TAURI__ = {};
  },
  // The browser says it runs on Windows.
  windows: () => {
    Object.defineProperty(navigator, "platform", { get: () => "Win32" });
    Object.defineProperty(navigator, "userAgentData", { get: () => ({ platform: "Windows" }) });
  },
  // An EventSource that always fails, and timers that fire at once (recording how long the
  // page asked to wait) so ten backoffs take milliseconds.
  backoff: () => {
    window.__es = { opened: 0, delays: [] };
    const realTimeout = window.setTimeout.bind(window);
    window.setTimeout = function arkTestTimeout(fn, ms, ...a) {
      // Only the transport's reconnect waits are compressed and recorded. The caller is the
      // frame after this one (V8 starts its stack with an "Error" line, WebKit does not).
      const frames = (new Error().stack || "").split("\n");
      const caller = frames[frames.findIndex((f) => f.includes("arkTestTimeout")) + 1] || "";
      if (ms >= 500 && caller.includes("transport.js")) {
        window.__es.delays.push(ms);
        return realTimeout(fn, 1, ...a);
      }
      return realTimeout(fn, ms, ...a);
    };
    window.EventSource = class {
      constructor() {
        window.__es.opened += 1;
        realTimeout(() => this.onerror && this.onerror(new Event("error")), 1);
      }
      close() {}
    };
  },
};

let dictationFixture;
scenarios.dictation = async (page) => {
  const fixtureWait = async (predicate) => {
    for (let i = 0; i < 200 && !predicate(); i++) await page.waitForTimeout(10);
    assert.ok(predicate(), "dictation transport settled");
  };
  const chooseServer = async () => {
    await page.locator("#place-chip").click();
    await page.locator(".places-pop.open").getByText("vps", { exact: true }).click();
    await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "ready");
  };
  const sample = () => page.evaluate(() => window.__dictationNode.onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array(9600).fill(0.15) } }));
  const input = page.locator("#input");
  const mic = page.locator("#dictate");
  await mic.waitFor();
  assert.equal(await mic.isDisabled(), true, "dictation waits for runtime availability");
  dictationFixture.releaseStatus();
  await page.waitForFunction(() => !document.querySelector("#dictate")?.disabled);
  assert.equal(await mic.getAttribute("aria-label"), "Start dictation");
  await input.fill("Hola 👋 mundo");
  await input.evaluate((el) => { el.focus(); el.setSelectionRange(7, 7); el.dispatchEvent(new Event("select")); });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => {
    const samples = new Float32Array(9600).fill(0.15);
    window.__dictationNode.onaudioprocess({ inputBuffer: { getChannelData: () => samples } });
  });
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Audio could not reach" }).waitFor();
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => {
    const samples = new Float32Array(9600).fill(0.15);
    window.__dictationNode.onaudioprocess({ inputBuffer: { getChannelData: () => samples } });
  });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictation-status")?.textContent === "Dictation added");
  assert.equal(await input.inputValue(), "Hola 👋 Texto dictado. mundo", "UTF-16 caret and every draft character are preserved");
  assert.equal(await page.evaluate(() => window.__trackStopped), true, "the microphone is released");
  assert.equal(await input.evaluate((el) => el === document.activeElement), true, "completion does not steal focus");

  await page.evaluate(() => { window.__trackStopped = false; });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => dispatchEvent(new Event("pagehide")));
  assert.equal(await page.evaluate(() => window.__trackStopped), true, "hiding the page releases the microphone");

  await page.evaluate(() => {
    window.__trackStopped = false;
    const track = { stop() { window.__trackStopped = true; } };
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia: async () => ({ getTracks: () => [track] }) }, configurable: true });
    window.AudioContext = class { constructor() { throw new Error("audio setup failed"); } };
  });
  await mic.click();
  await page.locator("#dictation-status", { hasText: "could not be transcribed" }).waitFor();
  assert.equal(await page.evaluate(() => window.__trackStopped), true, "audio setup failure releases the microphone");

  await page.evaluate(() => Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia: async () => { throw new DOMException("denied", "NotAllowedError"); } }, configurable: true }));
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Microphone access is off" }).waitFor();
  assert.equal(await input.inputValue(), "Hola 👋 Texto dictado. mundo", "a permission error preserves the draft");

  await page.evaluate(() => {
    window.AudioContext = window.__TestAudio;
    Object.defineProperty(navigator, "mediaDevices", { value: window.__TestMedia, configurable: true });
    window.__trackStopped = false;
  });
  dictationFixture.failBegin = true;
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Audio could not reach" }).waitFor();
  assert.equal(await page.evaluate(() => window.__trackStopped), true, "Begin failure stops capture without waiting for Stop");
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => window.__dictationNode.onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array(9600).fill(0.15) } }));
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Dictation added" }).waitFor();
  await shot(page, "dictation-complete");
  await page.setViewportSize({ width: 390, height: 800 });
  await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce" });
  await page.evaluate(() => window.arkTheme.set("dark"));
  if (await page.locator("#app").getAttribute("data-drawer") !== "open") await page.locator("#reveal-sidebar").click();
  await page.locator("#toggle-sidebar").click();
  await page.waitForFunction(() => document.querySelector(".sidebar").getBoundingClientRect().right <= 0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, "dictation keeps the narrow composer within the window");
  await shot(page, "dictation-narrow-dark");
  await page.setViewportSize({ width: 1100, height: 800 });
  await page.emulateMedia({ colorScheme: "light", reducedMotion: "no-preference" });
  await page.evaluate(() => window.arkTheme.set("light"));

  // Stop before the first recording poll. First-use download progress must still appear,
  // then change to Transcribing when preparation finishes.
  dictationFixture.downloading = true;
  dictationFixture.finishWait = new Promise((resolve) => { dictationFixture.releaseFinish = resolve; });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await sample();
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Downloading 148 MB speech model" }).waitFor();
  dictationFixture.downloading = false;
  await page.locator("#dictation-status", { hasText: /^Transcribing$/ }).waitFor();
  dictationFixture.releaseFinish();
  dictationFixture.finishWait = null;
  await page.locator("#dictation-status", { hasText: "Dictation added" }).waitFor();

  // Cancel while Begin is delayed, switch chats, then release the late upload. Cleanup must
  // still name the original computer and its result must not reach the new draft.
  dictationFixture.localUnavailable = true;
  await chooseServer();
  dictationFixture.beginWait = new Promise((resolve) => { dictationFixture.releaseBegin = resolve; });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  const beforeCancel = dictationFixture.seen.filter((r) => r.method === "dictation_cancel").length;
  await fixtureWait(() => dictationFixture.seen.at(-1)?.method === "dictation_begin");
  const firstSession = await page.evaluate(() => window.ark.summary().session.id);
  await page.evaluate(() => window.ark.newSession());
  await page.waitForFunction((id) => window.ark.summary()?.session?.id !== id, firstSession);
  const switchedDraft = await input.inputValue();
  dictationFixture.releaseBegin();
  dictationFixture.beginWait = null;
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "unavailable");
  await fixtureWait(() => dictationFixture.seen.filter((r) => r.method === "dictation_cancel").length > beforeCancel && dictationFixture.active.size === 0);
  assert.equal(await input.inputValue(), switchedDraft, "a late upload cannot alter the next chat's draft");
  assert.equal(dictationFixture.active.size, 0, "late Begin is cancelled");
  assert.equal(dictationFixture.seen.filter((r) => r.method === "dictation_cancel").at(-1).place, "vps", "late cleanup stays on the original computer");
  await mic.click();
  assert.equal(await mic.getAttribute("data-state"), "unavailable", "a missing local provider never uses the remote provider implicitly");
  await chooseServer();
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await sample();
  await mic.click();
  await page.locator("#dictation-status", { hasText: "Dictation added" }).waitFor();
  assert.equal(dictationFixture.seen.filter((r) => r.method === "dictation_finish").at(-1).place, "vps", "pending-place dictation runs there before the first message");

  // A result arriving after a chat change is discarded, even when inference cannot be stopped.
  dictationFixture.finishWait = new Promise((resolve) => { dictationFixture.releaseFinish = resolve; });
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await sample();
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "transcribing");
  await fixtureWait(() => dictationFixture.seen.filter((r) => r.method === "dictation_finish").length >= 3 && dictationFixture.active.size === 0);
  const secondSession = await page.evaluate(() => window.ark.summary().session.id);
  await page.evaluate(() => window.ark.newSession());
  await page.waitForFunction((id) => window.ark.summary()?.session?.id !== id, secondSession);
  const laterDraft = await input.inputValue();
  dictationFixture.releaseFinish();
  dictationFixture.finishWait = null;
  await page.waitForTimeout(50);
  assert.equal(await input.inputValue(), laterDraft, "late inference never inserts into the next chat");
  dictationFixture.localUnavailable = false;
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => dispatchEvent(new Event("pagehide")));

  // Two real split composers own distinct microphone controls. A pending permission prompt
  // reserves capture; disposing the second pane releases it without removing the first mic.
  await page.locator("#chat-split").click();
  await page.waitForFunction(() => document.getElementById("app").dataset.split === "true");
  const secondary = page.locator(".chat-surface.secondary");
  const otherMic = secondary.locator(".dictate");
  await otherMic.waitFor();
  await page.waitForFunction(() => !document.querySelector(".secondary .dictate")?.disabled);
  assert.equal(await page.locator(".dictate").count(), 2);
  const statusIds = await page.locator(".dictate").evaluateAll((buttons) => buttons.map((b) => b.getAttribute("aria-describedby")));
  assert.equal(new Set(statusIds).size, 2, "each microphone describes its own status");
  await page.evaluate(() => {
    window.__permissionRequests = 0;
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia: () => {
      window.__permissionRequests++;
      return new Promise((resolve) => { window.__allowRecording = () => resolve({ getTracks: () => [{ stop() { window.__lateTrackStopped = true; } }] }); });
    } }, configurable: true });
  });
  await otherMic.click();
  await mic.click();
  assert.equal(await page.evaluate(() => window.__permissionRequests), 1, "one permission prompt across split composers");
  assert.match(await page.locator("#dictation-status").innerText(), /Another chat is listening/);
  await shot(page, "dictation-split");
  // Focus the primary pane before closing the secondary one.
  await input.click();
  await page.locator("#chat-split").click();
  await page.evaluate(() => window.__allowRecording());
  await page.waitForFunction(() => window.__lateTrackStopped === true);
  assert.equal(await page.locator("#dictate").count(), 1, "closing a split preserves the primary microphone");
  assert.equal(await secondary.count(), 0);
  await page.evaluate(() => Object.defineProperty(navigator, "mediaDevices", { value: window.__TestMedia, configurable: true }));
  await mic.click();
  await page.waitForFunction(() => document.querySelector("#dictate")?.dataset.state === "recording");
  await page.evaluate(() => dispatchEvent(new Event("pagehide")));
};

// A chat keeps working when the person opens another: its question waits in the tray with the
// chat's name, the sidebar marks it, the new chat runs its own turn, and the first one's answer
// is there when the person goes back.
async function background(page) {
  await page.locator("#input").fill("Save the notes for later, please");
  await page.keyboard.press("Enter");
  const question = page.locator("#thread .approval");
  await question.waitFor();
  const first = await page.evaluate(() => window.ark.summary().session.id);

  await page.locator("#new-session").click();
  await page.waitForFunction((id) => window.ark.summary().session.id !== id, first);
  const tray = page.locator("#asks .approval");
  await tray.waitFor();
  assert.match(await tray.innerText(), /^In /, "the tray says which chat asks");
  assert.equal(await page.locator("#thread .approval").count(), 0);
  const firstRow = page.locator(`.session[data-id="${first}"]`);
  await firstRow.locator(".session-attention").waitFor();
  assert.equal(await firstRow.locator(".session-attention").innerText(), "Needs you");
  assert.equal(await firstRow.locator(".session-working").count(), 0);
  assert.equal(await page.locator("#app").getAttribute("data-running"), "false", "the new chat is free");

  await finished(page, () => page.locator("#input").fill("And a second question?").then(() => page.keyboard.press("Enter")));
  assert.deepEqual(await shape(page), ["answer: Second answer."]);

  await tray.locator("button", { hasText: "Allow once" }).click();
  await page.locator(`.session[data-id="${first}"] .session-time`).waitFor();
  assert.equal(await page.locator("#asks .approval").count(), 0);
  assert.deepEqual(await shape(page), ["answer: Second answer."], "the other chat's answer stays out");

  await page.locator(`.session[data-id="${first}"]`).click();
  await page.waitForFunction((id) => window.ark.summary().session.id === id, first);
  await page.locator(".turn.stored .answer", { hasText: "Saved the notes." }).waitFor();
}
scenarios.background = background;

// An agent using the computer shows who, and Stop takes it back: the click it then asks for
// fails, and the pill goes away.
async function computer(page) {
  const pill = page.locator("#computer-use");
  assert.equal(await pill.isVisible(), false, "nothing shows while nobody uses it");
  await page.locator("#input").fill("Save my note in Notes, please");
  await page.keyboard.press("Enter");
  const look = page.locator("#thread .approval:not(.decided)");
  await look.waitFor();
  assert.match(await look.innerText(), /Looking at Notes/);
  await look.locator("button", { hasText: "Allow once" }).click();
  const click = page.locator("#thread .approval:not(.decided)", { hasText: "Clicking “Save” in Notes" });
  await click.waitFor();
  // Using the computer is asked each time: no answer covers later calls.
  assert.deepEqual(await click.locator(".approval-actions button").allInnerTexts().then((t) => t.map((x) => x.split("\n")[0].trim())), ["Allow once", "Deny"]);
  await pill.waitFor();
  assert.match(await pill.innerText(), /ARK is using your computer/);
  await pill.locator("button", { hasText: "Stop" }).click();
  await pill.waitFor({ state: "hidden" });
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await click.locator("button", { hasText: "Allow once" }).click();
  await done;
  await page.locator(".answer", { hasText: "took the computer back" }).waitFor();
  assert.equal(await pill.isVisible(), false);
}
scenarios.computer = computer;

// ADR 0042: the picture the agent took shows under its step; a click in it is asked with the
// picture and a ring on the spot, so the person judges the spot itself.
async function computerPicture(page) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.locator("#input").fill("Press Save in Notes");
  await page.keyboard.press("Enter");
  const see = page.locator("#thread .approval:not(.decided)");
  await see.waitFor();
  assert.match(await see.innerText(), /Taking a picture of Notes's window/);
  await see.locator("button", { hasText: "Allow once" }).click();
  const point = page.locator("#thread .approval:not(.decided)", { hasText: "Clicking the Save button in Notes" });
  await point.waitFor();
  const shot = point.locator(".approval-shot");
  await shot.waitFor();
  assert.match(await shot.locator("img").getAttribute("src"), /^data:image\/png;base64,/);
  // The fake picture is 400 by 300: (100, 150) is a quarter across and half down.
  const ring = await shot.locator(".approval-spot").evaluate((r) => [r.style.left, r.style.top]);
  assert.deepEqual(ring, ["25%", "50%"]);
  assert.equal(await point.locator(".approval-class, .approval-summary").count(), 0, "no caption");
  await point.locator("button", { hasText: "Allow once" }).click();
  await done;
  // What the agent saw, under its step.
  await page.locator(".act[data-action='computer.see'] .act-shot img").waitFor({ state: "attached" });
}
scenarios["computer-picture"] = computerPicture;

// The window on a runtime it visits (ADR 0029): the page talks HTTP but leaves room for the
// window's controls, and `ark.openLocal()` loads the address the window catches to go back to
// its own page. An app's row sits above the projects.
async function visit(page) {
  assert.equal(await page.locator("#app").getAttribute("data-shell"), "desktop");
  assert.equal(await page.evaluate(() => typeof window.__TAURI__.core), "undefined");
  assert.ok(await page.evaluate(() => window.ark.call("hello")), "the page talks to its runtime over HTTP");
  const fixture = await destinationBrowserFixture(page);
  const location = await page.evaluate(() => window.ark.summary().location);
  await page.evaluate(() => window.ark.openPanel("browser", { locator: "http://localhost:3000/home" }));
  await page.locator("img.remote-browser[src^='data:image/png']").waitFor();
  assert.equal(await page.locator("iframe.wb-frame").count(), 0);
  const navigate = fixture.calls.find(call => call.operation.kind === "browser_navigate");
  assert.deepEqual(navigate.ref.location, location);
  assert.equal(navigate.operation.input.url, "http://localhost:3000/home");
  await page.evaluate(() => window.ark.sidebar({ id: "place", render: (el) => el.append("This Mac · Cloud") }));
  const row = page.locator("#sidebar-ext .sidebar-item[data-id=place]");
  await row.waitFor();
  assert.equal(await row.innerText(), "This Mac · Cloud");
  // Above the projects.
  const [r, p] = await Promise.all([row.boundingBox(), page.locator("#projects").boundingBox()]);
  assert.ok(r.y < p.y, "the row is above the projects");
  let asked = null;
  await page.route("https://ark-local.invalid/**", (route) => {
    asked = route.request().url();
    route.abort();
  });
  await page.evaluate(() => window.ark.openLocal());
  for (let i = 0; !asked; i++) {
    assert.ok(i < 50, "ark.openLocal() did not ask for the window's own page");
    await page.waitForTimeout(50);
  }
  assert.equal(asked, "https://ark-local.invalid/");
}
scenarios.visit = visit;

// An app's action on a project: an item in that project's menu, given the project.
async function projectAction(page) {
  await page.evaluate(() => {
    window.ark.action({
      id: "copy", label: "Copy elsewhere", where: "project",
      when: ({ project }) => project.current,
      run: ({ project }) => { window.copied = project.name; },
    });
  });
  const current = page.locator(".project-row[aria-current=true]").first();
  await current.waitFor();
  const more = current.locator(".project-more");
  await more.click();
  const item = page.locator(".popover .menu-item", { hasText: "Copy elsewhere" });
  await item.waitFor();
  await item.click();
  const name = await current.locator(".project-name").innerText();
  assert.equal(await page.evaluate(() => window.copied), name);
  assert.equal(await page.locator(".popover[open]").count(), 0, "the menu closes");
}
scenarios["project-action"] = projectAction;

// A project row exposes a direct new-chat action beside its options. It opens a different
// project before creating the chat, so the new chat belongs to the project the person chose.
async function projectNewChat(page) {
  const before = await page.evaluate(() => window.ark.summary());
  await page.evaluate(() => window.ark.call("add_project", { name: "Other project" }));
  await page.evaluate((project) => window.ark.call("open_project", { id: project }), before.project.id);
  await page.waitForFunction((project) => window.ark.summary().project.id === project, before.project.id);
  const other = page.locator(".project-row", { hasText: "Other project" });
  await other.waitFor();
  const action = other.locator(".project-new-chat");
  await action.hover();
  await action.click();
  await page.waitForFunction((session) => window.ark.summary().session.id !== session, before.session.id);
  assert.notEqual(await page.evaluate(() => window.ark.summary().project.id), before.project.id);
  assert.equal(await page.evaluate(() => window.ark.summary().project.name), "Other project");
}
scenarios["project-new-chat"] = projectNewChat;

// Project expansion is independent. Moving to another project keeps every project the person
// already unfolded open instead of imposing accordion behavior.
async function expandedProjects(page) {
  const original = await page.evaluate(() => window.ark.summary().project);
  await page.evaluate(() => window.ark.call("add_project", { name: "Second project" }));
  await page.evaluate(() => window.ark.call("add_project", { name: "Third project" }));
  await page.evaluate((id) => window.ark.call("open_project", { id }), original.id);
  await page.waitForFunction((id) => window.ark.summary().project.id === id, original.id);

  const second = page.locator(".project-row", { hasText: "Second project" });
  const third = page.locator(".project-row", { hasText: "Third project" });
  await second.waitFor();
  if (await second.getAttribute("aria-expanded") !== "true") await second.locator(".project-chev").click();
  await page.waitForFunction(() => [...document.querySelectorAll(".project-row")]
    .some((row) => row.textContent.includes("Second project") && row.getAttribute("aria-expanded") === "true"));

  await third.click();
  await page.waitForFunction(() => window.ark.summary().project.name === "Third project");
  await page.waitForFunction(() => [...document.querySelectorAll('.project-row')]
    .some(row => row.textContent.includes('Third project') && row.getAttribute('aria-expanded') === 'true'));
  for (const name of [original.name, "Second project", "Third project"]) {
    assert.equal(await page.locator(".project-row", { hasText: name }).getAttribute("aria-expanded"), "true", `${name} stays expanded`);
  }
}
scenarios["expanded-projects"] = expandedProjects;

// Removing a project from the sidebar changes only a local presentation preference. Its current
// chat keeps running, the host still lists the project, the preference survives reload, and the
// Add project menu provides the recovery path.
async function hiddenProject(page) {
  const before = await page.evaluate(() => ({
    project: window.ark.summary().project,
    session: window.ark.summary().session.id,
  }));
  const row = page.locator(".project-row[aria-current='true']");
  await row.locator(".project-more").click();
  await page.getByRole("button", { name: "Remove from sidebar", exact: true }).click();
  await page.waitForFunction((id) => ![...document.querySelectorAll(".project")]
    .some((project) => project.dataset.id === id), before.project.id);
  assert.equal(await page.evaluate(() => window.ark.summary().session.id), before.session);
  assert.equal(await page.evaluate(() => window.ark.summary().project.id), before.project.id);
  assert.ok(await page.evaluate((id) => window.ark.call("projects").then((projects) => projects.some((project) => project.id === id)), before.project.id));

  await page.reload();
  await page.locator("#input").waitFor();
  assert.equal(await page.locator(`.project[data-id="${before.project.id}"]`).count(), 0, "hidden project stays hidden after reload");
  await page.locator("#add-project").click();
  const name = page.getByRole("textbox", { name: "New project name" });
  await name.fill(before.project.name);
  await name.press("Enter");
  await page.locator(`.project[data-id="${before.project.id}"]`).waitFor();

  await page.locator(`.project[data-id="${before.project.id}"] .project-more`).click();
  await page.getByRole("button", { name: "Remove from sidebar", exact: true }).click();
  await page.locator(`.project[data-id="${before.project.id}"]`).waitFor({ state: "detached" });
  await page.locator("#add-project").click();
  await page.getByRole("button", { name: `Restore ${before.project.name}`, exact: true }).click();
  await page.locator(`.project[data-id="${before.project.id}"]`).waitFor();
}
scenarios["hidden-project"] = hiddenProject;

// Multi-chat controls use the same quiet tab grammar as artifacts. Split view has one clear
// scope, so project views disappear while it is open and return on every exit path.
async function chatWorkspacePolish(page) {
  const initial = await page.evaluate(() => window.ark.summary().session.id);
  // A person upgrading from the early spatial-tabs experiment gets one complete conversation,
  // not a stale empty pane or an unexplained extra tab.
  await page.evaluate((id) => {
    localStorage.setItem("ark-chat-layout", JSON.stringify({
      kind: "split", direction: "column", ratio: 0.5,
      first: { kind: "chat", session: id }, second: { kind: "chat", session: "missing-chat" },
    }));
    localStorage.setItem("ark-open-chats", JSON.stringify([id, "missing-chat"]));
  }, initial);
  await page.reload();
  await page.locator("#input").waitFor();
  const recovery = await page.evaluate(() => {
    const workspace = document.querySelector("#chat-workspace").getBoundingClientRect();
    const surface = document.querySelector(".chat-surface").getBoundingClientRect();
    return {
      leaves: document.querySelectorAll(".chat-layout-leaf").length,
      chatTabsHidden: document.querySelector("#chat-tabs").hidden,
      heightDelta: Math.round(Math.abs(workspace.height - surface.height)),
    };
  });
  assert.deepEqual(recovery, { leaves: 1, chatTabsHidden: true, heightDelta: 0 }, JSON.stringify(recovery));

  const first = await page.evaluate(() => window.ark.summary().session.id);
  await page.locator("#new-session").click();
  await page.waitForFunction((id) => window.ark.summary().session.id !== id, first);
  const primary = await page.evaluate(() => window.ark.summary().session.id);
  await page.waitForFunction(() => document.querySelectorAll(".chat-tab").length === 2);

  await page.evaluate(() => {
    window.ark.view({
      id: "split-view-proof", title: "Proof", nav: true,
      render(el) { el.textContent = "Proof"; },
    });
    window.ark.panel({
      id: "artifact-tab-proof", title: "Artifact proof", icon: "file",
      render(el) { el.textContent = "Artifact"; },
    });
    window.ark.openPanel("artifact-tab-proof");
  });
  await page.waitForFunction(() =>
    document.querySelector("#app").dataset.pane === "open"
    && document.querySelector(".pane").getBoundingClientRect().width >= 320);
  const paneGeometry = await page.evaluate(() => {
    const main = document.querySelector(".topbar").getBoundingClientRect();
    const pane = document.querySelector(".pane").getBoundingClientRect();
    const paneHead = document.querySelector(".pane-head").getBoundingClientRect();
    const tab = document.querySelector('.pane-tab-item[data-selected="true"]').getBoundingClientRect();
    return {
      paneWidth: Math.round(pane.width),
      headerCenterDelta: Math.round(Math.abs((main.top + main.height / 2) - (paneHead.top + paneHead.height / 2))),
      tabCenterDelta: Math.round(Math.abs((paneHead.top + paneHead.height / 2) - (tab.top + tab.height / 2))),
    };
  });
  assert.ok(paneGeometry.paneWidth >= 320, JSON.stringify(paneGeometry));
  assert.deepEqual({ headerCenterDelta: paneGeometry.headerCenterDelta, tabCenterDelta: paneGeometry.tabCenterDelta }, { headerCenterDelta: 0, tabCenterDelta: 0 });
  const shell = await page.evaluate(() => {
    const app = document.querySelector("#app");
    const topbar = document.querySelector(".topbar");
    const chats = document.querySelector("#chat-tabs");
    const views = document.querySelector("#view-tabs");
    const proof = document.querySelector('.workspace-destination[data-view="split-view-proof"]');
    const account = document.querySelector("#open-settings");
    const browserHeight = Math.round(topbar.getBoundingClientRect().height);
    app.dataset.shell = "desktop";
    const nativeHeight = Math.round(topbar.getBoundingClientRect().height);
    app.dataset.shell = "browser";
    return {
      chatsParent: chats.parentElement.className,
      chatLabel: chats.getAttribute("aria-label"),
      browserHeight,
      nativeHeight,
      titleDisplay: getComputedStyle(document.querySelector("#session-title")).display,
      titleBarViewsHidden: views.hidden,
      proofParent: proof.parentElement.id,
      proofLabel: proof.getAttribute("aria-label"),
      accountParent: account.parentElement.id,
      accountLabel: account.getAttribute("aria-label"),
      railMaterial: getComputedStyle(document.querySelector(".destinations")).backgroundColor,
      sidebarMaterial: getComputedStyle(document.querySelector(".sidebar")).backgroundColor,
    };
  });
  assert.deepEqual(shell, {
    chatsParent: "topbar", chatLabel: "Open chats", browserHeight: 44, nativeHeight: 44,
    titleDisplay: "none", titleBarViewsHidden: true, proofParent: "workspace-destinations",
    proofLabel: "Proof", accountParent: "destinations", accountLabel: "Account and settings",
    railMaterial: "rgb(241, 241, 239)", sidebarMaterial: "rgb(247, 247, 246)",
  });
  await shot(page, "chat-workspace-shell");
  const grammar = await page.evaluate(() => {
    const pick = (selector) => {
      const s = getComputedStyle(document.querySelector(selector));
      return {
        height: s.height,
        radius: s.borderRadius,
        fontSize: s.fontSize,
        fontWeight: s.fontWeight,
        selected: s.backgroundColor,
      };
    };
    return { chat: pick('.chat-tab-item[data-selected="true"]'), artifact: pick('.pane-tab-item[data-selected="true"]') };
  });
  assert.deepEqual(grammar.chat, grammar.artifact, "chat and artifact tabs share one visual grammar");
  assert.equal(grammar.chat.height, "30px", "workspace tabs keep the compact Terminal-tab scale");

  const artifactItem = page.locator('.pane-tab-item[data-selected="true"]');
  const artifactTab = artifactItem.getByRole("tab");
  const artifactClose = artifactItem.getByRole("button", { name: /^Close / });
  assert.equal(await artifactTab.count(), 1, "panel activation is its own accessible control");
  assert.equal(await artifactClose.count(), 1, "panel close is its own accessible control");
  const artifactCloseBox = await artifactClose.boundingBox();
  assert.ok(artifactCloseBox.width >= 30 && artifactCloseBox.height >= 30, JSON.stringify(artifactCloseBox));

  const firstItem = page.locator(`.chat-tab-item[data-session="${first}"]`);
  const firstTab = firstItem.getByRole("tab");
  const firstClose = firstItem.getByRole("button", { name: /^Close / });
  assert.equal(await firstTab.count(), 1, "chat activation is its own accessible control");
  assert.equal(await firstClose.count(), 1, "chat close is its own accessible control");
  const closeBox = await firstClose.boundingBox();
  assert.ok(closeBox.width >= 30 && closeBox.height >= 30, JSON.stringify(closeBox));
  await firstTab.focus();
  await page.keyboard.press("Tab");
  assert.equal(await firstClose.evaluate((el) => el === document.activeElement), true, "Tab reaches the matching close action");

  const selectedTab = page.locator('.chat-tab[aria-selected="true"]');
  await selectedTab.dblclick();
  const renameInput = page.getByRole("textbox", { name: "Chat title" });
  await renameInput.fill("Discarded title");
  await renameInput.press("Escape");
  assert.equal(await selectedTab.innerText(), "Untitled chat", "Escape keeps the saved title");
  await page.waitForFunction(() => document.querySelector('.chat-tab[aria-selected="true"]') === document.activeElement);
  assert.equal(await selectedTab.evaluate((el) => el === document.activeElement), true, "Escape returns focus to the active chat");

  await selectedTab.dblclick();
  await renameInput.fill("Blurred chat");
  await renameInput.evaluate((el) => el.blur());
  await page.waitForFunction(() => document.querySelector('.chat-tab[aria-selected="true"]')?.textContent.includes("Blurred chat"));
  await page.waitForFunction(() => document.querySelector('.chat-tab[aria-selected="true"]') === document.activeElement);
  assert.equal(await selectedTab.evaluate((el) => el === document.activeElement), true, "blur saves and returns focus");

  await selectedTab.dblclick();
  await renameInput.fill("Aligned chat");
  await renameInput.press("Enter");
  await page.waitForFunction(() => document.querySelector('.chat-tab[aria-selected="true"]')?.textContent.includes("Aligned chat"));
  await page.waitForFunction(() => document.querySelector('.chat-tab[aria-selected="true"]') === document.activeElement);
  assert.equal(await page.locator('.chat-tab[aria-selected="true"]').evaluate((el) => el === document.activeElement), true, "rename returns focus to the active chat");

  const centers = await page.evaluate(() => {
    const center = (selector) => {
      const r = document.querySelector(selector).getBoundingClientRect();
      return r.top + r.height / 2;
    };
    return [center("#model-chip"), center("#mode-chip"), center("#send")];
  });
  assert.ok(Math.max(...centers) - Math.min(...centers) <= 1, JSON.stringify(centers));
  const composerOrder = await page.evaluate(() => {
    const left = (selector) => Math.round(document.querySelector(selector).getBoundingClientRect().left);
    return { attach: left("#attach"), mode: left("#mode-chip"), model: left("#model-chip"), send: left("#send") };
  });
  assert.ok(composerOrder.attach < composerOrder.model && composerOrder.model < composerOrder.mode
    && composerOrder.mode < composerOrder.send, JSON.stringify(composerOrder));

  const newChat = page.locator(".project-row[aria-current='true'] .project-new-chat");
  await newChat.hover();
  await page.waitForTimeout(140);
  const hover = await newChat.evaluate((el) => ({
    button: getComputedStyle(el).backgroundColor,
    inset: getComputedStyle(el, "::after").inset,
    opacity: getComputedStyle(el, "::after").opacity,
  }));
  assert.equal(hover.button, "rgba(0, 0, 0, 0)", "the 40px hit target is not a 40px hover block");
  assert.equal(hover.inset, "6px");
  assert.equal(hover.opacity, "1");

  await page.locator("#close-pane").click();
  const proofDestination = page.locator('.workspace-destination[data-view="split-view-proof"]');
  assert.equal(await proofDestination.isVisible(), true);
  await page.locator("#chat-split").click();
  await page.waitForFunction(() => document.getElementById("app").dataset.split === "true");
  assert.equal(await proofDestination.isVisible(), true, "the fixed rail remains the way to the project view during split view");
  assert.equal(await page.locator('.chat-tab[aria-selected="true"]').count(), 1, "one chat owns composer focus");
  assert.equal(await page.locator(".secondary-head").count(), 0, "the chat tab is the only title for the secondary chat");

  const primaryTab = page.locator(`.chat-tab-item[data-session="${primary}"] .chat-tab`);
  const secondaryTab = page.locator(`.chat-tab-item[data-session="${first}"] .chat-tab`);
  const orderBeforeSelection = await page.locator(".chat-tab-item").evaluateAll((tabs) => tabs.map((tab) => tab.dataset.session));
  await primaryTab.click();
  assert.equal(await primaryTab.getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".chat-surface.primary").getAttribute("data-active"), "true");
  await secondaryTab.click();
  assert.equal(await secondaryTab.getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".chat-surface.secondary").getAttribute("data-active"), "true");
  assert.deepEqual(await page.locator(".chat-tab-item").evaluateAll((tabs) => tabs.map((tab) => tab.dataset.session)),
    orderBeforeSelection, "selecting a chat never reorders its tab");
  await page.evaluate(() => {
    window.__workspaceFetch = window.fetch;
    window.fetch = async (input, init) => {
      const call = typeof init?.body === "string" ? JSON.parse(init.body) : null;
      if (String(input).endsWith("/rpc") && call?.method === "chat_rename") {
        return new Response(JSON.stringify({ ok: false, error: "A working chat cannot be renamed" }), {
          status: 200, headers: { "content-type": "application/json" },
        });
      }
      return window.__workspaceFetch(input, init);
    };
  });
  await secondaryTab.dblclick();
  await page.getByRole("textbox", { name: "Chat title" }).fill("Unsaved split title");
  await page.getByRole("textbox", { name: "Chat title" }).press("Enter");
  await page.waitForFunction(() => !document.querySelector('.chat-tab-item[data-selected="true"] input'));
  assert.equal(await secondaryTab.innerText(), "Untitled chat", "a host-rejected split rename never changes the displayed title");
  await page.evaluate(() => { window.fetch = window.__workspaceFetch; delete window.__workspaceFetch; });
  await shot(page, "chat-workspace-polish");

  await page.setViewportSize({ width: 800, height: 700 });
  assert.equal(await page.locator(".chat-surface:not([hidden])").count(), 2);
  assert.equal(await page.locator(".chat-surface:not([hidden])").evaluateAll((els) => els.filter((el) => getComputedStyle(el).display !== "none").length), 1);
  await primaryTab.click();
  assert.equal(await primaryTab.getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".chat-surface.primary").evaluate((el) => getComputedStyle(el).display), "flex");
  assert.equal(await page.locator(".chat-surface.secondary").evaluate((el) => getComputedStyle(el).display), "none");
  await secondaryTab.click();
  assert.equal(await secondaryTab.getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".chat-surface.primary").evaluate((el) => getComputedStyle(el).display), "none");
  assert.equal(await page.locator(".chat-surface.secondary").evaluate((el) => getComputedStyle(el).display), "flex");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.setViewportSize({ width: 1100, height: 800 });

  await page.locator("#chat-split").click();
  assert.equal(await page.locator("#app").getAttribute("data-split"), "false");
  assert.equal(await proofDestination.isVisible(), true, "the fixed rail stays stable when split closes");

  await page.locator("#chat-split").click();
  await page.waitForFunction(() => document.getElementById("app").dataset.split === "true");
  await page.evaluate(() => window.ark.openView("split-view-proof"));
  assert.equal(await page.locator("#app").getAttribute("data-split"), "false");
  assert.equal(await proofDestination.isVisible(), true, "opening a project view exits split and keeps the way back");

  await page.getByRole("navigation", { name: "Destinations" }).getByRole("button", { name: "Chat" }).click();
  for (let index = 0; index < 8; index++) {
    const before = await page.evaluate(() => window.ark.summary().session.id);
    await page.locator("#new-session").click();
    await page.waitForFunction((id) => window.ark.summary().session.id !== id, before);
  }
  const overflow = await page.evaluate(() => {
    const list = document.querySelector("#chat-tab-list");
    const selected = list.querySelector('[data-selected="true"]');
    const bounds = list.getBoundingClientRect();
    const chosen = selected.getBoundingClientRect();
    const effectiveTarget = (selector) => {
      const el = document.querySelector(selector);
      const box = el.getBoundingClientRect();
      return [Math.round(box.width), Math.round(box.height), getComputedStyle(el, "::before").content];
    };
    return {
      overflow: list.scrollWidth > list.clientWidth,
      selectedVisible: chosen.right > bounds.left && chosen.left < bounds.right,
      splitTarget: effectiveTarget("#chat-split"),
      paneTarget: effectiveTarget("#toggle-pane"),
    };
  });
  assert.deepEqual(overflow, { overflow: true, selectedVisible: true, splitTarget: [32, 32, "none"], paneTarget: [32, 32, "none"] });

  await proofDestination.click();
  const projectName = await page.evaluate(() => window.ark.summary().project.name);
  assert.equal(await page.locator("#session-title").innerText(), projectName);
  assert.equal(await page.locator("#session-title").evaluate((el) => getComputedStyle(el).display), "block", "project views keep the project title");
  const chat = page.getByRole("navigation", { name: "Destinations" }).getByRole("button", { name: "Chat" });
  await chat.focus();
  await chat.press("Enter");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("chat-tab")), true, "keyboard Chat focuses the visible selected chat");
  assert.equal(await page.evaluate(() => !!document.activeElement && document.activeElement.getClientRects().length > 0), true, "keyboard Chat focuses visible conversation context");
}
scenarios["chat-workspace-polish"] = chatWorkspacePolish;

// ADR 0068: the tab is the object being arranged. Edge drops create a nested spatial tree,
// dividers keep an accessible value, and the fifth visible chat is constrained without damage.
async function spatialChatLayouts(page) {
  const sessions = [await page.evaluate(() => window.ark.summary().session.id)];
  for (let index = 0; index < 3; index++) {
    await page.locator("#new-session").click();
    await page.waitForFunction((known) => !known.includes(window.ark.summary().session.id), sessions);
    sessions.push(await page.evaluate(() => window.ark.summary().session.id));
  }
  const primary = sessions[3];
  await page.locator(`.chat-tab-item[data-session="${sessions[0]}"]`).dragTo(
    page.locator(`.chat-tab-item[data-session="${sessions[2]}"]`), { force: true },
  );
  assert.deepEqual(await page.locator(".chat-tab-item").evaluateAll((tabs) => tabs.map((tab) => tab.dataset.session)),
    [sessions[1], sessions[0], sessions[2], sessions[3]], "dropping on a tab reorders without opening it");
  const dragToEdge = async (session, target, edge) => {
    const tab = page.locator(`.chat-tab-item[data-session="${session}"]`);
    const leaf = page.locator(`.chat-layout-leaf[data-session="${target}"]`);
    await tab.dragTo(leaf.locator(`.chat-drop-target.${edge}`), { force: true });
    await page.waitForFunction((id) => document.querySelector(`.chat-layout-leaf[data-session="${id}"]`), session);
  };
  await dragToEdge(sessions[0], primary, "left");
  await dragToEdge(sessions[1], sessions[0], "bottom");
  await dragToEdge(sessions[2], primary, "top");

  const shape = await page.evaluate(() => ({
    leaves: [...document.querySelectorAll(".chat-layout-leaf")].map((leaf) => leaf.dataset.session),
    rows: document.querySelectorAll(".chat-layout-split.row").length,
    columns: document.querySelectorAll(".chat-layout-split.column").length,
    surfaces: document.querySelectorAll(".chat-surface").length,
    active: document.querySelector('.chat-surface[data-active="true"]')?.dataset.session,
  }));
  assert.deepEqual(new Set(shape.leaves), new Set(sessions));
  assert.ok(shape.rows >= 1 && shape.columns >= 1, JSON.stringify(shape));
  assert.equal(shape.surfaces, 4);
  assert.equal(shape.active, sessions[2]);

  const divider = page.locator('.chat-divider[aria-orientation="vertical"]').first();
  const before = Number(await divider.getAttribute("aria-valuenow"));
  await divider.focus();
  await divider.press("ArrowRight");
  const resized = before + 5;
  assert.equal(Number(await page.locator('.chat-divider[aria-orientation="vertical"]').first().getAttribute("aria-valuenow")), resized);

  const leavesBeforeCancel = await page.locator(".chat-layout-leaf").count();
  await page.evaluate((session) => {
    const tab = document.querySelector(`.chat-tab-item[data-session="${CSS.escape(session)}"]`);
    const transfer = new DataTransfer();
    tab.dispatchEvent(new DragEvent("dragstart", { bubbles: true, dataTransfer: transfer }));
    tab.dispatchEvent(new DragEvent("dragend", { bubbles: true, dataTransfer: transfer }));
  }, sessions[0]);
  assert.equal(await page.locator(".chat-layout-leaf").count(), leavesBeforeCancel, "an outside-window cancel keeps the layout");
  assert.equal(await page.locator("#app").getAttribute("data-arranging"), null);

  await page.reload();
  await page.locator("#input").waitFor();
  await page.waitForFunction(() => document.querySelectorAll(".chat-layout-leaf").length === 4);
  assert.equal(Number(await page.locator('.chat-divider[aria-orientation="vertical"]').first().getAttribute("aria-valuenow")), resized, "the divider ratio restores");

  await page.locator("#new-session").click();
  await page.waitForFunction((known) => !known.includes(window.ark.summary().session.id), sessions);
  const fifth = await page.evaluate(() => window.ark.summary().session.id);
  await page.locator(`.chat-tab-item[data-session="${primary}"]`).dragTo(
    page.locator(`.chat-layout-leaf[data-session="${fifth}"] .chat-drop-target.right`), { force: true },
  );
  assert.equal(await page.locator(".chat-layout-leaf").count(), 4, "a fifth visible chat is constrained");
  assert.equal(await page.locator("#toast").innerText(), "A window can show up to four chats");

  await page.setViewportSize({ width: 800, height: 700 });
  assert.equal(await page.locator(".chat-layout-leaf").evaluateAll((leaves) => leaves.filter((leaf) => getComputedStyle(leaf).display !== "none").length), 1);
  await page.setViewportSize({ width: 1200, height: 800 });

  const visibleBeforeClose = await page.locator(".chat-layout-leaf").count();
  await page.locator(`.chat-tab-item[data-session="${sessions[0]}"] .chat-tab-close`).click();
  assert.equal(await page.locator(".chat-layout-leaf").count(), visibleBeforeClose - 1);
  assert.equal(await page.locator(".chat-layout-split").count(), visibleBeforeClose - 2, "an empty split collapses with its leaf");
  const primaryBeforeClose = await page.evaluate(() => window.ark.summary().session.id);
  await page.locator(`.chat-tab-item[data-session="${primaryBeforeClose}"] .chat-tab-close`).click();
  await page.waitForFunction((before) => window.ark.summary().session.id !== before, primaryBeforeClose);
  assert.equal(await page.locator(".chat-layout-leaf").count(), visibleBeforeClose - 2);
  assert.equal(await page.locator(".chat-surface.primary").getAttribute("data-session"), await page.evaluate(() => window.ark.summary().session.id));
  await shot(page, "spatial-chat-layouts");
}
scenarios["spatial-chat-layouts"] = spatialChatLayouts;

// A detached page addresses its own chat. Opening another chat in that native-window mode does
// not move the runtime-wide foreground that the original window owns, and its layout has its own
// storage key.
async function detachedChatWindow(page) {
  const sessions = [await page.evaluate(() => window.ark.summary().session.id)];
  for (let index = 0; index < 2; index++) {
    await page.locator("#new-session").click();
    await page.waitForFunction((known) => !known.includes(window.ark.summary().session.id), sessions);
    sessions.push(await page.evaluate(() => window.ark.summary().session.id));
  }
  const originalForeground = sessions[2];
  await page.evaluate(() => localStorage.setItem("ark-chat-layout", "main-window-value"));
  const detached = new URL(page.url());
  detached.searchParams.set("detached", sessions[0]);
  detached.searchParams.set("window", "chat-page-test");
  await rpcIdle();
  await page.goto(detached.href);
  await page.waitForFunction((session) => window.ark.summary()?.session?.id === session, sessions[0]);
  await page.locator(`.chat-surface.secondary[data-session="${sessions[0]}"]`).waitFor();
  assert.equal(await page.locator("#app").getAttribute("data-window"), "detached");
  assert.equal((await page.evaluate(() => window.ark.windowCall("hello"))).summary.session.id, originalForeground);

  await page.evaluate((session) => window.ark.openSession(session), sessions[1]);
  await page.waitForFunction((session) => window.ark.summary()?.session?.id === session, sessions[1]);
  assert.equal((await page.evaluate(() => window.ark.windowCall("hello"))).summary.session.id, originalForeground,
    "a detached window does not move the original window's foreground");
  assert.equal(await page.evaluate(() => localStorage.getItem("ark-chat-layout")), "main-window-value");
  assert.ok(await page.evaluate(() => localStorage.getItem("ark-chat-layout:chat-page-test")));
  await shot(page, "detached-chat-window");
}
scenarios["detached-chat-window"] = detachedChatWindow;

// ADR 0041: a process start asks with its command and where, then runs beside the chat: the
// Processes panel shows its output, its page opens in the Browser once ready, Stop ends it.
async function processes(page) {
  const done = page.evaluate(() => new Promise((ok) => window.ark.on("ark:turn_finished", ok)));
  await page.locator("#input").fill("Run the app and show me the home page");
  await page.keyboard.press("Enter");
  const ask = page.locator(".approval");
  await ask.waitFor();
  const words = await ask.innerText();
  assert.match(words, /Keep this running in the project folder, then open \/home in the browser\. Not contained: it runs as you\.\n\s*python3 -u -m http\.server 0 --bind 127\.0\.0\.1/);
  const buttons = (await ask.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
  assert.deepEqual(buttons, ["Allow once", "Allow this command for this session", "Always allow this command", "Deny"]);
  await ask.locator('button[data-decision="allow_once"]').click();

  // The panel opens with the process and its output; the row turns ready (by then the Browser
  // tab is in front, so it is checked in the page, not for visibility).
  await page.locator(".proc-row").first().waitFor();
  await page.waitForFunction(() => document.querySelector(".proc-out .xterm-rows")?.textContent.includes("Serving HTTP"), null, { timeout: 20000 });
  await page.waitForFunction(() => document.querySelector('.proc-row[data-state="ready"]'), null, { timeout: 20000 });
  // The page the agent asked for, in the Browser panel, in front.
  await page.locator("img.remote-browser[src^='data:image/']").waitFor();
  assert.match(await page.locator('input[aria-label="Address"]').inputValue(), /^http:\/\/localhost:\d+\/home$/);
  assert.equal(await page.locator("iframe.wb-frame").count(), 0);
  assert.equal(await page.locator(".pane-tab[aria-selected=true]").innerText(), "localhost");
  await done;

  // The process's manual Open takes the same destination route after the Browser closes.
  await page.evaluate(() => window.ark.closePanel("browser"));
  await page.locator('.pane-tab', { hasText: "Processes" }).click();
  await page.getByRole("button", { name: "Open in the Browser", exact: true }).click();
  await page.locator("img.remote-browser[src^='data:image/']").waitFor();
  assert.equal(await page.locator("iframe.wb-frame").count(), 0);

  // Stop ends it, and it stays listed as stopped.
  await page.locator('.pane-tab', { hasText: "Processes" }).click();
  await page.locator(".proc-stop").click();
  await page.locator('.proc-row[data-state="stopped"]').waitFor();
  const list = await page.evaluate(() => window.ark.call("procs"));
  assert.equal(list[0].state, "stopped");
}
scenarios.processes = processes;

// Work that waits for the person (ADR 0043): rows in the tray, newest first, above any
// question. Opening a result goes to its chat; opening a failure with no chat shows why under
// the row; the x and Delete clear. Cleared stays cleared after a reload.
async function waiting(page) {
  const rows = page.locator(".asks .waiting .wait-row");
  await rows.first().waitFor();
  assert.equal(await rows.count(), 3, "three shown, the fourth behind More");
  assert.equal(await rows.nth(0).getAttribute("data-kind"), "finished");
  assert.match(await rows.nth(0).innerText(), /File the notes/);
  await page.locator(".wait-more").click();
  assert.equal(await rows.count(), 4);

  // A failure with no chat: why it failed, under the row.
  const summarize = rows.filter({ hasText: "Summarize the notes" });
  await summarize.focus();
  await page.keyboard.press("Enter");
  await summarize.locator(".wait-detail").waitFor();
  assert.equal(await summarize.locator(".wait-detail").innerText(), "The notes folder is gone.");
  await summarize.click();
  await summarize.waitFor({ state: "detached" });

  // The x clears one; Delete clears another.
  const tidy = rows.filter({ hasText: "Tidy the drafts" });
  await tidy.hover();
  await tidy.locator(".wait-clear").click();
  await tidy.waitFor({ state: "detached" });
  const rename = rows.filter({ hasText: "Rename the chapters" });
  await rename.focus();
  await page.keyboard.press("Delete");
  await rename.waitFor({ state: "detached" });

  // A result posted into a chat: opening it opens the chat, where the post is.
  await rows.first().click();
  await page.locator('.chat-tab[aria-selected="true"]', { hasText: "hi" }).waitFor();
  await page.locator("#thread", { hasText: "Filed." }).waitFor();
  await page.locator(".asks .waiting").waitFor({ state: "detached" });

  await page.reload();
  await page.locator("#input").waitFor();
  await page.waitForTimeout(300);
  assert.equal(await page.locator(".asks .waiting").count(), 0, "cleared stays cleared");
}
scenarios.waiting = waiting;

// What waits on another computer (ADR 0043 step 4): its question is a card in this window's
// tray, saying where; answering it here decides it there. Its page, opened with a token and an
// item, signs in and keeps the item to open.
async function linked(page) {
  const card = page.locator(".asks .waiting-cards .approval");
  await card.waitFor();
  assert.match(await card.innerText(), /In My machine · novel/);
  // At phone width the card still fits and its choices show.
  await page.setViewportSize({ width: 390, height: 800 });
  const allow = card.getByRole("button", { name: "Allow once" });
  await allow.waitFor();
  const box = await card.boundingBox();
  assert.ok(box.x >= 0 && box.x + box.width <= 390, `the card fits: ${JSON.stringify(box)}`);

  // The other computer's page, as a window opens it: the token leaves the address, the item stays.
  const there = await page.context().browser().newPage();
  await there.addInitScript(() => {
    window.__addresses = [];
    const replace = history.replaceState.bind(history);
    history.replaceState = (s, t, u) => { window.__addresses.push(String(u)); replace(s, t, u); };
  });
  await there.goto(process.env.ARK_TEST_ELSEWHERE);
  await there.locator("#input").waitFor();
  await there.locator("#asks .approval").waitFor();
  const addresses = await there.evaluate(() => window.__addresses);
  assert.ok(addresses.every((a) => !a.includes("token=")), `the token left the address: ${addresses}`);
  assert.ok(addresses[0].includes("#item="), `the item stayed for the page: ${addresses}`);
  assert.equal(new URL(there.url()).hash, "", "and was opened");
  await there.close();

  await allow.click();
  await card.waitFor({ state: "detached" });
}
scenarios.linked = linked;

// Where a chat runs (ADR 0052): the chip of an empty chat offers the server; the first message
// starts the chat there; the header, the sidebar and the chip say where it runs; a file it wrote
// comes back with one button.
scenarios.places = async (page) => {
  const chip = page.locator("#place-chip");
  await chip.waitFor();
  assert.match(await chip.innerText(), /This (Mac|computer)/);
  await chip.click();
  const vps = page.locator(".places-pop .place-item", { hasText: "vps" });
  await vps.waitFor();
  await shot(page, "places-menu");
  await vps.click();
  assert.equal((await chip.innerText()).trim(), "vps");
  assert.equal((await page.locator("#model-chip").innerText()).trim(), "vps default");

  // Before the chat exists, its picker already comes from the selected computer. Commands that
  // only work here are not advertised for a chat aimed there.
  await page.locator("#input").fill("/");
  await page.locator("#slash").waitFor();
  assert.equal(await page.locator("#slash .slash-item[data-name='model']").count(), 0);
  assert.equal(await page.locator("#slash .slash-item[data-name='help']").count(), 1);
  await page.locator("#input").fill("");
  await page.keyboard.press("Meta+k");
  await page.locator(".palette-input").waitFor();
  assert.equal(await page.locator(".palette-item", { hasText: "/model" }).count(), 0);
  await page.keyboard.press("Escape");
  await page.locator("#model-chip").click();
  const remoteClaude = page.locator(".models-pop .model-group", { hasText: "Claude subscription" });
  await remoteClaude.waitFor();
  assert.equal(await remoteClaude.locator(".model-dot").getAttribute("data-ok"), "false");
  assert.equal(await remoteClaude.getByRole("button", { name: "Sign in" }).count(), 0);
  await remoteClaude.getByText("Set up after this chat starts").waitFor();
  await page.keyboard.press("Escape");
  await finished(page, () => page.locator("#input").fill("/model").then(() => page.keyboard.press("Enter")));
  await page.locator(".turn .answer", { hasText: "Hello from B." }).waitFor();
  await finished(page, () => page.locator("#input").fill("jot it down").then(() => page.keyboard.press("Enter")));
  await page.locator(".turn .answer", { hasText: "Done." }).waitFor();
  assert.equal(await chip.getAttribute("aria-disabled"), "true", "fixed after the first message");
  await page.locator("#session-title .place-mark", { hasText: "vps" }).waitFor({ state: "attached" });
  await page.locator(".session[aria-current='true'] .session-place").waitFor();
  const line = page.locator("#place-line");
  await line.getByText("1 file changed on vps").waitFor();
  await shot(page, "places-light");
  await page.emulateMedia({ colorScheme: "dark" });
  await shot(page, "places-dark");
  await page.setViewportSize({ width: 390, height: 800 });
  const box = await line.boundingBox();
  assert.ok(box.x >= 0 && box.x + box.width <= 390, `the line fits: ${JSON.stringify(box)}`);
  await shot(page, "places-narrow");
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.emulateMedia({ colorScheme: "light" });
  await line.getByRole("button", { name: "Bring to this Mac" }).click();
  await page.locator("#toast", { hasText: "1 file brought to this Mac" }).waitFor();
  await line.waitFor({ state: "hidden" });

  // The model menu is the server's: its program signs in there, answered here.
  await page.locator("#model-chip").click();
  const sign = page.locator(".models-pop .model-group", { hasText: "Claude subscription" }).getByRole("button", { name: "Sign in" });
  await sign.waitFor();
  await sign.click();
  const sheet = page.locator(".program-sheet");
  await sheet.getByText("Paste code here").waitFor();
  await shot(page, "places-sign-in");
  await sheet.locator(".xterm-helper-textarea").focus();
  await page.keyboard.type("abc-123");
  await page.keyboard.press("Enter");
  await sheet.getByText("signed in with abc-123").waitFor();
  await sheet.getByRole("button", { name: "Close", exact: true }).last().click();
  await sheet.waitFor({ state: "detached" });
};

// The server went away: its chat stays, reads only, and says so with a way to try again.
scenarios.places_away = async (page) => {
  const line = page.locator("#place-line");
  await line.getByText("vps is not answering").waitFor();
  await line.getByRole("button", { name: "Try again" }).waitFor();
  assert.equal(await page.locator("#composer").getAttribute("data-away"), "true");
  await page.locator(".session-place[data-state]:not([data-state='ok'])").waitFor();
  assert.equal(await page.locator(".welcome").count(), 0, "not shown as an empty chat");
  await page.waitForTimeout(400);
  await shot(page, "places-away");
};

// Settings › Computers (ADR 0052): the hosts of the ssh config to pick; a setup that cannot
// connect names its step and offers Try again; an address asks for a code.
scenarios.computers = async (page) => {
  // The chip of an empty chat offers Add a computer, which opens the section.
  const chip = page.locator("#place-chip");
  await chip.waitFor();
  await chip.click();
  await page.locator(".places-pop").getByText("Add a computer").click();
  const host = page.getByRole("button", { name: "closed-box" });
  await host.waitFor();
  await shot(page, "computers-empty");
  await host.click();
  await page.locator(".computer-steps li[data-state='failed']").waitFor({ timeout: 20000 });
  assert.match(await page.locator(".computer-failure").innerText(), /closed-box/);
  await page.getByRole("button", { name: "Try again" }).waitFor();
  await shot(page, "computers-failed");
  await page.setViewportSize({ width: 390, height: 800 });
  await page.emulateMedia({ colorScheme: "dark" });
  await shot(page, "computers-failed-narrow");
  await page.setViewportSize({ width: 1100, height: 800 });
  await page.emulateMedia({ colorScheme: "light" });
  await page.getByRole("button", { name: "Cancel" }).click();
  const field = page.locator(".computer-target");
  await field.fill("https://ark.example.invalid");
  await page.locator(".computer-code").waitFor();
  await shot(page, "computers-address");
};

// Text on the accent reads, light and dark: with the app's accent (`ArkUi::accent`), with a
// consumer palette that sets its own text for its own accent (ARK Desktop's dark), and with any
// other accent an app may set.
async function accent(page) {
  // Text on the button, and the button on the page behind it.
  const contrast = (selector, against = "text") => page.locator(selector).first().evaluate((el, against) => {
    // `color-mix()` reads back as `color(srgb r g b)` with 0 to 1 channels.
    const rgb = (c) => c.match(/[\d.]+/g).slice(0, 3).map(Number).map((v) => c.startsWith("color(") ? v * 255 : v);
    const lum = (c) => rgb(c).map((v) => v / 255).map((v) => v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)
      .reduce((sum, v, i) => sum + v * [0.2126, 0.7152, 0.0722][i], 0);
    const s = getComputedStyle(el);
    const other = against === "text" ? s.color : getComputedStyle(document.body).backgroundColor;
    const [hi, lo] = [lum(s.backgroundColor), lum(other)].sort((a, b) => b - a);
    return Math.round(((hi + 0.05) / (lo + 0.05)) * 100) / 100;
  }, against);
  // Colours are read once the button's transition has ended.
  const mode = (m) => page.evaluate((m) => window.arkTheme.setMode(m), m).then(() => page.waitForTimeout(250));
  assert.equal(await page.evaluate(() => document.documentElement.style.getPropertyValue("--app-accent")), "rgb(51, 94, 183)");
  await page.locator("#input").fill("Save the notes");
  await page.keyboard.press("Enter");
  const allow = ".approval:not(.decided) .btn.primary";
  await page.locator(allow).waitFor();
  for (const m of ["light", "dark"]) {
    await mode(m);
    assert.ok(await contrast(allow) >= 4.5, `${m}: ${await contrast(allow)}`);
    assert.ok(await contrast(allow, "page") >= 3, `${m} button on the page: ${await contrast(allow, "page")}`);
    await shot(page, `accent-${m}`);
  }
  // ARK Desktop's dark palette: its own dark text for its own lighter accent.
  await page.addStyleTag({ content: ':root[data-theme="dark"] { --accent-fg: #0e0f12; }' });
  await mode("light");
  await mode("dark");
  assert.ok(await contrast(allow) >= 4.5, `consumer dark: ${await contrast(allow)}`);
  await shot(page, "accent-consumer-dark");
  for (const color of ["#ffd60a", "#1e3a8a", "#5b8def", "#787878", "rgb(16, 185, 129)"]) {
    await page.evaluate((c) => window.arkTheme.accent(c), color);
    await page.waitForTimeout(250);
    for (const m of ["light", "dark"]) {
      await mode(m);
      assert.ok(await contrast(allow) >= 4.5, `${color} ${m}: ${await contrast(allow)}`);
      await page.locator(allow).hover();
      await page.waitForTimeout(200);
      assert.ok(await contrast(allow) >= 4.5, `${color} ${m} hovered: ${await contrast(allow)}`);
      await page.mouse.move(0, 0);
    }
  }
  await mode("light");
  await page.locator(allow).click();
  await page.locator(".agent", { hasText: "Saved." }).waitFor();
}
scenarios.accent = accent;

// At 760 px and below the destinations sit at the bottom and the side pane takes the whole
// window: what opens in it shows, and closing it brings the chat back.
async function narrowPane(page) {
  const box = (selector) => page.locator(selector).evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { left: Math.round(r.left), width: Math.round(r.width), height: Math.round(r.height) };
  });
  for (const m of ["light", "dark"]) {
    await page.evaluate((m) => window.arkTheme.setMode(m), m);
    await page.setViewportSize({ width: 760, height: 800 });
    await page.waitForFunction(() => document.getElementById("app").dataset.compact === "true");
    await page.evaluate(() => window.ark.openPanel("files"));
    await page.waitForFunction(() => document.getElementById("app").dataset.pane === "open");
    await page.waitForTimeout(350);
    const pane = await box("#pane");
    assert.deepEqual({ left: pane.left, width: pane.width }, { left: 0, width: 760 }, JSON.stringify(pane));
    assert.ok(pane.height > 600, JSON.stringify(pane));
    assert.equal((await box(".main")).width, 0);
    assert.ok(await page.locator(".pane-tab", { hasText: "Files" }).isVisible(), "the pane's tab shows");
    assert.ok(await page.locator("#close-pane").isVisible(), "the way back shows");
    const rail = await page.locator(".destinations").boundingBox();
    assert.ok(rail.y >= 700 && rail.width === 760, JSON.stringify(rail));
    await shot(page, `narrow-pane-${m}`);
    await page.click("#close-pane");
    await page.waitForFunction(() => document.getElementById("app").dataset.pane !== "open");
    await page.waitForTimeout(350);
    assert.equal((await box(".main")).width, 760);
    assert.ok(await page.locator("#input").isVisible(), "the chat is back");
    await page.setViewportSize({ width: 1100, height: 800 });
  }
  await page.evaluate(() => window.arkTheme.setMode("light"));
}
scenarios["narrow-pane"] = narrowPane;

// ADR 0045: the Computer is off until the person turns it on, in Plugins (Turn on, Turn off),
// or by Allow when an agent asks mid-task; the turn then goes on in the app.
async function computerSwitch(page) {
  const destination = page.locator('.workspace-destination[data-view="marketplace"]');
  const row = page.locator('.market-row[data-id="capability:computer"]');
  const state = () => page.evaluate(async () => (await window.ark.call("capabilities")).find((c) => c.id === "computer")?.state);
  await destination.click();
  await row.waitFor();
  assert.equal(await row.locator(".market-copy strong").innerText(), "Computer");
  assert.equal(await state(), "off");
  await row.getByRole("button", { name: "Turn on" }).click();
  await row.getByRole("button", { name: "Turn off" }).waitFor();
  assert.equal(await state(), "ready");
  await page.waitForTimeout(300);
  await shot(page, "computer-row-on-light");
  await page.evaluate(() => window.arkTheme.setMode("dark"));
  await shot(page, "computer-row-on-dark");
  await page.evaluate(() => window.arkTheme.setMode("light"));
  await row.getByRole("button", { name: "Turn off" }).click();
  await row.getByRole("button", { name: "Turn on" }).waitFor();
  assert.equal(await state(), "off");
  await shot(page, "computer-row-off");

  await page.locator('.destination[data-destination="home"]').click();
  await page.locator("#input").fill("Open Notes");
  await page.keyboard.press("Enter");
  const card = page.locator(".approval:not(.decided)");
  await card.waitFor();
  assert.equal(await card.locator(".approval-title").innerText(), "ARK needs this to finish:");
  assert.match(await card.innerText(), /Allow ARK to use the apps on this computer/);
  assert.match(await card.innerText(), /It still asks before each step\./);
  const buttons = (await card.locator(".approval-actions button").allInnerTexts()).map((t) => t.split("\n")[0]);
  assert.deepEqual(buttons, ["Allow", "Not now"]);
  await shot(page, "computer-allow-light");
  await page.evaluate(() => window.arkTheme.setMode("dark"));
  await shot(page, "computer-allow-dark");
  await page.evaluate(() => window.arkTheme.setMode("light"));
  await card.locator('button[data-decision="allow_once"]').click();
  await page.locator(".agent", { hasText: "Notes is open." }).waitFor({ timeout: 15000 });
  assert.equal(await state(), "ready");
  await destination.click();
  await row.getByRole("button", { name: "Turn off" }).waitFor();
}
scenarios["computer-switch"] = computerSwitch;

const run = scenarios[scenario];
if (!run) {
  console.error(`unknown scenario ${scenario}`);
  process.exit(2);
}
const engine = process.env.ARK_UI_BROWSER === "chromium" ? chromium : webkit;
const browser = await engine.launch();
const errors = [];
try {
  const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
  page.on("pageerror", (e) => errors.push(e.stack || String(e)));
  let rpcInFlight = 0;
  const isRpc = (r) => r.url().endsWith("/rpc");
  page.on("request", (r) => { if (isRpc(r)) rpcInFlight++; });
  page.on("requestfinished", (r) => { if (isRpc(r)) rpcInFlight--; });
  page.on("requestfailed", (r) => { if (isRpc(r)) rpcInFlight--; });
  // Leaving a page while a call is in flight makes that call fail in the page being left.
  globalThis.rpcIdle = async () => {
    for (let quiet = 0; quiet < 3;) {
      await page.waitForTimeout(30);
      quiet = rpcInFlight === 0 ? quiet + 1 : 0;
    }
  };
  if (scenario === 'file-viewer-consumer') page.on('console', message => { if (message.type() === 'error') console.error('consumer console:', message.text()); });
  if (inits[scenario]) await page.addInitScript(inits[scenario]);
  if (scenario === "dictation") {
    let firstStatus = true;
    let nextUpload = 0;
    dictationFixture = { active: new Map(), seen: [], failChunk: true, failBegin: false };
    dictationFixture.statusGate = new Promise((release) => { dictationFixture.releaseStatus = release; });
    await page.route("**/rpc", async (route) => {
      const body = JSON.parse(route.request().postData() || "{}");
      const fixture = dictationFixture;
      const params = body.params || {};
      const answer = (result) => route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true, result }) });
      const error = (message) => route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: false, error: message }) });
      if (body.method === "places") return answer({ here: "This computer", places: [{ name: "vps", state: "ok" }] });
      if (!body.method?.startsWith("dictation_")) return route.continue();
      fixture.seen.push({ method: body.method, ...params });
      if (body.method === "dictation_status" && firstStatus) {
        firstStatus = false;
        await fixture.statusGate;
      }
      if (body.method === "dictation_status") return answer({ status: !params.place && fixture.localUnavailable ? "unavailable" : fixture.downloading ? "downloading" : "ready", runtime: params.runtime || params.place || "local" });
      if (body.method === "dictation_begin") {
        if (fixture.failBegin) { fixture.failBegin = false; return error("network failed"); }
        await fixture.beginWait;
        if ([...fixture.active.values()].includes(params.place)) return error("already active");
        const id = `test-upload-${++nextUpload}`;
        fixture.active.set(id, params.place);
        return answer({ id, runtime: params.runtime || params.place || "local", chunk_bytes: 262144, max_bytes: 16777216 });
      }
      const owner = fixture.active.get(params.id);
      if (owner !== undefined && owner !== params.place) return error("audio reached the wrong computer");
      if (body.method === "dictation_cancel") {
        fixture.active.delete(params.id);
        return answer({ cancelled: true });
      }
      if (owner === undefined) return error("upload missing");
      if (body.method === "dictation_chunk") {
        if (fixture.failChunk) { fixture.failChunk = false; return error("network failed"); }
        return answer({ next: Number(params.index || 0) + 1 });
      }
      fixture.active.delete(params.id);
      await fixture.finishWait;
      return answer({ text: "Texto dictado." });
    });
  }
  if (scenario === "ready") {
    await page.route("**/*", async (route) => {
      const body = route.request().postData() || "";
      if (body.includes('"method":"hello"')) await new Promise((resolve) => setTimeout(resolve, 250));
      await route.continue();
    });
  }
  await page.goto(url);
  if (scenario !== "startup-extension-failure") await page.locator("#input").waitFor();
  await run(page);
  if (scenario !== "startup-extension-failure") assert.deepEqual(errors, [], "the page threw");
  console.log(`${scenario}: ok`);
} catch (e) {
  console.error(`${scenario}: ${e.stack || e}`);
  if (scenario === 'file-viewer-consumer') {
    const page = browser.contexts()[0]?.pages()[0];
    if (page) console.error('File view state:', await page.locator('.fv').innerText().catch(() => 'unavailable'));
    if (page) console.error('Frame states:', await Promise.all(page.frames().map(async frame => [frame.url(), await frame.locator('body').innerText().catch(() => '')])));
  }
  // What the page threw before it failed, if anything.
  for (const err of errors) console.error(`page error: ${err}`);
  process.exitCode = 1;
} finally {
  await browser.close();
}
