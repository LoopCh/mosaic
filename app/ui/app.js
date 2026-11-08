'use strict';

// ------------------------------------------------------------------ bridge
const TAURI = window.__TAURI__;
const invoke = (cmd, args = {}) => {
  if (!TAURI) return Promise.reject('Mosaic UI открыт вне приложения');
  return TAURI.core.invoke(cmd, args);
};

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => Array.from(el.querySelectorAll(s));
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function fmtBytes(n) {
  const u = ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ'];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i ? n.toFixed(n < 10 ? 2 : n < 100 ? 1 : 0) : n) + ' ' + u[i];
}
const fmtRate = (n) => fmtBytes(n) + '/с';
function fmtDur(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(r).padStart(2, '0')}` : `${m}:${String(r).padStart(2, '0')}`;
}
function toast(msg, kind = 'info', ms = 4500) {
  const t = document.createElement('div');
  t.className = 'toast ' + kind;
  t.textContent = String(msg);
  $('#toasts').appendChild(t);
  setTimeout(() => t.remove(), kind === 'error' ? Math.max(ms, 9000) : ms);
}
async function withBusy(btn, fn) {
  if (btn) btn.disabled = true;
  try { return await fn(); }
  catch (e) { toast(e, 'error'); }
  finally { if (btn) btn.disabled = false; }
}
const engineBadge = (k) => k ? `<span class="badge ${k}">${k === 'xray' ? 'Xray' : 'sing-box'}</span>` : '<span class="badge none">нет ядра</span>';

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
  tickView();
}

// ------------------------------------------------------------------ polling
async function poll() {
  try {
    snap = await invoke('snapshot');
    renderStatus();
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

function renderStatus() {
  const st = snap.status;
  const now = Date.now();
  if (last) {
    const dt = Math.max(0.2, (now - last.t) / 1000);
    traffic.up.push(Math.max(0, (st.up - last.up) / dt));
    traffic.down.push(Math.max(0, (st.down - last.down) / dt));
    if (traffic.up.length > 120) { traffic.up.shift(); traffic.down.shift(); }
  }
  last = { t: now, up: st.up, down: st.down };

  const dot = $('#side-dot');
  dot.className = 'dot' + (connecting ? ' busy' : st.connected ? ' on' : '');
  $('#side-status').textContent = connecting ? 'Подключение…' : st.connected ? 'Подключено' : 'Отключено';
  const sel = selectedNode();
  $('#side-sub').textContent = st.connected && sel ? sel.name : 'MVP 0.1 · Windows';
}

function renderHome() {
  const st = snap.status;
  const power = $('#power');
  power.classList.toggle('on', st.connected && !connecting);
  power.classList.toggle('busy', connecting);
  $('#hero-state').textContent = connecting ? 'Подключение…' : st.connected ? 'Подключено' : 'Отключено';
  if (st.connected) {
    const how = st.tun ? 'TUN: весь трафик системы' : st.system_proxy ? `системный прокси 127.0.0.1:${st.listen_port}` : `прокси 127.0.0.1:${st.listen_port}`;
    $('#hero-sub').textContent = how;
  } else {
    $('#hero-sub').textContent = snap.nodes.length ? 'Нажмите, чтобы подключиться' : 'Добавьте узел во вкладке «Узлы»';
  }
  $$('#mode-seg button').forEach((b) => b.classList.toggle('active', b.dataset.mode === snap.settings.mode));
  $('#mode-hint').textContent = snap.settings.mode === 'tun'
    ? (snap.is_admin ? 'Весь трафик через TUN-интерфейс, DNS через fake-ip.' : 'Для TUN запустите Mosaic от имени администратора.')
    : 'Браузеры и приложения, которые уважают системный прокси.';

  const n = selectedNode();
  const card = $('#current-node');
  if (n) {
    const lat = n.latency && n.latency.ms != null ? `${n.latency.ms} мс` : '';
    card.innerHTML = `
      <div class="flag">${esc(avatar(n.name))}</div>
      <div class="grow"><div class="title">${esc(n.name)}</div>
      <div class="sub">${esc(n.summary)} · ${esc(n.server)}:${n.port}</div></div>
      ${engineBadge(n.effective)}
      <span class="lat ${latClass(n)}">${lat}</span>
      <button class="btn" id="go-nodes">Сменить</button>`;
    $('#go-nodes').onclick = () => show('nodes');
  } else {
    card.innerHTML = `<div class="grow"><div class="title">Нет узлов</div><div class="sub">Импортируйте ссылку или подписку</div></div><button class="btn primary" id="go-nodes">Добавить</button>`;
    $('#go-nodes').onclick = () => { show('nodes'); $('#import-panel').classList.remove('hidden'); };
  }

  const up = traffic.up.at(-1) || 0, down = traffic.down.at(-1) || 0;
  $('#rate-up').textContent = fmtRate(up);
  $('#rate-down').textContent = fmtRate(down);
  $('#total-up').textContent = 'всего ' + fmtBytes(st.up);
  $('#total-down').textContent = 'всего ' + fmtBytes(st.down);
  $('#conn-count').textContent = st.active_conns;
  $('#uptime').textContent = st.connected && st.since_ms ? 'в сети ' + fmtDur(Date.now() - st.since_ms) : '—';
  drawSpark();
}

function drawSpark() {
  const c = $('#spark');
  const w = c.clientWidth, h = 120, dpr = window.devicePixelRatio || 1;
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
  renderHome(); renderStatus();
  try {
    if (snap.status.connected) await invoke('disconnect');
    else { await invoke('connect'); toast('Подключено', 'ok', 2500); }
  } catch (e) { toast(e, 'error'); }
  connecting = false;
  snap = await invoke('snapshot');
  renderStatus(); renderHome();
});

$$('#mode-seg button').forEach((b) => b.addEventListener('click', async () => {
  if (!snap || snap.settings.mode === b.dataset.mode) return;
  if (b.dataset.mode === 'tun' && !snap.is_admin) toast('TUN-режим требует запуска от имени администратора. Режим сохранён — перезапустите Mosaic с правами администратора.', 'info', 7000);
  const s = { ...snap.settings, mode: b.dataset.mode };
  connecting = snap.status.connected;
  try { await invoke('set_settings', { settings: s }); } catch (e) { toast(e, 'error'); }
  connecting = false;
  snap = await invoke('snapshot'); renderHome(); renderStatus();
}));

// ------------------------------------------------------------------ nodes
function avatar(name) {
  const m = name.match(/^\p{RI}\p{RI}/u);
  return m ? m[0] : (Array.from(name.trim())[0] || '?').toUpperCase();
}

function latClass(n) {
  if (!n.latency) return '';
  if (n.latency.ms == null) return 'bad';
  return n.latency.ms < 300 ? 'good' : n.latency.ms < 800 ? 'mid' : 'bad';
}

function renderNodes() {
  const filter = $('#node-filter').value.trim().toLowerCase();
  const key = JSON.stringify([snap.selected, filter, snap.nodes.map((n) => [n.id, n.name, n.engine, n.effective, n.latency]), snap.subscriptions]);
  if (key === nodesKey) return;
  nodesKey = key;

  $('#subs').innerHTML = snap.subscriptions.map((s) => `
    <div class="sub-chip ${s.last_error ? 'err' : ''}" title="${esc(s.url)}${s.last_error ? '\n' + esc(s.last_error) : ''}">
      <span>⟳ ${esc(s.name || s.url)}</span>
      <span class="muted">${s.updated_ms ? new Date(s.updated_ms).toLocaleString('ru-RU', { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' }) : '—'}</span>
      <button data-sub="${esc(s.url)}" title="Удалить подписку и её узлы">×</button>
    </div>`).join('');
  $$('#subs button').forEach((b) => b.onclick = () => withBusy(null, async () => {
    if (!confirm('Удалить подписку и все её узлы?')) return;
    await invoke('remove_subscription', { url: b.dataset.sub });
    snap = await invoke('snapshot'); renderNodes();
  }));

  const nodes = snap.nodes.filter((n) => !filter || [n.name, n.summary, n.server].join(' ').toLowerCase().includes(filter));
  $('#node-count').textContent = snap.nodes.length ? `${nodes.length} из ${snap.nodes.length}` : '';
  if (!snap.nodes.length) {
    $('#node-list').innerHTML = `<div class="empty">Узлов пока нет. Нажмите «+ Импорт» и вставьте ссылку, подписку или WireGuard-конфиг.</div>`;
    return;
  }
  $('#node-list').innerHTML = nodes.map((n) => {
    const lat = n.latency ? (n.latency.ms != null ? `${n.latency.ms} мс` : 'ошибка') : '';
    const latTitle = n.latency && n.latency.error ? esc(n.latency.error) : '';
    const autoLabel = n.engine === 'auto' && n.effective ? ` (${n.effective === 'xray' ? 'Xray' : 'sing-box'})` : '';
    const opt = (v, label, ok) => `<option value="${v}" ${n.engine === v ? 'selected' : ''} ${ok ? '' : 'disabled'}>${label}</option>`;
    return `
      <div class="node ${n.id === snap.selected ? 'selected' : ''}" data-id="${n.id}">
        <div class="radio"></div>
        <div style="min-width:0">
          <div class="node-name">${esc(n.name)}</div>
          <div class="node-meta">${esc(n.summary)} · ${esc(n.server)}:${n.port}${n.source ? ' · подписка' : ''}</div>
        </div>
        ${engineBadge(n.effective)}
        <span class="lat ${latClass(n)}" title="${latTitle}">${lat}</span>
        <select class="engine-sel" title="${esc(n.effective_error || 'Какое ядро обслуживает этот узел')}">
          ${opt('auto', 'Auto' + autoLabel, true)}
          ${opt('xray', 'Xray', n.supported.includes('xray'))}
          ${opt('singbox', 'sing-box', n.supported.includes('singbox'))}
        </select>
        <button class="icon-btn del" title="Удалить">×</button>
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
      toast('Ядро узла изменено', 'ok', 2000);
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
  catch { toast('Буфер обмена недоступен — вставьте Ctrl+V', 'info'); }
});
$('#import-text').addEventListener('input', () => {
  const t = $('#import-text').value.trim();
  $('#import-hint').textContent = /^https?:\/\/\S+$/.test(t) ? 'будет добавлено как подписка' : '';
});
$('#import-go').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const t = $('#import-text').value.trim();
  if (!t) return;
  const res = /^https?:\/\/\S+$/.test(t) ? await invoke('add_subscription', { url: t }) : await invoke('import_text', { text: t });
  const errs = res.errors.length ? `\nПропущено ${res.errors.length}:\n` + res.errors.slice(0, 5).join('\n') : '';
  toast(`Добавлено ${res.added}, обновлено ${res.updated}${errs}`, res.added + res.updated ? 'ok' : 'error', errs ? 9000 : 3000);
  if (res.added + res.updated) { $('#import-text').value = ''; $('#import-hint').textContent = ''; $('#import-panel').classList.add('hidden'); }
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));
$('#btn-test').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const b = e.currentTarget; const label = b.textContent; b.textContent = 'Тестирую…';
  try { await invoke('test_latency'); } finally { b.textContent = label; }
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));
$('#btn-update-subs').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const res = await invoke('update_subscriptions');
  toast(`Подписки: добавлено ${res.added}, обновлено ${res.updated}` + (res.errors.length ? `\n${res.errors.slice(0, 4).join('\n')}` : ''), res.errors.length ? 'error' : 'ok');
  snap = await invoke('snapshot'); nodesKey = ''; renderNodes();
}));

// ------------------------------------------------------------------ rules
const KINDS = [['domain_suffix', 'Домен и поддомены'], ['domain', 'Домен точно'], ['domain_keyword', 'Домен содержит'], ['ip_cidr', 'IP / подсеть'], ['port', 'Порт']];
const PRESETS = {
  ru: { kind: 'domain_suffix', value: 'ru,su,xn--p1ai,xn--d1acj3b,yandex.net,vk.com,userapi.com,mail.ru', target: 'direct', enabled: true },
  ads: { kind: 'domain_suffix', value: 'doubleclick.net,googlesyndication.com,googleadservices.com,adservice.google.com,an.yandex.ru,mc.yandex.ru', target: 'block', enabled: true },
};

function targetOptions(cur) {
  const base = [['proxy', 'Выбранный узел'], ['direct', 'Напрямую'], ['block', 'Блокировать']];
  const nodes = snap.nodes.map((n) => [`node:${n.id}`, `Узел: ${n.name}${n.effective ? ' · ' + (n.effective === 'xray' ? 'Xray' : 'sing-box') : ''}`]);
  return [...base, ...nodes].map(([v, l]) => `<option value="${esc(v)}" ${v === cur ? 'selected' : ''}>${esc(l)}</option>`).join('');
}

function renderRules() {
  if (!snap) return;
  if (!rulesDraft) rulesDraft = { rules: JSON.parse(JSON.stringify(snap.rules)), final: snap.settings.final_target };
  $('#final-target').innerHTML = targetOptions(rulesDraft.final);
  $('#rules').innerHTML = rulesDraft.rules.map((r, i) => `
    <div class="rule ${r.enabled ? '' : 'off'}" data-i="${i}">
      <input type="checkbox" class="r-on" ${r.enabled ? 'checked' : ''} title="Включено" />
      <select class="r-kind">${KINDS.map(([v, l]) => `<option value="${v}" ${v === r.kind ? 'selected' : ''}>${l}</option>`).join('')}</select>
      <input type="text" class="r-val" value="${esc(r.value)}" placeholder="значения через запятую" />
      <select class="r-target">${targetOptions(r.target)}</select>
      <button class="icon-btn r-up" title="Выше">↑</button>
      <button class="icon-btn r-down" title="Ниже">↓</button>
      <button class="icon-btn r-del" title="Удалить">×</button>
    </div>`).join('') || '<div class="empty">Правил нет — весь трафик идёт по правилу «Всё остальное».</div>';

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
}
$('#final-target').addEventListener('change', (e) => { rulesDraft.final = e.target.value; });
$('#rule-add').addEventListener('click', () => { rulesDraft.rules.push({ kind: 'domain_suffix', value: '', target: 'proxy', enabled: true }); renderRules(); $$('#rules .r-val').at(-1).focus(); });
$$('[data-preset]').forEach((b) => b.addEventListener('click', () => {
  const p = b.dataset.preset;
  if (p === 'reset') {
    rulesDraft.rules = [
      { kind: 'ip_cidr', value: '10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,127.0.0.0/8,169.254.0.0/16,100.64.0.0/10,::1/128,fc00::/7,fe80::/10', target: 'direct', enabled: true },
      { kind: 'domain_suffix', value: 'local,localhost,lan', target: 'direct', enabled: true },
    ];
    rulesDraft.final = 'proxy';
  } else {
    rulesDraft.rules.push({ ...PRESETS[p] });
  }
  renderRules();
}));
$('#rules-save').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const rules = rulesDraft.rules.map((r) => ({ ...r, value: r.value.trim() })).filter((r) => r.value);
  await invoke('set_rules', { rules, finalTarget: rulesDraft.final });
  snap = await invoke('snapshot');
  rulesDraft = null; renderRules();
  toast(snap.status.connected ? 'Правила применены без переподключения' : 'Правила сохранены', 'ok', 2500);
}));

// ------------------------------------------------------------------ connections
function renderConns(list) {
  const f = $('#conn-filter').value.trim().toLowerCase();
  const activeOnly = $('#conn-active-only').checked;
  const now = Date.now();
  const rows = list
    .filter((c) => !activeOnly || c.ended_ms == null)
    .filter((c) => !f || `${c.host}:${c.port} ${c.rule} ${c.outbound} ${c.engine || ''}`.toLowerCase().includes(f))
    .slice(0, 400)
    .map((c) => {
      const out = c.outbound === 'direct' ? '<span class="badge direct">напрямую</span>'
        : c.outbound === 'block' ? '<span class="badge block">блок</span>'
        : `${esc(c.outbound)} ${engineBadge(c.engine === 'Xray' ? 'xray' : 'singbox')}`;
      const dur = fmtDur((c.ended_ms || now) - c.started_ms);
      return `<tr class="${c.ended_ms ? 'closed' : ''} ${c.error ? 'err' : ''}" title="${esc(c.error || '')}">
        <td>${esc(c.host)}:${c.port}</td><td class="rule-cell">${esc(c.rule)}</td><td>${out}</td>
        <td class="num">${fmtBytes(c.up)}</td><td class="num">${fmtBytes(c.down)}</td><td class="num">${dur}</td></tr>`;
    });
  $('#conn-table tbody').innerHTML = rows.join('') || `<tr><td colspan="6" class="muted">Соединений пока нет</td></tr>`;
}

// ------------------------------------------------------------------ engines
let versions = {};
function renderEngines() {
  if (!snap) return;
  $('#engine-grid').innerHTML = snap.engines.map((e) => {
    const supported = snap.nodes.filter((n) => n.supported.includes(e.kind)).length;
    const state = !e.path ? '<span class="state missing">не найден</span>' : e.running ? '<span class="state run">работает</span>' : '<span class="state">остановлен</span>';
    return `<div class="card engine">
      <h3>${engineBadge(e.kind)} ${esc(e.name)} ${state}</h3>
      <dl>
        <dt>Файл</dt><dd>${e.path ? esc(e.path) : 'положите ' + (e.kind === 'xray' ? 'xray.exe' : 'sing-box.exe') + ' в папку engines<br><span class="muted">scripts\\get-engines.ps1 скачает автоматически</span>'}</dd>
        <dt>Версия</dt><dd>${esc(versions[e.kind] || '—')}</dd>
        <dt>Узлов</dt><dd>поддерживает ${supported} из ${snap.nodes.length}${e.running ? `, обслуживает ${e.nodes}` : ''}</dd>
        <dt>PID</dt><dd>${e.pids.length ? e.pids.join(', ') : '—'}</dd>
      </dl>
    </div>`;
  }).join('');
}
$('#btn-versions').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  for (const v of await invoke('engine_versions')) versions[v.kind] = v.version || 'не удалось определить';
  renderEngines();
}));

// ------------------------------------------------------------------ logs
let logLen = -1;
function renderLogs(lines) {
  if (lines.length === logLen && lines.length < 2000) return;
  logLen = lines.length;
  const el = $('#logs');
  el.textContent = lines.join('\n') || 'Пока пусто';
  if ($('#log-follow').checked) el.scrollTop = el.scrollHeight;
}
$('#log-copy').addEventListener('click', async () => {
  try { await navigator.clipboard.writeText($('#logs').textContent); toast('Скопировано', 'ok', 1500); }
  catch { toast('Не удалось скопировать — выделите текст вручную', 'error'); }
});

// ------------------------------------------------------------------ settings
function renderSettings() {
  if (!snap) return;
  const s = snap.settings;
  $('#set-port').value = s.listen_port;
  $('#set-quic').checked = s.block_quic;
  $('#set-testurl').value = s.test_url;
  $('#set-ua').value = s.user_agent;
  $('#set-dns').value = s.direct_dns || '';
  $('#set-info').innerHTML = `Данные: ${esc(snap.data_dir)}<br>Права администратора: ${snap.is_admin ? 'да' : 'нет (нужны для TUN)'}`;
}
$('#settings-save').addEventListener('click', (e) => withBusy(e.currentTarget, async () => {
  const s = {
    ...snap.settings,
    listen_port: parseInt($('#set-port').value, 10) || 7890,
    block_quic: $('#set-quic').checked,
    test_url: $('#set-testurl').value.trim(),
    user_agent: $('#set-ua').value.trim(),
    direct_dns: $('#set-dns').value.trim() || '8.8.8.8',
  };
  await invoke('set_settings', { settings: s });
  snap = await invoke('snapshot'); renderSettings();
  toast('Сохранено', 'ok', 2000);
}));

// ------------------------------------------------------------------ go
document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && /^[1-7]$/.test(e.key)) {
    const views = ['home', 'nodes', 'rules', 'conns', 'engines', 'logs', 'settings'];
    show(views[+e.key - 1]);
    e.preventDefault();
  }
});
poll();
