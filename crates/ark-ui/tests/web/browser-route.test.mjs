import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { createContext, SourceTextModule, SyntheticModule } from "node:vm";

const source = await readFile(new URL("../../web/js/workbench.js", import.meta.url), "utf8");
const destination = { runtime: "destination", project: "notes", session: "chat" };
const url = "http://localhost:3000/home";

async function fixture({ remoteResource = false, destinationBrowser = true, fail = false, nativeBridge = true, snapshot, navigate, control } = {}) {
  const nodes = [], calls = [], timers = new Map();
  let sequence = 0;
  const document = { activeElement: null };
  const h = (selector, ...args) => {
    const node = {
      tag: selector.split(".")[0], classes: selector.split(".").slice(1), children: [],
      hidden: false, textContent: "", attributes: {}, handlers: {},
      classList: { add() {} },
      append(...children) { this.children.push(...children); },
      replaceChildren(...children) { this.children = children; },
      addEventListener(name, callback) { this.handlers[name] = callback; },
      setAttribute(name, value) { this.attributes[name] = value; },
      toggleAttribute(name, enabled) { this.attributes[name] = enabled; },
      contains(child) { return this.children.includes(child); },
      focus() { document.activeElement = this; },
      getBoundingClientRect() { return { width: 1280, height: 900, left: 0, top: 0 }; },
    };
    if (args[0] && typeof args[0] === "object" && !args[0].tag) Object.assign(node, args.shift());
    node.append(...args);
    nodes.push(node);
    return node;
  };
  const context = createContext({ document, devicePixelRatio: 1, performance: { now: () => 0 },
    window: { open: (...args) => calls.push({ kind: "window.open", args }) },
    requestAnimationFrame: () => 0, addEventListener() {}, removeEventListener() {},
    setTimeout: callback => { const id = ++sequence; timers.set(id, callback); return id; },
    clearTimeout: id => timers.delete(id),
  });
  const exports = {
    "./dom.js": { h, icon: () => "", clear: () => {} },
    "./file.js": { KIND_ICON: {} },
    "./processes.js": { processes() {}, watchProcesses() {} },
    "./keys.js": { mod: () => false },
    "./links.js": { openOutside: (url) => calls.push({ kind: "window.open", args: [url, "_blank", "noopener"] }) },
  };
  const module = new SourceTextModule(source, { context });
  await module.link(specifier => {
    assert.ok(Object.hasOwn(exports, specifier), `unexpected import ${specifier}`);
    return new SyntheticModule(Object.keys(exports[specifier]), function () {
      for (const [name, value] of Object.entries(exports[specifier])) this.setExport(name, value);
    }, { context });
  });
  await module.evaluate();
  const state = { fail };
  const native = name => async (...args) => { calls.push({ kind: `native:${name}`, args }); return { reached: true }; };
  const api = {
    location: destination, remoteResource, destinationBrowser,
    browserView: nativeBridge ? { show: native("show"), act: native("act"), listen: native("listen"), reach: native("reach") } : undefined,
    online: () => true, setPanelTitle() {}, reportFocus() {}, toast() {},
    resourceRef: (kind, id, location) => ({ kind, id, location: { ...location } }),
    resource: async (ref, kind, input) => {
      calls.push({ ref, kind, input });
      if (kind === "browser_snapshot" && snapshot) return snapshot();
      if (kind === "browser_navigate" && navigate) return navigate(input);
      if (["browser_back", "browser_reload"].includes(kind) && control) return control(kind);
      if (state.fail) throw new Error("Chromium is not installed on this computer");
      return kind === "browser_snapshot"
        ? { image: "fixture", media_type: "image/png", width: 1280, height: 900, url, title: "Destination content", loading: false }
        : {};
    },
    call: async (kind, input) => { calls.push({ kind, input }); return {}; },
  };
  const root = h("div");
  const panel = module.namespace.BUILTINS.find(panel => panel.id === "browser").render(root, { locator: url }, api);
  const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
  await settle();
  return { nodes, calls, panel, state, settle,
    retry: () => nodes.find(node => node.tag === "button" && node.children.includes("Try again")),
    error: () => nodes.find(node => node.classes.includes("wb-refused") && node.children.some(child => child?.tag === "p")),
    poll: async () => { const pending = [...timers.values()]; timers.clear(); for (const callback of pending) callback(); await settle(); },
  };
}

function assertDestination(f) {
  const navigate = f.calls.find(call => call.kind === "browser_navigate");
  assert.ok(navigate, "the actual Browser renderer must navigate on the destination");
  assert.deepEqual(navigate.ref, { location: destination, kind: "browser", id: "main" });
  assert.equal(navigate.input.url, url);
  assert.ok(f.calls.some(call => call.kind === "browser_snapshot"));
  assert.equal(f.nodes.some(node => node.tag === "iframe"), false);
  assert.equal(f.calls.some(call => call.kind.startsWith("native:") || call.kind === "frame_check" || call.kind === "window.open"), false);
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

const frame = url => ({ image: "fixture", media_type: "image/png", width: 1280, height: 900, url, loading: false });

test("same-runtime HTTP Browser uses destination rendering even with a native bridge", async () => {
  const f = await fixture();
  assertDestination(f);
  assert.equal(f.nodes.find(node => node.tag === "img").src, "data:image/png;base64,fixture");
  f.panel.dispose();
});

test("whole-window HTTP Browser without a native bridge never creates an iframe", async () => {
  const f = await fixture({ nativeBridge: false });
  assertDestination(f);
  f.panel.dispose();
});

test("public and loopback tunnel hosts use the HTTP carrier in a desktop window", async () => {
  const transportSource = await readFile(new URL("../../web/js/transport.js", import.meta.url), "utf8");
  for (const address of ["https://runtime.example", "http://localhost:8000", "http://127.0.0.1:8000", "http://[::1]:8000"]) {
    const module = new SourceTextModule(transportSource, { context: createContext({
      location: new URL(`${address}/?token=fixture`), window: { __TAURI__: {} }, URL, URLSearchParams,
      history: { replaceState() {} }, sessionStorage: { setItem() {} },
    }) });
    await module.link(() => { throw Error("transport must not import dependencies"); });
    await module.evaluate();
    const transport = module.namespace.connect();
    assert.equal(transport.kind, "browser", address);
    assert.equal(transport.shell, "desktop", address);
    assert.equal(transport.browserView, undefined, address);
  }
});

test("held remote Browser keeps destination rendering", async () => {
  const f = await fixture({ remoteResource: true });
  assertDestination(f);
  f.panel.dispose();
});

test("native local Desktop Browser keeps its existing route", async () => {
  const f = await fixture({ destinationBrowser: false });
  assert.ok(f.calls.some(call => call.kind === "native:reach"));
  assert.equal(f.calls.some(call => call.kind === "browser_navigate"), false);
  assert.equal(f.nodes.some(node => node.tag === "iframe"), false);
  f.panel.dispose();
});

test("unavailable Chromium is visible and Try again navigates the requested URL", async () => {
  const f = await fixture({ remoteResource: true, fail: true });
  assertDestination(f);
  assert.equal(f.error().hidden, false);
  assert.equal(f.error().children[0].textContent, "Chromium is not installed on this computer");
  f.state.fail = false;
  f.retry().onclick();
  await f.settle();
  assert.equal(f.calls.filter(call => call.kind === "browser_navigate").length, 2);
  assert.equal(f.calls.some(call => call.kind === "browser_reload"), false);
  assert.equal(f.error().hidden, true);
  f.panel.dispose();
});

test("a failed frame is visible and Try again reopens an ended Browser", async () => {
  const f = await fixture({ remoteResource: true });
  f.state.fail = true;
  await f.poll();
  assert.equal(f.error().hidden, false);
  assert.equal(f.error().children[0].textContent, "Chromium is not installed on this computer");
  f.state.fail = false;
  f.retry().onclick();
  await f.settle();
  assert.equal(f.calls.filter(call => call.kind === "browser_navigate").length, 2);
  assert.equal(f.error().hidden, true);
  f.panel.dispose();
});

test("a delayed pre-navigation snapshot cannot clear failure or change the retry address", async () => {
  const oldSnapshot = deferred();
  const f = await fixture({ snapshot: () => oldSnapshot.promise,
    navigate: async () => { throw Error("The requested page failed"); } });
  assert.equal(f.error().hidden, false);
  oldSnapshot.resolve(frame("http://localhost:2999/previous"));
  await f.settle();
  assert.equal(f.error().hidden, false, "a stale snapshot must not clear the current failure");
  assert.equal(f.nodes.find(node => node.tag === "input").value, url);
  f.retry().onclick();
  await f.settle();
  assert.equal(f.calls.filter(call => call.kind === "browser_navigate").at(-1).input.url, url);
  f.panel.dispose();
});

test("an older navigation failure cannot replace a newer navigation result", async () => {
  const oldNavigation = deferred();
  const next = "http://localhost:3000/new";
  let current = "";
  const f = await fixture({ snapshot: () => frame(current), navigate: input => {
    if (input.url === url) return oldNavigation.promise;
    current = input.url;
  } });
  f.panel.update({ locator: next });
  await f.settle();
  assert.equal(f.nodes.find(node => node.tag === "input").value, next);
  oldNavigation.reject(Error("The old page failed"));
  await f.settle();
  assert.equal(f.error().hidden, true, "the newer navigation must stay visible");
  assert.equal(f.nodes.find(node => node.tag === "input").value, next);
  f.panel.dispose();
});

test("a pending navigation cannot update the Browser after disposal", async () => {
  const navigation = deferred(), snapshot = deferred();
  const f = await fixture({ navigate: () => navigation.promise, snapshot: () => snapshot.promise });
  f.panel.dispose();
  navigation.reject(Error("The disposed page failed"));
  snapshot.resolve(frame("http://localhost:2999/previous"));
  await f.settle();
  assert.equal(f.error().hidden, true, "disposed navigation must not publish its error");
  assert.equal(f.nodes.find(node => node.tag === "input").value, url);
});

for (const outcome of ["resolve", "reject"]) {
  test(`current polling resumes after an older snapshot ${outcome}s`, async () => {
    const oldSnapshot = deferred();
    const next = "http://localhost:3000/new";
    let snapshots = 0;
    const f = await fixture({ snapshot: () => ++snapshots === 1 ? oldSnapshot.promise : frame(next) });
    f.panel.update({ locator: next });
    await f.settle();
    if (outcome === "resolve") oldSnapshot.resolve(frame("http://localhost:2999/previous"));
    else oldSnapshot.reject(Error("The previous frame failed"));
    await f.settle();
    assert.equal(f.nodes.find(node => node.tag === "input").value, next);
    assert.equal(f.error().hidden, true);
    await f.poll();
    assert.equal(snapshots, 2, "the stale request must release the in-flight frame slot");
    assert.equal(f.nodes.find(node => node.tag === "img").src, "data:image/png;base64,fixture");
    assert.equal(f.nodes.find(node => node.tag === "input").value, next);
    f.panel.dispose();
  });
}

test("an older navigation success cannot clear the newer request's failure", async () => {
  const oldNavigation = deferred();
  const next = "http://localhost:3000/new";
  const f = await fixture({ navigate: input => {
    if (input.url === url) return oldNavigation.promise;
    throw Error("The newer page failed");
  } });
  f.panel.update({ locator: next });
  await f.settle();
  oldNavigation.resolve();
  await f.settle();
  assert.equal(f.error().hidden, false);
  assert.equal(f.error().children[0].textContent, "The newer page failed");
  f.retry().onclick();
  await f.settle();
  assert.equal(f.calls.filter(call => call.kind === "browser_navigate").at(-1).input.url, next);
  f.panel.dispose();
});

for (const [title, operation] of [["Back", "browser_back"], ["Reload", "browser_reload"]]) {
  test(`${title} restores the current page after a failed navigation`, async () => {
    const f = await fixture({ fail: true });
    assert.equal(f.error().hidden, false);
    f.state.fail = false;
    f.nodes.find(node => node.tag === "button" && node.title === title).onclick();
    await f.settle();
    assert.ok(f.calls.some(call => call.kind === operation));
    assert.equal(f.error().hidden, true, `${title} must restore frame polling after navigation failure`);
    assert.equal(f.nodes.find(node => node.tag === "img").src, "data:image/png;base64,fixture");
    f.panel.dispose();
  });

  test(`a stale ${title} reply cannot clear a newer navigation failure`, async () => {
    const control = deferred();
    const f = await fixture({ control: () => control.promise });
    f.nodes.find(node => node.tag === "button" && node.title === title).onclick();
    f.state.fail = true;
    f.panel.update({ locator: "http://localhost:3000/new" });
    await f.settle();
    control.resolve();
    await f.settle();
    assert.equal(f.error().hidden, false);
    assert.equal(f.nodes.find(node => node.tag === "input").value, "http://localhost:3000/new");
    f.state.fail = false;
    await f.poll();
    assert.equal(f.error().hidden, false, "background polling cannot clear failed navigation");
    f.panel.dispose();
  });

  test(`a pending ${title} error cannot publish after disposal`, async () => {
    const control = deferred();
    const f = await fixture({ control: () => control.promise });
    f.nodes.find(node => node.tag === "button" && node.title === title).onclick();
    f.panel.dispose();
    control.reject(Error("The disposed control failed"));
    await f.settle();
    assert.equal(f.error().hidden, true);
  });
}
