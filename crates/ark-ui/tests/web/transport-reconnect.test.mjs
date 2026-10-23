import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { SourceTextModule, createContext } from "node:vm";

const sourcePath = new URL("../../web/js/transport.js", import.meta.url);
const source = await readFile(sourcePath, "utf8");

async function connectedSequence({ failBeforeFirstOpen = false, manualRetry = false } = {}) {
  const events = [];
  const streams = [];
  const timers = [];
  let retry;
  const context = createContext({
    URL,
    URLSearchParams,
    location: { href: "http://127.0.0.1/", protocol: "http:", hostname: "127.0.0.1", pathname: "/", search: "", hash: "", assign() {}, replace() {} },
    history: { replaceState() {} },
    sessionStorage: { getItem() { return null; }, setItem() {} },
    window: {},
    fetch: async () => ({ ok: true, json: async () => ({ signed_in: true, pairing: false }) }),
    EventSource: class { constructor() { streams.push(this); } close() {} },
    setTimeout(callback) { timers.push(callback); return timers.length; },
  });
  const module = new SourceTextModule(source, { context });
  await module.link(() => { throw new Error("transport should not import dependencies"); });
  await module.evaluate();
  module.namespace.connect().subscribe(event => { events.push(event.kind); if (event.retry) retry = event.retry; });
  const flush = async () => { await Promise.resolve(); await Promise.resolve(); await new Promise(resolve => setImmediate(resolve)); };
  await flush();
  assert.equal(streams.length, 1, "initial event stream opens");
  if (manualRetry) {
    for (let attempt = 0; attempt <= 10; attempt += 1) {
      await streams.at(-1).onerror();
      await flush();
      if (timers.length) { timers.shift()(); await flush(); }
    }
    assert.equal(events.at(-1), "disconnected");
    assert.equal(typeof retry, "function");
    events.length = 0;
    retry();
    streams.at(-1).onopen();
    return events;
  }
  if (failBeforeFirstOpen) {
    streams[0].onerror();
    await flush();
    assert.deepEqual(events, ["reconnecting"]);
    timers.shift()();
    await flush();
  } else {
    streams[0].onopen();
    assert.deepEqual(events, [], "first successful connection does not trigger a resync");
    streams[0].onerror();
    await flush();
    assert.deepEqual(events, ["reconnecting"]);
    timers.shift()();
    await flush();
  }
  assert.equal(streams.length, 2, "recovery opens a new event stream");
  streams[1].onopen();
  return events;
}

assert.deepEqual(await connectedSequence({ failBeforeFirstOpen: true }), ["reconnecting", "resync", "connected"]);
assert.deepEqual(await connectedSequence(), ["reconnecting", "resync", "connected"]);
console.log("PASS initial-failure recovery and established-stream recovery resync before connected; initial success remains unchanged");

assert.deepEqual(await connectedSequence({ manualRetry: true }), ["resync", "connected"]);
