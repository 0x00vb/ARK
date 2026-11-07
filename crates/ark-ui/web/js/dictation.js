// Capture belongs to one composer. Every recording keeps the computer chosen at its start.
import { icon } from "./dom.js";

const OUT_RATE = 16000;
const MAX_SAMPLES = 8 * 60 * OUT_RATE;
const CHUNK = 256 * 1024;
const FLUSH_MS = 700;
const PREVIEW_MS = 500;
const MIN_PREVIEW = OUT_RATE / 2;
let lease = null;

function reason(error) {
  const name = error?.name || "";
  if (name === "NotAllowedError" || name === "SecurityError") return "Microphone access is off. Allow it in system settings.";
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "No microphone was found.";
  if (name === "NotReadableError" || name === "AbortError") return "The microphone stopped. Try again.";
  const message = error?.message || String(error || "");
  if (/not included|unavailable/i.test(message)) return message;
  if (/network|reach|fetch|answering|connection|linked/i.test(message)) return "Audio could not reach this chat's computer. Try again.";
  if (/prepare|model|download|checksum/i.test(message)) return "Speech recognition could not be prepared. Try again.";
  return "That recording could not be transcribed. Try again.";
}

function b64(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i += 0x8000) out += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(out);
}

export class DictationController {
  constructor({ api, button, status, insert, live, beginLive, endLive }) {
    Object.assign(this, { api, button, status });
    this.options = { insert, live, beginLive, endLive };
    this.token = 0;
    this.checkToken = 0;
    this.onClick = () => void this.toggle();
    this.onPageHidden = () => this.cancel();
    this.onVisibility = () => { if (document.hidden) this.cancel(); };
    button.addEventListener("click", this.onClick);
    addEventListener("pagehide", this.onPageHidden);
    document.addEventListener("visibilitychange", this.onVisibility);
    this.routeChanged();
  }

  target() { return this.api.dictationRoute(); }

  bind(route) {
    const place = route.place;
    let runtime = route.runtime;
    return async (method, params = {}) => {
      const result = await this.api.windowCall(method, { ...params, place, ...(runtime ? { runtime } : {}) });
      // Pending places have no chat yet. Begin returns their actual runtime identity.
      if (method === "dictation_begin" || method === "dictation_status") runtime = result.runtime || runtime;
      return result;
    };
  }

  routeChanged() {
    const route = this.target();
    const key = JSON.stringify(route);
    if (this.routeKey === key || this.disposed) return;
    this.cancel();
    this.routeKey = key;
    this.unavailable = null;
    this.paint("checking");
    void this.check();
  }

  async check() {
    const check = ++this.checkToken;
    // Construction precedes hello. Wait for the restored chat's computer.
    await this.api.ready?.();
    if (this.disposed || check !== this.checkToken) return false;
    const call = this.bind(this.target());
    let unavailable = null;
    if (!globalThis.isSecureContext && !["localhost", "127.0.0.1", "::1", "[::1]"].includes(location.hostname)) {
      unavailable = "Dictation needs a secure HTTPS page.";
    } else if (!navigator.mediaDevices?.getUserMedia || !(globalThis.AudioContext || globalThis.webkitAudioContext)) {
      unavailable = "Dictation is not available in this browser.";
    } else {
      try {
        const state = await call("dictation_status");
        if (state.status === "unavailable") unavailable = "Dictation is not included in this app.";
      } catch (error) { unavailable = reason(error); }
    }
    if (this.disposed || check !== this.checkToken) return false;
    this.unavailable = unavailable;
    this.paint(unavailable ? "unavailable" : "ready");
    return !unavailable;
  }

  /// The button is the only sign of dictation: the microphone, or a square while it listens.
  /// Words appear only for what the person must know: the speech model downloading, a failure.
  paint(phase, line = "") {
    if (this.disposed) return;
    this.phase = phase;
    const active = ["requesting", "recording", "finishing"].includes(phase);
    this.button.replaceChildren(icon(active ? "stop" : "mic"));
    this.button.setAttribute("aria-label", phase === "recording" ? "Stop dictation" : active ? "Cancel dictation" : phase === "unavailable" ? "Dictation unavailable" : "Start dictation");
    this.button.setAttribute("aria-pressed", String(phase === "recording"));
    this.button.disabled = phase === "checking";
    this.button.dataset.state = phase;
    this.status.textContent = line;
    this.status.hidden = !line;
    this.status.setAttribute("aria-live", "polite");
  }

  async toggle() {
    if (this.disposed || this.phase === "checking") return;
    if (this.phase === "recording") return this.stop();
    if (["requesting", "finishing"].includes(this.phase)) return this.cancel();
    if (this.unavailable) {
      const token = this.token;
      this.paint("checking");
      const available = await this.check();
      if (token !== this.token || this.disposed) return;
      if (!available) return this.paint("unavailable", this.unavailable);
    }
    if (!this.disposed) await this.start();
  }

  async start() {
    if (lease && lease !== this) return this.paint("error", "Another chat is listening.");
    lease = this; // Reserve before the permission prompt, not after it resolves.
    const token = ++this.token;
    const call = this.bind(this.target());
    this.recordingCall = call;
    this.parts = [];
    this.samples = 0;
    this.sent = 0;
    this.index = 0;
    this.limit = false;
    this.downloading = false;
    this.preview = true;
    this.flushing = Promise.resolve();
    clearTimeout(this.completeTimer);
    this.paint("requesting");
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true }, video: false });
      if (token !== this.token) return stream.getTracks().forEach((track) => track.stop());
      this.stream = stream;
      const Audio = globalThis.AudioContext || globalThis.webkitAudioContext;
      this.context = new Audio({ sampleRate: OUT_RATE });
      this.source = this.context.createMediaStreamSource(stream);
      this.node = this.context.createScriptProcessor(4096, 1, 1);
      this.node.onaudioprocess = (event) => this.capture(event.inputBuffer.getChannelData(0));
      this.source.connect(this.node);
      this.node.connect(this.context.destination);
      await this.context.resume?.();
      if (token !== this.token) return;
      this.uploadPromise = call("dictation_begin").then(async (upload) => {
        if (token !== this.token) {
          await call("dictation_cancel", { id: upload.id }).catch(() => {});
          return null;
        }
        this.upload = upload.id;
        return upload;
      }).catch((error) => {
        if (token === this.token) this.fail(error);
        return null; // Begin failure must release capture even without Stop.
      });
      this.statusTimer = setInterval(() => void this.providerLine(token, call), 700);
      this.flushTimer = setInterval(() => void this.flush(token, call), FLUSH_MS);
      this.options.beginLive?.();
      this.paint("recording");
      void this.preview_loop(token, call);
    } catch (error) { if (token === this.token) this.fail(error); }
  }

  async providerLine(token, call) {
    try {
      const state = await call("dictation_status");
      if (token !== this.token) return;
      const downloading = state.status === "downloading";
      if (downloading === this.downloading) return;
      this.downloading = downloading;
      if (["recording", "finishing"].includes(this.phase)) this.paint(this.phase, downloading ? "Downloading speech model" : "");
    } catch {}
  }

  /// Send what was heard since the last call. Calls queue, so chunks arrive in order.
  flush(token, call) {
    const run = this.flushing.then(async () => {
      if (token !== this.token || !this.parts.length) return;
      const upload = await this.uploadPromise;
      if (!upload || token !== this.token) return;
      const parts = this.parts;
      this.parts = [];
      const pcm = new Int16Array(parts.reduce((all, part) => all + part.length, 0));
      let offset = 0;
      for (const part of parts) { pcm.set(part, offset); offset += part.length; }
      const bytes = new Uint8Array(pcm.buffer);
      for (let at = 0; at < bytes.length; at += CHUNK) {
        if (token !== this.token) return;
        await call("dictation_chunk", { id: upload.id, index: this.index++, data: b64(bytes.subarray(at, at + CHUNK)) });
      }
      this.sent += pcm.length;
    });
    this.flushing = run.catch((error) => { if (token === this.token) this.fail(error); });
    return this.flushing;
  }

  /// While recording, ask for the words so far and show them where the transcript will land.
  /// A computer that cannot preview (an older ARK there) leaves the words to Stop.
  async preview_loop(token, call) {
    let failures = 0;
    while (token === this.token && ["requesting", "recording"].includes(this.phase)) {
      await new Promise((resolve) => setTimeout(resolve, PREVIEW_MS));
      const upload = await this.uploadPromise;
      if (!upload || token !== this.token || this.phase !== "recording" || !this.preview) continue;
      if (this.sent < MIN_PREVIEW) continue;
      try {
        const result = await call("dictation_preview", { id: upload.id });
        failures = 0;
        if (token === this.token && this.phase === "recording" && typeof result?.text === "string") this.options.live?.(result.text);
      } catch (error) {
        // An older computer refuses the call; a slow link may fail once. Either way the words
        // come at Stop, and the draft is editable again.
        if (token === this.token && (++failures >= 3 || /does not cross|unknown|not supported/i.test(error?.message || ""))) {
          this.preview = false;
          this.options.endLive?.();
        }
      }
    }
  }

  capture(input) {
    if (this.phase !== "recording" || !this.context) return;
    const ratio = this.context.sampleRate / OUT_RATE;
    const count = Math.min(Math.floor(input.length / ratio), MAX_SAMPLES - this.samples);
    const part = new Int16Array(count);
    for (let i = 0; i < count; i++) {
      const from = Math.floor(i * ratio);
      const to = Math.min(input.length, Math.max(from + 1, Math.floor((i + 1) * ratio)));
      let sum = 0;
      for (let at = from; at < to; at++) sum += input[at];
      const value = Math.max(-1, Math.min(1, sum / Math.max(1, to - from)));
      part[i] = value < 0 ? value * 32768 : value * 32767;
    }
    this.parts.push(part);
    this.samples += part.length;
    if (this.samples >= MAX_SAMPLES) { this.limit = true; void this.stop(); }
  }

  releaseCapture() {
    clearInterval(this.flushTimer);
    clearInterval(this.statusTimer);
    this.node?.disconnect();
    this.source?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    void this.context?.close().catch(() => {});
    this.node = this.source = this.stream = this.context = null;
  }

  async stop() {
    if (this.phase !== "recording") return;
    const token = this.token;
    const call = this.recordingCall;
    const pending = this.uploadPromise;
    this.releaseCapture();
    this.paint("finishing", this.downloading ? "Downloading speech model" : "");
    this.statusTimer = setInterval(() => void this.providerLine(token, call), 700);
    try {
      await this.flush(token, call);
      if (token !== this.token) return;
      const upload = await pending;
      if (!upload || token !== this.token) return;
      if (this.sent < 3200 / 2) throw new Error("No speech was detected");
      const result = await call("dictation_finish", { id: upload.id });
      if (token !== this.token) return;
      clearInterval(this.statusTimer);
      this.upload = null;
      this.uploadPromise = null;
      this.recordingCall = null;
      if (lease === this) lease = null;
      if (!result.text?.trim()) throw new Error("No speech was detected");
      this.options.insert(result.text);
      this.paint("ready");
    } catch (error) { if (token === this.token) this.fail(error); }
  }

  fail(error) { this.cancel(); this.paint("error", reason(error)); }

  cancel() {
    ++this.token;
    this.releaseCapture();
    clearTimeout(this.completeTimer);
    if (this.upload && this.recordingCall) void this.recordingCall("dictation_cancel", { id: this.upload }).catch(() => {});
    // Begin still in flight handles its late upload id against its captured route.
    this.upload = this.uploadPromise = this.recordingCall = null;
    this.parts = [];
    this.downloading = false;
    if (lease === this) lease = null;
    this.options.endLive?.();
    this.paint("ready");
  }

  beforeSubmit() {
    if (this.phase === "recording") { void this.stop(); return true; }
    return ["requesting", "finishing"].includes(this.phase);
  }

  dispose() {
    this.cancel();
    this.disposed = true;
    ++this.checkToken;
    this.button.removeEventListener("click", this.onClick);
    removeEventListener("pagehide", this.onPageHidden);
    document.removeEventListener("visibilitychange", this.onVisibility);
    this.button.remove();
    this.status.remove();
  }
}
