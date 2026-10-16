(() => {
  const metadata = window.__TAURI_INTERNALS__?.metadata;
  const location = window.location;
  // Eligibility only. Tauri's unchanged capabilities still authorize every native call.
  if (window !== window.top
      || metadata?.currentWindow?.label !== 'main'
      || metadata?.currentWebview?.label !== 'main'
      || !(location.protocol === 'tauri:'
        || (location.protocol === 'http:' && location.hostname === 'tauri.localhost'))) return;
  /* __ARK_NOTIFICATION_SCRIPT__ */
})();
