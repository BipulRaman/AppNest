const API = '/api/apps';
let pollTimer = null, currentLogId = null, currentLogTab = 'run';
let searchQuery = '';
let statusFilter = 'all'; // 'all' | 'running' | 'stopped'
const expandedPreviews = new Set(); // ids whose inline preview is open
let previewTimer = null;

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
  play:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><polygon points="6 3 20 12 6 21 6 3"/></svg>',
  stop:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>',
  restart: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>',
  logs:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>',
  tail:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 12 15 18 9"/></svg>',
  ext:     '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>',
  edit:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>',
  trash:   '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>',
  drag:    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="9" cy="6" r="1.5"/><circle cx="15" cy="6" r="1.5"/><circle cx="9" cy="12" r="1.5"/><circle cx="15" cy="12" r="1.5"/><circle cx="9" cy="18" r="1.5"/><circle cx="15" cy="18" r="1.5"/></svg>',
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

  $list.innerHTML = apps.map(renderRow).join('');
  applySearchFilter();
  initDragAndDrop();
  // Refresh any open inline previews immediately
  for (const id of expandedPreviews) refreshPreview(id);
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
        ? `<a class="port-chip is-live" href="http://localhost:${a.port}" target="_blank" rel="noopener" title="Open in browser" onclick="event.stopPropagation()">Port <b>${a.port}</b>${IC.ext}</a>`
        : `<span class="port-chip">Port <b>${a.port}</b></span>`)
    : '';

  const tailBtn = (isUp || isBuild)
    ? `<button class="act-btn ${expandedPreviews.has(a.id) ? 'is-active' : ''}" onclick="togglePreview(${a.id})" title="Quick log preview">${IC.tail}</button>`
    : '';

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
        <span class="app-dir" title="${esc(a.projectDir)}">${esc(a.projectDir)}</span>
      </div>
    </div>
    <div class="app-actions">
      ${!isUp && !isBuild ? `
        <button class="act-btn act-start" onclick="startApp(${a.id})">${IC.play} Start</button>
        <button class="act-btn" onclick="startApp(${a.id},true)" title="Start without build">${IC.play}</button>
      ` : ''}
      ${isUp ? `
        <button class="act-btn act-stop" onclick="stopApp(${a.id})">${IC.stop} Stop</button>
        <button class="act-btn" onclick="restartApp(${a.id})">${IC.restart}</button>
      ` : ''}
      ${tailBtn}
      <button class="act-btn" onclick="showLogs(${a.id},'${esc(a.name)}')">${IC.logs}</button>
      <button class="act-btn" onclick="editApp(${a.id})">${IC.edit}</button>
      <button class="act-btn" onclick="deleteApp(${a.id})">${IC.trash}</button>
    </div>
  </div>
  <div class="log-preview" id="preview-${a.id}"><span class="empty">Loading…</span></div>
  </div>`;
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
  const r = await api(`${API}/${id}/start${skip ? '?skipBuild=true' : ''}`, 'POST');
  r.error ? toast(r.error, 'error') : toast('Started', 'success');
  loadApps();
}

async function stopApp(id) {
  const r = await api(`${API}/${id}/stop`, 'POST');
  r.error ? toast(r.error, 'error') : toast('Stopped', 'success');
  loadApps();
}

async function restartApp(id) {
  const r = await api(`${API}/${id}/restart`, 'POST');
  r.error ? toast(r.error, 'error') : toast('Restarted', 'success');
  loadApps();
}

async function deleteApp(id) {
  if (!confirm('Remove this application?')) return;
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
async function showLogs(id, name) {
  currentLogId = id;
  currentLogTab = 'run';
  document.getElementById('logTitle').textContent = name;
  document.getElementById('logModal').classList.remove('hidden');
  updateTabs();
  await refreshLogs();
  startPoll();
}

function startPoll() { stopPoll(); pollTimer = setInterval(refreshLogs, 2000); }
function stopPoll() { if (pollTimer) { clearInterval(pollTimer); pollTimer = null; } }

function classifyLine(raw) {
  if (/\b(?:error|fail|fatal|exception|unhandled|panic)\b/i.test(raw)) return 'log-err';
  if (/\b(?:warn|warning)\b/i.test(raw)) return 'log-warn';
  if (/\b(?:debug|trace)\b/i.test(raw)) return 'log-debug';
  return '';
}

function linkifyUrls(text) {
  const urlRe = /(https?:\/\/[^\s]+)/;
  return text.split('\n').map(raw => {
    const cls = classifyLine(raw);
    const parts = raw.split(/(https?:\/\/[^\s]+)/g);
    let h = parts.map(p => urlRe.test(p)
      ? `<a class="log-url" href="${p.replace(/&/g, '&amp;').replace(/"/g, '&quot;')}" target="_blank" rel="noopener">${esc(p)}</a>`
      : esc(p)
    ).join('');
    h = h.replace(/(\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\])/g, '<span class="log-ts">$1</span>');
    return cls ? `<span class="${cls}">${h}</span>` : h;
  }).join('\n');
}

async function refreshLogs() {
  if (!currentLogId) return;
  const $el = document.getElementById('logContent');
  const wasAtBottom = $el.scrollHeight - $el.scrollTop - $el.clientHeight < 30;
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
  $el.innerHTML = linkifyUrls(raw);
  if (wasAtBottom) $el.scrollTop = $el.scrollHeight;
}

document.querySelectorAll('.tab-bar .tab').forEach(b => {
  b.onclick = () => { currentLogTab = b.dataset.tab; updateTabs(); refreshLogs(); };
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

// ─── Modal close helpers ────────────────────────────
document.querySelectorAll('.overlay-bg').forEach(bg => {
  bg.onclick = () => {
    const m = bg.parentElement;
    m.classList.add('hidden');
    if (m.id === 'logModal') { currentLogId = null; stopPoll(); }
  };
});

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') {
    document.querySelectorAll('.overlay:not(.hidden)').forEach(m => {
      m.classList.add('hidden');
      if (m.id === 'logModal') { currentLogId = null; stopPoll(); }
    });
  }
});

// ─── Auto-refresh ───────────────────────────────────
loadPresets().then(() => {
  setInterval(loadApps, 3000);
  loadApps();
});
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
  $search.addEventListener('input', e => {
    searchQuery = e.target.value;
    applySearchFilter();
  });
}

// ─── Status filter chips ──────────────────
document.querySelectorAll('.filter-chips .chip').forEach(c => {
  c.addEventListener('click', () => {
    statusFilter = c.dataset.filter || 'all';
    document.querySelectorAll('.filter-chips .chip').forEach(b =>
      b.classList.toggle('active', b === c)
    );
    applySearchFilter();
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
  }
});

// ─── Inline log tail ──────────────────────────
async function refreshPreview(id) {
  const el = document.getElementById(`preview-${id}`);
  if (!el) return;
  try {
    const d = await api(`${API}/${id}/logs`);
    const raw = (d.logs || '').trim();
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

// ─── Inline log tail ───────────────────────
async function refreshPreview(id) {
  const el = document.getElementById(`preview-${id}`);
  if (!el) return;
  try {
    const d = await api(`${API}/${id}/logs`);
    const raw = (d.logs || '').trim();
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
  if (expandedPreviews.has(id)) {
    expandedPreviews.delete(id);
  } else {
    expandedPreviews.add(id);
  }
  const wrap = document.querySelector(`.row-wrap[data-wrap-id="${id}"]`);
  if (wrap) wrap.classList.toggle('expanded', expandedPreviews.has(id));
  // Update the tail button's active state
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