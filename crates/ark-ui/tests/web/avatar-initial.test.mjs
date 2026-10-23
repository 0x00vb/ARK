import assert from "node:assert/strict";
import { test } from "node:test";

class FixtureNode {
  constructor(tag = "#text", text = "") {
    this.tagName = tag;
    this.nodeType = tag === "#text" ? 3 : 1;
    this.text = text;
    this.children = [];
    this.attributes = {};
    this.dataset = {};
    this.style = { setProperty() {} };
    this.className = "";
  }
  append(child) { this.children.push(child); }
  setAttribute(name, value) {
    this.attributes[name] = String(value);
    if (name.startsWith("data-")) {
      const key = name.slice(5).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
      this.dataset[key] = String(value);
    }
  }
  getAttribute(name) { return this.attributes[name] ?? null; }
  querySelector(selector) {
    const className = selector.startsWith(".") ? selector.slice(1) : "";
    return this.children.find(child => child.className.split(/\s+/).includes(className)) ?? null;
  }
  get textContent() { return this.nodeType === 3 ? this.text : this.children.map(child => child.textContent).join(""); }
}

globalThis.Node = FixtureNode;
globalThis.document = {
  createElement: tag => new FixtureNode(tag),
  createTextNode: text => new FixtureNode("#text", String(text)),
};

const { chatAvatar, setAgentIdentity } = await import("../../web/js/dom.js");

test("chat avatar keeps a grapheme initial and raw identity mapping", () => {
  setAgentIdentity(name => `identity:${name}`);
  const cases = [
    ["astral plus ZWJ", "🧑‍🚀pilot", "🧑‍🚀"],
    ["combining mark", "e\u0301clair", "E\u0301"],
    ["Latin", "writer", "W"],
  ];
  for (const [label, name, initial] of cases) {
    const mark = chatAvatar([name]);
    const face = mark.querySelector(".chat-face");
    assert.equal(face.textContent, initial, label);
    assert.equal(face.dataset.variant, `identity:${name}`, `${label} keeps identity input`);
  }
  const empty = chatAvatar([""]);
  assert.equal(empty.dataset.count, "0");
  assert.equal(empty.querySelector(".chat-face"), null);
});


test("chat avatar fallback preserves an astral code point", async () => {
  const segmenter = Intl.Segmenter;
  let fallback;
  try {
    Intl.Segmenter = undefined;
    fallback = await import("../../web/js/dom.js?without-segmenter");
  } finally {
    Intl.Segmenter = segmenter;
  }
  const face = fallback.chatAvatar(["🪐pilot"]).querySelector(".chat-face");
  assert.equal(face.textContent, "🪐");
});
