// VM coverage of page bootstrap eligibility. This does not certify native IPC permissions.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const template = readFileSync(new URL('../../src/desktop_notification.js', import.meta.url), 'utf8');
const marker = '/* __ARK_NOTIFICATION_SCRIPT__ */';
assert.equal(template.split(marker).length, 2, 'One upstream script insertion point');
// Rust's optional focused test supplies the real plugin script. The normal page gate also
// checks the same guard with a small payload, without adding a native dependency to the page.
const supplied = process.argv.includes('--upstream') ? JSON.parse(readFileSync(0, 'utf8')) : null;
const original = supplied?.original ?? `window.Notification = function Notification() {};
void window.__TAURI_INTERNALS__.invoke('plugin:notification|is_permission_granted', {});`;
const guarded = supplied?.guarded ?? template.replace(marker, original);
const script = process.argv.includes('--unguarded') ? original : guarded;
const cases = [
  { name: 'own main', expected: 1 },
  { name: 'Windows own main', protocol: 'http:', hostname: 'tauri.localhost', expected: 1 },
  { name: 'detached local', windowLabel: 'chat-existing' },
  { name: 'remote main', protocol: 'https:', hostname: 'example.test' },
  { name: 'remote loopback', protocol: 'http:', hostname: '127.0.0.1' },
  { name: 'remote localhost', protocol: 'http:', hostname: 'localhost' },
  { name: 'lookalike suffix', protocol: 'http:', hostname: 'tauri.localhost.evil.test' },
  { name: 'lookalike prefix', protocol: 'http:', hostname: 'evil-tauri.localhost' },
  { name: 'https is not own', protocol: 'https:', hostname: 'tauri.localhost' },
  { name: 'file is not own', protocol: 'file:', hostname: '' },
  { name: 'browser webview', webviewLabel: 'ark-browser' },
  { name: 'child own main frame', frame: true },
  { name: 'no internals', absent: 'internals' },
  { name: 'no metadata', absent: 'metadata' },
  { name: 'no window metadata', absent: 'window' },
  { name: 'no webview metadata', absent: 'webview' },
  { name: 'empty window label', windowLabel: '' },
  { name: 'empty webview label', webviewLabel: '' },
];
async function execute(script, scenario) {
  const calls = [];
  const internals = { metadata: {
    currentWindow: { label: scenario.windowLabel ?? 'main' },
    currentWebview: { label: scenario.webviewLabel ?? 'main' },
  }, invoke: async (command, args) => { calls.push({ command, args }); return false; } };
  function NativeNotification() {}
  const window = { Notification: NativeNotification, __TAURI_INTERNALS__: internals,
    location: { protocol: scenario.protocol ?? 'tauri:', hostname: scenario.hostname ?? 'localhost' } };
  window.top = scenario.frame ? {} : window;
  if (scenario.absent === 'internals') delete window.__TAURI_INTERNALS__;
  if (scenario.absent === 'metadata') delete internals.metadata;
  if (scenario.absent === 'window') delete internals.metadata.currentWindow;
  if (scenario.absent === 'webview') delete internals.metadata.currentWebview;
  vm.runInNewContext(script, { window }, { timeout: 1000 });
  await new Promise(resolve => setImmediate(resolve));
  return { window, internals, calls, NativeNotification };
}
// The pinned Windows script skips the query and sets granted. Preserve that behavior rather
// than expecting the macOS/Linux query on every platform. Bootstrap execution is checked too.
const baseline = await execute(original, { name: 'original own main' });
assert.notEqual(baseline.window.Notification, baseline.NativeNotification);
assert(baseline.calls.length <= 1);
if (baseline.calls.length) assert.equal(baseline.calls[0].command, 'plugin:notification|is_permission_granted');
for (const scenario of cases) {
  const { window, internals, calls, NativeNotification } = await execute(script, scenario);
  assert.equal(calls.length, scenario.expected ? baseline.calls.length : 0, scenario.name);
  if (scenario.expected) {
    assert.deepEqual(JSON.parse(JSON.stringify(calls)), JSON.parse(JSON.stringify(baseline.calls)), scenario.name);
    assert.notEqual(window.Notification, NativeNotification, scenario.name);
    if (supplied) assert.equal(window.Notification.permission, baseline.window.Notification.permission, scenario.name);
  } else {
    assert.equal(window.Notification, NativeNotification, scenario.name);
  }
  if (scenario.absent !== 'internals') assert.equal(window.__TAURI_INTERNALS__, internals, scenario.name);
}
console.log(`${cases.length} notification bootstrap VM cases passed${supplied ? ' with actual upstream payload' : ''}`);
