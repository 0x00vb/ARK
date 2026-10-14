// A window layout is a small binary tree. Leaves name visible chats; split nodes preserve the
// direction and divider position. The model has no DOM so restore and drag operations can be
// checked without depending on the renderer.

export const MAX_VISIBLE_CHATS = 4;

export const chatLeaf = (session) => ({ kind: "chat", session });

export function visibleChats(node, into = []) {
  if (!node) return into;
  if (node.kind === "chat") into.push(node.session);
  else {
    visibleChats(node.first, into);
    visibleChats(node.second, into);
  }
  return into;
}

export function splitChat(node, target, session, edge) {
  if (!node || !target || !session || target === session) return node;
  if (visibleChats(node).includes(session)) node = removeChat(node, session);
  const direction = edge === "left" || edge === "right" ? "row" : "column";
  const before = edge === "left" || edge === "top";
  const insert = (entry) => entry.kind === "chat" && entry.session === target
    ? {
      kind: "split", direction, ratio: 0.5,
      first: before ? chatLeaf(session) : entry,
      second: before ? entry : chatLeaf(session),
    }
    : entry.kind === "split"
      ? { ...entry, first: insert(entry.first), second: insert(entry.second) }
      : entry;
  return insert(node);
}

export function removeChat(node, session) {
  if (!node) return null;
  if (node.kind === "chat") return node.session === session ? null : node;
  const first = removeChat(node.first, session);
  const second = removeChat(node.second, session);
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

export function replaceChat(node, before, after) {
  if (!node || !before || !after) return node;
  if (node.kind === "chat") return node.session === before ? chatLeaf(after) : node;
  return { ...node, first: replaceChat(node.first, before, after), second: replaceChat(node.second, before, after) };
}

export function updateRatio(node, path, ratio) {
  if (!node || node.kind !== "split") return node;
  if (!path.length) return { ...node, ratio: Math.min(0.8, Math.max(0.2, ratio)) };
  const [side, ...rest] = path;
  return side === "first"
    ? { ...node, first: updateRatio(node.first, rest, ratio) }
    : { ...node, second: updateRatio(node.second, rest, ratio) };
}

export function restoreLayout(value, available, primary) {
  const allowed = new Set(available);
  const seen = new Set();
  const clean = (node) => {
    if (!node || typeof node !== "object") return null;
    if (node.kind === "chat") {
      if (typeof node.session !== "string" || !allowed.has(node.session) || seen.has(node.session)) return null;
      seen.add(node.session);
      return chatLeaf(node.session);
    }
    if (node.kind !== "split" || !["row", "column"].includes(node.direction)) return null;
    const first = clean(node.first);
    const second = clean(node.second);
    if (!first) return second;
    if (!second) return first;
    const ratio = Number(node.ratio);
    return { kind: "split", direction: node.direction, ratio: Number.isFinite(ratio) ? Math.min(0.8, Math.max(0.2, ratio)) : 0.5, first, second };
  };
  let restored = clean(value);
  if (!restored || !visibleChats(restored).includes(primary)) restored = chatLeaf(primary);
  while (visibleChats(restored).length > MAX_VISIBLE_CHATS) {
    const last = [...visibleChats(restored)].reverse().find((session) => session !== primary);
    if (!last) break;
    restored = removeChat(restored, last);
  }
  return restored;
}
