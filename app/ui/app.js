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
function pickLang(setting) {
  const want = setting && setting !== 'auto' ? setting : (navigator.language || 'en').slice(0, 2).toLowerCase();
  return window.I18N[want] ? want : 'en';
}
function t(key, ...args) {
  const s = (window.I18N[lang] && window.I18N[lang][key]) ?? window.I18N.en[key] ?? key;
  return s.replace(/\{(\d)\}/g, (_, i) => String(args[+i] ?? ''));
}
function applyI18n() {
  document.documentElement.lang = lang;
  $$('[data-i18n]').forEach((el) => { el.textContent = t(el.dataset.i18n); });
  $$('[data-i18n-ph]').forEach((el) => { el.placeholder = t(el.dataset.i18nPh); });
}

const UNITS = { en: ['B', 'KB', 'MB', 'GB', 'TB'], ru: ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ'] };
function fmtBytes(n) {
  const u = UNITS[lang] || UNITS.en;
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i ? n.toFixed(n < 10 ? 2 : n < 100 ? 1 : 0) : n) + ' ' + u[i];
}
const fmtRate = (n) => fmtBytes(n) + (lang === 'ru' ? '/с' : '/s');
function fmtDur(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(r).padStart(2, '0')}` : `${m}:${String(r).padStart(2, '0')}`;
}
function toast(msg, kind = 'info', ms = 4500) {
  const el = document.createElement('div');
  el.className = 'toast ' + kind;
  el.textContent = String(msg);
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), kind === 'error' ? Math.max(ms, 9000) : ms);
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
let connecting = false;
let rulesDraft = null;
let nodesKey = '';
const traffic = { up: [], down: [] };
let last = null;

// ------------------------------------------------------------------ navigation
$$('.nav button').forEach((b) => b.addEventListener('click', () => show(b.dataset.view)));
function show(v) {
  view = v;
  $$('.nav button').forEach((b) => b.classList.toggle('active', b.dataset.view === v));
  $$('.view').forEach((s) => s.classList.toggle('active', s.id === 'view-' + v));
  if (v === 'rules') { rulesDraft = null; renderRules(); }
  if (v === 'settings') renderSettings();
  if (v === 'nodes') { nodesKey = ''; renderNodes(); }
  if (v === 'engines') renderEngines();
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

// ------------------------------------------------------------------ polling
async function poll() {
  try {
    snap = await invoke('snapshot');
    const l = pickLang(snap.settings.ui_language);
    if (l !== lang) { lang = l; rerenderAll(); }
    renderStatus(true);
    if (view === 'home') renderHome();
    if (view === 'nodes') renderNodes();
    if (view === 'engines') renderEngines();
  } catch (e) {
    console.error(e);
  }
  setTimeout(poll, 1000);
}

async function tickView() {
  try {
    if (view === 'conns') renderConns(await invoke('connections'));
    if (view === 'logs') renderLogs(await invoke('logs'));
  } catch (e) { console.error(e); }
}
setInterval(tickView, 1000);

// ------------------------------------------------------------------ status / home
function selectedNode() {
  return snap && snap.nodes.find((n) => n.id === snap.selected);
}

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
  $('#side-dot').className = 'dot' + (connecting ? ' busy' : st.connected ? ' on' : '');
  $('#side-status').textContent = connecting ? t('status.busy') : st.connected ? t('status.on') : t('status.off');
  const sel = selectedNode();
  $('#side-sub').textContent = st.connected && sel ? sel.name : 'Mosaic 0.2';
}

function renderHome() {
  const st = snap.status;
  const power = $('#power');
  power.classList.toggle('on', st.connected && !connecting);
  power.classList.toggle('busy', connecting);
  $('#hero-state').textContent = connecting ? t('status.busy') : st.connected ? t('status.on') : t('status.off');
  if (st.connected) {
    $('#hero-sub').textContent = st.tun ? (st.tun_stack === 'native' ? t('hero.viaTunNative') : t('hero.viaTun'))
      : st.system_proxy ? t('hero.viaProxy', st.listen_port) : t('hero.viaPort', st.listen_port);
  } else {
    $('#hero-sub').textContent = snap.nodes.length ? t('hero.click') : t('hero.addNode');
  }
  $$('#mode-seg button').forEach((b) => b.classList.toggle('active', b.dataset.mode === snap.settings.mode));
  $('#mode-hint').textContent = snap.settings.mode === 'tun'
    ? (!snap.is_admin ? t('hint.tunAdmin') : snap.settings.tun_stack === 'native' ? t('hint.tunNative') : t('hint.tunSingbox'))
    : t('hint.proxy');

  const n = selectedNode();
  const card = $('#current-node');
  if (n) {
    const lat = n.latency && n.latency.ms != null ? `${n.latency.ms} ms` : '';
    card.innerHTML = `
      <div class="flag">${esc(avatar(n.name))}</div>
      <div class="grow"><div class="title">${esc(n.name)}</div>
      <div class="sub">${esc(n.summary)} · ${esc(n.server)}:${n.port}</div></div>
      ${engineBadge(n.effective)}
      <span class="lat ${latClass(n)}">${lat}</span>
      <button class="btn" id="go-nodes">${t('home.change')}</button>`;
    $('#go-nodes').onclick = () => show('nodes');
  } else {
    card.innerHTML = `<div class="grow"><div class="title">${t('home.noNodes')}</div><div class="sub">${t('home.importHint')}</div></div><button class="btn primary" id="go-nodes">${t('home.add')}</button>`;
    $('#go-nodes').onclick = () => { show('nodes'); $('#import-panel').classList.remove('hidden'); };
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

function drawSpark() {
  const c = $('#spark');
  const w = c.clientWidth, h = 120, dpr = window.devicePixelRatio || 1;
  if (!w) return;
  if (c.width !== w * dpr) { c.width = w * dpr; c.height = h * dpr; }
  const g = c.getContext('2d');
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
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

$('#power').addEventListener('click', async () => {
  if (connecting || !snap) return;
  connecting = true;
  renderHome(); renderStatus(false);
  try {
    if (snap.status.connected) await invoke('disconnect');
    else { await invoke('connect'); toast(t('toast.connected'), 'ok', 2500); }
  } catch (e) { toast(e, 'error'); }
  connecting = false;
  try { snap = await invoke('snapshot'); } catch (e) { /* next poll */ }
  renderStatus(false); renderHome();
});

$$('#mode-seg button').forEach((b) => b.addEventListener('click', async () => {
  if (!snap || snap.settings.mode === b.dataset.mode) return;
  if (b.dataset.mode === 'tun' && !snap.is_admin) toast(t('toast.tunNeedsAdmin'), 'info', 7000);
  const s = { ...snap.settings, mode: b.dataset.mode };
  connecting = snap.status.connected;
  try { await invoke('set_settings', { settings: s }); } catch (e) { toast(e, 'error'); }
  connecting = false;
  snap = await invoke('snapshot'); renderHome(); renderStatus(false);
}));

// ------------------------------------------------------------------ nodes
function latClass(n) {
  if (!n.latency) return '';
  if (n.latency.ms == null) return 'bad';
  return n.latency.ms < 300 ? 'good' : n.latency.ms < 800 ? 'mid' : 'bad';
}

function renderNodes() {
  const filter = $('#node-filter').value.trim().toLowerCase();
  const key = JSON.stringify([lang, snap.selected, filter, snap.nodes.map((n) => [n.id, n.name, n.engine, n.effective, n.latency]), snap.subscriptions]);
  if (key === nodesKey) return;
  nodesKey = key;

  const locale = lang === 'ru' ? 'ru-RU' : undefined;
  $('#subs').innerHTML = snap.subscriptions.map((s) => `
    <div class="sub-chip ${s.last_error ? 'err' : ''}" title="${esc(s.url)}${s.last_error ? '\n' + esc(s.last_error) : ''}">
      <span>⟳ ${esc(s.name || s.url)}</span>
      <span class="muted">${s.updated_ms ? new Date(s.updated_ms).toLocaleString(locale, { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' }) : '—'}</span>
      <button data-sub="${esc(s.url)}" title="${t('nodes.subDelete')}">×</button>
    </div>`).join('');
  $$('#subs button').forEach((b) => b.onclick = () => withBusy(null, async () => {
    if (!confirm(t('nodes.subConfirm'))) return;
    await invoke('remove_subscription', { url: b.dataset.sub });
    snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
  }));

  const nodes = snap.nodes.filter((n) => !filter || [n.name, n.summary, n.server].join(' ').toLowerCase().includes(filter));
  $('#node-count').textContent = snap.nodes.length ? t('nodes.count', nodes.length, snap.nodes.length) : '';
  if (!snap.nodes.length) {
    $('#node-list').innerHTML = `<div class="empty">${t('nodes.empty')}</div>`;
    return;
  }
  $('#node-list').innerHTML = nodes.map((n) => {
    const lat = n.latency ? (n.latency.ms != null ? `${n.latency.ms} ms` : t('nodes.error')) : '';
    const latTitle = n.latency && n.latency.error ? esc(n.latency.error) : '';
    const autoLabel = n.engine === 'auto' && n.effective ? ` (${engineName(n.effective)})` : '';
    const opt = (v, label, ok) => `<option value="${v}" ${n.engine === v ? 'selected' : ''} ${ok ? '' : 'disabled'}>${label}</option>`;
    return `
      <div class="node ${n.id === snap.selected ? 'selected' : ''}" data-id="${n.id}">
        <div class="radio"></div>
        <div style="min-width:0">
          <div class="node-name">${esc(n.name)}</div>
          <div class="node-meta">${esc(n.summary)} · ${esc(n.server)}:${n.port}${n.source ? ' · ' + t('nodes.fromSub') : ''}</div>
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
  }).join('');

  $$('#node-list .node').forEach((row) => {
    const id = row.dataset.id;
    row.addEventListener('click', (e) => {
      if (e.target.closest('select, button')) return;
      withBusy(null, async () => { await invoke('select_node', { id }); snap = await invoke('snapshot'); nodesKey = ''; renderNodes(); });
    });
    $('.engine-sel', row).addEventListener('change', (e) => withBusy(null, async () => {
      await invoke('set_node_engine', { id, engine: e.target.value });
      snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
      toast(t('nodes.engineChanged'), 'ok', 2000);
    }));
    $('.del', row).addEventListener('click', () => withBusy(null, async () => {
      await invoke('remove_node', { id });
      snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
    }));
  });
}

$('#node-filter').addEventListener('input', () => renderNodes());
$('#btn-import').addEventListener('click', () => { $('#import-panel').classList.toggle('hidden'); $('#import-text').focus(); });
$('#import-paste').addEventListener('click', async () => {
  try { $('#import-text').value = await navigator.clipboard.readText(); }
  catch { toast(t('nodes.clipboard'), 'info'); }
});
const isUrl = (s) => /^https?:\/\/\S+$/.test(s);
$('#import-text').addEventListener('input', () => {
  $('#import-hint').textContent = isUrl($('#import-text').value.trim()) ? t('nodes.asSub') : '';
});
$('#import-go').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const text = $('#import-text').value.trim();
  if (!text) return;
  const res = isUrl(text) ? await invoke('add_subscription', { url: text }) : await invoke('import_text', { text });
  const errs = res.errors.length ? `\n${t('nodes.skipped', res.errors.length)}\n` + res.errors.slice(0, 5).join('\n') : '';
  toast(t('nodes.added', res.added, res.updated) + errs, res.added + res.updated ? 'ok' : 'error', errs ? 9000 : 3000);
  if (res.added + res.updated) { $('#import-text').value = ''; $('#import-hint').textContent = ''; $('#import-panel').classList.add('hidden'); }
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
const KINDS = ['domain_suffix', 'domain', 'domain_keyword', 'ip_cidr', 'geoip', 'geosite', 'port'];
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
      <input type="text" class="r-val" value="${esc(r.value)}" placeholder="${t('rules.valuePh')}" />
      <select class="r-target">${targetOptions(r.target)}</select>
      <button class="icon-btn r-up" title="${t('rules.up')}">↑</button>
      <button class="icon-btn r-down" title="${t('rules.down')}">↓</button>
      <button class="icon-btn r-del" title="${t('rules.del')}">×</button>
    </div>`).join('') || `<div class="empty">${t('rules.empty')}</div>`;

  $$('#rules .rule').forEach((row) => {
    const i = +row.dataset.i;
    const r = rulesDraft.rules[i];
    $('.r-on', row).onchange = (e) => { r.enabled = e.target.checked; row.classList.toggle('off', !r.enabled); };
    $('.r-kind', row).onchange = (e) => { r.kind = e.target.value; };
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
function renderConns(list) {
  const f = $('#conn-filter').value.trim().toLowerCase();
  const activeOnly = $('#conn-active-only').checked;
  const now = Date.now();
  const rows = list
    .filter((c) => !activeOnly || c.ended_ms == null)
    .filter((c) => !f || `${c.host}:${c.port} ${c.network} ${c.rule} ${c.outbound} ${c.engine || ''}`.toLowerCase().includes(f))
    .slice(0, 400)
    .map((c) => {
      const out = c.outbound === 'direct' ? `<span class="badge direct">${t('conns.direct')}</span>`
        : c.outbound === 'block' ? `<span class="badge block">${t('conns.block')}</span>`
        : `${esc(c.outbound)} ${engineBadge(c.engine === 'Xray' ? 'xray' : 'singbox')}`;
      const dur = fmtDur((c.ended_ms || now) - c.started_ms);
      const net = c.network === 'udp' ? '<span class="net">UDP</span> ' : '';
      return `<tr class="${c.ended_ms ? 'closed' : ''} ${c.error ? 'err' : ''}" title="${esc(c.error || '')}">
        <td>${net}${esc(c.host)}:${c.port}</td><td class="rule-cell">${esc(c.rule)}</td><td>${out}</td>
        <td class="num">${fmtBytes(c.up)}</td><td class="num">${fmtBytes(c.down)}</td><td class="num">${dur}</td></tr>`;
    });
  $('#conn-table tbody').innerHTML = rows.join('') || `<tr><td colspan="6" class="muted">${t('conns.empty')}</td></tr>`;
}

// ------------------------------------------------------------------ engines
const versions = {};
function renderEngines() {
  if (!snap) return;
  $('#engine-grid').innerHTML = snap.engines.map((e) => {
    const supported = snap.nodes.filter((n) => n.supported.includes(e.kind)).length;
    const state = !e.path ? `<span class="state missing">${t('engines.missing')}</span>`
      : e.running ? `<span class="state run">${t('engines.running')}</span>` : `<span class="state">${t('engines.stopped')}</span>`;
    const exe = e.kind === 'xray' ? 'xray.exe' : 'sing-box.exe';
    return `<div class="card engine">
      <h3>${engineBadge(e.kind)} ${esc(e.name)} ${state}</h3>
      <dl>
        <dt>${t('engines.file')}</dt><dd>${e.path ? esc(e.path) : t('engines.put', exe) + `<br><span class="muted">${t('engines.script')}</span>`}</dd>
        <dt>${t('engines.version')}</dt><dd>${esc(versions[e.kind] || '—')}</dd>
        <dt>${t('engines.nodes')}</dt><dd>${t('engines.supports', supported, snap.nodes.length)}${e.running ? t('engines.serves', e.nodes) : ''}</dd>
        <dt>PID</dt><dd>${e.pids.length ? e.pids.join(', ') : '—'}</dd>
      </dl>
    </div>`;
  }).join('');
}
$('#btn-versions').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  for (const v of await invoke('engine_versions')) versions[v.kind] = v.version || t('engines.unknown');
  renderEngines();
}));

// ------------------------------------------------------------------ logs
let logLen = -1;
function renderLogs(lines) {
  if (lines.length === logLen && lines.length < 2000) return;
  logLen = lines.length;
  const el = $('#logs');
  el.textContent = lines.join('\n') || t('logs.empty');
  if ($('#log-follow').checked) el.scrollTop = el.scrollHeight;
}
$('#log-copy').addEventListener('click', async () => {
  try { await navigator.clipboard.writeText($('#logs').textContent); toast(t('logs.copied'), 'ok', 1500); }
  catch { toast(t('logs.copyFail'), 'error'); }
});

// ------------------------------------------------------------------ settings
function renderSettings() {
  if (!snap) return;
  const s = snap.settings;
  $('#set-lang').value = s.ui_language || 'auto';
  $('#set-port').value = s.listen_port;
  $('#set-quic').checked = s.block_quic;
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
    test_url: $('#set-testurl').value.trim(),
    user_agent: $('#set-ua').value.trim(),
    direct_dns: $('#set-dns').value.trim() || '8.8.8.8',
    domain_strategy: $('#set-strategy').value,
    tun_stack: $('#set-tunstack').value,
  };
  await invoke('set_settings', { settings: s });
  snap = await invoke('snapshot');
  const l = pickLang(snap.settings.ui_language);
  if (l !== lang) { lang = l; rerenderAll(); }
  renderSettings();
  toast(t('settings.saved'), 'ok', 2000);
}));

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
