// Android bridge: the same `window.__TAURI__.core.invoke` the desktop UI uses,
// backed by MosaicBridge (MainActivity) and the Rust core.
(() => {
  const pending = new Map();
  let seq = 0;
  window.__mosaicReply = (id, json) => {
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    let r;
    try { r = JSON.parse(json); } catch (e) { p.reject(String(e)); return; }
    if (r && 'err' in r) p.reject(r.err); else p.resolve(r ? r.ok : null);
  };
  const invoke = (cmd, args = {}) => new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, { resolve, reject });
    window.MosaicBridge.invoke(id, cmd, JSON.stringify(args || {}));
  });
  window.__TAURI__ = { core: { invoke } };
  document.documentElement.dataset.platform = 'android';
})();
