const API = '/api/apps';
let pollTimer = null, currentLogId = null, currentLogTab = 'run';
let searchQuery = '';
let statusFilter = 'all'; // 'all' | 'running' | 'stopped'
const expandedPreviews = new Set(); // ids whose inline preview is open
const pendingStart = new Set(); // ids where a start/restart has been fired and we're waiting for status to flip
let previewTimer = null;

// ─── Persisted UI state ───────────────────
const LS_KEY = 'appnest.ui';
function saveUiState() {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify({
      search: searchQuery,
      filter: statusFilter,
    }));
  } catch (e) {}
}
function loadUiState() {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return;
    const s = JSON.parse(raw);
    if (typeof s.search === 'string') searchQuery = s.search;
    if (typeof s.filter === 'string') statusFilter = s.filter;
  } catch (e) {}
}

// ─── Utilities ──────────────────────────────────────
async function api(url, method = 'GET', body) {
  const o = { method, headers: { 'Content-Type': 'application/json' } };
  if (body) o.body = JSON.stringify(body);
  return (await fetch(url, o)).json();
}

function esc(s) {
  return s ? String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;') : '';
}

function toast(msg, type = 'info') {
  const el = document.createElement('div');
  el.className = `toast toast-${type}`;
  el.textContent = msg;
  document.getElementById('toastBox').appendChild(el);
  requestAnimationFrame(() => el.classList.add('show'));
  setTimeout(() => { el.classList.remove('show'); setTimeout(() => el.remove(), 300); }, 3000);
}

function closeModal(id) {
  document.getElementById(id).classList.add('hidden');
}

async function pickFolder(inputId) {
  const r = await api('/api/pick-folder');
  if (r.path) document.getElementById(inputId).value = r.path;
}

async function pickScript() {
  const r = await api('/api/pick-file?ext=script');
  if (r.path) document.getElementById('fScript').value = r.path;
}

// ─── SVG Icons ──────────────────────────────────────
const IC = {
  play:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><polygon points="6 3 20 12 6 21 6 3"/></svg>',
  stop:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>',
  restart: '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>',
  logs:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>',
  tail:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 12 15 18 9"/></svg>',
  ext:     '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>',
  edit:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>',
  spinner: '<svg aria-hidden="true" class="spin" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12a9 9 0 1 1-6.219-8.56"/></svg>',
  folder:  '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>',
  terminal:'<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="16" rx="2"/><polyline points="6 9 9 12 6 15"/><line x1="12" y1="15" x2="17" y2="15"/></svg>',
  trash:   '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>',
  drag:    '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="9" cy="6" r="1.5"/><circle cx="15" cy="6" r="1.5"/><circle cx="9" cy="12" r="1.5"/><circle cx="15" cy="12" r="1.5"/><circle cx="9" cy="18" r="1.5"/><circle cx="15" cy="18" r="1.5"/></svg>',
};

// ─── Presets ────────────────────────────────────────
let presets = {};

async function loadPresets() {
  const list = await (await fetch('/presets.json')).json();
  const $type = document.getElementById('fType');
  $type.innerHTML = '';
  presets = {};
  for (const p of list) {
    presets[p.value] = { dev: p.dev, release: p.release };
    const opt = document.createElement('option');
    opt.value = p.value;
    opt.textContent = p.label;
    $type.appendChild(opt);
  }
}

// ─── Render App List ────────────────────────────────
const $list = document.getElementById('appList');

async function loadApps() {
  const apps = await api(API);

  // Clear "pending start" flags once backend confirms the transition
  if (pendingStart.size) {
    for (const a of apps) {
      if (pendingStart.has(a.id) && (a.building || a.status === 'running')) {
        pendingStart.delete(a.id);
      }
    }
  }

  const running = apps.filter(a => a.status === 'running').length;
  document.getElementById('statRunning').textContent = running;
  document.getElementById('statStopped').textContent = apps.length - running;
  const $all = document.getElementById('statAll');
  if ($all) $all.textContent = apps.length;

  if (!apps.length) {
    $list.innerHTML = `
      <div class="empty-state">
        <h3>No applications yet</h3>
        <p>Click "New Application" to add your first project.</p>
      </div>`;
    return;
  }

  // If the list was empty (or contained empty-state), seed it fresh
  if (!$list.querySelector('.row-wrap')) {
    $list.innerHTML = apps.map(renderRow).join('');
    applySearchFilter();
    initDragAndDrop();
    for (const id of expandedPreviews) refreshPreview(id);
    return;
  }

  // Keyed diff update — avoid rebuilding the whole DOM each tick (no flicker)
  const existing = new Map();
  $list.querySelectorAll('.row-wrap').forEach(w => {
    existing.set(Number(w.dataset.wrapId), w);
  });

  const seen = new Set();
  let prev = null;
  for (const a of apps) {
    seen.add(a.id);
    const cur = existing.get(a.id);
    const sig = rowSignature(a);
    if (cur) {
      if (cur.dataset.sig !== sig) {
        // Replace only the .app-row inside, keep wrapper + preview element
        const tmp = document.createElement('div');
        tmp.innerHTML = renderRow(a).trim();
        const freshWrap = tmp.firstElementChild;
        const freshRow = freshWrap.querySelector('.app-row');
        const oldRow = cur.querySelector('.app-row');
        if (oldRow && freshRow) cur.replaceChild(freshRow, oldRow);
        cur.className = freshWrap.className; // keep expanded class if any already on cur
        if (expandedPreviews.has(a.id)) cur.classList.add('expanded');
        cur.dataset.sig = sig;
      } else {
        // Same signature — just refresh the live uptime text in place
        updateUptimeInRow(cur, a);
      }
      // Reorder if needed without detaching when already in correct spot
      const expectedNext = prev ? prev.nextSibling : $list.firstChild;
      if (expectedNext !== cur) $list.insertBefore(cur, expectedNext);
      prev = cur;
    } else {
      const tmp = document.createElement('div');
      tmp.innerHTML = renderRow(a).trim();
      const wrap = tmp.firstElementChild;
      wrap.dataset.sig = sig;
      const expectedNext = prev ? prev.nextSibling : $list.firstChild;
      $list.insertBefore(wrap, expectedNext);
      prev = wrap;
    }
  }
  // Remove rows that no longer exist
  for (const [id, node] of existing) {
    if (!seen.has(id)) node.remove();
  }

  applySearchFilter();
  initDragAndDrop();
  for (const id of expandedPreviews) refreshPreview(id);
}

function rowSignature(a) {
  const st = a.building ? 'building' : a.status;
  return [
    st, a.name, a.type, a.projectDir, a.port || '', a.pid || '',
    a.autoStart ? '1' : '0', a.staticDir || '', a.scriptFile || '',
    pendingStart.has(a.id) ? 'P' : ''
  ].join('|');
}

function updateUptimeInRow(wrap, a) {
  const meta = wrap.querySelector('.app-meta');
  if (!meta) return;
  const isUp = (a.building ? 'building' : a.status) === 'running';
  const existing = meta.querySelector('.uptime');
  if (isUp && a.uptimeSeconds != null) {
    const txt = formatUptime(a.uptimeSeconds);
    if (existing) {
      if (existing.textContent !== txt) existing.textContent = txt;
    } else {
      const span = document.createElement('span');
      span.className = 'uptime';
      span.title = 'Uptime';
      span.textContent = txt;
      // Insert before the project-dir span
      const dir = meta.querySelector('.app-dir');
      meta.insertBefore(span, dir);
    }
  } else if (existing) {
    existing.remove();
  }
}

function formatUptime(sec) {
  if (!sec || sec < 0) return '';
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  const mm = m % 60;
  if (h < 24) return mm ? `${h}h ${mm}m` : `${h}h`;
  const d = Math.floor(h / 24);
  const hh = h % 24;
  return hh ? `${d}d ${hh}h` : `${d}d`;
}

function renderRow(a) {
  const st = a.building ? 'building' : a.status;
  const tagCls = st === 'running' ? 'tag-running' : st === 'building' ? 'tag-building' : 'tag-stopped';
  const tagLabel = st === 'running' ? 'Running' : st === 'building' ? 'Building' : 'Stopped';
  const isUp = st === 'running';
  const isBuild = st === 'building';
  const rowClass = isUp ? 'is-running' : (isBuild ? 'is-building' : '');
  const wrapClass = expandedPreviews.has(a.id) ? 'row-wrap expanded' : 'row-wrap';

  const portHtml = a.port
    ? (isUp
        ? `<a class="port-chip is-live" href="http://localhost:${a.port}" target="_blank" rel="noopener" title="Left-click: open · Right-click: copy URL" onclick="event.stopPropagation()" oncontextmenu="return copyPortUrl(event, ${a.port})">Port <b>${a.port}</b>${IC.ext}</a>`
        : `<span class="port-chip">Port <b>${a.port}</b></span>`)
    : '';

  const tailBtn = (isUp || isBuild)
    ? `<button class="act-btn ${expandedPreviews.has(a.id) ? 'is-active' : ''}" onclick="togglePreview(${a.id})" title="Quick log preview" aria-label="Toggle quick log preview for ${esc(a.name)}" aria-expanded="${expandedPreviews.has(a.id)}">${IC.tail}</button>`
    : '';

  const uptime = isUp && a.uptimeSeconds != null ? formatUptime(a.uptimeSeconds) : '';

  return `
  <div class="${wrapClass}" data-wrap-id="${a.id}">
  <div class="app-row ${rowClass}" draggable="true" data-id="${a.id}" data-status="${isUp ? 'running' : 'stopped'}" data-name="${esc(a.name).toLowerCase()}" data-type="${esc(a.type).toLowerCase()}" data-dir="${esc(a.projectDir).toLowerCase()}">
    <div class="drag-handle" title="Drag to reorder">${IC.drag}</div>
    <div class="status-dot ${st}"></div>
    <div class="app-info">
      <div class="app-name">
        ${esc(a.name)}
        <span class="tag ${tagCls}">${tagLabel}</span>
        ${a.autoStart ? '<span class="tag tag-auto">Auto</span>' : ''}
      </div>
      <div class="app-meta">
        <span><span class="meta-label">${esc(a.type)}</span></span>
        ${portHtml}
        ${a.pid ? `<span>PID <b>${a.pid}</b></span>` : ''}
        ${uptime ? `<span class="uptime" title="Uptime">${uptime}</span>` : ''}
        <span class="app-dir" title="${esc(a.projectDir)}">${esc(a.projectDir)}</span>
      </div>
    </div>
    <div class="app-actions">
      ${!isUp && !isBuild ? (pendingStart.has(a.id) ? `
        <button class="act-btn act-start is-pending" disabled aria-label="Starting ${esc(a.name)}" aria-busy="true">${IC.spinner || IC.play} Starting…</button>
      ` : `
        <button class="act-btn act-start" onclick="startApp(${a.id})" aria-label="Start ${esc(a.name)}">${IC.play} Start</button>
        <button class="act-btn" onclick="startApp(${a.id},true)" title="Start without build" aria-label="Start ${esc(a.name)} without build">${IC.play}</button>
      `) : ''}
      ${isBuild ? `
        <button class="act-btn act-stop" onclick="stopApp(${a.id})" title="Cancel start / build" aria-label="Cancel starting ${esc(a.name)}">${IC.stop} Cancel</button>
      ` : ''}
      ${isUp ? `
        <button class="act-btn act-stop" onclick="stopApp(${a.id})" aria-label="Stop ${esc(a.name)}">${IC.stop} Stop</button>
        <button class="act-btn" onclick="restartApp(${a.id})" ${pendingStart.has(a.id) ? 'disabled' : ''} title="Restart" aria-label="Restart ${esc(a.name)}">${IC.restart}</button>
      ` : ''}
      ${tailBtn}
      <button class="act-btn" onclick="showLogs(${a.id},'${esc(a.name)}')" title="View logs" aria-label="View logs for ${esc(a.name)}">${IC.logs}</button>
      <button class="act-btn" onclick="editApp(${a.id})" title="Edit" aria-label="Edit ${esc(a.name)}">${IC.edit}</button>
      <button class="act-btn" onclick="deleteApp(${a.id})" title="Remove" aria-label="Remove ${esc(a.name)}">${IC.trash}</button>
    </div>
  </div>
  <div class="log-preview" id="preview-${a.id}"><span class="empty">Loading…</span></div>
  </div>`;
}

function copyPortUrl(e, port) {
  e.preventDefault();
  e.stopPropagation();
  const url = `http://localhost:${port}`;
  navigator.clipboard.writeText(url)
    .then(() => toast('Copied URL: ' + url, 'success'))
    .catch(() => toast('Copy failed', 'error'));
  return false;
}

async function openInExplorer(id) {
  const r = await api(`${API}/${id}/open-explorer`, 'POST');
  if (r.ok) toast('Opened folder', 'success');
  else toast(r.error || 'Failed to open folder', 'error');
}

async function openInTerminal(id) {
  const r = await api(`${API}/${id}/open-terminal`, 'POST');
  if (r.ok) toast('Opened terminal', 'success');
  else toast(r.error || 'Failed to open terminal', 'error');
}

// ─── Search ----------------------------------------
function applySearchFilter() {
  const q = searchQuery.trim().toLowerCase();
  $list.querySelectorAll('.row-wrap').forEach(w => {
    const row = w.querySelector('.app-row');
    if (!row) return;
    const hay = (row.dataset.name || '') + ' ' + (row.dataset.type || '') + ' ' + (row.dataset.dir || '');
    const matchesText = q.length === 0 || hay.indexOf(q) !== -1;
    const matchesStatus = statusFilter === 'all' || row.dataset.status === statusFilter;
    w.classList.toggle('filtered-out', !(matchesText && matchesStatus));
  });
}

// ─── Drag & Drop Reorder ────────────────────────────
let dragSrcEl = null; // .row-wrap being dragged

function initDragAndDrop() {
  const rows = $list.querySelectorAll('.app-row');
  rows.forEach(row => {
    row.addEventListener('dragstart', handleDragStart);
    row.addEventListener('dragover', handleDragOver);
    row.addEventListener('dragenter', handleDragEnter);
    row.addEventListener('dragleave', handleDragLeave);
    row.addEventListener('drop', handleDrop);
    row.addEventListener('dragend', handleDragEnd);
  });
}

function handleDragStart(e) {
  dragSrcEl = this.parentElement; // .row-wrap
  this.classList.add('dragging');
  e.dataTransfer.effectAllowed = 'move';
  e.dataTransfer.setData('text/plain', this.dataset.id);
}

function handleDragOver(e) {
  e.preventDefault();
  e.dataTransfer.dropEffect = 'move';
  const target = this.closest('.app-row');
  if (!target || target.parentElement === dragSrcEl) return;

  const rect = target.getBoundingClientRect();
  const midY = rect.top + rect.height / 2;
  target.classList.remove('drop-above', 'drop-below');
  target.classList.add(e.clientY < midY ? 'drop-above' : 'drop-below');
}

function handleDragEnter(e) {
  e.preventDefault();
}

function handleDragLeave() {
  this.classList.remove('drop-above', 'drop-below');
}

function handleDrop(e) {
  e.stopPropagation();
  e.preventDefault();
  const target = this.closest('.app-row');
  if (!target || target.parentElement === dragSrcEl) return;

  const targetWrap = target.parentElement;
  const rect = target.getBoundingClientRect();
  const before = e.clientY < rect.top + rect.height / 2;

  if (before) {
    $list.insertBefore(dragSrcEl, targetWrap);
  } else {
    $list.insertBefore(dragSrcEl, targetWrap.nextSibling);
  }

  target.classList.remove('drop-above', 'drop-below');
  saveOrder();
}

function handleDragEnd() {
  this.classList.remove('dragging');
  $list.querySelectorAll('.app-row').forEach(r => r.classList.remove('drop-above', 'drop-below'));
}

async function saveOrder() {
  const ids = [...$list.querySelectorAll('.app-row')].map(r => +r.dataset.id);
  const r = await api(`${API}/reorder`, 'POST', { ids });
  if (r.error) toast(r.error, 'error');
}

// ─── Actions ────────────────────────────────────────
async function startApp(id, skip) {
  if (pendingStart.has(id)) return; // already waiting
  pendingStart.add(id);
  // Re-render the affected row right away so the button flips to "Starting…"
  updateRowPendingUI(id);
  // Safety net: clear pending after 20s if backend never transitions
  setTimeout(() => {
    if (pendingStart.has(id)) { pendingStart.delete(id); loadApps(); }
  }, 20000);
  try {
    const r = await api(`${API}/${id}/start${skip ? '?skipBuild=true' : ''}`, 'POST');
    if (r.error) {
      pendingStart.delete(id);
      toast(r.error, 'error');
    } else {
      toast('Starting…', 'success');
    }
  } catch (e) {
    pendingStart.delete(id);
    toast(String(e), 'error');
  }
  loadApps();
}

async function stopApp(id) {
  const r = await api(`${API}/${id}/stop`, 'POST');
  r.error ? toast(r.error, 'error') : toast('Stopped', 'success');
  loadApps();
}

async function restartApp(id) {
  if (pendingStart.has(id)) return;
  pendingStart.add(id);
  updateRowPendingUI(id);
  setTimeout(() => {
    if (pendingStart.has(id)) { pendingStart.delete(id); loadApps(); }
  }, 20000);
  try {
    const r = await api(`${API}/${id}/restart`, 'POST');
    if (r.error) {
      pendingStart.delete(id);
      toast(r.error, 'error');
    } else {
      toast('Restarting…', 'success');
    }
  } catch (e) {
    pendingStart.delete(id);
    toast(String(e), 'error');
  }
  loadApps();
}

function updateRowPendingUI(id) {
  const wrap = $list.querySelector(`.row-wrap[data-wrap-id="${id}"]`);
  if (!wrap) return;
  // Disable every button in the actions area; mark the Start button visually
  wrap.querySelectorAll('.app-actions .act-btn').forEach(btn => {
    btn.disabled = true;
  });
  const startBtn = wrap.querySelector('.act-start');
  if (startBtn) {
    startBtn.classList.add('is-pending');
    startBtn.innerHTML = (IC.spinner || IC.play) + ' Starting…';
  }
  // Invalidate cached signature so the next diff will fully re-render
  wrap.dataset.sig = '';
}

async function deleteApp(id) {
  const ok = await confirmDialog({
    title: 'Remove application?',
    message: 'This cannot be undone. The app configuration will be deleted.',
    confirmLabel: 'Remove',
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await fetch(`${API}/${id}`, { method: 'DELETE' });
    const d = await r.json();
    d.error ? toast(d.error, 'error') : toast('Removed', 'success');
  } catch (e) {
    toast('Failed: ' + e.message, 'error');
  }
  loadApps();
}

// ─── Add / Edit Form ────────────────────────────────
const $form = document.getElementById('appForm');
const ids = { id: 'fId', name: 'fName', type: 'fType', mode: 'fMode', dir: 'fDir', serve: 'fServe', port: 'fPort', static: 'fStatic', script: 'fScript', build: 'fBuild', env: 'fEnv', auto: 'fAuto' };
const $ = Object.fromEntries(Object.entries(ids).map(([k, v]) => [k, document.getElementById(v)]));

function applyPreset() {
  const preset = presets[$.type.value];
  if (!preset) return;
  const p = preset[$.mode.value] || preset.dev;
  if (!p) return;
  $.build.value = p.build;
  $.port.value = p.port;
  $.env.value = p.env;
  $.serve.value = p.serve;
  $.static.value = p.static;
  toggleServe();
}

function toggleServe() {
  const mode = $.serve.value;
  document.getElementById('fStaticWrap').classList.toggle('hidden', mode !== 'static');
  document.getElementById('fScriptWrap').classList.toggle('hidden', mode !== 'script');
}

$.type.onchange = () => { if (!$.id.value) applyPreset(); };
$.mode.onchange = () => { if (!$.id.value) applyPreset(); };
$.serve.onchange = toggleServe;

document.getElementById('btnAdd').onclick = () => {
  $.id.value = '';
  $.name.value = '';
  $.dir.value = '';
  $.script.value = '';
  $.auto.checked = false;
  $.type.value = 'dotnet';
  applyPreset();
  document.getElementById('modalTitle').textContent = 'New Application';
  document.getElementById('modal').classList.remove('hidden');
  $.name.focus();
};

async function editApp(id) {
  const apps = await api(API);
  const a = apps.find(x => x.id === id);
  if (!a) return;
  $.id.value = a.id;
  $.name.value = a.name;
  $.dir.value = a.projectDir;
  $.type.value = a.type;
  $.port.value = a.port || '';
  const buildLines = (a.buildSteps || []).slice();
  if (a.runCommand) buildLines.push(a.runCommand);
  $.build.value = buildLines.join('\n');
  $.static.value = a.staticDir || '';
  $.serve.value = a.scriptFile ? 'script' : a.staticDir ? 'static' : 'command';
  $.script.value = a.scriptFile || '';
  $.env.value = Object.entries(a.envVars || {}).map(([k, v]) => `${k}=${v}`).join('\n');
  $.auto.checked = !!a.autoStart;
  toggleServe();
  document.getElementById('modalTitle').textContent = 'Edit Application';
  document.getElementById('modal').classList.remove('hidden');
  $.name.focus();
}

function parseEnv(text) {
  const env = {};
  for (const line of text.split('\n')) {
    const t = line.trim();
    if (!t || !t.includes('=')) continue;
    const i = t.indexOf('=');
    env[t.slice(0, i).trim()] = t.slice(i + 1).trim();
  }
  return env;
}

$form.onsubmit = async (e) => {
  e.preventDefault();
  const mode = $.serve.value;
  const allLines = $.build.value.split('\n').map(s => s.trim()).filter(Boolean);
  let buildSteps, runCommand;
  if (mode === 'command' && allLines.length > 0) {
    buildSteps = allLines.slice(0, -1);
    runCommand = allLines[allLines.length - 1];
  } else {
    buildSteps = allLines;
    runCommand = null;
  }
  const payload = {
    name: $.name.value.trim(),
    projectDir: $.dir.value.trim(),
    projectType: $.type.value,
    buildSteps: buildSteps,
    runCommand: runCommand,
    staticDir: mode === 'static' ? ($.static.value.trim() || null) : null,
    scriptFile: mode === 'script' ? ($.script.value.trim() || null) : null,
    port: $.port.value ? +$.port.value : null,
    envVars: parseEnv($.env.value),
    autoStart: $.auto.checked,
  };
  const r = $.id.value
    ? await api(`${API}/${$.id.value}`, 'PUT', payload)
    : await api(API, 'POST', payload);
  if (r.error) { toast(r.error, 'error'); return; }
  toast($.id.value ? 'Updated' : 'Added', 'success');
  closeModal('modal');
  loadApps();
};

// ─── Logs ───────────────────────────────────────────
let logStream = null;
let runBuf = '', buildBuf = '';
let logSearchQuery = '';
let followMode = true;

async function showLogs(id, name) {
  currentLogId = id;
  currentLogTab = 'run';
  runBuf = '';
  buildBuf = '';
  logSearchQuery = '';
  followMode = true;
  const $searchInput = document.getElementById('logSearchInput');
  if ($searchInput) $searchInput.value = '';
  updateFollowPill();
  document.getElementById('logTitle').textContent = name;
  document.getElementById('logModal').classList.remove('hidden');
  updateTabs();
  document.getElementById('logContent').innerHTML = '<span class="log-debug">(connecting…)</span>';
  openLogStream(id);
}

function openLogStream(id) {
  closeLogStream();
  try {
    const es = new EventSource(`${API}/${id}/logs/stream`);
    logStream = es;
    es.addEventListener('snapshot', (ev) => {
      try {
        const d = JSON.parse(ev.data);
        runBuf = d.logs || '';
        buildBuf = d.buildLogs || '';
        renderStreamedLogs();
      } catch (e) {}
    });
    es.addEventListener('line', (ev) => {
      try {
        const d = JSON.parse(ev.data);
        if (d.kind === 'build') buildBuf += d.text + (d.text.endsWith('\n') ? '' : '\n');
        else runBuf += d.text + (d.text.endsWith('\n') ? '' : '\n');
        // Cap buffer size in browser too
        if (runBuf.length > 500_000) runBuf = runBuf.slice(-400_000);
        if (buildBuf.length > 500_000) buildBuf = buildBuf.slice(-400_000);
        if (currentLogTab === 'run' || currentLogTab === 'build') renderStreamedLogs();
      } catch (e) {}
    });
    es.onerror = () => {
      // Browser auto-reconnects; if the server has closed, just leave the stream.
    };
  } catch (e) {
    // Fallback to polling if EventSource fails
    startPoll();
  }
}

function closeLogStream() {
  if (logStream) { logStream.close(); logStream = null; }
}

// Legacy polling kept for applog tab + fallback
function startPoll() { stopPoll(); pollTimer = setInterval(refreshLogs, 2000); }
function stopPoll() { if (pollTimer) { clearInterval(pollTimer); pollTimer = null; } }

function classifyLine(raw) {
  if (/\b(?:error|fail|fatal|exception|unhandled|panic)\b/i.test(raw)) return 'log-err';
  if (/\b(?:warn|warning)\b/i.test(raw)) return 'log-warn';
  if (/\b(?:debug|trace)\b/i.test(raw)) return 'log-debug';
  return '';
}

function stripAnsi(s) {
  // Remove CSI sequences (colors, cursor moves, etc.) and other escape codes
  return String(s)
    .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, '')
    .replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, '')
    .replace(/\x1b[PX^_][^\x1b]*\x1b\\/g, '')
    .replace(/\x1b[=>]/g, '')
    // Common orphan bytes we often see boxed in the browser
    .replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, '');
}

// ANSI SGR (color / style) -> HTML with classes.
// Preserves colors, strips all other escape sequences.
const ANSI_COLOR_MAP = {
  30: 'ansi-black', 31: 'ansi-red', 32: 'ansi-green', 33: 'ansi-yellow',
  34: 'ansi-blue', 35: 'ansi-magenta', 36: 'ansi-cyan', 37: 'ansi-white',
  90: 'ansi-bright-black', 91: 'ansi-bright-red', 92: 'ansi-bright-green', 93: 'ansi-bright-yellow',
  94: 'ansi-bright-blue', 95: 'ansi-bright-magenta', 96: 'ansi-bright-cyan', 97: 'ansi-bright-white',
};
function ansiToHtml(input) {
  // First remove non-SGR escapes (OSC, DCS, C1, etc.) and control chars
  // Important: the \x0E-\x1F range would include ESC (\x1b); split it so ESC survives.
  const cleaned = String(input)
    .replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, '')
    .replace(/\x1b[PX^_][^\x1b]*\x1b\\/g, '')
    .replace(/\x1b[=>]/g, '')
    .replace(/[\x00-\x08\x0B\x0C\x0E-\x1A\x1C-\x1F\x7F]/g, '');

  let html = '';
  let open = 0; // number of currently-open spans
  const sgrRe = /\x1b\[([0-9;]*)m/g;
  let last = 0;
  let m;
  const closeAll = () => { while (open > 0) { html += '</span>'; open--; } };

  while ((m = sgrRe.exec(cleaned)) !== null) {
    // Emit escaped text up to this escape
    if (m.index > last) html += esc(cleaned.slice(last, m.index));
    const codes = m[1] === '' ? [0] : m[1].split(';').map(Number);
    for (const code of codes) {
      if (code === 0) { closeAll(); continue; }
      let cls = null;
      if (code === 1) cls = 'ansi-bold';
      else if (code === 2) cls = 'ansi-dim';
      else if (code === 3) cls = 'ansi-italic';
      else if (code === 4) cls = 'ansi-underline';
      else if (ANSI_COLOR_MAP[code]) cls = ANSI_COLOR_MAP[code];
      // 39 = default fg, 22 = normal intensity, etc. -> close all to be safe
      else if (code === 39 || code === 22 || code === 23 || code === 24) { closeAll(); continue; }
      if (cls) { html += `<span class="${cls}">`; open++; }
    }
    last = sgrRe.lastIndex;
  }
  if (last < cleaned.length) html += esc(cleaned.slice(last));
  closeAll();
  // Strip any CSI sequences that weren't SGR (cursor moves etc.) from the final HTML just in case
  html = html.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, '');
  return html;
}

function linkifyUrls(text) {
  // Convert ANSI first to get HTML with colored spans + escaped text;
  // then process line-by-line to add per-line classification, URL links, timestamp styling.
  const ansiHtml = ansiToHtml(text);
  const urlRe = /(https?:\/\/[^\s<]+)/;
  return ansiHtml.split('\n').map(line => {
    const plain = line.replace(/<[^>]*>/g, ''); // for classification only
    const cls = classifyLine(plain);
    let h = line.replace(/(https?:\/\/[^\s<]+)/g, (u) => {
      const safe = u.replace(/&/g, '&amp;').replace(/"/g, '&quot;');
      return `<a class="log-url" href="${safe}" target="_blank" rel="noopener">${u}</a>`;
    });
    h = h.replace(/(\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\])/g, '<span class="log-ts">$1</span>');
    return cls ? `<span class="${cls}">${h}</span>` : h;
  }).join('\n');
}

function getBufFor(tab) {
  if (tab === 'build') return buildBuf;
  return runBuf;
}

function applyLogFilter(raw) {
  const q = (logSearchQuery || '').trim();
  if (!q) return raw;
  const qLower = q.toLowerCase();
  return raw.split('\n').filter(l => l.toLowerCase().includes(qLower)).join('\n');
}

function highlightMatches($el, q) {
  if (!q) { document.getElementById('logSearchCount').textContent = ''; return; }
  const qLower = q.toLowerCase();
  // Walk text nodes inside the terminal and wrap matches in <mark>
  const walker = document.createTreeWalker($el, NodeFilter.SHOW_TEXT, null);
  const targets = [];
  let node;
  while ((node = walker.nextNode())) {
    if (node.parentElement && node.parentElement.closest('mark.log-match')) continue;
    const idx = node.nodeValue.toLowerCase().indexOf(qLower);
    if (idx !== -1) targets.push(node);
  }
  let count = 0;
  targets.forEach(n => {
    const text = n.nodeValue;
    const parent = n.parentNode;
    const frag = document.createDocumentFragment();
    let from = 0;
    let pos = text.toLowerCase().indexOf(qLower, from);
    while (pos !== -1) {
      if (pos > from) frag.appendChild(document.createTextNode(text.slice(from, pos)));
      const mark = document.createElement('mark');
      mark.className = 'log-match';
      mark.textContent = text.slice(pos, pos + q.length);
      frag.appendChild(mark);
      count++;
      from = pos + q.length;
      pos = text.toLowerCase().indexOf(qLower, from);
    }
    if (from < text.length) frag.appendChild(document.createTextNode(text.slice(from)));
    parent.replaceChild(frag, n);
  });
  document.getElementById('logSearchCount').textContent = count > 0 ? `${count} matches` : 'no matches';
}

function renderStreamedLogs() {
  const $el = document.getElementById('logContent');
  if (!$el) return;
  const raw = currentLogTab === 'run'
    ? (runBuf || '(waiting for output…)')
    : (buildBuf || '(no build output)');
  const filtered = applyLogFilter(raw);
  $el.innerHTML = linkifyUrls(filtered);
  highlightMatches($el, logSearchQuery);
  if (followMode) $el.scrollTop = $el.scrollHeight;
}

async function refreshLogs() {
  if (!currentLogId) return;
  const $el = document.getElementById('logContent');
  let raw;
  if (currentLogTab === 'applog') {
    const d = await api(`${API}/${currentLogId}/applogs`);
    raw = d.log || '(empty)';
  } else {
    const d = await api(`${API}/${currentLogId}/logs`);
    raw = currentLogTab === 'run'
      ? (d.logs || '(waiting for output…)')
      : (d.buildLogs || '(no build output)');
  }
  const filtered = applyLogFilter(raw);
  $el.innerHTML = linkifyUrls(filtered);
  highlightMatches($el, logSearchQuery);
  if (followMode) $el.scrollTop = $el.scrollHeight;
}

function updateFollowPill() {
  const pill = document.getElementById('followPill');
  if (!pill) return;
  pill.classList.toggle('is-following', followMode);
  pill.classList.toggle('is-paused', !followMode);
  pill.textContent = followMode ? 'Following' : 'Paused';
  pill.title = followMode ? 'Auto-scrolling. Click to pause.' : 'Scroll paused. Click to resume.';
}

// Follow pill + scroll-based auto toggle
(function() {
  const pill = document.getElementById('followPill');
  const $el = document.getElementById('logContent');
  if (pill) pill.onclick = () => {
    followMode = !followMode;
    updateFollowPill();
    if (followMode && $el) $el.scrollTop = $el.scrollHeight;
  };
  if ($el) $el.addEventListener('scroll', () => {
    const atBottom = $el.scrollHeight - $el.scrollTop - $el.clientHeight < 10;
    if (!atBottom && followMode) { followMode = false; updateFollowPill(); }
    else if (atBottom && !followMode) { followMode = true; updateFollowPill(); }
  });
  const $search = document.getElementById('logSearchInput');
  if ($search) $search.addEventListener('input', e => {
    logSearchQuery = e.target.value;
    if (currentLogTab === 'applog') refreshLogs();
    else renderStreamedLogs();
  });
})();

document.querySelectorAll('.tab-bar .tab').forEach(b => {
  b.onclick = () => {
    currentLogTab = b.dataset.tab;
    updateTabs();
    if (currentLogTab === 'applog') {
      // Applog is file-backed; poll
      stopPoll();
      refreshLogs();
      startPoll();
    } else {
      stopPoll();
      renderStreamedLogs();
    }
  };
});

function updateTabs() {
  document.querySelectorAll('.tab-bar .tab').forEach(b =>
    b.classList.toggle('active', b.dataset.tab === currentLogTab)
  );
}

document.getElementById('btnCloseLogs').onclick = () => {
  closeModal('logModal');
  currentLogId = null;
  stopPoll();
  closeLogStream();
};

const wrapIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="ic"><path d="M3 6h18"/><path d="M3 12h15a3 3 0 1 1 0 6h-4"/><polyline points="13 16 11 18 13 20"/><path d="M3 18h4"/></svg>';
document.getElementById('btnWrapLog').onclick = () => {
  const $el = document.getElementById('logContent');
  const btn = document.getElementById('btnWrapLog');
  $el.classList.toggle('nowrap');
  btn.innerHTML = wrapIcon + ($el.classList.contains('nowrap') ? ' Text Wrap' : ' Text Unwrap');
};

document.getElementById('btnCopyLog').onclick = () => {
  const text = document.getElementById('logContent').textContent;
  navigator.clipboard.writeText(text).then(() => toast('Copied to clipboard', 'success')).catch(() => toast('Copy failed', 'error'));
};

document.getElementById('btnExportLog').onclick = () => {
  if (currentLogId) window.open(`${API}/${currentLogId}/applogs/export`, '_blank');
};

document.getElementById('btnOpenFolder').onclick = () => {
  if (currentLogId) openInExplorer(currentLogId);
};

document.getElementById('btnOpenTerminal').onclick = () => {
  if (currentLogId) openInTerminal(currentLogId);
};

// ─── Modal close helpers ────────────────────────────
document.querySelectorAll('.overlay-bg').forEach(bg => {
  bg.onclick = () => {
    const m = bg.parentElement;
    m.classList.add('hidden');
    if (m.id === 'logModal') { currentLogId = null; stopPoll(); closeLogStream(); }
  };
});

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') {
    document.querySelectorAll('.overlay:not(.hidden)').forEach(m => {
      m.classList.add('hidden');
      if (m.id === 'logModal') { currentLogId = null; stopPoll(); closeLogStream(); }
    });
  }
});

// ─── Auto-refresh ───────────────────────────────────
loadUiState();
loadPresets().then(() => {
  setInterval(loadApps, 3000);
  loadApps();
});

// ─── Update check ───────────────────────────────────
async function checkForUpdates(manual = false) {
  const btn = document.getElementById('btnCheckUpdate');
  if (manual && btn) { btn.disabled = true; btn.classList.add('is-checking'); }
  try {
    const info = await api('/api/update-check');
    if (!info) {
      if (manual) toast('Could not reach update server', 'error');
      return;
    }
    if (info.error && manual) {
      toast('Update check failed: ' + info.error, 'error');
      return;
    }
    if (!info.update_available) {
      if (manual) toast(`You’re up to date (v${info.current})`, 'success');
      return;
    }
    // An update is available — show the banner. Manual checks also bypass the dismissed flag.
    const dismissed = (() => { try { return localStorage.getItem('updateDismissed'); } catch (e) { return null; } })();
    if (!manual && dismissed && dismissed === info.latest) return;
    const banner = document.getElementById('updateBanner');
    const text = document.getElementById('updateBannerText');
    if (text) text.textContent = `New version ${info.latest} available (you have ${info.current}).`;
    const openBtn = document.getElementById('btnOpenUpdate');
    if (openBtn) openBtn.onclick = async () => {
      await api('/api/update-open', 'POST', { url: info.release_url });
    };
    const dismissBtn = document.getElementById('btnDismissUpdate');
    if (dismissBtn) dismissBtn.onclick = () => {
      banner.classList.add('hidden');
      try { localStorage.setItem('updateDismissed', info.latest); } catch (e) {}
    };
    banner.classList.remove('hidden');
    if (manual) toast(`Update available: ${info.latest}`, 'info');
  } catch (e) {
    if (manual) toast('Update check failed', 'error');
  } finally {
    if (manual && btn) { btn.disabled = false; btn.classList.remove('is-checking'); }
  }
}
// Run once on load, then once every 6 hours while the app is open.
checkForUpdates();
setInterval(() => checkForUpdates(false), 6 * 60 * 60 * 1000);
const $btnCheckUpdate = document.getElementById('btnCheckUpdate');
if ($btnCheckUpdate) $btnCheckUpdate.onclick = () => checkForUpdates(true);
// ─── Theme toggle ───────────────────────────
const $btnTheme = document.getElementById('btnTheme');
if ($btnTheme) {
  $btnTheme.onclick = () => {
    const cur = document.documentElement.getAttribute('data-theme') || 'light';
    const next = cur === 'dark' ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', next);
    try { localStorage.setItem('theme', next); } catch (e) {}
  };
}

// ─── Search ──────────────────────────────
const $search = document.getElementById('searchInput');
if ($search) {
  $search.value = searchQuery;
  $search.addEventListener('input', e => {
    searchQuery = e.target.value;
    applySearchFilter();
    saveUiState();
  });
}

// ─── Status filter chips ──────────────────
document.querySelectorAll('.filter-chips .chip').forEach(c => {
  // Initial active sync
  c.classList.toggle('active', (c.dataset.filter || 'all') === statusFilter);
  c.addEventListener('click', () => {
    statusFilter = c.dataset.filter || 'all';
    document.querySelectorAll('.filter-chips .chip').forEach(b =>
      b.classList.toggle('active', b === c)
    );
    applySearchFilter();
    saveUiState();
  });
});

// ─── Keyboard shortcuts ─────────────────────
document.addEventListener('keydown', e => {
  const tag = (e.target.tagName || '').toLowerCase();
  const typing = tag === 'input' || tag === 'textarea' || tag === 'select' || e.target.isContentEditable;
  if (e.key === '/' && !typing && !e.ctrlKey && !e.metaKey && !e.altKey) {
    e.preventDefault();
    $search && $search.focus();
    $search && $search.select();
  } else if ((e.key === 'n' || e.key === 'N') && !typing && !e.ctrlKey && !e.metaKey && !e.altKey) {
    const anyOpen = document.querySelector('.overlay:not(.hidden)');
    if (!anyOpen) {
      e.preventDefault();
      document.getElementById('btnAdd').click();
    }
  } else if ((e.key === 'k' || e.key === 'K') && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    openPalette();
  }
});

// ─── Inline log tail ──────────────────────────
async function refreshPreview(id) {
  const el = document.getElementById(`preview-${id}`);
  if (!el) return;
  try {
    const d = await api(`${API}/${id}/logs`);
    const raw = stripAnsi(d.logs || '').trim();
    if (!raw) { el.innerHTML = '<span class="empty">(waiting for output…)</span>'; return; }
    const lines = raw.split('\n');
    const tail = lines.slice(-6).join('\n');
    const wasAtBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 20;
    el.textContent = tail;
    if (wasAtBottom) el.scrollTop = el.scrollHeight;
  } catch (e) {
    el.innerHTML = '<span class="empty">(failed to load)</span>';
  }
}

function togglePreview(id) {
  if (expandedPreviews.has(id)) expandedPreviews.delete(id);
  else expandedPreviews.add(id);
  const wrap = document.querySelector(`.row-wrap[data-wrap-id="${id}"]`);
  if (wrap) wrap.classList.toggle('expanded', expandedPreviews.has(id));
  const btn = wrap && wrap.querySelector('.act-btn[onclick*="togglePreview"]');
  if (btn) btn.classList.toggle('is-active', expandedPreviews.has(id));
  if (expandedPreviews.has(id)) refreshPreview(id);
  updatePreviewPolling();
}

function updatePreviewPolling() {
  if (expandedPreviews.size > 0 && !previewTimer) {
    previewTimer = setInterval(() => {
      for (const id of expandedPreviews) refreshPreview(id);
    }, 1500);
  } else if (expandedPreviews.size === 0 && previewTimer) {
    clearInterval(previewTimer);
    previewTimer = null;
  }
}

// ─── Confirm dialog ─────────────────────────
function confirmDialog({ title = 'Are you sure?', message = '', confirmLabel = 'Confirm', danger = false } = {}) {
  return new Promise(resolve => {
    const modal = document.getElementById('confirmModal');
    const btnOk = document.getElementById('confirmOk');
    const btnCancel = document.getElementById('confirmCancel');
    const bg = modal.querySelector('.overlay-bg');
    document.getElementById('confirmTitle').textContent = title;
    document.getElementById('confirmMessage').textContent = message;
    btnOk.textContent = confirmLabel;
    btnOk.classList.toggle('btn-danger', !!danger);
    btnOk.classList.toggle('btn-accent', !danger);
    modal.classList.remove('hidden');
    btnOk.focus();

    const close = (val) => {
      modal.classList.add('hidden');
      btnOk.onclick = null;
      btnCancel.onclick = null;
      bg.onclick = null;
      document.removeEventListener('keydown', onKey);
      resolve(val);
    };
    const onKey = (e) => {
      if (e.key === 'Escape') { e.preventDefault(); close(false); }
      else if (e.key === 'Enter') { e.preventDefault(); close(true); }
    };
    btnOk.onclick = () => close(true);
    btnCancel.onclick = () => close(false);
    bg.onclick = () => close(false);
    document.addEventListener('keydown', onKey);
  });
}

// ─── Command palette ────────────────────────
let paletteItems = [];
let paletteActiveIdx = 0;
const $palette = document.getElementById('paletteModal');
const $paletteInput = document.getElementById('paletteInput');
const $paletteResults = document.getElementById('paletteResults');

async function openPalette() {
  $palette.classList.remove('hidden');
  $paletteInput.value = '';
  paletteActiveIdx = 0;
  // Load apps fresh for the palette
  let apps = [];
  try { apps = await api(API); } catch (e) {}
  buildPaletteItems(apps, '');
  $paletteInput.focus();
}

function closePalette() {
  $palette.classList.add('hidden');
}

function buildPaletteItems(apps, query) {
  const q = query.trim().toLowerCase();
  const items = [];

  // Global actions first (always shown; filtered by query)
  const globalActions = [
    { type: 'action', title: 'New application', hint: 'Create', icon: IC.play, run: () => document.getElementById('btnAdd').click() },
    { type: 'action', title: 'Toggle dark mode', hint: 'Theme', icon: IC.edit, run: () => document.getElementById('btnTheme').click() },
    { type: 'action', title: 'Show: All apps', hint: 'Filter', icon: IC.logs, run: () => setFilter('all') },
    { type: 'action', title: 'Show: Running', hint: 'Filter', icon: IC.logs, run: () => setFilter('running') },
    { type: 'action', title: 'Show: Stopped', hint: 'Filter', icon: IC.logs, run: () => setFilter('stopped') },
  ];

  // App actions
  for (const a of apps) {
    const isUp = a.status === 'running';
    const st = a.building ? 'building' : a.status;
    if (!isUp && !a.building) {
      items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Start ${a.name}`, icon: IC.play, run: () => startApp(a.id) });
    }
    if (isUp) {
      items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Stop ${a.name}`, icon: IC.stop, run: () => stopApp(a.id) });
      items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Restart ${a.name}`, icon: IC.restart, run: () => restartApp(a.id) });
    }
    items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Logs: ${a.name}`, icon: IC.logs, run: () => showLogs(a.id, a.name) });
    items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Edit: ${a.name}`, icon: IC.edit, run: () => editApp(a.id) });
    if (isUp && a.port) {
      items.push({ type: 'app', appId: a.id, appName: a.name, status: st, title: `Open http://localhost:${a.port}`, icon: IC.ext, run: () => window.open(`http://localhost:${a.port}`, '_blank', 'noopener') });
    }
  }

  const allItems = [...globalActions, ...items];
  const filtered = q
    ? allItems.filter(it => it.title.toLowerCase().includes(q) || (it.appName && it.appName.toLowerCase().includes(q)))
    : allItems;

  paletteItems = filtered;
  paletteActiveIdx = 0;
  renderPalette();
}

function renderPalette() {
  if (!paletteItems.length) {
    $paletteResults.innerHTML = '<div class="palette-empty">No matches</div>';
    return;
  }
  let html = '';
  let lastType = '';
  paletteItems.forEach((it, idx) => {
    if (it.type !== lastType) {
      html += `<div class="palette-section-title">${it.type === 'action' ? 'Actions' : 'Apps'}</div>`;
      lastType = it.type;
    }
    const active = idx === paletteActiveIdx ? ' is-active' : '';
    const statusChip = it.type === 'app'
      ? `<span class="p-status ${it.status === 'running' ? 'running' : 'stopped'}">${it.status}</span>`
      : '';
    html += `
      <div class="palette-item${active}" data-idx="${idx}">
        <span class="p-icon">${it.icon || ''}</span>
        <span class="p-title">${esc(it.title)}</span>
        ${statusChip}
        ${it.hint ? `<span class="p-hint">${esc(it.hint)}</span>` : ''}
      </div>`;
  });
  $paletteResults.innerHTML = html;

  $paletteResults.querySelectorAll('.palette-item').forEach(el => {
    el.addEventListener('mouseenter', () => {
      paletteActiveIdx = +el.dataset.idx;
      updatePaletteActive();
    });
    el.addEventListener('click', () => runPaletteItem(+el.dataset.idx));
  });

  scrollActiveIntoView();
}

function updatePaletteActive() {
  $paletteResults.querySelectorAll('.palette-item').forEach(el => {
    el.classList.toggle('is-active', +el.dataset.idx === paletteActiveIdx);
  });
  scrollActiveIntoView();
}

function scrollActiveIntoView() {
  const el = $paletteResults.querySelector('.palette-item.is-active');
  if (el) el.scrollIntoView({ block: 'nearest' });
}

function runPaletteItem(idx) {
  const it = paletteItems[idx];
  if (!it) return;
  closePalette();
  try { it.run(); } catch (e) { toast('Action failed', 'error'); }
}

function setFilter(val) {
  statusFilter = val;
  document.querySelectorAll('.filter-chips .chip').forEach(b =>
    b.classList.toggle('active', (b.dataset.filter || 'all') === val)
  );
  applySearchFilter();
  saveUiState();
}

$paletteInput.addEventListener('input', async e => {
  const q = e.target.value;
  let apps = [];
  try { apps = await api(API); } catch (err) {}
  buildPaletteItems(apps, q);
});

$paletteInput.addEventListener('keydown', e => {
  if (e.key === 'ArrowDown') {
    e.preventDefault();
    paletteActiveIdx = Math.min(paletteItems.length - 1, paletteActiveIdx + 1);
    updatePaletteActive();
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    paletteActiveIdx = Math.max(0, paletteActiveIdx - 1);
    updatePaletteActive();
  } else if (e.key === 'Enter') {
    e.preventDefault();
    runPaletteItem(paletteActiveIdx);
  } else if (e.key === 'Escape') {
    e.preventDefault();
    closePalette();
  }
});

$palette.querySelector('.overlay-bg').addEventListener('click', closePalette);