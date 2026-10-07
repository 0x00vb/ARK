// Notifications from the runtime (ADR 0047). The runtime decides what to send and when: a
// message that arrives is always shown (a phone stops delivering to a worker that shows
// nothing). A tap opens the page on the item: a page already open is focused and told which.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("push", (e) => {
  let m = {};
  try {
    m = e.data ? e.data.json() : {};
  } catch {}
  e.waitUntil(
    self.registration.showNotification(m.title || "Something waits for you", {
      body: m.body || "",
      tag: m.tag || m.id || "ark",
      renotify: true,
      data: { id: m.id || "" },
    }),
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const id = e.notification.data?.id || "";
  e.waitUntil(
    (async () => {
      const open = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const c of open) {
        if (new URL(c.url).origin !== self.location.origin) continue;
        await c.focus();
        c.postMessage({ kind: "open_item", id });
        return;
      }
      await self.clients.openWindow(`/#item=${encodeURIComponent(id)}`);
    })(),
  );
});
