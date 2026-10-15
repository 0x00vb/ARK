// Capture belongs to one composer. Every recording keeps the computer chosen at its start.
import { icon } from "./dom.js";

const OUT_RATE = 16000;
const MAX_SAMPLES = 8 * 60 * OUT_RATE;
const CHUNK = 256 * 1024;
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
  constructor({ api, button, status, insert }) {
    Object.assign(this, { api, button, status, insert });
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

  paint(phase, line = "") {
    if (this.disposed) return;
    this.phase = phase;
    const active = ["requesting", "recording", "preparing", "transcribing"].includes(phase);
    this.button.replaceChildren(icon(active ? "stop" : "mic"));
    this.button.setAttribute("aria-label", phase === "recording" ? "Stop dictation" : active ? "Cancel dictation" : phase === "unavailable" ? "Dictation unavailable" : "Start dictation");
    this.button.setAttribute("aria-pressed", String(phase === "recording"));
    this.button.disabled = phase === "checking";
    this.button.dataset.state = phase;
    this.status.textContent = line;
    this.status.hidden = !line;
    this.status.setAttribute("aria-live", phase === "recording" ? "off" : "polite");
  }

  async toggle() {
    if (this.disposed || this.phase === "checking") return;
    if (this.phase === "recording") return this.stop();
    if (["requesting", "preparing", "transcribing"].includes(this.phase)) return this.cancel();
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
    this.limit = false;
    this.downloading = false;
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
      this.started = performance.now();
      this.statusTimer = setInterval(() => void this.providerLine(token, call), 700);
      this.timer = setInterval(() => {
        if (token !== this.token || this.phase !== "recording") return;
        const elapsed = Math.floor((performance.now() - this.started) / 1000);
        const model = this.downloading ? " · downloading 148 MB speech model" : "";
        this.paint("recording", `Listening · ${Math.floor(elapsed / 60)}:${String(elapsed % 60).padStart(2, "0")}${model}`);
      }, 1000);
      this.paint("recording", "Listening · 0:00");
    } catch (error) { if (token === this.token) this.fail(error); }
  }

  async providerLine(token, call) {
    try {
      const state = await call("dictation_status");
      if (token !== this.token) return;
      this.downloading = state.status === "downloading";
      if (["preparing", "transcribing"].includes(this.phase)) {
        this.paint(this.phase, this.downloading ? "Downloading 148 MB speech model"
          : this.phase === "transcribing" ? "Transcribing" : "Preparing speech recognition");
      }
    } catch {}
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
    clearInterval(this.timer);
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
    this.paint("preparing", this.limit ? "Eight-minute limit reached · transcribing" : "Preparing speech recognition");
    this.statusTimer = setInterval(() => void this.providerLine(token, call), 700);
    try {
      await this.providerLine(token, call);
      if (token !== this.token) return;
      const upload = await pending;
      if (!upload || token !== this.token) return;
      const pcm = new Int16Array(this.samples);
      let offset = 0;
      for (const part of this.parts) { pcm.set(part, offset); offset += part.length; }
      this.parts = [];
      const bytes = new Uint8Array(pcm.buffer);
      for (let index = 0, at = 0; at < bytes.length; index++, at += CHUNK) {
        if (token !== this.token) return;
        await call("dictation_chunk", { id: upload.id, index, data: b64(bytes.subarray(at, at + CHUNK)) });
      }
      if (token !== this.token) return;
      this.paint("transcribing", this.downloading ? "Downloading 148 MB speech model" : "Transcribing");
      const result = await call("dictation_finish", { id: upload.id });
      if (token !== this.token) return;
      clearInterval(this.statusTimer);
      this.upload = null;
      this.uploadPromise = null;
      this.recordingCall = null;
      if (lease === this) lease = null;
      if (!result.text?.trim()) throw new Error("No speech was detected");
      this.insert(result.text);
      this.paint("complete", "Dictation added");
      this.completeTimer = setTimeout(() => token === this.token && this.paint("ready"), 2200);
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
    this.paint("ready");
  }

  beforeSubmit() {
    if (this.phase === "recording") { void this.stop(); return true; }
    return ["requesting", "preparing", "transcribing"].includes(this.phase);
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
