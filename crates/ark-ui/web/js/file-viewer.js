import { h, clear } from './dom.js';

// A registered viewer runs with page authority. This API is a convenience boundary, not a
// sandbox. Only immutable, route-bound bytes cross it; consumer parsers run in workers.
export function extensionFileView(el, file, ctx, viewer) {
  const { ark } = ctx;
  let alive = true, generation = 0, controller = null, selection = null, pending = null;
  const scene = h('div.fv-scene');
  const state = h('div.fv-opening', { role: 'status', 'aria-live': 'polite', hidden: true });
  el.append(scene, state);
  // With no view yet the state is the pane's only content, so it sits in the middle.
  const show = () => { state.classList.toggle('fv-center', !controller); state.hidden = false; };

  const close = p => p?.id && ark.call('file_snapshot_cancel', { path: file.path, request: p.id }).catch(() => {});
  const cancel = () => {
    generation++;
    if (pending) { pending.abort.abort(); close(pending); }
    pending = null;
    clear(state).append(h('span', 'Opening cancelled'), h('button', { type: 'button', onclick: open }, 'Retry'));
    show();
  };

  async function open() {
    const run = ++generation;
    if (pending) { pending.abort.abort(); close(pending); }
    const p = { id: null, abort: new AbortController() };
    pending = p;
    const current = () => alive && run === generation && !p.abort.signal.aborted;
    const title = h('span', controller ? 'File changed, loading the new version' : 'Opening');
    const progress = h('progress');
    clear(state).append(title, progress, h('button', { type: 'button', onclick: cancel }, 'Cancel'));
    const timer = setTimeout(() => { if (current()) show(); }, 1000);
    const next = h('div.fv-scene');
    next.style.cssText = 'position:absolute;inset:0;visibility:hidden';
    scene.append(next);
    let replacement;
    try {
      const begin = await ark.call('file_snapshot_open', { path: file.path });
      p.id = begin.request;
      if (!current()) return;
      let meta;
      for (;;) {
        meta = await ark.call('file_snapshot_status', { path: file.path, request: p.id });
        if (!current()) return;
        if (meta.state === 'refused') throw new Error(meta.why);
        if (meta.state === 'ready') break;
        title.textContent = `${meta.state === 'verifying' ? 'Checking' : 'Opening'}${meta.size ? ` · ${Math.floor(meta.bytes / Math.max(meta.size, 1) * 100)}%` : ''}`;
        if (meta.size) { progress.max = meta.size; progress.value = meta.bytes; }
        await new Promise(resolve => setTimeout(resolve, 100));
      }
      let reading = 0, readBytes = 0, confirming = false;
      const api = {
        h,
        signal: p.abort.signal,
        changed: () => open(),
        cancel,
        confirm(label) {
          if (!current()) return Promise.reject(new Error('Opening cancelled'));
          if (confirming || typeof label !== 'string' || !label.trim() || label.length > 80) return Promise.reject(new Error('Invalid file view confirmation'));
          confirming = true;
          return new Promise((resolve, reject) => {
            const abort = () => { confirming = false; reject(new Error('Opening cancelled')); };
            const allow = h('button', { type: 'button', onclick: () => {
              if (!current() || !confirming) return;
              confirming = false; p.abort.signal.removeEventListener('abort', abort);
              title.textContent = 'Opening';
              clear(state).append(title, progress, h('button', { type: 'button', onclick: cancel }, 'Cancel'));
              resolve();
            } }, label);
            allow.className = 'primary';
            clear(state).append(allow, h('button', { type: 'button', onclick: cancel }, 'Cancel'));
            show(); p.abort.signal.addEventListener('abort', abort, { once: true });
          });
        },
        async read(offset) {
          if (!current()) throw new Error('Opening cancelled');
          if (reading >= 2) throw new Error('Only two file chunks may be read at once');
          reading++;
          try {
            const reply = await ark.call('file_snapshot_chunk', { path: file.path, snapshot: meta.snapshot, offset });
            if (!current()) throw new Error('Opening cancelled');
            if (reply.snapshot !== meta.snapshot || reply.offset !== offset) throw new Error('File view changed while opening');
            const raw = atob(reply.data), bytes = Uint8Array.from(raw, c => c.charCodeAt(0));
            readBytes += bytes.length;
            title.textContent = `Opening · ${Math.floor(readBytes / Math.max(meta.size, 1) * 100)}%`;
            progress.max = meta.size; progress.value = readBytes;
            return bytes;
          } finally { reading--; }
        },
        setFocus(value) {
          if (!alive || run !== generation) return;
          if (value && new TextEncoder().encode(JSON.stringify(value)).length > 2048) throw new Error('Selection exceeds 2 KiB');
          selection = value;
          ark.reportFocus();
        },
      };
      replacement = await viewer.render(next, { ...meta, path: file.path }, api) || {};
      if (!current()) { replacement.dispose?.(); return; }
      if (ctx.accept && !await ctx.accept(meta)) {
        replacement.dispose?.(); replacement = null; next.remove();
        if (current()) return open();
        return;
      }
      if (!current()) { replacement.dispose?.(); return; }
      controller?.dispose?.();
      controller = replacement;
      next.style.visibility = '';
      // Reparenting an iframe destroys its loaded document and MessagePort in WebKit.
      // The replacement already occupies this scene; remove only the former view.
      for (const child of [...scene.children]) if (child !== next) child.remove();
      state.hidden = true;
      ctx.loaded?.(meta);
      ark.reportFocus();
    } catch (error) {
      replacement?.dispose?.();
      if (current()) {
        clear(state).append(h('span', String(error.message || 'This file could not be shown').slice(0, 240)),
          h('button', { type: 'button', onclick: open }, 'Retry'));
        show();
      }
    } finally {
      if (controller !== replacement) next.remove();
      clearTimeout(timer);
      close(p);
      if (pending === p) pending = null;
    }
  }
  open();
  return {
    focus: () => ({ selection }),
    update(next) { file = next; selection = null; return open(); },
    dispose() { alive = false; generation++; pending?.abort.abort(); close(pending); controller?.dispose?.(); },
  };
}
