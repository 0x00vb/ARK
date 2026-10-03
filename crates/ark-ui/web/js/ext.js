// The extension points an app's scripts use, available as `window.ark`:
//
//   ark.panel({ id, title, icon, nav, presents, wide, fill, render(el, params, api) })
//   ark.view({ id, title, icon, nav, render(el, params, api) })
//                      a view in the main column, in place of the conversation; `nav` lists
//                      it in the sidebar. Choosing a session brings the conversation back.
//   ark.artifactViewer({ id, label, match(artifact), primary, presents, wide, render(el, artifact, api, params) })
//   ark.toolRenderer({ match(action), render(el, call, api) })
//   ark.action({ id, label, icon, where: "artifact" | "palette", when(ctx), run(ctx, api) })
//   ark.status({ id, render(el, api) })
//   ark.on(name, fn)   an app handler's events, and ARK's own as `ark:<kind>`
//                      (`ark:turn_finished`, `ark:event`, `ark:summary`, ...)
//
// A panel's or viewer's `render` may return `{ update(params), refresh(), focus(), dispose() }`.
// `focus()` says what it shows (`{ surface, selection }`); the agent sees it while visible.
// `wide` asks for a wider side pane; `fill` drops the pane's padding (editors, terminals).
//
// Nothing here knows any domain; the registry only holds what apps register.

export const registry = {
  panels: new Map(),
  views: new Map(),
  viewers: [],
  toolRenderers: [],
  actions: [],
  statuses: [],
};

const listeners = new Map();

export function onExt(name, fn) {
  if (!listeners.has(name)) listeners.set(name, new Set());
  listeners.get(name).add(fn);
  return () => listeners.get(name).delete(fn);
}

export function dispatchExt(name, payload) {
  for (const fn of listeners.get(name) || []) {
    try {
      fn(payload);
    } catch (e) {
      console.error(`ark: listener for ${name} failed`, e);
    }
  }
}

function need(obj, keys, what) {
  for (const k of keys) if (!obj || obj[k] == null) throw new Error(`ark.${what} needs \`${k}\``);
}

export function installApi(api) {
  const ark = {
    ...api,
    panel(p) {
      need(p, ["id", "title", "render"], "panel");
      registry.panels.set(p.id, p);
      api._changed();
    },
    view(v) {
      need(v, ["id", "title", "render"], "view");
      registry.views.set(v.id, v);
      api._changed();
    },
    artifactViewer(v) {
      need(v, ["id", "label", "match", "render"], "artifactViewer");
      registry.viewers.push(v);
    },
    toolRenderer(r) {
      need(r, ["match", "render"], "toolRenderer");
      registry.toolRenderers.push(r);
    },
    action(a) {
      need(a, ["id", "label", "run"], "action");
      registry.actions.push(a);
    },
    status(s) {
      need(s, ["id", "render"], "status");
      registry.statuses.push(s);
      api._changed();
    },
    on: onExt,
  };
  delete ark._changed;
  window.ark = ark;
  return ark;
}

/// Who handles a `present` directive for `view`: a panel or an artifact viewer that lists it.
export function presenterFor(view) {
  for (const p of registry.panels.values()) if ((p.presents || []).includes(view)) return { panel: p };
  for (const v of registry.viewers) if ((v.presents || []).includes(view)) return { viewer: v };
  return null;
}
