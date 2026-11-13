'use strict';

// ------------------------------------------------------------------ bridge & helpers
const TAURI = window.__TAURI__;
const invoke = (cmd, args = {}) => {
  if (!TAURI) return Promise.reject('Mosaic UI opened outside the app');
  return TAURI.core.invoke(cmd, args);
};

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => Array.from(el.querySelectorAll(s));
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

let lang = 'en';
// Platform: set from the first snapshot ("windows", "android", …). The UI hides what does not apply.
const isAndroid = () => !!snap && snap.platform === 'android';
// Android has no system proxy: the saved desktop default shows as VPN there.
const uiMode = () => (isAndroid() && snap.settings.mode === 'system_proxy' ? 'tun' : snap.settings.mode);
let platformApplied = false;
function applyPlatform() {
  if (platformApplied || !snap) return;
  platformApplied = true;
  document.documentElement.dataset.platform = snap.platform || 'windows';
  applyI18n();
}
function pickLang(setting) {
  const want = setting && setting !== 'auto' ? setting : (navigator.language || 'en').slice(0, 2).toLowerCase();
  return window.I18N[want] ? want : 'en';
}
function t(key, ...args) {
  // Platform-specific wording: "key@android" / "key@linux" win over "key" there.
  const plat = snap && snap.platform;
  if (plat && plat !== 'windows' && window.I18N.en[key + '@' + plat] !== undefined) key += '@' + plat;
  const s = (window.I18N[lang] && window.I18N[lang][key]) ?? window.I18N.en[key] ?? key;
  return s.replace(/\{(\d)\}/g, (_, i) => String(args[+i] ?? ''));
}
function applyI18n() {
  document.documentElement.lang = lang;
  $$('[data-i18n]').forEach((el) => { el.textContent = t((isAndroid() && el.dataset.i18nAndroid) || el.dataset.i18n); });
  $$('[data-i18n-ph]').forEach((el) => { el.placeholder = t(el.dataset.i18nPh); });
  $$('[data-i18n-title]').forEach((el) => { el.title = t(el.dataset.i18nTitle); });
  try { fillConnSort(); } catch { /* connections code not initialised yet */ }
}

const UNITS = { en: ['B', 'KB', 'MB', 'GB', 'TB'], ru: ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ'] };
function fmtBytes(n) {
  const u = UNITS[lang] || UNITS.en;
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i ? n.toFixed(n < 10 ? 2 : n < 100 ? 1 : 0) : Math.round(n)) + ' ' + u[i];
}
const fmtRate = (n) => fmtBytes(n) + (lang === 'ru' ? '/с' : '/s');
function fmtDur(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(r).padStart(2, '0')}` : `${m}:${String(r).padStart(2, '0')}`;
}
// A toast with a button (e.g. "Open folder").
function toastAction(msg, label, fn, ms = 9000) {
  const el = document.createElement('div');
  el.className = 'toast ok with-action';
  el.innerHTML = `<span></span><button class="btn"></button>`;
  el.firstChild.textContent = String(msg);
  const b = el.lastChild;
  b.textContent = label;
  b.addEventListener('click', () => { el.remove(); fn(); });
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), ms);
}
function toast(msg, kind = 'info', ms = 4500) {
  const el = document.createElement('div');
  el.className = 'toast ' + kind;
  el.textContent = String(msg);
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), kind === 'error' ? Math.max(ms, 9000) : ms);
}
// In-app confirmation dialog (window.confirm is unreliable in embedded web views:
// Android's WebView answers "Cancel" without showing anything).
function ask(message, okLabel) {
  return new Promise((resolve) => {
    const wrap = document.createElement('div');
    wrap.className = 'modal-wrap';
    wrap.innerHTML = `<div class="modal card" role="dialog" aria-modal="true">
      <div class="modal-text"></div>
      <div class="modal-actions"><button class="btn ghost" data-a="0"></button><button class="btn primary" data-a="1"></button></div>
    </div>`;
    $('.modal-text', wrap).textContent = message;
    $('[data-a="0"]', wrap).textContent = t('dlg.cancel');
    $('[data-a="1"]', wrap).textContent = okLabel || t('dlg.ok');
    const done = (v) => { wrap.remove(); document.removeEventListener('keydown', onKey); resolve(v); };
    const onKey = (e) => { if (e.key === 'Escape') done(false); if (e.key === 'Enter') done(true); };
    wrap.addEventListener('click', (e) => {
      if (e.target === wrap) done(false);
      const a = e.target.closest('[data-a]');
      if (a) done(a.dataset.a === '1');
    });
    document.addEventListener('keydown', onKey);
    document.body.appendChild(wrap);
    $('[data-a="1"]', wrap).focus();
  });
}

async function withBusy(btn, fn) {
  if (btn) btn.disabled = true;
  try { return await fn(); }
  catch (e) { toast(e, 'error'); }
  finally { if (btn) btn.disabled = false; }
}
const engineName = (k) => (k === 'xray' ? 'Xray' : 'sing-box');
const engineBadge = (k) => k ? `<span class="badge ${k}">${engineName(k)}</span>` : `<span class="badge none">${t('nodes.noEngine')}</span>`;
function avatar(name) {
  const m = name.match(/^\p{RI}\p{RI}/u);
  return m ? m[0] : (Array.from(name.trim())[0] || '?').toUpperCase();
}

// ------------------------------------------------------------------ state
let snap = null;
let view = 'home';
let rulesDraft = null;
let nodesKey = '';
const traffic = { up: [], down: [] };
let last = null;

// ------------------------------------------------------------------ navigation
$$('.nav button').forEach((b) => b.addEventListener('click', () => show(b.dataset.view)));
// The window is destroyed when it goes to the tray and built again when opened: come back to
// the same screen if it was closed recently (a fresh start opens Home).
function saveView() { try { localStorage.setItem('mosaic.view', JSON.stringify({ v: view, at: Date.now() })); } catch {} }
window.addEventListener('pagehide', saveView);
window.addEventListener('beforeunload', saveView);
let viewRestored = false;
function restoreView() {
  if (viewRestored) return;
  viewRestored = true;
  try {
    const s = JSON.parse(localStorage.getItem('mosaic.view') || 'null');
    if (s && s.v && s.v !== 'home' && Date.now() - s.at < 30 * 60e3 && $('#view-' + s.v)) show(s.v);
  } catch {}
}

function show(v) {
  view = v;
  saveView();
  $$('.nav button').forEach((b) => b.classList.toggle('active', b.dataset.view === v));
  $$('.view').forEach((s) => s.classList.toggle('active', s.id === 'view-' + v));
  if (v === 'rules') { rulesDraft = null; renderRules(); }
  if (v === 'settings') renderSettings();
  if (v === 'nodes') { nodesKey = ''; renderNodes(); }
  if (v === 'engines') { renderEngines(); if (!Object.keys(versions).length) loadEngineVersions().catch(() => {}); }
  if (v === 'home' && snap) renderHome();
  tickView();
}

function rerenderAll() {
  applyI18n();
  nodesKey = '';
  if (!snap) return;
  renderStatus(false);
  renderHome();
  if (view === 'nodes') renderNodes();
  if (view === 'rules') renderRules();
  if (view === 'engines') renderEngines();
  if (view === 'settings') renderSettings();
}

// ------------------------------------------------------------------ app update
let appUpdating = false;
function renderUpdateBanner() {
  // Android: updates come as a new APK from the release page / store, not from inside the app.
  const u = snap && !isAndroid() && snap.app_update;
  $('#update-banner').classList.toggle('hidden', !u);
  if (u) {
    $('#update-text').textContent = t('upd.available', u.version);
    $('#update-text').title = u.notes || '';
    // No installer for this platform (Android): open the release page instead.
    $('#update-install').textContent = appUpdating ? t('upd.downloading') : u.installer ? t('upd.install') : t('upd.open');
  }
}
// Note: event.currentTarget is null once the handler has awaited, so the button is kept here.
const updateBtn = $('#update-install');
updateBtn.addEventListener('click', () => withBusy(updateBtn, async () => {
  if (!snap.app_update.installer) { await invoke('open_url', { url: snap.app_update.page }); return; }
  if (!(await ask(t('upd.confirm', snap.app_update.version), t('upd.install')))) return;
  appUpdating = true;
  updateBtn.textContent = t('upd.downloading');
  try {
    await invoke('install_app_update');
  } finally {
    appUpdating = false;
    renderUpdateBanner();
  }
}));

// ------------------------------------------------------------------ polling
async function poll() {
  try {
    snap = await invoke('snapshot');
    const l = pickLang(snap.settings.ui_language);
    if (l !== lang) { lang = l; rerenderAll(); }
    applyPlatform();
    renderStatus(true);
    restoreView();
    if (!appVisible) { pollTimer = setTimeout(poll, 5000); return; }
    renderUpdateBanner();
    if (view === 'home') renderHome();
    if (view === 'nodes') renderNodes();
    if (view === 'engines') renderEngines();
  } catch (e) {
    console.error(e);
  }
  pollTimer = setTimeout(poll, appVisible ? 1000 : 5000);
}

// Hidden in the tray or minimized, the page still runs (WebView2 does not know the window is
// hidden): stop the animations, the per-second redraws and slow the polling down — otherwise
// the GPU process keeps rendering a picture nobody sees.
let appVisible = true, pollTimer = null;
function setAppVisible(v) {
  if (appVisible === v) return;
  appVisible = v;
  document.documentElement.classList.toggle('app-hidden', !v);
  if (v) { clearTimeout(pollTimer); poll(); }
}
document.addEventListener('visibilitychange', () => setAppVisible(!document.hidden));
try {
  if (TAURI && TAURI.event) TAURI.event.listen('mosaic://visible', (e) => setAppVisible(!!e.payload));
  if (TAURI && TAURI.window) {
    const w = TAURI.window.getCurrentWindow();
    Promise.all([w.isVisible(), w.isMinimized()]).then(([vis, min]) => setAppVisible(vis && !min)).catch(() => {});
  }
} catch (e) { console.error(e); }

async function tickView() {
  if (!appVisible) return;
  try {
    if (view === 'conns') renderConns(await invoke('connections'));
    if (view === 'logs') await pollLogs();
  } catch (e) { console.error(e); }
}
setInterval(tickView, 1000);

// ------------------------------------------------------------------ status / home
function selectedNode() {
  return snap && snap.nodes.find((n) => n.id === snap.selected);
}

// One state for the whole UI, so what the user sees always matches what happens:
//   off · connecting · reloading · disconnecting · on · degraded · error
const op = { kind: null, started: 0, progress: null, error: null };
function phase() {
  if (!snap) return 'off';
  if (op.kind) return op.kind;
  if (snap.status.connected) return snap.status.health && snap.status.health.state === 'degraded' ? 'degraded' : 'on';
  if (snap.status.kill_switch === 'blocking') return 'blocked';
  return op.error ? 'error' : 'off';
}
const busy = () => !!op.kind;
const fmtSecs = (s) => (lang === 'ru' ? s.toFixed(1).replace('.', ',') + ' с' : s.toFixed(1) + ' s');

let progressTimer = null;
function watchProgress() {
  clearInterval(progressTimer);
  progressTimer = setInterval(async () => {
    if (!op.kind) { clearInterval(progressTimer); return; }
    try { op.progress = await invoke('progress'); } catch { return; }
    renderHero();
  }, 250);
}

/// Run a connect / disconnect / reconnect with the UI showing every step of it.
async function runOp(kind, fn) {
  if (op.kind) return { err: 'busy', secs: 0 };
  Object.assign(op, { kind, started: performance.now(), progress: null, error: null });
  if (kind !== 'disconnecting') watchProgress();
  renderLive();
  let err = null;
  try { await fn(); } catch (e) { err = String(e); }
  const secs = (performance.now() - op.started) / 1000;
  op.kind = null; op.progress = null;
  try { snap = await invoke('snapshot'); } catch { /* next poll */ }
  if (err && kind !== 'disconnecting') op.error = err;
  renderLive();
  return { err, secs };
}

async function connectNow() {
  if (!snap || busy()) return;
  if (!snap.nodes.length) { show('nodes'); $('#import-panel').classList.remove('hidden'); return; }
  const r = await runOp('connecting', () => invoke('connect'));
  if (!r.err) { mosaicBurst(); if (view !== 'home') toast(t('toast.connectedIn', fmtSecs(r.secs)), 'ok', 2500); }
  else if (view !== 'home') toast(errorText(parseError(r.err)).title, 'error');
}
async function disconnectNow() {
  if (!snap || busy()) return;
  op.error = null;
  await runOp('disconnecting', () => invoke('disconnect'));
}

// Settings that make the backend reconnect (mirrors Controller::set_settings).
function needsRestart(a, b) {
  if (!snap.status.connected) return false;
  if (isAndroid()) return true; // the VPN service reconnects on every change
  return a.mode !== b.mode || a.listen_port !== b.listen_port || a.block_quic !== b.block_quic || a.tun_stack !== b.tun_stack || !!a.kill_switch !== !!b.kill_switch
    || a.direct_dns !== b.direct_dns || a.dns_leak_protection !== b.dns_leak_protection || a.allow_lan !== b.allow_lan
    || (b.mode === 'tun' && b.tun_stack === 'singbox' && a.remote_dns !== b.remote_dns);
}

/// Save settings. A change that needs a reconnect shows it (mosaic reshuffles, steps,
/// "applied · reconnected in 1.4 s"); one that applies live says so.
async function applyChange(settings, label) {
  if (needsRestart(snap.settings, settings)) return reloadAround(() => invoke('set_settings', { settings }), label);
  try {
    await invoke('set_settings', { settings });
    snap = await invoke('snapshot');
    toast(snap.status.connected ? t('toast.appliedLive', label) : t('settings.saved'), 'ok', 2200);
  } catch (e) { toast(e, 'error'); snap = await invoke('snapshot').catch(() => snap); }
  renderLive();
}

async function reloadAround(fn, label) {
  if (!snap.status.connected) {
    try { await fn(); toast(t('settings.saved'), 'ok', 2000); } catch (e) { toast(e, 'error'); }
    snap = await invoke('snapshot').catch(() => snap);
    renderLive();
    return;
  }
  const r = await runOp('reloading', fn);
  if (r.err) toast(t('toast.reloadFailed', errorText(parseError(r.err)).title), 'error', 6000);
  else { mosaicBurst(); toast(t('toast.reloaded', label, fmtSecs(r.secs)), 'ok', 3000); }
}

function parseError(e) {
  let m = /^\[node-down\] (.*?) :: ([\s\S]*)$/.exec(e);
  if (m) return { kind: 'node', name: m[1], detail: m[2] };
  m = /^\[no-node\] (.*?) :: ([\s\S]*)$/.exec(e);
  if (m) return { kind: 'none', name: m[1], detail: m[2] };
  return { kind: 'other', detail: e };
}
function errorText(p) {
  if (p.kind === 'node') return { title: t('err.nodeDown.t'), sub: t('err.nodeDown.d', p.name) };
  if (p.kind === 'none') return { title: t('err.noNode.t'), sub: t('err.noNode.d') };
  return { title: t('err.other.t'), sub: p.detail };
}

// ---- the mosaic: 4×4 tiles that assemble while connecting
const MOSAIC_ORDER = [5, 10, 0, 15, 6, 9, 3, 12, 1, 14, 4, 11, 2, 13, 7, 8]; // tiles light up in this order
const STEP_LIT = { prepare: 2, engines: 6, check: 10, route: 13, dns: 15 };
(function buildMosaic() {
  const rnd = (i, k) => ((Math.sin(i * 12.9898 + k * 78.233) * 43758.5453) % 1 + 1) % 1; // stable per tile
  $('#mosaic').innerHTML = Array.from({ length: 16 }, (_, i) => {
    const r = i >> 2, c = i & 3, q = (r >> 1) * 2 + (c >> 1);
    const dist = Math.hypot(r - 1.5, c - 1.5).toFixed(2);
    return `<i style="--q:var(--q${q});--g:var(--g${q});--d:${r + c};--r:${dist};--sx:${(rnd(i, 1) * 10 - 5).toFixed(1)}px;--sy:${(rnd(i, 2) * 10 - 5).toFixed(1)}px;--sr:${(rnd(i, 3) * 24 - 12).toFixed(0)}deg"></i>`;
  }).join('');
})();
function renderMosaic(ph) {
  const m = $('#mosaic');
  const step = op.progress && op.progress.step;
  const lit = ph === 'on' || ph === 'degraded' ? 16 : (ph === 'connecting' || ph === 'reloading') ? (STEP_LIT[step] || 1) : 0;
  const tiles = m.children;
  MOSAIC_ORDER.forEach((idx, k) => tiles[idx].classList.toggle('lit', k < lit));
  // Live traffic makes the connected mosaic glow brighter.
  const rate = (traffic.down.at(-1) || 0) + (traffic.up.at(-1) || 0);
  // In steps of a quarter: the glow is redrawn only when the level really changes.
  const act = String(Math.round(Math.min(1, Math.log10(rate + 1) / 7) * 4) / 4);
  if (m.style.getPropertyValue('--act') !== act) m.style.setProperty('--act', act);
  m.disabled = busy();
  m.title = ph === 'on' || ph === 'degraded' ? t('hero.disconnect') : t('hero.connect');
}
function mosaicBurst() {
  const m = $('#mosaic');
  m.classList.remove('burst'); void m.offsetWidth; m.classList.add('burst');
  setTimeout(() => m.classList.remove('burst'), 1100);
}

// ---- status in the sidebar and on Home
function renderStatus(sample) {
  const st = snap.status;
  if (sample) {
    const now = Date.now();
    if (last) {
      const dt = Math.max(0.2, (now - last.t) / 1000);
      traffic.up.push(Math.max(0, (st.up - last.up) / dt));
      traffic.down.push(Math.max(0, (st.down - last.down) / dt));
      if (traffic.up.length > 120) { traffic.up.shift(); traffic.down.shift(); }
    }
    last = { t: now, up: st.up, down: st.down };
  }
  const ph = phase();
  const dot = { on: ' on', degraded: ' warn', connecting: ' busy', reloading: ' busy', disconnecting: ' busy', error: ' err', blocked: ' err' }[ph] || '';
  $('#side-dot').className = 'dot' + dot;
  $('#side-status').textContent = t('phase.' + ph);
  const sel = selectedNode();
  $('#side-sub').textContent = st.connected && sel ? sel.name : 'Mosaic ' + (snap.version || '');
}

function renderLive() {
  if (!snap) return;
  renderStatus(false);
  if (view === 'home') renderHome();
}

function viaText(st) {
  return st.tun && isAndroid() ? t('hero.viaVpn')
    : st.tun
    ? (st.tun_stack === 'native' ? t('hero.viaTunNative') : t('hero.viaTun')) + (snap.platform === 'windows' ? ' · ' + (st.dns_protected ? t('hero.dnsOk') : t('hero.dnsOff')) : '')
    : st.system_proxy ? t('hero.viaProxy', st.listen_port)
    : t('hero.viaPort', (st.lan ? (snap.lan_ip || t('hero.lanAddr')) : '127.0.0.1') + ':' + st.listen_port);
}

function stepItems() {
  const p = op.progress || {};
  const mode = uiMode();
  const items = [['engines', t('step.engines')]];
  const n = selectedNode();
  items.push(['check', p.step === 'check' && p.detail === '*' ? t('step.search') : t('step.check', (p.step === 'check' && p.detail) || (n ? n.name : ''))]);
  items.push(['route', t('step.route.' + (mode === 'tun' ? 'tun' : mode === 'proxy' ? 'proxy' : 'system'))]);
  if (mode === 'tun' && snap.platform === 'windows' && snap.settings.dns_leak_protection) items.push(['dns', t('step.dns')]);
  const order = ['prepare', 'engines', 'check', 'route', 'dns'];
  const cur = order.indexOf(p.step || 'prepare');
  return items.map(([k, label]) => {
    const i = order.indexOf(k);
    const cls = i < cur ? 'done' : i === cur ? 'active' : '';
    return `<li class="${cls}"><span class="st-dot"></span>${esc(label)}</li>`;
  }).join('');
}

function renderHero() {
  if (!snap) return;
  const ph = phase();
  const st = snap.status;
  const hero = $('#hero');
  hero.dataset.phase = ph;
  renderMosaic(ph);
  const n = selectedNode();
  let title = t('phase.' + ph), sub = '', actions = '', meta = '';
  const btn = (act, label, cls = '') => `<button class="btn ${cls}" data-hero="${act}">${esc(label)}</button>`;
  if (ph === 'off') {
    sub = snap.nodes.length ? (n ? t('hero.ready', n.name) : t('hero.click')) : t('hero.addNode');
    actions = snap.nodes.length ? btn('connect', t('hero.connect'), 'primary big') : btn('add', t('home.add'), 'primary big');
  } else if (ph === 'connecting' || ph === 'reloading' || ph === 'disconnecting') {
    sub = ph === 'reloading' ? t('hero.applying') : ph === 'connecting' && n ? n.name : '';
    if (ph !== 'disconnecting') {
      const el = $('#steps');
      el.innerHTML = stepItems();
    }
  } else if (ph === 'on') {
    sub = viaText(st);
    actions = btn('disconnect', t('hero.disconnect'), 'ghost big');
    const h = st.health || {};
    meta = [st.since_ms ? t('home.online', fmtDur(Date.now() - st.since_ms)) : '', h.ms != null ? t('hero.answers', h.ms) : '', st.kill_switch === 'armed' ? t('hero.ksOn') : ''].filter(Boolean).join(' · ');
  } else if (ph === 'degraded') {
    sub = t('hero.degraded', n ? n.name : '');
    actions = btn('pick', t('hero.pick'), 'primary') + (snap.settings.smart_connect ? '' : btn('smart', t('hero.smartOn'))) + btn('disconnect', t('hero.disconnect'), 'ghost');
    meta = viaText(st);
  } else if (ph === 'blocked') {
    sub = t('hero.blocked');
    actions = btn('connect', t('hero.connect'), 'primary') + btn('unblock', t('hero.unblock'), 'ghost');
    if (op.error) meta = t('err.detail', errorText(parseError(op.error)).title);
  } else if (ph === 'error') {
    const p = parseError(op.error);
    const e = errorText(p);
    title = e.title; sub = e.sub;
    if (p.kind === 'node') {
      actions = btn('pick', t('hero.pickOther'), 'primary') + btn('smart-connect', t('hero.smartConnect')) + btn('connect', t('hero.retry'), 'ghost');
      meta = t('err.detail', p.detail);
    } else if (p.kind === 'none') {
      actions = btn('connect', t('hero.retry'), 'primary') + btn('subs', t('nodes.updateSubs')) + btn('logs', t('nav.logs'), 'ghost');
      meta = t('err.detail', p.detail);
    } else {
      actions = btn('connect', t('hero.retry'), 'primary') + btn('logs', t('nav.logs'), 'ghost');
    }
  }
  $('#hero-state').textContent = title;
  $('#hero-sub').textContent = sub;
  $('#steps').classList.toggle('hidden', !(ph === 'connecting' || ph === 'reloading'));
  const acts = $('#hero-actions');
  if (acts.dataset.key !== ph + actions) { acts.innerHTML = actions; acts.dataset.key = ph + actions; }
  $('#hero-meta').textContent = meta;
}

$('#hero-actions').addEventListener('click', async (e) => {
  const b = e.target.closest('[data-hero]');
  if (!b) return;
  switch (b.dataset.hero) {
    case 'connect': return connectNow();
    case 'disconnect': return disconnectNow();
    case 'add': show('nodes'); $('#import-panel').classList.remove('hidden'); return;
    case 'pick': return nodeSheet();
    case 'smart': await invoke('set_smart', { on: true }); snap = await invoke('snapshot'); toast(t('smart.on'), 'ok', 3000); return renderLive();
    case 'smart-connect': await invoke('set_smart', { on: true }); snap = await invoke('snapshot'); return connectNow();
    case 'subs': return withBusy(b, async () => { await invoke('update_subscriptions'); snap = await invoke('snapshot'); toast(t('settings.saved'), 'ok', 2000); });
    case 'logs': return show('logs');
    case 'unblock':
      return withBusy(b, async () => {
        try { await invoke('release_kill_switch'); op.error = null; toast(t('toast.unblocked'), 'ok', 3000); }
        catch (e) { toast(e, 'error'); }
        snap = await invoke('snapshot'); renderLive();
      });
  }
});
$('#mosaic').addEventListener('click', () => {
  const ph = phase();
  if (ph === 'on' || ph === 'degraded') disconnectNow(); else if (ph === 'off' || ph === 'error' || ph === 'blocked') connectNow();
});

function renderHome() {
  const st = snap.status;
  renderHero();
  const mode = uiMode();
  $$('#mode-seg button').forEach((b) => { b.classList.toggle('active', b.dataset.mode === mode); b.disabled = busy(); });
  const port = snap.settings.listen_port;
  $('#mode-hint').textContent = mode === 'tun' && isAndroid() ? t('hint.vpn')
    : mode === 'tun'
    ? (!snap.is_admin ? t('hint.tunAdmin') : snap.settings.tun_stack === 'native' ? t('hint.tunNative') : t('hint.tunSingbox'))
    : mode === 'proxy' ? t('hint.proxyOnly', (snap.settings.allow_lan ? (snap.lan_ip || t('hero.lanAddr')) : '127.0.0.1') + ':' + port)
    : t('hint.proxy');

  const n = selectedNode();
  const card = $('#current-node');
  const key = JSON.stringify([lang, n && [n.id, n.name, n.latency, n.effective], snap.settings.smart_connect, snap.nodes.length]);
  if (card.dataset.key !== key) {
    card.dataset.key = key;
    if (n) {
      card.innerHTML = `
        <div class="flag">${esc(avatar(n.name))}</div>
        <div class="grow"><div class="title">${esc(n.name)}</div>
        <div class="sub">${snap.settings.smart_connect ? `<span class="badge auto">${t('smart.badge')}</span> ` : ''}${esc(n.summary)} · ${engineBadge(n.effective)}</div></div>
        <span class="lat ${latClass(n)}" title="${esc(latTip(n))}">${latHtml(n, false)}</span>
        <span class="chev">${t('home.change')} ▾</span>`;
    } else {
      card.innerHTML = `<div class="grow"><div class="title">${t('home.noNodes')}</div><div class="sub">${t('home.importHint')}</div></div><span class="chev">${t('home.add')} ＋</span>`;
    }
  }

  const up = traffic.up.at(-1) || 0, down = traffic.down.at(-1) || 0;
  $('#rate-up').textContent = fmtRate(up);
  $('#rate-down').textContent = fmtRate(down);
  $('#total-up').textContent = t('home.total', fmtBytes(st.up));
  $('#total-down').textContent = t('home.total', fmtBytes(st.down));
  $('#conn-count').textContent = st.active_conns;
  $('#uptime').textContent = st.connected && st.since_ms ? t('home.online', fmtDur(Date.now() - st.since_ms)) : '—';
  drawSpark();
}
$('#current-node').addEventListener('click', () => {
  if (!snap) return;
  if (!snap.nodes.length) { show('nodes'); $('#import-panel').classList.remove('hidden'); return; }
  nodeSheet();
});

function drawSpark() {
  const c = $('#spark');
  const w = c.clientWidth, h = 120, dpr = window.devicePixelRatio || 1;
  if (!w || !appVisible) return;
  // Whole pixels: with 125 % / 150 % scaling w × dpr is fractional, the canvas never matched
  // and was re-allocated (a new GPU buffer) every second.
  const W = Math.round(w * dpr), H = Math.round(h * dpr);
  if (c.width !== W || c.height !== H) { c.width = W; c.height = H; }
  const g = c.getContext('2d');
  g.setTransform(W / w, 0, 0, H / h, 0, 0);
  g.clearRect(0, 0, w, h);
  const max = Math.max(1024, ...traffic.up, ...traffic.down) * 1.15;
  g.strokeStyle = 'rgba(255,255,255,0.05)';
  for (let i = 1; i < 4; i++) { g.beginPath(); g.moveTo(0, (h / 4) * i); g.lineTo(w, (h / 4) * i); g.stroke(); }
  const line = (arr, color, fill) => {
    if (arr.length < 2) return;
    const step = w / 119, off = w - (arr.length - 1) * step;
    g.beginPath();
    arr.forEach((v, i) => { const x = off + i * step, y = h - 4 - (v / max) * (h - 12); i ? g.lineTo(x, y) : g.moveTo(x, y); });
    g.strokeStyle = color; g.lineWidth = 2; g.lineJoin = 'round'; g.stroke();
    g.lineTo(w, h); g.lineTo(off, h); g.closePath(); g.fillStyle = fill; g.fill();
  };
  line(traffic.down, '#38bdf8', 'rgba(56,189,248,0.10)');
  line(traffic.up, '#f472b6', 'rgba(244,114,182,0.08)');
  g.fillStyle = '#8a90a8'; g.font = '11px Segoe UI, sans-serif';
  g.fillText(fmtRate(max / 1.15), 6, 14);
}

$$('#mode-seg button').forEach((b) => b.addEventListener('click', async () => {
  if (!snap || busy() || uiMode() === b.dataset.mode) return;
  if (b.dataset.mode === 'tun' && !snap.is_admin) toast(t('toast.tunNeedsAdmin'), 'info', 7000);
  await applyChange({ ...snap.settings, mode: b.dataset.mode }, t('toast.modeLabel', b.textContent));
}));

// ---- quick node switch (a sheet on phones, a dialog on desktops)
function nodeSheet() {
  const wrap = document.createElement('div');
  wrap.className = 'modal-wrap sheet-wrap';
  wrap.innerHTML = `<div class="modal card node-sheet" role="dialog" aria-modal="true">
    <div class="ns-head"><h3>${t('sheet.title')}</h3><button class="btn" data-a="test">${t('nodes.test')}</button><button class="icon-btn" data-a="close" aria-label="${t('dlg.cancel')}">×</button></div>
    <input class="ns-filter" placeholder="${esc(t('nodes.filterPh'))}" />
    <label class="ns-auto"><span class="ns-auto-text"><b>${t('sheet.auto')}</b><span class="muted small">${t('sheet.autoD')}</span></span>
      <span class="switch"><input type="checkbox" class="ns-smart" ${snap.settings.smart_connect ? 'checked' : ''} /><span></span></span></label>
    <div class="ns-list"></div>
  </div>`;
  const list = $('.ns-list', wrap);
  const subName = (src) => { const s = snap.subscriptions.find((x) => x.url === src); return s ? (s.name || s.url) : t('subs.manual'); };
  const draw = () => {
    const f = $('.ns-filter', wrap).value.trim().toLowerCase();
    const nodes = snap.nodes.filter((n) => !f || `${n.name} ${n.summary} ${n.server}`.toLowerCase().includes(f))
      .sort((a, b) => latencyKey(a) - latencyKey(b));
    list.innerHTML = nodes.map((n) => `<button class="ns-row ${n.id === snap.selected ? 'sel' : ''}" data-id="${esc(n.id)}">
        <span class="flag">${esc(avatar(n.name))}</span>
        <span class="ns-main"><span class="ns-name">${esc(n.name)}</span><span class="muted small">${esc(n.summary)} · ${esc(subName(n.source))}</span></span>
        <span class="lat ${latClass(n)}" title="${esc(latTip(n))}">${latHtml(n, false)}</span></button>`).join('')
      || `<div class="empty">${t('nodes.nothing')}</div>`;
  };
  const close = () => { wrap.remove(); document.removeEventListener('keydown', onKey); };
  const onKey = (e) => { if (e.key === 'Escape') close(); };
  $('.ns-filter', wrap).addEventListener('input', draw);
  $('.ns-smart', wrap).addEventListener('change', async (e) => {
    await invoke('set_smart', { on: e.target.checked });
    snap = await invoke('snapshot');
    toast(e.target.checked ? t('smart.on') : t('toast.smartOff'), 'ok', 2500);
    draw(); renderLive();
  });
  wrap.addEventListener('click', async (e) => {
    if (e.target === wrap || e.target.closest('[data-a="close"]')) return close();
    const test = e.target.closest('[data-a="test"]');
    if (test) {
      return withBusy(test, async () => { await invoke('test_latency'); snap = await invoke('snapshot'); draw(); });
    }
    const row = e.target.closest('.ns-row');
    if (!row) return;
    const id = row.dataset.id;
    const n = snap.nodes.find((x) => x.id === id);
    close();
    if (id === snap.selected && !snap.settings.smart_connect) return;
    try {
      await invoke('select_node', { id });
      snap = await invoke('snapshot');
      nodesKey = '';
      toast(snap.status.connected ? t('toast.nodeLive', n.name) : t('toast.nodeSet', n.name), 'ok', 2500);
      if (op.error && !snap.status.connected) { op.error = null; }
    } catch (err) { toast(err, 'error'); }
    renderLive();
  });
  document.addEventListener('keydown', onKey);
  document.body.appendChild(wrap);
  draw();
  if (!isAndroid()) $('.ns-filter', wrap).focus();
}

// ------------------------------------------------------------------ nodes
function latClass(n) {
  if (!n.latency) return '';
  if (n.latency.ms == null) return 'bad';
  if (n.latency.rtt != null) return n.latency.rtt < 150 ? 'good' : n.latency.rtt < 400 ? 'mid' : 'bad';
  return n.latency.ms < 300 ? 'good' : n.latency.ms < 800 ? 'mid' : 'bad';
}
// Round trip (like "ping" in speed tests) first; the cost of a new connection underneath.
function latHtml(n, withError) {
  const l = n.latency;
  if (!l) return '';
  if (l.ms == null) return withError ? t('nodes.error') : '';
  if (l.rtt == null) return `${l.ms} ms`;
  return `${l.rtt} ms<small>${t('lat.conn', l.ms)}</small>`;
}
function latTip(n) {
  const l = n.latency;
  if (!l) return '';
  if (l.error) return l.error;
  return l.rtt != null ? t('lat.tip', l.rtt, l.ms) : t('lat.tipConn', l.ms);
}

// Collapsed node groups (subscription URL or "manual"), remembered per browser.
const collapsed = new Set((() => { try { return JSON.parse(localStorage.getItem('mosaic.collapsed') || '[]'); } catch { return []; } })());
function saveCollapsed() { try { localStorage.setItem('mosaic.collapsed', JSON.stringify([...collapsed])); } catch {} }
let nodeSort = (() => { try { return localStorage.getItem('mosaic.nodeSort') || 'order'; } catch { return 'order'; } })();

function latencyKey(n) {
  const l = n.latency;
  if (!l) return 1e9;            // not tested: after tested ones
  if (l.ms == null) return 2e9;  // failed: last
  return l.rtt ?? l.ms;
}

// Subscription header: title, traffic used / left, expiry, last update, actions.
function subHeader(s, count, open) {
  const locale = lang === 'ru' ? 'ru-RU' : undefined;
  const used = (s.upload || 0) + (s.download || 0);
  let traffic = '';
  if (s.total) {
    const pct = Math.min(100, Math.round((used / s.total) * 100));
    const cls = pct >= 90 ? 'bad' : pct >= 75 ? 'mid' : 'good';
    traffic = `<div class="sub-traffic"><div class="bar ${cls}"><span style="width:${pct}%"></span></div>
      <span class="small">${t('subs.used', fmtBytes(used), fmtBytes(s.total))} · ${t('subs.left', fmtBytes(Math.max(0, s.total - used)))}</span></div>`;
  } else if (used) {
    traffic = `<div class="sub-traffic"><span class="small">${t('subs.usedOnly', fmtBytes(used))}</span></div>`;
  }
  let expire = '';
  if (s.expire) {
    const days = Math.ceil((s.expire * 1000 - Date.now()) / 86400000);
    const date = new Date(s.expire * 1000).toLocaleDateString(locale, { day: '2-digit', month: '2-digit', year: 'numeric' });
    const cls = days <= 0 ? 'bad' : days <= 3 ? 'mid' : '';
    expire = `<span class="sub-exp ${cls}">${days <= 0 ? t('subs.expired', date) : t('subs.until', date, days)}</span>`;
  }
  const upd = s.updated_ms ? new Date(s.updated_ms).toLocaleString(locale, { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' }) : '—';
  return `<div class="group-head sub ${s.last_error ? 'err' : ''}" data-group="${esc(s.url)}">
    <button class="caret ${open ? 'open' : ''}" data-act="toggle" title="${t('subs.toggle')}">▸</button>
    <div class="gh-main">
      <div class="gh-title"><span>${esc(s.name || s.url)}</span><span class="muted small">${t(count === 1 ? 'subs.node1' : 'subs.nodes', count)}</span></div>
      <div class="gh-meta muted small">${expire}<span title="${esc(s.url)}">${t('subs.updated', upd)}</span></div>
      ${traffic}
      ${s.last_error ? `<div class="gh-err small">${esc(s.last_error)}</div>` : ''}
    </div>
    <div class="gh-actions">
      <button class="icon-btn" data-act="update" title="${t('subs.update')}">⟳</button>
      <button class="icon-btn" data-act="delete" title="${t('nodes.subDelete')}">×</button>
    </div>
  </div>`;
}

function nodeRow(n) {
  const lat = latHtml(n, true);
  const latTitle = esc(latTip(n));
  const autoLabel = n.engine === 'auto' && n.effective ? ` (${engineName(n.effective)})` : '';
  const opt = (v, label, ok) => `<option value="${v}" ${n.engine === v ? 'selected' : ''} ${ok ? '' : 'disabled'}>${label}</option>`;
  return `
    <div class="node ${n.id === snap.selected ? 'selected' : ''}" data-id="${n.id}">
      <div class="radio"></div>
      <div style="min-width:0">
        <div class="node-name">${esc(n.name)}</div>
        <div class="node-meta">${esc(n.summary)} · ${esc(n.server)}:${n.port}</div>
      </div>
      ${engineBadge(n.effective)}
      <span class="lat ${latClass(n)}" title="${latTitle}">${lat}</span>
      <select class="engine-sel" title="${esc(n.effective_error || t('nodes.engineTitle'))}">
        ${opt('auto', 'Auto' + autoLabel, true)}
        ${opt('xray', 'Xray', n.supported.includes('xray'))}
        ${opt('singbox', 'sing-box', n.supported.includes('singbox'))}
      </select>
      <button class="icon-btn del" title="${t('nodes.delete')}">×</button>
    </div>`;
}

function renderNodes() {
  const filter = $('#node-filter').value.trim().toLowerCase();
  const key = JSON.stringify([lang, nodeSort, smartShown, [...collapsed], snap.settings.smart_connect, snap.selected, filter, snap.nodes.map((n) => [n.id, n.name, n.engine, n.effective, n.latency, n.source]), snap.subscriptions]);
  if (key === nodesKey) return;
  nodesKey = key;

  const smart = !!snap.settings.smart_connect;
  $('#smart-toggle').checked = smart;
  $('#btn-smart').classList.toggle('on', smart);
  $('#btn-smart').setAttribute('aria-expanded', String(smartShown));
  $('#smart-card').classList.toggle('hidden', !smartShown);
  const active = selectedNode();
  $('#smart-state').textContent = smart && active ? t('smart.active', active.name) : '';
  $('#node-sort').value = nodeSort;

  const match = (n) => !filter || [n.name, n.summary, n.server].join(' ').toLowerCase().includes(filter);
  const sorted = (list) => {
    const l = list.slice();
    if (nodeSort === 'latency') l.sort((a, b) => latencyKey(a) - latencyKey(b));
    else if (nodeSort === 'name') l.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
    return l;
  };
  const shown = snap.nodes.filter(match);
  $('#node-count').textContent = snap.nodes.length ? t('nodes.count', shown.length, snap.nodes.length) : '';
  if (!snap.nodes.length && !snap.subscriptions.length) {
    $('#node-list').innerHTML = `<div class="empty">${t('nodes.empty')}</div>`;
    return;
  }

  // Groups: each subscription (even without nodes, to show errors/traffic), then manual nodes.
  const known = new Set(snap.subscriptions.map((s) => s.url));
  const groups = snap.subscriptions.map((s) => ({ id: s.url, sub: s, nodes: shown.filter((n) => n.source === s.url) }));
  const manual = shown.filter((n) => !n.source || !known.has(n.source));
  if (manual.length || !snap.subscriptions.length) groups.push({ id: 'manual', sub: null, nodes: manual });
  const single = groups.length === 1 && !groups[0].sub;

  $('#node-list').innerHTML = groups.map((g) => {
    // While searching, show matching nodes even in collapsed groups.
    const open = filter ? true : !collapsed.has(g.id);
    const head = single ? '' : g.sub ? subHeader(g.sub, g.nodes.length, open)
      : `<div class="group-head" data-group="manual"><button class="caret ${open ? 'open' : ''}" data-act="toggle">▸</button>
          <div class="gh-main"><div class="gh-title"><span>${t('subs.manual')}</span><span class="muted small">${t(g.nodes.length === 1 ? 'subs.node1' : 'subs.nodes', g.nodes.length)}</span></div></div></div>`;
    if (filter && !g.nodes.length) return '';
    return `<div class="group">${head}${open ? `<div class="group-body">${sorted(g.nodes).map(nodeRow).join('')}</div>` : ''}</div>`;
  }).join('') || `<div class="empty">${t('nodes.nothing')}</div>`;

  $$('#node-list .group-head').forEach((h) => {
    const id = h.dataset.group;
    $$('[data-act]', h).forEach((b) => b.addEventListener('click', (e) => {
      e.stopPropagation();
      const act = b.dataset.act;
      if (act === 'toggle') {
        if (collapsed.has(id)) collapsed.delete(id); else collapsed.add(id);
        saveCollapsed(); nodesKey = ''; renderNodes();
      } else if (act === 'update') {
        withBusy(b, async () => {
          const r = await invoke('update_subscription', { url: id });
          snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
          toast(t('nodes.subsResult', r.added, r.updated) + (r.errors.length ? `\n${r.errors.slice(0, 4).join('\n')}` : ''), r.errors.length ? 'error' : 'ok', 4000);
        });
      } else if (act === 'delete') {
        withBusy(null, async () => {
          if (!(await ask(t('nodes.subConfirm'), t('dlg.delete')))) return;
          await invoke('remove_subscription', { url: id });
          snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
        });
      }
    }));
    h.addEventListener('click', () => { if (collapsed.has(id)) collapsed.delete(id); else collapsed.add(id); saveCollapsed(); nodesKey = ''; renderNodes(); });
  });

  $$('#node-list .node').forEach((row) => {
    const id = row.dataset.id;
    row.addEventListener('click', (e) => {
      if (e.target.closest('select, button')) return;
      withBusy(null, async () => { await invoke('select_node', { id }); snap = await invoke('snapshot'); nodesKey = ''; renderNodes(); });
    });
    $('.engine-sel', row).addEventListener('change', async (e) => {
      const engine = e.target.value;
      const name = (snap.nodes.find((x) => x.id === id) || {}).name || '';
      await reloadAround(() => invoke('set_node_engine', { id, engine }), t('nodes.engineFor', name));
      nodesKey = ''; renderNodes();
    });
    $('.del', row).addEventListener('click', () => withBusy(null, async () => {
      await invoke('remove_node', { id });
      snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
    }));
  });
}

// The Smart Connect card is folded away by default (it is also on Home and in the node
// picker); the toolbar button shows it and tells whether Smart Connect is on.
let smartShown = (() => { try { return localStorage.getItem('mosaic.smartShown') === '1'; } catch { return false; } })();
$('#btn-smart').addEventListener('click', () => {
  smartShown = !smartShown;
  try { localStorage.setItem('mosaic.smartShown', smartShown ? '1' : '0'); } catch {}
  nodesKey = ''; renderNodes();
});

$('#node-sort').addEventListener('change', (e) => {
  nodeSort = e.target.value;
  try { localStorage.setItem('mosaic.nodeSort', nodeSort); } catch {}
  nodesKey = ''; renderNodes();
});
$('#node-filter').addEventListener('input', () => renderNodes());
$('#smart-toggle').addEventListener('change', (e) => withBusy(null, async () => {
  await invoke('set_smart', { on: e.target.checked });
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
  if (e.target.checked) toast(t('smart.on'), 'ok', 3500);
}));
$('#btn-import').addEventListener('click', () => { $('#import-panel').classList.toggle('hidden'); $('#import-text').focus(); });
$('#import-paste').addEventListener('click', async () => {
  try {
    $('#import-text').value = isAndroid() ? await invoke('read_clipboard') : await navigator.clipboard.readText();
    $('#import-text').dispatchEvent(new Event('input'));
  } catch { toast(t('nodes.clipboard'), 'info'); }
});
$('#import-qr').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const text = await invoke('scan_qr');
  if (!text) return;
  $('#import-text').value = text;
  $('#import-text').dispatchEvent(new Event('input'));
  $('#import-go').click();
}));
// An http(s) address with nothing after host:port (except a name) is a proxy, not a subscription.
const isProxyUrl = (s) => /^https?:\/\/([^\/?#@\s]+@)?[^\/?#\s]+:\d+\/?(#\S*)?$/i.test(s);
const isUrl = (s) => /^https?:\/\/\S+$/.test(s) && !isProxyUrl(s);
$('#import-text').addEventListener('input', () => {
  $('#import-hint').textContent = isUrl($('#import-text').value.trim()) ? t('nodes.asSub') : '';
});
function importDone(res) {
  const errs = res.errors.length ? `\n${t('nodes.skipped', res.errors.length)}\n` + res.errors.slice(0, 5).join('\n') : '';
  toast(t('nodes.added', res.added, res.updated) + errs, res.added + res.updated ? 'ok' : 'error', errs ? 9000 : 3000);
  return res.added + res.updated > 0;
}
$('#import-go').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const text = $('#import-text').value.trim();
  if (!text) return;
  const res = isUrl(text) ? await invoke('add_subscription', { url: text }) : await invoke('import_text', { text });
  if (importDone(res)) { $('#import-text').value = ''; $('#import-hint').textContent = ''; $('#import-panel').classList.add('hidden'); }
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));

// A SOCKS5 / HTTP(S) proxy by hand: becomes a share link (socks5://user:pass@host:port#name)
// and goes through the same import — so it can be exported and shared like any node.
$('#proxy-toggle').addEventListener('click', () => { $('#proxy-form').classList.toggle('hidden'); $('#px-host').focus(); });
$('#px-type').addEventListener('change', () => { $('#px-port').placeholder = $('#px-type').value === 'socks5' ? '1080' : '8080'; });
$('#px-add').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const ty = $('#px-type').value;
  let host = $('#px-host').value.trim().replace(/^\w+:\/\//, '').replace(/\/.*$/, '');
  const port = Number($('#px-port').value.trim());
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) { toast(t('px.bad'), 'error'); return; }
  if (host.includes(':') && !host.startsWith('[')) host = `[${host}]`;
  const user = $('#px-user').value, pass = $('#px-pass').value, name = $('#px-name').value.trim();
  const enc = encodeURIComponent;
  const auth = user ? enc(user) + (pass ? ':' + enc(pass) : '') + '@' : '';
  const link = `${ty}://${auth}${host}:${port}` + (name ? '#' + enc(name) : '');
  if (importDone(await invoke('import_text', { text: link }))) {
    for (const id of ['#px-host', '#px-port', '#px-user', '#px-pass', '#px-name']) $(id).value = '';
    $('#proxy-form').classList.add('hidden');
    $('#import-panel').classList.add('hidden');
  }
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));
$('#btn-test').addEventListener('click', (e) => {
  const b = e.currentTarget;
  withBusy(b, async () => {
    b.textContent = t('nodes.testing');
    try { await invoke('test_latency'); } finally { b.textContent = t('nodes.test'); }
    snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
  });
});
$('#btn-update-subs').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const res = await invoke('update_subscriptions');
  toast(t('nodes.subsResult', res.added, res.updated) + (res.errors.length ? `\n${res.errors.slice(0, 4).join('\n')}` : ''), res.errors.length ? 'error' : 'ok');
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));

// ------------------------------------------------------------------ rules
const KINDS = ['domain_suffix', 'domain', 'domain_keyword', 'process_name', 'ip_cidr', 'geoip', 'geosite', 'port'];
const PRIVATE = '10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,127.0.0.0/8,169.254.0.0/16,100.64.0.0/10,::1/128,fc00::/7,fe80::/10';
// Country code → its country-code TLD where they differ.
const CC_TLD = { GB: 'uk' };

function targetOptions(cur) {
  const base = [['proxy', t('target.proxy')], ['direct', t('target.direct')], ['block', t('target.block')]];
  const nodes = snap.nodes.map((n) => [`node:${n.id}`, t('target.node', n.name + (n.effective ? ' · ' + engineName(n.effective) : ''))]);
  return [...base, ...nodes].map(([v, l]) => `<option value="${esc(v)}" ${v === cur ? 'selected' : ''}>${esc(l)}</option>`).join('');
}

function renderRules() {
  if (!snap) return;
  if (!rulesDraft) rulesDraft = { rules: JSON.parse(JSON.stringify(snap.rules)), final: snap.settings.final_target };
  $('#final-target').innerHTML = targetOptions(rulesDraft.final);
  $('#rules').innerHTML = rulesDraft.rules.map((r, i) => `
    <div class="rule ${r.enabled ? '' : 'off'}" data-i="${i}">
      <input type="checkbox" class="r-on" ${r.enabled ? 'checked' : ''} title="${t('rules.on')}" />
      <select class="r-kind">${KINDS.map((k) => `<option value="${k}" ${k === r.kind ? 'selected' : ''}>${t('kind.' + k)}</option>`).join('')}</select>
      <input type="text" class="r-val" value="${esc(r.value)}" placeholder="${r.kind === 'process_name' ? t('rules.processPh') : t('rules.valuePh')}" />
      <select class="r-target">${targetOptions(r.target)}</select>
      <button class="icon-btn r-up" title="${t('rules.up')}">↑</button>
      <button class="icon-btn r-down" title="${t('rules.down')}">↓</button>
      <button class="icon-btn r-del" title="${t('rules.del')}">×</button>
    </div>`).join('') || `<div class="empty">${t('rules.empty')}</div>`;

  $$('#rules .rule').forEach((row) => {
    const i = +row.dataset.i;
    const r = rulesDraft.rules[i];
    $('.r-on', row).onchange = (e) => { r.enabled = e.target.checked; row.classList.toggle('off', !r.enabled); };
    $('.r-kind', row).onchange = (e) => { r.kind = e.target.value; renderRules(); };
    $('.r-val', row).oninput = (e) => { r.value = e.target.value; };
    $('.r-target', row).onchange = (e) => { r.target = e.target.value; };
    $('.r-up', row).onclick = () => { if (i > 0) { rulesDraft.rules.splice(i - 1, 0, rulesDraft.rules.splice(i, 1)[0]); renderRules(); } };
    $('.r-down', row).onclick = () => { if (i < rulesDraft.rules.length - 1) { rulesDraft.rules.splice(i + 1, 0, rulesDraft.rules.splice(i, 1)[0]); renderRules(); } };
    $('.r-del', row).onclick = () => { rulesDraft.rules.splice(i, 1); renderRules(); };
  });
  $('#geo-status').textContent = t('geo.status', snap.geoip_file ? '✓' : t('geo.missing'), snap.geosite_file ? '✓' : t('geo.missing'));
  fillCountries();
}

function fillCountries() {
  const sel = $('#country-select');
  if (sel.dataset.lang === lang) return;
  sel.dataset.lang = lang;
  let names = null;
  try { names = new Intl.DisplayNames([lang], { type: 'region' }); } catch { /* old webview */ }
  const items = window.COUNTRIES.map((c) => [c, names ? names.of(c) : c]).sort((a, b) => a[1].localeCompare(b[1], lang));
  const region = (navigator.language.split('-')[1] || '').toUpperCase();
  sel.innerHTML = items.map(([c, n]) => `<option value="${c}" ${c === region ? 'selected' : ''}>${esc(n)} (${c})</option>`).join('');
}

$('#final-target').addEventListener('change', (e) => { rulesDraft.final = e.target.value; });
$('#rule-add').addEventListener('click', () => { rulesDraft.rules.push({ kind: 'domain_suffix', value: '', target: 'proxy', enabled: true }); renderRules(); $$('#rules .r-val').at(-1).focus(); });
$('#preset-country').addEventListener('click', () => $('#country-panel').classList.toggle('hidden'));
$('#country-add').addEventListener('click', () => {
  const cc = $('#country-select').value;
  if (!cc) return;
  const tld = CC_TLD[cc] || cc.toLowerCase();
  // Put country rules right after the private-network rules.
  const at = rulesDraft.rules.findIndex((r) => r.target !== 'direct');
  const add = [
    { kind: 'domain_suffix', value: tld, target: 'direct', enabled: true },
    { kind: 'geoip', value: cc.toLowerCase(), target: 'direct', enabled: true },
  ];
  rulesDraft.rules.splice(at < 0 ? rulesDraft.rules.length : at, 0, ...add);
  $('#country-panel').classList.add('hidden');
  renderRules();
});
$$('[data-preset]').forEach((b) => b.addEventListener('click', () => {
  if (b.dataset.preset === 'reset') {
    rulesDraft.rules = [
      { kind: 'ip_cidr', value: PRIVATE, target: 'direct', enabled: true },
      { kind: 'domain_suffix', value: 'local,localhost,lan', target: 'direct', enabled: true },
    ];
    rulesDraft.final = 'proxy';
  } else if (b.dataset.preset === 'ads') {
    rulesDraft.rules.unshift({ kind: 'geosite', value: 'category-ads-all', target: 'block', enabled: true });
  }
  renderRules();
}));
$('#rules-save').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const rules = rulesDraft.rules.map((r) => ({ ...r, value: r.value.trim() })).filter((r) => r.value);
  await invoke('set_rules', { rules, finalTarget: rulesDraft.final });
  snap = await invoke('snapshot');
  rulesDraft = null; renderRules();
  toast(snap.status.connected ? t('rules.applied') : t('rules.saved'), 'ok', 2500);
}));

// ------------------------------------------------------------------ connections
// ------------------------------------------------------------------ connections
// Multi-part public suffixes where the registrable domain has three labels.
// Not the full Public Suffix List — just the common ones, enough for grouping.
const MULTI_SUFFIX = new Set(('co.uk,org.uk,ac.uk,gov.uk,me.uk,ltd.uk,plc.uk,net.uk,co.jp,ne.jp,or.jp,ac.jp,go.jp,co.kr,or.kr,ne.kr,'
  + 'com.au,net.au,org.au,edu.au,gov.au,co.nz,net.nz,org.nz,com.br,net.br,org.br,gov.br,com.cn,net.cn,org.cn,gov.cn,com.hk,'
  + 'com.tw,org.tw,com.sg,com.my,co.id,co.in,net.in,org.in,co.za,com.mx,com.ar,com.tr,com.ua,org.ua,net.ua,com.ru,org.ru,'
  + 'msk.ru,spb.ru,com.pl,net.pl,org.pl,co.il,com.eg,com.sa,com.vn,co.th,in.th,com.ph,com.pk,com.ng,co.ke,'
  + 'github.io,gitlab.io,pages.dev,workers.dev,vercel.app,netlify.app,herokuapp.com,appspot.com,web.app,firebaseapp.com,'
  + 'blogspot.com,cloudfront.net,azurewebsites.net,amazonaws.com,r2.dev,fly.dev').split(','));

const isIp = (h) => /^\d{1,3}(\.\d{1,3}){3}$/.test(h) || h.includes(':');
function regDomain(host) {
  if (isIp(host)) return host;
  const parts = host.toLowerCase().replace(/\.$/, '').split('.');
  if (parts.length <= 2) return parts.join('.');
  const last2 = parts.slice(-2).join('.');
  return parts.slice(MULTI_SUFFIX.has(last2) ? -3 : -2).join('.');
}

let connView = (() => { try { return localStorage.getItem('mosaic.connView') || 'full'; } catch { return 'full'; } })();
let connSort = { key: null, dir: 1 };
const connSel = new Map(); // id → connection, survives re-renders and pruning
let connShown = [];         // rows currently in the table, in order
let connList = [];

function destText(c) {
  const h = connView === 'domain' ? regDomain(c.host) : c.host;
  if (connView !== 'full') return h;
  return (h.includes(':') ? `[${h}]` : h) + ':' + c.port;
}
const outText = (c) => c.outbound === 'direct' ? t('conns.direct') : c.outbound === 'block' ? t('conns.block') : c.outbound;
const SORTS = {
  app: (c) => (c.process || '￿').toLowerCase(),
  dest: (c) => destText(c).toLowerCase(),
  rule: (c) => c.rule.toLowerCase(),
  out: (c) => outText(c).toLowerCase(),
  up: (c) => c.up, down: (c) => c.down,
  time: (c) => (c.ended_ms || Date.now()) - c.started_ms,
};
const NUMERIC = new Set(['up', 'down', 'time']);

function fillConnSort() {
  const sel = $('#conn-sort');
  if (!sel) return;
  const opts = [['', t('conns.sortNone')]];
  for (const k of Object.keys(SORTS)) {
    const name = { up: '↑ ' + t('conns.upload'), down: '↓ ' + t('conns.download') }[k] || t('conns.' + k);
    opts.push([`${k}:1`, `${name} ${NUMERIC.has(k) ? '↑' : 'A→Z'}`], [`${k}:-1`, `${name} ${NUMERIC.has(k) ? '↓' : 'Z→A'}`]);
  }
  sel.innerHTML = opts.map(([v, l]) => `<option value="${v}">${esc(l)}</option>`).join('');
  sel.value = connSort.key ? `${connSort.key}:${connSort.dir}` : '';
  $('#conn-view').value = connView;
}

// Grouping: by app, domain, rule or outbound; groups fold and unfold.
let connGroup = (() => { try { return localStorage.getItem('mosaic.connGroup') || 'app'; } catch { return 'app'; } })();
const openGroups = new Set();
const GROUP_KEY = {
  app: (c) => c.process || '—',
  domain: (c) => regDomain(c.host),
  rule: (c) => c.rule,
  out: (c) => outText(c),
};
// What the "+" of a group adds by default.
const GROUP_MATCH = { app: 'process_name', domain: 'domain_suffix' };

function outCell(c) {
  return c.outbound === 'direct' ? `<span class="badge direct">${t('conns.direct')}</span>`
    : c.outbound === 'block' ? `<span class="badge block">${t('conns.block')}</span>`
    : `${esc(c.outbound)} ${engineBadge(c.engine === 'Xray' ? 'xray' : 'singbox')}`;
}

function connRow(c, now, child) {
  const dur = fmtDur((c.ended_ms || now) - c.started_ms);
  const net = c.network === 'udp' ? '<span class="net">UDP</span> ' : '';
  const sel = connSel.has(c.id);
  const full = `${c.host}:${c.port}`;
  return `<tr data-id="${c.id}" class="${child ? 'child' : ''} ${c.ended_ms ? 'closed' : ''} ${c.error ? 'err' : ''} ${sel ? 'sel' : ''}" title="${esc(c.error || '')}">
    <td class="c-sel"><input type="checkbox" ${sel ? 'checked' : ''} aria-label="${t('conns.select')}" /></td>
    <td class="c-app">${esc(c.process || '—')}</td>
    <td class="c-dest" title="${esc(full)}">${net}${esc(destText(c))}</td>
    <td class="c-rule">${esc(c.rule)}</td><td class="c-out">${outCell(c)}</td>
    <td class="c-up num">${fmtBytes(c.up)}</td><td class="c-down num">${fmtBytes(c.down)}</td><td class="c-time num">${dur}</td>
    <td class="c-act"><button class="icon-btn add" title="${t('conns.toRulesOne')}">+</button></td></tr>`;
}

// The most frequent values of `get` in a group, for its one-line summary.
function topValues(items, get, n = 3) {
  const m = new Map();
  for (const c of items) { const v = get(c); if (v) m.set(v, (m.get(v) || 0) + 1); }
  const top = [...m.entries()].sort((a, b) => b[1] - a[1]);
  return top.slice(0, n).map((x) => x[0]).join(', ') + (top.length > n ? ` +${top.length - n}` : '');
}

function groupRow(g, now) {
  const items = g.items;
  const active = items.filter((c) => c.ended_ms == null).length;
  const errors = items.filter((c) => c.error).length;
  const up = items.reduce((a, c) => a + c.up, 0), down = items.reduce((a, c) => a + c.down, 0);
  const selN = items.filter((c) => connSel.has(c.id)).length;
  const sub = connGroup === 'app' ? topValues(items, (c) => regDomain(c.host))
    : connGroup === 'domain' ? topValues(items, (c) => c.process)
    : connGroup === 'rule' ? topValues(items, outText)
    : topValues(items, (c) => c.process);
  const open = openGroups.has(g.key);
  const net = items.some((c) => c.network === 'udp') ? '<span class="net">UDP</span> ' : '';
  return `<tr class="grp ${open ? 'open' : ''} ${selN ? 'sel' : ''} ${errors && errors === items.length ? 'err' : ''}" data-g="${esc(g.key)}">
    <td class="c-sel"><input type="checkbox" class="gsel" ${selN && selN === items.length ? 'checked' : ''} data-ind="${selN && selN < items.length ? 1 : 0}" aria-label="${t('conns.selectGroup')}" /></td>
    <td class="c-grp" colspan="4"><span class="caret">▸</span><span class="g-label">${net}${esc(g.label)}</span>
      <span class="g-count">${t('conns.gCount', items.length)}${active ? ` · <b>${t('conns.gActive', active)}</b>` : ''}${errors ? ` · <span class="g-err">${t('conns.gErrors', errors)}</span>` : ''}</span>
      <span class="g-sub muted">${esc(sub)}</span></td>
    <td class="c-up num">${fmtBytes(up)}</td><td class="c-down num">${fmtBytes(down)}</td>
    <td class="c-time num">${fmtDur(now - Math.min(...items.map((c) => c.started_ms)))}</td>
    <td class="c-act"><button class="icon-btn add" title="${t('conns.toRulesGroup')}">+</button></td></tr>`;
}

let connGroups = [];
function renderConns(list) {
  connList = list;
  const f = $('#conn-filter').value.trim().toLowerCase();
  const activeOnly = $('#conn-active-only').checked;
  const now = Date.now();
  let rows = list
    .filter((c) => !activeOnly || c.ended_ms == null)
    .filter((c) => !f || `${c.process || ''} ${c.host}:${c.port} ${destText(c)} ${c.network} ${c.rule} ${c.outbound} ${c.engine || ''}`.toLowerCase().includes(f));
  if (connSort.key) {
    const get = SORTS[connSort.key];
    const d = connSort.dir;
    rows = rows.map((c) => [get(c), c]).sort((a, b) => (a[0] < b[0] ? -d : a[0] > b[0] ? d : 0)).map((x) => x[1]);
  }
  $('#conn-group').value = connGroup;
  $('#conn-fold').classList.toggle('hidden', connGroup === 'none');
  let html = '';
  if (connGroup === 'none') {
    connGroups = [];
    connShown = rows.slice(0, 400);
    html = connShown.map((c) => connRow(c, now, false)).join('');
  } else {
    const keyOf = GROUP_KEY[connGroup];
    const map = new Map();
    for (const c of rows) {
      const k = keyOf(c);
      if (!map.has(k)) map.set(k, { key: k, label: k, items: [] });
      map.get(k).items.push(c);
    }
    let groups = [...map.values()];
    // Groups follow the column sort (sums for traffic, the oldest for time); by default the
    // group with the newest connection comes first.
    const agg = {
      up: (g) => g.items.reduce((a, c) => a + c.up, 0),
      down: (g) => g.items.reduce((a, c) => a + c.down, 0),
      time: (g) => now - Math.min(...g.items.map((c) => c.started_ms)),
    };
    if (connSort.key) {
      const get = agg[connSort.key] || ((g) => SORTS[connSort.key](g.items[0]));
      const d = connSort.dir;
      groups = groups.map((g) => [get(g), g]).sort((a, b) => (a[0] < b[0] ? -d : a[0] > b[0] ? d : 0)).map((x) => x[1]);
    } else {
      groups.sort((a, b) => Math.max(...b.items.map((c) => c.started_ms)) - Math.max(...a.items.map((c) => c.started_ms)));
    }
    connGroups = groups;
    connShown = rows;
    let budget = 400;
    for (const g of groups) {
      html += groupRow(g, now);
      if (openGroups.has(g.key) && budget > 0) {
        const part = g.items.slice(0, budget);
        budget -= part.length;
        html += part.map((c) => connRow(c, now, true)).join('');
        if (g.items.length > part.length) html += `<tr class="child more"><td colspan="9" class="muted">${t('conns.more', g.items.length - part.length)}</td></tr>`;
      }
    }
  }
  $('#conn-table tbody').innerHTML = html || `<tr><td colspan="9" class="muted">${t('conns.empty')}</td></tr>`;
  $$('#conn-table .gsel').forEach((cb) => { cb.indeterminate = cb.dataset.ind === '1'; });
  $('#conn-fold').title = t(groupsAllOpen() ? 'conns.foldAll' : 'conns.unfoldAll');

  $$('#conn-table th[data-sort]').forEach((th) => {
    th.classList.toggle('sort-asc', connSort.key === th.dataset.sort && connSort.dir === 1);
    th.classList.toggle('sort-desc', connSort.key === th.dataset.sort && connSort.dir === -1);
  });
  updateSelBar();
}
const groupsAllOpen = () => connGroups.length > 0 && connGroups.every((g) => openGroups.has(g.key));

$('#conn-group').addEventListener('change', (e) => {
  connGroup = e.target.value;
  openGroups.clear();
  try { localStorage.setItem('mosaic.connGroup', connGroup); } catch {}
  renderConns(connList);
});
$('#conn-fold').addEventListener('click', () => {
  if (groupsAllOpen()) openGroups.clear(); else connGroups.forEach((g) => openGroups.add(g.key));
  renderConns(connList);
});

function updateSelBar() {
  const n = connSel.size;
  $('#conn-selbar').classList.toggle('hidden', !n);
  $('#view-conns').classList.toggle('has-sel', n > 0);
  $('#conn-selcount').textContent = t('conns.selected', n);
  const all = $('#conn-all');
  const shownSel = connShown.filter((c) => connSel.has(c.id)).length;
  all.checked = connShown.length > 0 && shownSel === connShown.length;
  all.indeterminate = shownSel > 0 && shownSel < connShown.length;
}

function toggleConn(id, on) {
  const c = connShown.find((x) => x.id === id) || connList.find((x) => x.id === id);
  if (!c) return;
  if (on ?? !connSel.has(id)) connSel.set(id, c); else connSel.delete(id);
  const tr = $(`#conn-table tr[data-id="${id}"]`);
  if (tr) { tr.classList.toggle('sel', connSel.has(id)); $('input', tr).checked = connSel.has(id); }
  if (connGroup !== 'none') renderConns(connList); // group checkboxes follow
  else updateSelBar();
}

$('#conn-table tbody').addEventListener('click', (e) => {
  const gr = e.target.closest('tr.grp');
  if (gr) {
    const g = connGroups.find((x) => x.key === gr.dataset.g);
    if (!g) return;
    if (e.target.closest('.add')) { addToRulesDialog(g.items, GROUP_MATCH[connGroup]); return; }
    if (e.target.matches('.gsel')) {
      for (const c of g.items) { if (e.target.checked) connSel.set(c.id, c); else connSel.delete(c.id); }
    } else if (openGroups.has(g.key)) openGroups.delete(g.key); else openGroups.add(g.key);
    renderConns(connList);
    return;
  }
  const tr = e.target.closest('tr[data-id]');
  if (!tr) return;
  const id = +tr.dataset.id;
  if (e.target.closest('.add')) {
    const c = connShown.find((x) => x.id === id);
    if (c) addToRulesDialog([c]);
    return;
  }
  if (e.target.matches('input[type=checkbox]')) { toggleConn(id, e.target.checked); return; }
  // Tapping the row toggles selection, unless the user is selecting text.
  if (String(window.getSelection && window.getSelection()).length) return;
  toggleConn(id);
});
$('#conn-all').addEventListener('change', (e) => {
  for (const c of connShown) { if (e.target.checked) connSel.set(c.id, c); else connSel.delete(c.id); }
  renderConns(connList);
});
$$('#conn-table th[data-sort]').forEach((th) => th.addEventListener('click', () => {
  const k = th.dataset.sort;
  // First click: A→Z for text, largest first for numbers; second flips; third resets.
  const first = NUMERIC.has(k) ? -1 : 1;
  if (connSort.key !== k) connSort = { key: k, dir: first };
  else if (connSort.dir === first) connSort.dir = -first;
  else connSort = { key: null, dir: 1 };
  fillConnSort(); renderConns(connList);
}));
$('#conn-sort').addEventListener('change', (e) => {
  const [k, d] = e.target.value.split(':');
  connSort = k ? { key: k, dir: +d } : { key: null, dir: 1 };
  renderConns(connList);
});
$('#conn-view').addEventListener('change', (e) => {
  connView = e.target.value;
  try { localStorage.setItem('mosaic.connView', connView); } catch {}
  renderConns(connList);
});
['input', 'change'].forEach((ev) => {
  $('#conn-filter').addEventListener(ev, () => renderConns(connList));
  $('#conn-active-only').addEventListener(ev, () => renderConns(connList));
});
$('#conn-clear').addEventListener('click', () => { connSel.clear(); renderConns(connList); });
$('#conn-to-rules').addEventListener('click', () => addToRulesDialog([...connSel.values()]));
fillConnSort();

// What a rule can match, and the value each connection gives for it.
const MATCHERS = [
  { kind: 'domain_suffix', label: 'conns.mDomainSuffix', val: (c) => (isIp(c.host) ? null : regDomain(c.host)) },
  { kind: 'domain', label: 'conns.mDomain', val: (c) => (isIp(c.host) ? null : c.host.toLowerCase()) },
  { kind: 'process_name', label: 'conns.mProcess', val: (c) => c.process || null },
  { kind: 'ip_cidr', label: 'conns.mIp', val: (c) => (isIp(c.host) ? `${c.host}/${c.host.includes(':') ? 128 : 32}` : null) },
  { kind: 'port', label: 'conns.mPort', val: (c) => String(c.port) },
];

function existingValues(kind, target) {
  const set = new Set();
  for (const r of snap.rules) {
    if (r.kind !== kind || r.target !== target || !r.enabled) continue;
    r.value.split(',').map((v) => v.trim().toLowerCase()).filter(Boolean).forEach((v) => set.add(v));
  }
  return set;
}

function addToRulesDialog(conns, preferKind) {
  if (!conns.length) return;
  const counts = MATCHERS.map((m) => new Set(conns.map(m.val).filter(Boolean)).size);
  let mi = counts.findIndex((n) => n > 0);
  const pref = MATCHERS.findIndex((m) => m.kind === preferKind);
  if (pref >= 0 && counts[pref] > 0) mi = pref;
  // Sensible default target: send elsewhere than where they went now.
  const outs = new Set(conns.map((c) => c.outbound));
  let target = outs.has('direct') || outs.has('block') ? 'proxy' : 'direct';
  let top = true;

  const wrap = document.createElement('div');
  wrap.className = 'modal-wrap';
  wrap.innerHTML = `<div class="modal card add-rule" role="dialog" aria-modal="true">
    <div class="ar-head"><h3>${t('conns.toRules')}</h3><span class="muted small">${t('conns.fromN', conns.length)}</span></div>
    <div class="ar-label muted small">${t('conns.matchBy')}</div>
    <div class="ar-kinds">${MATCHERS.map((m, i) => `
      <label class="ar-kind ${counts[i] ? '' : 'disabled'}"><input type="radio" name="ar-kind" value="${i}" ${i === mi ? 'checked' : ''} ${counts[i] ? '' : 'disabled'} />
        <span>${t(m.label)}</span><span class="muted small">${counts[i] || '—'}</span></label>`).join('')}</div>
    <div class="ar-label muted small">${t('conns.values')}</div>
    <textarea class="ar-values" rows="3" spellcheck="false"></textarea>
    <div class="ar-note muted small"></div>
    <div class="ar-row">
      <label class="ar-target"><span class="muted small">${t('conns.sendTo')}</span><select class="ar-tsel">${targetOptions(target)}</select></label>
      <label class="check"><input type="checkbox" class="ar-top" checked /> <span>${t('conns.atTop')}</span></label>
    </div>
    <div class="modal-actions"><button class="btn ghost" data-a="0">${t('dlg.cancel')}</button><button class="btn primary" data-a="1">${t('conns.addApply')}</button></div>
  </div>`;
  const ta = $('.ar-values', wrap);
  const note = $('.ar-note', wrap);
  const fill = () => {
    const m = MATCHERS[mi];
    const have = existingValues(m.kind, target);
    const vals = [...new Set(conns.map(m.val).filter(Boolean))];
    const fresh = vals.filter((v) => !have.has(v.toLowerCase()));
    ta.value = fresh.join(', ');
    const skipped = conns.length - conns.filter((c) => m.val(c)).length;
    note.textContent = [
      vals.length - fresh.length ? t('conns.already', vals.length - fresh.length) : '',
      skipped ? t('conns.noValue', skipped) : '',
      m.kind === 'domain_suffix' ? t('conns.suffixHint') : '',
    ].filter(Boolean).join(' · ');
  };
  fill();
  const done = () => { wrap.remove(); document.removeEventListener('keydown', onKey); };
  const onKey = (e) => { if (e.key === 'Escape') done(); };
  $$('input[name=ar-kind]', wrap).forEach((r) => r.addEventListener('change', () => { mi = +r.value; fill(); }));
  $('.ar-tsel', wrap).addEventListener('change', (e) => { target = e.target.value; fill(); });
  $('.ar-top', wrap).addEventListener('change', (e) => { top = e.target.checked; });
  wrap.addEventListener('click', async (e) => {
    if (e.target === wrap || e.target.closest('[data-a="0"]')) return done();
    const ok = e.target.closest('[data-a="1"]');
    if (!ok) return;
    const values = [...new Set(ta.value.split(/[,\s]+/).map((v) => v.trim()).filter(Boolean))];
    if (!values.length) { toast(t('conns.nothingToAdd'), 'error', 3000); return; }
    await withBusy(ok, async () => {
      const rule = { kind: MATCHERS[mi].kind, value: values.join(','), target, enabled: true };
      const rules = JSON.parse(JSON.stringify(snap.rules));
      if (top) rules.unshift(rule); else rules.push(rule);
      await invoke('set_rules', { rules, finalTarget: snap.settings.final_target });
      snap = await invoke('snapshot');
      rulesDraft = null;
      done();
      for (const c of conns) connSel.delete(c.id);
      renderConns(connList);
      toast(t(snap.status.connected ? 'conns.ruleAdded' : 'conns.ruleSaved', values.length), 'ok', 3000);
    });
  });
  document.addEventListener('keydown', onKey);
  document.body.appendChild(wrap);
}

// ------------------------------------------------------------------ engines
const versions = {};
let engineUpdates = {};
let updatingKind = null;
function renderEngines() {
  if (!snap) return;
  $('#btn-engines-check').classList.toggle('hidden', !snap.engine_updates);
  $('#btn-engines-reset').classList.toggle('hidden', !snap.engines_updated);
  $('#engine-grid').innerHTML = snap.engines.map((e) => {
    const supported = snap.nodes.filter((n) => n.supported.includes(e.kind)).length;
    const state = !e.path ? `<span class="state missing">${t('engines.missing')}</span>`
      : e.running ? `<span class="state run">${t('engines.running')}</span>` : `<span class="state">${t('engines.stopped')}</span>`;
    const exe = e.kind === 'xray' ? 'xray.exe' : 'sing-box.exe';
    return `<div class="card engine">
      <h3>${engineBadge(e.kind)} ${esc(e.name)} ${state}</h3>
      <dl>
        <dt>${t('engines.file')}</dt><dd>${e.path ? esc(e.path) : t('engines.put', exe) + `<br><span class="muted">${t('engines.script')}</span>`}</dd>
        <dt>${t('engines.version')}</dt><dd>${esc(engineVersionText(e.kind))}</dd>
        <dt>${t('engines.nodes')}</dt><dd>${t('engines.supports', supported, snap.nodes.length)}${e.running ? t('engines.serves', e.nodes) : ''}</dd>
        <dt>PID</dt><dd>${e.pids.length ? e.pids.join(', ') : '—'}</dd>
      </dl>
      ${engineUpdateRow(e.kind)}
    </div>`;
  }).join('');
  $$('#engine-grid [data-update]').forEach((b) => b.addEventListener('click', async () => {
    if (updatingKind) return;
    const kind = b.dataset.update;
    updatingKind = kind;
    renderEngines();
    try {
      const tag = await invoke('update_engine', { kind });
      toast(t('engines.updated', engineName(kind), tag), 'ok', 5000);
      engineUpdates = {};
    } catch (err) {
      toast(String(err), 'err', 8000);
    } finally {
      updatingKind = null;
      await loadEngineVersions().catch(() => {});
    }
  }));
}

function engineVersionText(kind) {
  const u = engineUpdates[kind];
  return (u && u.current) || versions[kind] || '—';
}

function engineUpdateRow(kind) {
  if (updatingKind === kind) return `<div class="eng-upd"><span>${t('engines.updating')}</span><span class="spinner"></span></div>`;
  const u = engineUpdates[kind];
  if (!u) return '';
  if (u.error) return `<div class="eng-upd muted small">${esc(u.error)}</div>`;
  if (u.available) return `<div class="eng-upd"><span>${t('engines.newVersion', esc(u.latest))}</span><button class="btn primary" data-update="${kind}" ${updatingKind ? 'disabled' : ''}>${t('engines.update')}</button></div>`;
  return `<div class="eng-upd muted small">✓ ${t('engines.upToDate')}</div>`;
}

async function loadEngineVersions() {
  for (const v of await invoke('engine_versions')) versions[v.kind] = v.version || t('engines.unknown');
  renderEngines();
}
$('#btn-engines-check').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  engineUpdates = {};
  for (const u of await invoke('check_engine_updates')) engineUpdates[u.kind] = u;
  renderEngines();
}));
$('#btn-engines-reset').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  if (!(await ask(t('engines.resetConfirm'), t('engines.reset')))) return;
  await invoke('reset_engines');
  engineUpdates = {};
  await loadEngineVersions();
  toast(t('engines.resetDone'), 'ok', 3000);
}));

// ------------------------------------------------------------------ logs
// ---- logs: levels, sources, search; polled incrementally
const logs = { entries: [], seq: 0, level: 'all', source: '', shown: 0, key: '' };
const LOG_KEEP = 5000;
const LEVEL_RANK = { debug: 0, info: 1, warn: 2, error: 3 };

// "Verbose log": every connection goes to the log (opened, closed, rule, result).
$('#log-verbose').addEventListener('change', async (e) => {
  const on = e.target.checked;
  try {
    await invoke('set_settings', { settings: { ...snap.settings, verbose_log: on } });
    snap = await invoke('snapshot');
    toast(t(on ? 'logs.verboseOn' : 'logs.verboseOff'), 'ok', 3000);
  } catch (err) { e.target.checked = !on; toast(err, 'error'); }
});

async function pollLogs() {
  $('#log-verbose').checked = !!(snap && snap.settings.verbose_log);
  const fresh = await invoke('log_entries', { since: logs.seq });
  if (fresh.length) {
    logs.seq = fresh.at(-1).seq;
    logs.entries.push(...fresh);
    if (logs.entries.length > LOG_KEEP) logs.entries.splice(0, logs.entries.length - LOG_KEEP);
  }
  renderLogs(fresh.length > 0);
}

function logMatches(e, f) {
  if (logs.level === 'warn' && LEVEL_RANK[e.level] < 2) return false;
  if (logs.level === 'error' && e.level !== 'error') return false;
  if (logs.source && e.source !== logs.source) return false;
  return !f || `${e.source} ${e.msg}`.toLowerCase().includes(f);
}

const two = (n) => String(n).padStart(2, '0');
function logTime(ts) {
  const d = new Date(ts);
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}
function logRow(e) {
  const lvl = { error: 'ERR', warn: 'WARN', info: 'INFO', debug: 'DBG' }[e.level] || e.level;
  return `<div class="lg lv-${e.level}"><span class="lg-t" title="${esc(new Date(e.ts).toLocaleString())}">${logTime(e.ts)}</span><span class="lg-l">${lvl}</span><button class="lg-s" data-src="${esc(e.source)}">${esc(e.source)}</button><span class="lg-m">${esc(e.msg)}</span></div>`;
}

function renderLogs(appended) {
  const el = $('#logs');
  const f = $('#log-filter').value.trim().toLowerCase();
  // Level buttons show how many warnings / errors there are.
  const warn = logs.entries.filter((e) => e.level === 'warn' || e.level === 'error').length;
  const err = logs.entries.filter((e) => e.level === 'error').length;
  $('#log-level [data-level="warn"]').textContent = t('logs.warn') + (warn ? ` (${warn})` : '');
  $('#log-level [data-level="error"]').textContent = t('logs.errors') + (err ? ` (${err})` : '');
  const sources = [...new Set(logs.entries.map((e) => e.source))].sort();
  const srcSel = $('#log-source');
  const srcKey = sources.join('|') + lang;
  if (srcSel.dataset.key !== srcKey) {
    srcSel.dataset.key = srcKey;
    srcSel.innerHTML = `<option value="">${t('logs.allSources')}</option>` + sources.map((x) => `<option value="${esc(x)}">${esc(x)}</option>`).join('');
    srcSel.value = logs.source;
  }
  const key = [f, logs.level, logs.source, lang].join('\u0001');
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  if (key !== logs.key || !appended || logs.shown > logs.entries.length) {
    // Full redraw (filters changed): the last 2000 matching lines.
    logs.key = key;
    const rows = logs.entries.filter((e) => logMatches(e, f)).slice(-2000);
    el.innerHTML = rows.map(logRow).join('') || `<div class="empty">${t(logs.entries.length ? 'logs.nothing' : 'logs.empty')}</div>`;
    logs.shown = logs.entries.length;
  } else {
    // Only the new lines.
    const add = logs.entries.slice(logs.shown).filter((e) => logMatches(e, f));
    logs.shown = logs.entries.length;
    if (add.length) {
      const empty = $('.empty', el);
      if (empty) empty.remove();
      el.insertAdjacentHTML('beforeend', add.map(logRow).join(''));
      while (el.childElementCount > 2500) el.firstElementChild.remove();
    }
  }
  if ($('#log-follow').checked && (atBottom || !appended)) el.scrollTop = el.scrollHeight;
}

$('#log-filter').addEventListener('input', () => renderLogs(false));
$('#log-source').addEventListener('change', (e) => { logs.source = e.target.value; renderLogs(false); });
$$('#log-level button').forEach((b) => b.addEventListener('click', () => {
  logs.level = b.dataset.level;
  $$('#log-level button').forEach((x) => x.classList.toggle('active', x === b));
  renderLogs(false);
}));
$('#logs').addEventListener('click', (e) => {
  const s = e.target.closest('.lg-s');
  if (!s) return;
  logs.source = logs.source === s.dataset.src ? '' : s.dataset.src;
  $('#log-source').value = logs.source;
  renderLogs(false);
});
$('#log-follow').addEventListener('change', (e) => { if (e.target.checked) $('#logs').scrollTop = $('#logs').scrollHeight; });

async function copyText(text, okMsg) {
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; }
  catch {
    // WebViews without clipboard permission: the old way.
    const ta = document.createElement('textarea');
    ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta); ta.select();
    ok = document.execCommand('copy'); ta.remove();
  }
  if (okMsg) toast(ok ? okMsg : t('logs.copyFail'), ok ? 'ok' : 'error', 2500);
  return ok;
}
$('#log-copy').addEventListener('click', () => {
  const f = $('#log-filter').value.trim().toLowerCase();
  const text = logs.entries.filter((e) => logMatches(e, f))
    .map((e) => `${new Date(e.ts).toISOString()} ${e.level.toUpperCase().padEnd(5)} [${e.source}] ${e.msg}`).join('\n');
  copyText(text, t('logs.copied'));
});
$('#log-folder').addEventListener('click', () => invoke('open_folder', { which: 'logs' }).catch((e) => toast(e, 'error')));
$('#log-report').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const r = await invoke('diagnostics');
  if (isAndroid()) {
    await invoke('share_text', { text: r.text, title: 'Mosaic diagnostic report' });
    return;
  }
  const copied = await copyText(r.text, null);
  if (r.path) toastAction(t(copied ? 'logs.reportSavedCopied' : 'logs.reportSaved', r.path), t('logs.openFolder'), () => invoke('open_folder', { which: 'downloads' }));
  else toast(t('logs.reportCopied'), 'ok', 3000);
}));

// ------------------------------------------------------------------ settings
// Kill switch: where it works and what it is doing right now.
function renderKsNote() {
  const el = $('#ks-note');
  if (!el || !snap) return;
  const s = snap.settings, ks = snap.status.kill_switch;
  const stack = $('#set-tunstack') ? $('#set-tunstack').value : s.tun_stack;
  let note = '';
  if (ks === 'armed') note = t('ks.noteArmed');
  else if (ks === 'blocking') note = t('ks.noteBlocking');
  else if (snap.platform === 'linux' && stack !== 'native') note = t('ks.noteLinuxStack');
  else if ((s.mode || 'system_proxy') !== 'tun') note = t('ks.noteTunOnly');
  el.textContent = note;
  el.classList.toggle('warn', ks === 'blocking' || (snap.platform === 'linux' && stack !== 'native' && $('#set-ks').checked));
}
async function renderAndroidLockdown() {
  try {
    const r = await invoke('vpn_lockdown');
    const el = $('#ks-android-state');
    if (!r.supported) el.textContent = t('ks.androidOld');
    else if (r.lockdown == null) el.textContent = t('ks.androidUnknown');
    else el.textContent = r.lockdown ? t('ks.androidOn') : t('ks.androidOff');
    el.classList.toggle('warn', r.lockdown === false);
  } catch { /* desktop */ }
}
$('#ks-open-vpn').addEventListener('click', () => invoke('open_vpn_settings').catch(() => {}));
$('#set-ks').addEventListener('change', renderKsNote);
$('#set-tunstack').addEventListener('change', renderKsNote);

function renderSettings() {
  if (!snap) return;
  const s = snap.settings;
  $('#set-lang').value = s.ui_language || 'auto';
  $('#set-port').value = s.listen_port;
  $('#set-quic').checked = s.block_quic;
  $('#set-webrtc').checked = s.webrtc_protection !== false;
  $('#set-ks').checked = !!s.kill_switch;
  renderKsNote();
  if (isAndroid()) renderAndroidLockdown();
  $('#set-dnsleak').checked = s.dns_leak_protection !== false;
  $('#set-tray').checked = s.close_to_tray !== false;
  $('#set-autostart').checked = !!s.autostart;
  $('#set-autoconnect').checked = !!s.auto_connect;
  $('#set-appsmode').value = s.apps_mode || 'all';
  renderAppsSummary();
  $('#set-remotedns').value = s.remote_dns || '';
  $('#set-iprule').value = s.ip_rule_dns || 'remote';
  $('#set-startup').value = s.startup_mode || 'last';
  $('#set-lan').checked = !!s.allow_lan;
  $('#set-subs').value = String(s.sub_update_hours ?? 12);
  $('#set-updates').checked = s.check_updates !== false;
  $('#set-version').textContent = t('set.updates.t', snap.version || '');
  $('#set-testurl').value = s.test_url;
  $('#set-ua').value = s.user_agent;
  $('#set-dns').value = s.direct_dns || '';
  $('#set-strategy').value = s.domain_strategy || 'as_is';
  const native = $('#set-tunstack option[value="native"]');
  native.disabled = !snap.native_tun_available;
  native.textContent = snap.native_tun_available ? t('settings.tunNative') : t('settings.tunNativeMissing');
  $('#set-tunstack').value = s.tun_stack || 'singbox';
  $('#set-info').innerHTML = t('settings.info', esc(snap.data_dir), snap.is_admin ? t('yes') : t('no'));
}
$('#settings-save').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const s = {
    ...snap.settings,
    ui_language: $('#set-lang').value,
    listen_port: parseInt($('#set-port').value, 10) || 7890,
    block_quic: $('#set-quic').checked,
    webrtc_protection: $('#set-webrtc').checked,
    kill_switch: $('#set-ks').checked,
    dns_leak_protection: $('#set-dnsleak').checked,
    close_to_tray: $('#set-tray').checked,
    autostart: $('#set-autostart').checked,
    auto_connect: $('#set-autoconnect').checked,
    apps_mode: $('#set-appsmode').value,
    remote_dns: $('#set-remotedns').value.trim() || '1.1.1.1',
    ip_rule_dns: $('#set-iprule').value,
    startup_mode: $('#set-startup').value,
    allow_lan: $('#set-lan').checked,
    sub_update_hours: parseInt($('#set-subs').value, 10) || 0,
    check_updates: $('#set-updates').checked,
    test_url: $('#set-testurl').value.trim(),
    user_agent: $('#set-ua').value.trim(),
    direct_dns: $('#set-dns').value.trim() || '8.8.8.8',
    domain_strategy: $('#set-strategy').value,
    tun_stack: $('#set-tunstack').value,
  };
  await applyChange(s, t('settings.title'));
  const l = pickLang(snap.settings.ui_language);
  if (l !== lang) { lang = l; rerenderAll(); }
  renderSettings();
}));

$('#btn-app-check').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const u = await invoke('check_app_update');
  snap = await invoke('snapshot');
  renderUpdateBanner();
  toast(u ? t('upd.available', u.version) : t('upd.none'), u ? 'ok' : 'info', 4000);
}));

// ------------------------------------------------------------------ apps (Android: which apps use the VPN)
let appsCache = null;
function renderAppsSummary() {
  const n = (snap.settings.apps || []).length;
  $('#set-apps-summary').textContent = n ? t('set.apps.count', n) : t('set.apps.none');
}
$('#btn-apps').addEventListener('click', () => {
  const chosen = new Set(snap.settings.apps || []);
  const wrap = document.createElement('div');
  wrap.className = 'modal-wrap';
  wrap.innerHTML = `<div class="modal card picker" role="dialog" aria-modal="true">
    <div class="picker-head"><b>${esc(t('set.apps.list'))}</b><span class="muted small" id="pk-count"></span></div>
    <input id="pk-filter" placeholder="${esc(t('set.apps.search'))}" />
    <div class="picker-list"><div class="muted picker-msg">${esc(t('set.apps.loading'))}</div></div>
    <div class="modal-actions"><button class="btn ghost" data-a="0">${esc(t('dlg.cancel'))}</button><button class="btn primary" data-a="1">${esc(t('set.apps.done'))}</button></div>
  </div>`;
  const list = $('.picker-list', wrap);
  const count = () => { $('#pk-count', wrap).textContent = t('set.apps.count', chosen.size); };
  const draw = () => {
    if (!appsCache) return;
    const f = $('#pk-filter', wrap).value.trim().toLowerCase();
    // Chosen apps first, then by name.
    const apps = appsCache
      .filter((a) => !f || a.name.toLowerCase().includes(f) || a.pkg.toLowerCase().includes(f))
      .sort((a, b) => (chosen.has(b.pkg) - chosen.has(a.pkg)) || a.name.localeCompare(b.name));
    list.innerHTML = apps.map((a) => `
      <label class="app-item"><img src="${esc(a.icon || '')}" alt="" loading="lazy" /><span><span class="app-name">${esc(a.name)}</span><span class="muted small">${esc(a.pkg)}</span></span>
      <input type="checkbox" data-pkg="${esc(a.pkg)}" ${chosen.has(a.pkg) ? 'checked' : ''} /></label>`).join('');
  };
  list.addEventListener('change', (ev) => {
    const pkg = ev.target.dataset.pkg;
    if (!pkg) return;
    if (ev.target.checked) chosen.add(pkg); else chosen.delete(pkg);
    count();
  });
  $('#pk-filter', wrap).addEventListener('input', draw);
  wrap.addEventListener('click', async (ev) => {
    const a = ev.target.closest('[data-a]');
    if (!a) return;
    wrap.remove();
    if (a.dataset.a !== '1') return;
    let mode = $('#set-appsmode').value;
    if (mode === 'all' && chosen.size) { mode = 'bypass'; $('#set-appsmode').value = mode; }
    await applyChange({ ...snap.settings, apps_mode: mode, apps: [...chosen] }, t('set.apps.t'));
    renderAppsSummary();
  });
  document.body.appendChild(wrap);
  count();
  // Open first, then load the list (it can take a moment on phones with many apps).
  (appsCache ? Promise.resolve(appsCache) : invoke('list_apps'))
    .then((apps) => { appsCache = apps; if (!apps.length) list.innerHTML = `<div class="muted picker-msg">${esc(t('set.apps.empty'))}</div>`; else draw(); })
    .catch((err) => { list.innerHTML = `<div class="picker-msg" style="color:var(--bad)">${esc(String(err))}</div>`; });
});

// ------------------------------------------------------------------ go
document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && /^[1-7]$/.test(e.key)) {
    const views = ['home', 'nodes', 'rules', 'conns', 'engines', 'logs', 'settings'];
    show(views[+e.key - 1]);
    e.preventDefault();
  }
});
lang = pickLang('auto');
applyI18n();
poll();
