// Composer dictation. Capture stays in the page; transcription belongs to the selected runtime
// through ark-ui's injected provider (ADR 0070).

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
  if (/network|reach|fetch|answering|connection/i.test(message)) return "Audio could not reach this chat's computer. Try again.";
  if (/prepare|model|download|checksum/i.test(message)) return "Speech recognition could not be prepared. Try again.";
  return "That recording could not be transcribed. Try again.";
}

function b64(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i += 0x8000) out += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(out);
}

export class DictationController {
  constructor({ api, button, status, insert, remember }) {
    this.api = api;
    this.button = button;
    this.status = status;
    this.insert = insert;
    this.remember = remember;
    this.token = 0;
    this.phase = "checking";
    this.onPageHidden = () => this.cancel();
    button.addEventListener("click", () => void this.toggle());
    addEventListener("pagehide", this.onPageHidden);
    document.addEventListener("visibilitychange", this.onPageHidden);
    this.paint("checking");
    void this.check();
  }

  async check() {
    if (!globalThis.isSecureContext && !["localhost", "127.0.0.1", "::1"].includes(location.hostname)) {
      this.unavailable = "Dictation needs a secure HTTPS page.";
    } else if (!navigator.mediaDevices?.getUserMedia || !(globalThis.AudioContext || globalThis.webkitAudioContext)) {
      this.unavailable = "Dictation is not available in this browser.";
    } else {
      try {
        const state = await this.api.call("dictation_status");
        if (state.status === "unavailable") this.unavailable = "Dictation is not included in this app.";
      } catch (error) {
        this.unavailable = reason(error);
      }
    }
    if (this.phase === "checking") this.paint(this.unavailable ? "unavailable" : "ready", this.unavailable);
  }

  paint(phase, line = "") {
    this.phase = phase;
    const active = ["recording", "preparing", "transcribing"].includes(phase);
    this.button.replaceChildren(icon(phase === "recording" ? "stop" : "mic"));
    this.button.setAttribute("aria-label", phase === "recording" ? "Stop dictation" : active ? "Cancel dictation" : phase === "unavailable" ? "Dictation unavailable" : "Start dictation");
    this.button.setAttribute("aria-pressed", String(phase === "recording"));
    this.button.disabled = phase === "checking";
    this.button.dataset.state = phase;
    this.status.textContent = line;
    this.status.hidden = !line;
    this.status.setAttribute("aria-live", phase === "recording" ? "off" : "polite");
  }

  async toggle() {
    if (this.phase === "recording") return this.stop();
    if (this.phase === "preparing" || this.phase === "transcribing") return this.cancel();
    if (this.unavailable) return this.paint("unavailable", this.unavailable);
    if (lease && lease !== this) return this.paint("error", "Another chat is listening.");
    await this.start();
  }

  async start() {
    const token = ++this.token;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true }, video: false });
      if (token !== this.token) return stream.getTracks().forEach((track) => track.stop());
      lease = this;
      this.stream = stream;
      this.parts = [];
      this.samples = 0;
      this.started = performance.now();
      const Audio = globalThis.AudioContext || globalThis.webkitAudioContext;
      this.context = new Audio({ sampleRate: OUT_RATE });
      this.source = this.context.createMediaStreamSource(stream);
      this.node = this.context.createScriptProcessor(4096, 1, 1);
      this.node.onaudioprocess = (event) => this.capture(event.inputBuffer.getChannelData(0));
      this.source.connect(this.node);
      this.node.connect(this.context.destination);
      this.uploadPromise = this.api.call("dictation_begin").then((upload) => {
        this.upload = upload.id;
        return upload;
      });
      this.statusTimer = setInterval(() => void this.providerLine(), 700);
      this.timer = setInterval(() => {
        const elapsed = Math.floor((performance.now() - this.started) / 1000);
        const model = this.downloading ? " · downloading 148 MB speech model" : "";
        this.paint("recording", `Listening · ${Math.floor(elapsed / 60)}:${String(elapsed % 60).padStart(2, "0")}${model}`);
      }, 1000);
      this.paint("recording", "Listening · 0:00");
    } catch (error) {
      this.release();
      this.parts = [];
      this.paint("error", reason(error));
    }
  }

  async providerLine() {
    if (this.phase !== "recording") return;
    try {
      const state = await this.api.call("dictation_status");
      this.downloading = state.status === "downloading";
    } catch {}
  }

  capture(input) {
    const ratio = this.context.sampleRate / OUT_RATE;
    const count = Math.floor(input.length / ratio);
    const part = new Int16Array(count);
    for (let i = 0; i < count; i++) {
      const from = Math.floor(i * ratio);
      const to = Math.max(from + 1, Math.floor((i + 1) * ratio));
      let sum = 0;
      for (let at = from; at < to && at < input.length; at++) sum += input[at];
      const value = Math.max(-1, Math.min(1, sum / Math.max(1, Math.min(to, input.length) - from)));
      part[i] = value < 0 ? value * 32768 : value * 32767;
    }
    this.parts.push(part);
    this.samples += part.length;
    if (this.samples >= MAX_SAMPLES) {
      this.limit = true;
      void this.stop();
    }
  }

  release() {
    clearInterval(this.timer);
    clearInterval(this.statusTimer);
    this.node?.disconnect();
    this.source?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    void this.context?.close();
    this.node = this.source = this.stream = this.context = null;
    if (lease === this) lease = null;
  }

  async stop() {
    if (this.phase !== "recording") return;
    const token = ++this.token;
    this.release();
    this.paint("preparing", this.limit ? "Eight-minute limit reached · transcribing" : "Preparing speech recognition");
    let upload;
    try {
      if (!this.limit) {
        const state = await this.api.call("dictation_status").catch(() => null);
        if (state?.status === "downloading") this.paint("preparing", "Downloading 148 MB speech model");
      }
      upload = await this.uploadPromise;
      const pcm = new Int16Array(Math.min(this.samples, MAX_SAMPLES));
      let offset = 0;
      for (const part of this.parts) {
        const take = Math.min(part.length, pcm.length - offset);
        pcm.set(part.subarray(0, take), offset);
        offset += take;
        if (offset >= pcm.length) break;
      }
      const bytes = new Uint8Array(pcm.buffer);
      for (let index = 0, at = 0; at < bytes.length; index++, at += CHUNK) {
        if (token !== this.token) return;
        await this.api.call("dictation_chunk", { id: upload.id, index, data: b64(bytes.subarray(at, at + CHUNK)) });
      }
      this.paint("transcribing", "Transcribing");
      const result = await this.api.call("dictation_finish", { id: upload.id });
      this.upload = null;
      if (token !== this.token) return;
      if (!result.text) throw new Error("No speech was detected. Try again.");
      this.insert(result.text);
      this.paint("complete", "Dictation added");
      setTimeout(() => token === this.token && this.paint("ready"), 2200);
    } catch (error) {
      if (token !== this.token) return;
      const abandoned = upload?.id || this.upload;
      if (abandoned) void this.api.call("dictation_cancel", { id: abandoned }).catch(() => {});
      else if (this.uploadPromise) {
        void this.uploadPromise.then((pending) => this.api.call("dictation_cancel", { id: pending.id })).catch(() => {});
      }
      this.upload = null;
      this.uploadPromise = null;
      this.paint("error", reason(error));
    } finally {
      this.parts = [];
    }
  }

  cancel() {
    if (["ready", "checking", "unavailable"].includes(this.phase) && !this.stream && !this.uploadPromise) return;
    ++this.token;
    this.release();
    if (this.upload) void this.api.call("dictation_cancel", { id: this.upload }).catch(() => {});
    else if (this.uploadPromise) void this.uploadPromise.then((upload) => this.api.call("dictation_cancel", { id: upload.id })).catch(() => {});
    this.upload = null;
    this.uploadPromise = null;
    this.parts = [];
    this.downloading = false;
    this.paint("ready");
  }

  beforeSubmit() {
    if (this.phase !== "recording") return false;
    void this.stop();
    return true;
  }

  dispose() {
    this.cancel();
    removeEventListener("pagehide", this.onPageHidden);
    document.removeEventListener("visibilitychange", this.onPageHidden);
    this.button.remove();
    this.status.remove();
  }
}
