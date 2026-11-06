// The first line of an empty chat. It is written here, not by the model: nothing is sent, nothing
// is stored, and it costs no tokens. The line is picked by a hash of the chat's key, so a chat
// keeps its line while another gets a different one.

import { registry } from "./ext.js";

// [with the person's name, without it]
const LINES = Object.freeze([
  ["Hey {name}, good to meet you. I'm ready whenever you are.", "Hey, good to meet you. I'm ready whenever you are."],
  ["Hi {name}. What are we working on today?", "Hi. What are we working on today?"],
  ["Hello {name}. Tell me what you need and I'll get started.", "Hello. Tell me what you need and I'll get started."],
  ["Hey {name}. Ready when you are.", "Hey. Ready when you are."],
  ["Good to see you, {name}. What's on your mind?", "Good to see you. What's on your mind?"],
  ["Hi {name}, I'm here. Where should we start?", "Hi, I'm here. Where should we start?"],
]);

function hash(text) {
  let value = 2166136261;
  for (const byte of new TextEncoder().encode(text)) value = Math.imul(value ^ byte, 16777619) >>> 0;
  return value;
}

/// The person's first word of their name, when the app knows it.
function firstName() {
  try {
    const name = String(registry.profile?.()?.name || "").trim().split(/\s+/)[0];
    return name || "";
  } catch {
    return "";
  }
}

export function greetingFor(key) {
  const [named, plain] = LINES[hash(String(key || "")) % LINES.length];
  const name = firstName();
  return name ? named.replace("{name}", name) : plain;
}

export const GREETING_LINES = LINES;
