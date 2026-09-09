/* Speckle — client core: chrome, grid, viewer, video, keyboard.
 * Pages (storage, settings, collections, people, upload) live in views.js.
 *
 * The grid is virtualised against a single columnar payload of every matching
 * id, timestamp and dimension. Fetching that once rather than paging as you
 * scroll is what makes the scrollbar honest, puts the date headers in the right
 * place, and lets the viewer size its frame before a picture arrives.
 */
'use strict';

// ============================================================== helpers ====

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

const fmtBytes = b => {
  if (b == null) return '—';
  if (b >= 1099511627776) return (b / 1099511627776).toFixed(2) + ' TB';
  if (b >= 1073741824) return (b / 1073741824).toFixed(1) + ' GB';
  if (b >= 1048576) return (b / 1048576).toFixed(1) + ' MB';
  if (b >= 1024) return Math.round(b / 1024) + ' KB';
  return b + ' B';
};
const fmtDur = s => {
  s = Math.max(0, Math.round(s || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = s % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(x).padStart(2, '0')}`
           : `${m}:${String(x).padStart(2, '0')}`;
};
const fmtCount = n => (n || 0).toLocaleString();
const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

const DAY = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const MON = ['January', 'February', 'March', 'April', 'May', 'June', 'July',
             'August', 'September', 'October', 'November', 'December'];

function dayKey(ts) { const d = new Date(ts * 1000); return d.getFullYear() * 10000 + (d.getMonth() + 1) * 100 + d.getDate(); }
function dayLabel(ts) {
  const d = new Date(ts * 1000), now = new Date();
  const same = (a, b) => a.toDateString() === b.toDateString();
  const y = new Date(now); y.setDate(y.getDate() - 1);
  if (same(d, now)) return 'Today';
  if (same(d, y)) return 'Yesterday';
  if (now - d < 6 * 864e5) return DAY[d.getDay()];
  return `${d.getDate()} ${MON[d.getMonth()]}${d.getFullYear() !== now.getFullYear() ? ' ' + d.getFullYear() : ''}`;
}
function dateSub(ts) {
  const d = new Date(ts * 1000);
  return `${DAY[d.getDay()].slice(0, 3)} ${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')}.${d.getFullYear()}`;
}

/** Explorer only exists on the machine running the server. */
const isLocal = () => ['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname);

async function api(path, opts) {
  const r = await fetch(path, opts);
  const ct = r.headers.get('content-type') || '';
  const body = ct.includes('json') ? await r.json() : await r.text();
  if (!r.ok) throw new Error((body && body.error) || r.statusText);
  return body;
}
const post = (p, b) => api(p, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(b || {}) });
const del = p => api(p, { method: 'DELETE' });

function toast(msg, kind = 'ok', action) {
  const el = document.createElement('div');
  el.className = 'toast ' + kind;
  el.innerHTML = `${kind === 'err' ? I('alert', 16) : I('check', 16)}<span>${esc(msg)}</span>`;
  if (action) {
    const b = document.createElement('button');
    b.textContent = action.label;
    b.onclick = () => { action.fn(); el.remove(); };
    el.appendChild(b);
  }
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), action ? 9000 : 4200);
}

// -------------------------------------------------------------- icons -----

const PATHS = {
  grid: '<path d="M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z"/>',
  heart: '<path d="M12 20.4S3.8 15 3.8 9.4A4.4 4.4 0 0 1 12 7a4.4 4.4 0 0 1 8.2 2.4c0 5.6-8.2 11-8.2 11z"/>',
  star: '<path d="M12 2.6l2.9 5.9 6.5.9-4.7 4.6 1.1 6.5L12 17.4 6.2 20.5l1.1-6.5L2.6 9.4l6.5-.9z"/>',
  film: '<rect x="2.5" y="4.5" width="19" height="15" rx="2"/><path d="M7 4.5v15M17 4.5v15M2.5 12h19"/>',
  trash: '<path d="M4 7h16M9.5 7V4.6h5V7M6.5 7l1 13h9l1-13"/>',
  disk: '<ellipse cx="12" cy="6" rx="8" ry="3.2"/><path d="M4 6v12c0 1.8 3.6 3.2 8 3.2s8-1.4 8-3.2V6"/><path d="M4 12c0 1.8 3.6 3.2 8 3.2s8-1.4 8-3.2"/>',
  gear: '<circle cx="12" cy="12" r="3.2"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.2 5.2l2.1 2.1M16.7 16.7l2.1 2.1M18.8 5.2l-2.1 2.1M7.3 16.7l-2.1 2.1"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="M16.5 16.5 21 21"/>',
  sort: '<path d="M4 7h16M6 12h12M9 17h6"/>',
  check: '<path d="M4 12.5 9.5 18 20 6.5"/>',
  chev: '<path d="M9 5l7 7-7 7"/>',
  chevD: '<path d="M6 9.5 12 15.5l6-6"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.6v.1"/>',
  close: '<path d="M6 6l12 12M18 6 6 18"/>',
  play: '<path d="M7 4.5v15l13-7.5z" fill="currentColor" stroke="none"/>',
  pause: '<path d="M8.5 5v14M15.5 5v14"/>',
  vol: '<path d="M4 9.5h3.5L12 5.5v13L7.5 14.5H4z"/><path d="M15.5 9a4.5 4.5 0 0 1 0 6"/>',
  mute: '<path d="M4 9.5h3.5L12 5.5v13L7.5 14.5H4z"/><path d="M16 10l4 4M20 10l-4 4"/>',
  back10: '<path d="M4 11a8 8 0 1 0 2.4-5.7"/><path d="M4 4v5h5"/>',
  fwd10: '<path d="M20 11a8 8 0 1 1-2.4-5.7"/><path d="M20 4v5h-5"/>',
  expand: '<path d="M4 9V4h5M20 15v5h-5M20 9V4h-5M4 15v5h5"/>',
  edit: '<path d="M4 20h4L20 8l-4-4L4 16z"/><path d="M14.5 5.5 18.5 9.5"/>',
  download: '<path d="M12 4v11M7 11l5 5 5-5M4 20h16"/>',
  upload: '<path d="M12 20V5M6 11l6-6 6 6"/>',
  restore: '<path d="M4 11a8 8 0 1 1 2.4 5.7"/><path d="M4 4v5h5"/>',
  copy: '<rect x="8.5" y="8.5" width="12" height="12" rx="2"/><path d="M15.5 5.5v-1a1 1 0 0 0-1-1h-10a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h1"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  minus: '<path d="M5 12h14"/>',
  folder: '<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4l2 2.5h7A1.5 1.5 0 0 1 19 9v8.5a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 3 17.5z"/>',
  cloud: '<path d="M7 18.5a4.2 4.2 0 0 1-.3-8.4A5.6 5.6 0 0 1 17.6 11a3.8 3.8 0 0 1-.4 7.5z"/>',
  alert: '<path d="M12 3.5 22 20H2z"/><path d="M12 10v4.2M12 17.2v.1"/>',
  zap: '<path d="M13 2.5 4.5 13.5H11L10.5 21.5 19.5 10.5H13z"/>',
  eye: '<path d="M2 12s3.8-6.5 10-6.5S22 12 22 12s-3.8 6.5-10 6.5S2 12 2 12z"/><circle cx="12" cy="12" r="2.8"/>',
  menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
  refresh: '<path d="M20.5 12a8.5 8.5 0 1 1-2.5-6"/><path d="M20.5 4v5h-5"/>',
  copies: '<rect x="3" y="3" width="12" height="12" rx="2"/><path d="M9 21h10a2 2 0 0 0 2-2V9"/>',
  face: '<circle cx="12" cy="12" r="9"/><path d="M8.5 10v.1M15.5 10v.1M8.5 14.5a5 5 0 0 0 7 0"/>',
  stack: '<rect x="3" y="7" width="18" height="13" rx="2"/><path d="M6 4h12"/>',
  external: '<path d="M14 4h6v6M20 4l-8.5 8.5M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
  sparkle: '<path d="M12 3l1.9 5.6L19.5 10l-5.6 1.9L12 17.5l-1.9-5.6L4.5 10l5.6-1.4z"/>',
  up: '<path d="M6 15l6-6 6 6"/>',
  down: '<path d="M6 9l6 6 6-6"/>',
};
function I(name, size = 16, sw = 1.7) {
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor"
    stroke-width="${sw}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name] || ''}</svg>`;
}

/* App marks. Monochrome on purpose — they take the accent colour from the
   interface rather than carrying one of their own. */
const MARKS = {
  glimmer: {
    name: 'Glimmer',
    svg: '<path d="M12 2.5c.5 4.6 2.4 6.5 7 7-4.6.5-6.5 2.4-7 7-.5-4.6-2.4-6.5-7-7 4.6-.5 6.5-2.4 7-7z"/><path d="M19.3 15.6c.15 1.7.85 2.4 2.55 2.55-1.7.15-2.4.85-2.55 2.55-.15-1.7-.85-2.4-2.55-2.55 1.7-.15 2.4-.85 2.55-2.55z"/>',
  },
  speck: {
    name: 'Speck',
    svg: '<circle cx="12" cy="12" r="2.6" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="6.4"/><circle cx="12" cy="12" r="10" opacity=".45"/>',
  },
  scatter: {
    name: 'Scatter',
    svg: '<circle cx="5.5" cy="18.5" r="2.6" fill="currentColor" stroke="none"/><circle cx="11.5" cy="12.5" r="2" fill="currentColor" stroke="none"/><circle cx="16.5" cy="7.5" r="1.5" fill="currentColor" stroke="none"/><circle cx="20.5" cy="3.5" r="1" fill="currentColor" stroke="none"/><circle cx="6" cy="8" r="1.2" fill="currentColor" stroke="none" opacity=".55"/><circle cx="18" cy="17" r="1.2" fill="currentColor" stroke="none" opacity=".55"/>',
  },
  halftone: {
    name: 'Halftone',
    svg: '<circle cx="6" cy="6" r="3.1" fill="currentColor" stroke="none"/><circle cx="14" cy="6" r="2.2" fill="currentColor" stroke="none"/><circle cx="20" cy="6" r="1.3" fill="currentColor" stroke="none"/><circle cx="6" cy="14" r="2.2" fill="currentColor" stroke="none"/><circle cx="14" cy="14" r="1.5" fill="currentColor" stroke="none"/><circle cx="20" cy="14" r="1" fill="currentColor" stroke="none"/><circle cx="6" cy="20" r="1.3" fill="currentColor" stroke="none"/><circle cx="14" cy="20" r="1" fill="currentColor" stroke="none"/>',
  },
  aperture: {
    name: 'Aperture',
    svg: '<circle cx="12" cy="12" r="9.2"/><path d="M12 2.8 16.6 10M21 14.6h-8.6M16.6 21.4 12 14M3 14.6l4.3-7.4M7.4 21.4 12 14M3 9.4h8.6"/>',
  },
  grain: {
    name: 'Grain',
    svg: '<rect x="3.2" y="3.2" width="17.6" height="17.6" rx="4"/><circle cx="8.5" cy="9" r="1.35" fill="currentColor" stroke="none"/><circle cx="13.5" cy="7.5" r=".95" fill="currentColor" stroke="none"/><circle cx="16.5" cy="11.5" r="1.15" fill="currentColor" stroke="none"/><circle cx="9.5" cy="14.5" r="1.05" fill="currentColor" stroke="none"/><circle cx="14" cy="16.5" r="1.35" fill="currentColor" stroke="none"/>',
  },
};
function mark(size = 24, sw = 1.7) {
  const m = MARKS[S.settings.icon] || MARKS.glimmer;
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor"
    stroke-width="${sw}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${m.svg}</svg>`;
}

// =============================================================== state =====

const S = {
  view: 'grid',
  lib: null, sub: null, coll: null, person: null,
  filter: null, sort: 'newest', q: '', smart: false,
  cols: 7,
  sel: new Set(), selMode: false, lastClicked: -1,
  list: null, groups: [], layout: null,
  cur: -1, info: false,
  server: null, settings: {},
  storage: null, dupes: null, colls: [], people: [],
  compress: { quality: 82, max_edge: 4096, keep_originals: true, skip_rated: false, scope: 'oversized' },
  sideOpen: false,
  expanded: new Set(),
  psel: new Set(), pselMode: false,
};

const K = {
  kind: f => f & 3, fav: f => (f >> 2) & 1, rating: f => (f >> 3) & 7,
  err: f => (f >> 6) & 1, native: f => (f >> 7) & 1,
};

// ================================================================ boot =====

async function boot() {
  render();
  await refreshServer();
  applySettings(S.server.settings || {});
  await loadColls();
  render();
  await loadList();
  poll();
  let rt;
  window.addEventListener('resize', () => {
    clearTimeout(rt);
    rt = setTimeout(() => { layout(); paint(); if (V) sizeViewer(); }, 80);
  });
}

async function refreshServer() {
  try { S.server = await api('/api/state'); }
  catch (e) { S.server = { libraries: [], folders: [], counts: {}, job: {}, settings: {} }; }
}

function applySettings(s) {
  S.settings = s;
  setAccent(s.accent || '#4B79E4');
  if (s.theme) document.documentElement.dataset.theme = s.theme;
  if (s.cols) S.cols = +s.cols;
  if (s.sort) S.sort = s.sort;
  if (s.info != null) S.info = !!s.info;
  applyFavicon();
}

function saveSetting(k, v) {
  S.settings[k] = v;
  post('/api/settings', { [k]: v }).catch(() => {});
}

/* The accent drives the neutral ramp too: the greys borrow its hue at very low
   saturation, so choosing a new accent re-tunes the whole interface instead of
   dropping a foreign colour onto a fixed grey chrome. */
function setAccent(hex) {
  const { h, l } = hexToHsl(hex);
  const r = document.documentElement;
  r.style.setProperty('--accent', hex);
  r.style.setProperty('--nh', String(Math.round(h)));
  r.style.setProperty('--accent-ink', l > 0.62 ? '#0b0d10' : '#ffffff');
}
function hexToHsl(hex) {
  const m = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  if (!m) return { h: 224, s: 0.6, l: 0.5 };
  const [r, g, b] = [1, 2, 3].map(i => parseInt(m[i], 16) / 255);
  const mx = Math.max(r, g, b), mn = Math.min(r, g, b), d = mx - mn;
  let h = 0;
  if (d) {
    if (mx === r) h = ((g - b) / d) % 6;
    else if (mx === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60; if (h < 0) h += 360;
  }
  const l = (mx + mn) / 2;
  return { h, s: d ? d / (1 - Math.abs(2 * l - 1)) : 0, l };
}
function applyFavicon() {
  const m = MARKS[S.settings.icon] || MARKS.glimmer;
  const col = S.settings.accent || '#4B79E4';
  const svg = `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='${col}' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'>${m.svg}</svg>`;
  let link = $('link[rel="icon"]');
  if (!link) { link = document.createElement('link'); link.rel = 'icon'; document.head.appendChild(link); }
  link.href = 'data:image/svg+xml,' + encodeURIComponent(svg);
}

// ============================================================== chrome =====

function render() {
  $('#app').innerHTML = sidebar() + `<main class="main" id="main"></main>` + tabbar();
  renderMain();
  wireChrome();
}

/* Folders form a tree, and a library with forty sub-folders should not push
   everything else off the screen — so each node stays collapsed until asked. */
function buildTree(folders) {
  const root = { children: new Map(), count: 0 };
  for (const f of folders) {
    const parts = f.sub.split('/');
    let node = root, path = '';
    for (const p of parts) {
      path = path ? path + '/' + p : p;
      if (!node.children.has(p)) node.children.set(p, { name: p, path, count: 0, children: new Map() });
      node = node.children.get(p);
    }
    node.count = f.count;
  }
  const roll = n => {
    let t = n.count || 0;
    for (const c of n.children.values()) t += roll(c);
    n.total = t;
    return t;
  };
  roll(root);
  return root;
}

function treeRows(node, libId, depth) {
  let out = '';
  const kids = [...node.children.values()].sort((a, b) => a.name.localeCompare(b.name));
  for (const c of kids) {
    const key = `${libId}/${c.path}`;
    const open = S.expanded.has(key);
    const hasKids = c.children.size > 0;
    const cur = S.lib === libId && S.sub === c.path;
    out += `<button class="nav sub" data-lib="${libId}" data-sub="${esc(c.path)}"
        style="padding-left:${14 + depth * 12}px" ${cur ? 'aria-current="true"' : ''}>
        <span class="tw${hasKids ? '' : ' blank'}"${hasKids ? ` data-twist="${esc(key)}"` : ''}>${I(open ? 'chevD' : 'chev', 11, 2.4)}</span>
        ${I('folder', 13)}<span class="nm">${esc(c.name)}</span>
        <span class="count">${fmtCount(c.total)}</span></button>`;
    if (open && hasKids) out += treeRows(c, libId, depth + 1);
  }
  return out;
}

function sidebar() {
  const sv = S.server || { libraries: [], folders: [], counts: {}, job: {} };
  const c = sv.counts || {};
  const ml = sv.ml || {};
  const nav = (view, icon, label, count) =>
    `<button class="nav" data-view="${view}" ${S.view === view && !S.lib && !S.coll && !S.person ? 'aria-current="true"' : ''}>
      ${I(icon)}<span class="nm">${label}</span>${count != null ? `<span class="count">${fmtCount(count)}</span>` : ''}
    </button>`;

  const libs = (sv.libraries || []).map(l => {
    const folders = (sv.folders || []).filter(f => f.lib === l.id);
    const key = `${l.id}`;
    const open = S.expanded.has(key);
    return `<button class="nav" data-lib="${l.id}" ${S.lib === l.id && !S.sub ? 'aria-current="true"' : ''}>
        <span class="tw${folders.length ? '' : ' blank'}"${folders.length ? ` data-twist="${key}"` : ''}>${I(open ? 'chevD' : 'chev', 11, 2.4)}</span>
        <span class="dot" style="background:${esc(l.color)}"></span>
        <span class="nm">${esc(l.name)}</span><span class="count">${fmtCount(l.count)}</span>
      </button>` + (open ? treeRows(buildTree(folders), l.id, 1) : '');
  }).join('');

  const colls = (S.colls || []).map(x =>
    `<button class="nav" data-coll="${x.id}" ${S.coll === x.id ? 'aria-current="true"' : ''}>
      <span class="tw blank">${I('chev', 11, 2.4)}</span>${I('stack', 14)}<span class="nm">${esc(x.name)}</span>
      <span class="count">${fmtCount(x.count)}</span></button>`).join('');

  const j = sv.job || {};
  const t = (sv.task && sv.task.running) ? sv.task : null;
  const pct = j.total ? Math.round(j.done / j.total * 100) : 0;
  const tpct = t && t.total ? Math.round(t.done / t.total * 100) : 0;
  const phase = { scanning: 'Scanning', hashing: 'Hashing', thumbnailing: 'Building thumbnails',
                  tagging: 'Understanding photos', faces: 'Finding faces',
                  models: 'Downloading models' }[j.phase] || 'Working';

  return `<aside class="side ${S.sideOpen ? 'open' : ''}" id="side">
    <div class="brand">${mark(23, 1.8)}<b>Speckle</b></div>
    <div class="side-scroll">
      <div class="side-sec">Library</div>
      ${nav('grid', 'grid', 'All photos', c.total)}
      ${nav('fav', 'heart', 'Favourites', c.fav)}
      ${nav('videos', 'film', 'Videos', c.video)}
      ${nav('people', 'face', 'People', ml.people || null)}
      <div class="side-sec">Folders <button data-act="add-lib" title="Add a folder">${I('plus', 14)}</button></div>
      ${libs || '<div class="side-note">No folders yet</div>'}
      <div class="side-sec">Collections <button data-act="new-coll" title="New collection">${I('plus', 14)}</button></div>
      ${colls || '<div class="side-note">None yet — select photos and add them</div>'}
      <div class="side-sec">Manage</div>
      ${nav('bin', 'trash', 'Bin', c.bin)}
      ${nav('storage', 'disk', 'Storage')}
      ${nav('settings', 'gear', 'Settings')}
    </div>
    <div class="side-foot">
      ${j.running ? `<div class="idx">
        <div class="idx-top"><span>${phase}</span><span class="mono">${pct}%</span></div>
        <div class="bar"><i style="width:${pct}%"></i></div>
        <div class="idx-sub">${esc(j.label || '')}</div>
        <div class="idx-sub">${fmtCount(j.done)} / ${fmtCount(j.total)}${j.rate ? ` · ${Math.round(j.rate)}/s` : ''}${j.eta > 1 ? ` · ${fmtDur(j.eta)} left` : ''}</div>
      </div>` : t ? `<div class="idx">
        <div class="idx-top"><span>${esc(t.kind)}</span><span class="mono">${tpct}%</span></div>
        <div class="bar"><i style="width:${tpct}%"></i></div>
        <div class="idx-sub">${esc(t.label || '')}</div>
        <div class="idx-sub">${fmtCount(t.done)} / ${fmtCount(t.total)} · ${fmtBytes(t.freed)} freed</div>
      </div>` : `<div class="idx">
        <div class="idx-sub">${fmtCount(c.total)} items · ${fmtBytes(c.bytes)}</div>
        <div class="idx-sub">${sv.ffmpeg === false ? 'ffmpeg not found'
          : ml.ready ? `${fmtCount(ml.tagged)} understood · ${fmtCount(ml.faces)} faces` : 'Ready'}</div>
      </div>`}
    </div>
  </aside>`;
}

function tabbar() {
  const t = (view, icon, label) =>
    `<button class="tab" data-view="${view}" ${S.view === view ? 'aria-current="true"' : ''}>${I(icon, 21, 1.8)}<span>${label}</span></button>`;
  return `<nav class="tabbar glass mobile-only">
    ${t('grid', 'grid', 'Library')}${t('people', 'face', 'People')}
    ${t('upload', 'upload', 'Upload')}${t('collections', 'stack', 'Sets')}${t('settings', 'gear', 'More')}
  </nav>`;
}

function renderMain() {
  const m = $('#main');
  if (!m) return;
  // Replacing #main detaches every tile, but the reuse maps would still hold
  // those dead nodes and paint() would skip re-creating them — an empty grid.
  dropRendered();
  const page = window.PAGES && window.PAGES[S.view];
  m.innerHTML = page ? page() : gridShell();
  wireMain();
  if (isGridView()) { layout(); paint(); }
}

const isGridView = () => ['grid', 'fav', 'videos', 'raw', 'bin', 'coll', 'person'].includes(S.view);

function gridShell() {
  const bin = S.view === 'bin';
  const total = S.list ? S.list.total : 0;
  const chips = [
    ['', 'All'], ['photo', 'Photos'], ['video', 'Videos'],
    ['fav', 'Favourites'], ['raw', 'RAW'], ['error', 'Failed'],
  ];
  const sorts = [
    ['newest', 'Newest first'], ['oldest', 'Oldest first'], ['name', 'Name A–Z'],
    ['name_desc', 'Name Z–A'], ['size', 'Largest first'], ['size_asc', 'Smallest first'],
    ['added', 'Recently added'], ['random', 'Shuffle'],
  ];
  const nudge = S.storage && S.storage.oversized && !S.settings.hid_nudge && !bin;
  const ml = (S.server && S.server.ml) || {};

  return `
  <div class="topbar">
    <button class="btn icon mobile-only" data-act="menu">${I('menu', 18)}</button>
    <div class="search">${I('search', 14)}
      <input id="q" placeholder="${S.smart ? 'Describe what you are looking for…' : 'Search names, folders, tags…'}"
             value="${esc(S.q)}" spellcheck="false">
      <button class="smart ${S.smart ? 'on' : ''}" data-act="smart"
        title="${ml.ready ? 'Search by what the picture shows' : 'Needs the vision model — turn it on in Settings'}">
        ${I('sparkle', 12, 1.6)} Smart</button>
    </div>
    <div class="sep desktop-only"></div>
    <select id="sortSel" class="btn outline desktop-only" style="padding:5px 8px;background:var(--raised)">
      ${sorts.map(([v, l]) => `<option value="${v}" ${S.sort === v ? 'selected' : ''}>${l}</option>`).join('')}
    </select>
    <button class="btn ${S.selMode ? 'on' : ''} desktop-only" data-act="selmode">${I('check', 15)} Select</button>
    <span class="grow"></span>
    ${S.sel.size ? selActions(bin) : ''}
    <div class="zoom desktop-only">${I('grid', 13)}
      <input type="range" id="zoomer" min="3" max="14" value="${S.cols}" aria-label="Grid size">
      <span class="mono" style="font-size:11px;min-width:16px">${S.cols}</span>
    </div>
    <button class="btn icon" data-act="rescan" title="Rescan folders">${I('refresh', 16)}</button>
  </div>
  <div class="chips">
    ${chips.map(([v, l]) => `<button class="chip" data-filter="${v}" ${(S.filter || '') === v ? 'aria-pressed="true"' : ''}>${l}</button>`).join('')}
    <span class="count-label">${fmtCount(total)} shown${S.sub ? ' · ' + esc(S.sub) : ''}</span>
  </div>
  ${S.coll ? collHeader() : ''}
  ${S.person ? personHeader() : ''}
  ${bin ? binStrip() : ''}
  ${nudge ? `<div class="banner">
    <span class="ic">${I('alert', 15)}</span>
    <p><b>Your photos average ${(S.storage.avg_photo_mb).toFixed(1)} MB each</b> — well above typical for their resolution.
       Recompressing would return about ${fmtBytes((S.storage.reclaim.find(r => r.id === 'oversized') || {}).bytes)}.</p>
    <button class="btn solid" data-view="storage" style="margin-left:auto">Review in Storage</button>
    <button class="btn icon" data-act="hide-nudge">${I('close', 14)}</button>
  </div>` : ''}
  <div class="scroll" id="scroll"><div id="gridInner"></div></div>
  <div class="status desktop-only" id="statusbar"></div>`;
}

function collHeader() {
  const c = S.colls.find(x => x.id === S.coll);
  if (!c) return '';
  return `<div class="banner" style="background:var(--panel);border-color:var(--line-soft)">
    <span class="ic" style="background:var(--accent-soft);color:var(--accent)">${I('stack', 15)}</span>
    <p><b>${esc(c.name)}</b> — ${fmtCount(c.count)} items · ${fmtBytes(c.bytes)}</p>
    <button class="btn outline" data-act="export-coll" style="margin-left:auto">${I('download', 15)} Export as zip</button>
    <button class="btn outline" data-act="rename-coll">Rename</button>
    <button class="btn danger" data-act="delete-coll">${I('trash', 15)}</button>
  </div>`;
}

function personHeader() {
  const p = (S.people || []).find(x => x.id === S.person);
  return `<div class="banner" style="background:var(--panel);border-color:var(--line-soft)">
    <span class="ic" style="background:var(--accent-soft);color:var(--accent)">${I('face', 15)}</span>
    <p><b>${esc(p && p.name ? p.name : 'Unnamed person')}</b> — ${p ? fmtCount(p.count) : 0} photos</p>
    <button class="btn outline" data-act="rename-person" style="margin-left:auto">Name this person</button>
  </div>`;
}

function selActions(bin) {
  return `<span class="count-label" style="color:var(--accent)">${S.sel.size} selected</span>
    ${bin ? `<button class="btn outline" data-act="restore-sel">${I('restore', 15)} Restore</button>
             <button class="btn danger" data-act="purge-sel">${I('trash', 15)} Delete for good</button>`
          : `<button class="btn outline" data-act="add-to-coll">${I('stack', 15)} Add to…</button>
             ${S.coll ? `<button class="btn outline" data-act="remove-from-coll">${I('minus', 15)} Remove</button>` : ''}
             <button class="btn outline" data-act="fav-sel">${I('heart', 15)}</button>
             <button class="btn outline" data-act="download-sel">${I('download', 15)}</button>
             <button class="btn danger" data-act="bin-sel">${I('trash', 15)} Bin</button>`}
    <button class="btn icon" data-act="clear-sel">${I('close', 15)}</button><div class="sep"></div>`;
}

function binStrip() {
  const c = (S.server && S.server.counts) || {};
  return `<div class="banner" style="background:color-mix(in srgb,var(--danger) 9%,var(--panel));border-color:color-mix(in srgb,var(--danger) 30%,transparent)">
    <span class="ic" style="background:color-mix(in srgb,var(--danger) 20%,transparent);color:var(--danger)">${I('trash', 15)}</span>
    <p><b>${fmtCount(c.bin)} items in the bin.</b> They stay on the same drive and are restorable until you empty it.</p>
    <button class="btn outline" data-act="restore-all" style="margin-left:auto">${I('restore', 15)} Restore all</button>
    <button class="btn danger" data-act="empty-bin">${I('trash', 15)} Empty bin</button>
  </div>`;
}

// ================================================================ list =====

function query() {
  const p = new URLSearchParams();
  if (S.q && !S.smart) p.set('q', S.q);
  if (S.view === 'bin') p.set('bin', '1');
  let filter = S.filter || '';
  if (S.view === 'fav') filter = 'fav';
  if (S.view === 'videos') filter = 'video';
  if (filter) p.set('filter', filter);
  if (S.lib) p.set('lib', S.lib);
  if (S.sub) p.set('sub', S.sub);
  if (S.coll) p.set('coll', S.coll);
  if (S.person) p.set('person', S.person);
  p.set('sort', S.sort);
  return p.toString();
}

async function loadList() {
  try {
    if (S.smart && S.q.trim()) S.list = await api('/api/search?q=' + encodeURIComponent(S.q.trim()));
    else S.list = await api('/api/media?' + query());
  } catch (e) {
    S.list = { total: 0, ids: [], ts: [], fl: [], dur: [], w: [], h: [] };
    toast('Could not load: ' + e.message, 'err');
  }
  buildGroups();
  if (isGridView()) renderMain();
  updateStatusbar();
}

/* Date grouping only makes sense for date orders. Under a name or size sort the
   list is one continuous run and a date header would be a lie; a relevance sort
   is ordered by score, so the same applies. */
function buildGroups() {
  const L = S.list;
  S.groups = [];
  if (!L || !L.ids.length) return;
  const dated = ['newest', 'oldest', 'added'].includes(S.sort) && !(S.smart && S.q.trim());
  if (!dated) {
    S.groups = [{ key: 0, start: 0, n: L.ids.length, label: '', sub: '' }];
    return;
  }
  let last = null;
  for (let i = 0; i < L.ts.length; i++) {
    const k = dayKey(L.ts[i]);
    if (k !== last) {
      S.groups.push({ key: k, start: i, n: 0, label: dayLabel(L.ts[i]), sub: dateSub(L.ts[i]) });
      last = k;
    }
    S.groups[S.groups.length - 1].n++;
  }
}

// ==================================================== virtualised grid =====

const GAP = 5, HEAD = 44, GROUP_PAD = 6;

function layout() {
  const scroll = $('#scroll'), inner = $('#gridInner');
  if (!scroll || !inner) return;
  const cs = getComputedStyle(scroll);
  const width = scroll.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
  const isNarrow = window.innerWidth <= 860;
  const cols = isNarrow ? clamp(Math.round(S.cols / 2), 2, 5) : S.cols;
  const tile = Math.floor((width - GAP * (cols - 1)) / cols);
  const rowH = tile + GAP;

  let y = 0;
  const blocks = [];
  for (const g of S.groups) {
    const headH = g.label ? HEAD : 6;
    const rows = Math.ceil(g.n / cols);
    blocks.push({ ...g, y, headH, rows, h: headH + rows * rowH });
    y += headH + rows * rowH + GROUP_PAD;
  }
  S.layout = { blocks, total: Math.max(y, 1), tile, rowH, cols };
  inner.style.height = S.layout.total + 'px';
}

const tiles = new Map();
const heads = new Map();
const pending = new Set();

/* Tiles scrolled out of view are detached but kept, not destroyed.
 *
 * Rebuilding an <img> forces the engine to fetch and decode again — even from
 * cache that costs tens of milliseconds each, which is why scrolling back up
 * used to crawl. Holding the element keeps its decoded bitmap alive, so coming
 * back is instant. The pool is capped so a long scroll cannot grow without
 * bound; at ~24 KB of decoded pixels per 320px tile this is a few tens of MB. */
const parked = new Map();
const PARK_MAX = 1200;

function park(id, el) {
  el.remove();
  parked.delete(id);       // re-insert so Map order stays least-recent-first
  parked.set(id, el);
  while (parked.size > PARK_MAX) {
    const oldest = parked.keys().next().value;
    parked.delete(oldest);
  }
}

function dropRendered() {
  tiles.forEach(el => el.remove());
  heads.forEach(el => el.remove());
  tiles.clear(); heads.clear(); pending.clear(); parked.clear();
}

function paint() {
  const scroll = $('#scroll'), inner = $('#gridInner');
  if (!scroll || !inner || !S.layout || !S.list) return;

  if (!S.list.ids.length) { inner.innerHTML = ''; tiles.clear(); heads.clear(); inner.appendChild(emptyState()); return; }
  if (inner.firstElementChild && inner.firstElementChild.classList.contains('empty')) inner.innerHTML = '';

  const { blocks, tile, rowH, cols } = S.layout;
  const top = scroll.scrollTop, vh = scroll.clientHeight;
  // Generous overscan in both directions: rows above matter as much as below,
  // because scrolling back up is the common case.
  const y0 = top - rowH * 6, y1 = top + vh + rowH * 8;

  const wantTiles = new Map(), wantHeads = new Set();
  for (const b of blocks) {
    if (b.y + b.h < y0 || b.y > y1) continue;
    if (b.label) wantHeads.add(b);
    const first = Math.max(0, Math.floor((y0 - (b.y + b.headH)) / rowH));
    const last = Math.min(b.rows - 1, Math.floor((y1 - (b.y + b.headH)) / rowH));
    for (let r = first; r <= last; r++) {
      for (let c = 0; c < cols; c++) {
        const k = r * cols + c;
        if (k >= b.n) break;
        const idx = b.start + k;
        wantTiles.set(S.list.ids[idx], { x: c * (tile + GAP), y: b.y + b.headH + r * rowH, idx });
      }
    }
  }

  for (const [id, el] of tiles) if (!wantTiles.has(id)) { park(id, el); tiles.delete(id); }
  const keys = new Set([...wantHeads].map(b => b.key));
  for (const [key, el] of heads) if (!keys.has(key)) { el.remove(); heads.delete(key); }

  for (const b of wantHeads) {
    let el = heads.get(b.key);
    if (!el) {
      el = document.createElement('div');
      el.className = 'dategroup';
      el.innerHTML = `<h3>${esc(b.label)}</h3><span class="sub">${esc(b.sub)} · ${b.n}</span>
        <button class="pick" data-group="${b.start}:${b.n}">Select all</button>`;
      inner.appendChild(el);
      heads.set(b.key, el);
    }
    el.style.transform = `translateY(${b.y}px)`;
  }

  for (const [id, pos] of wantTiles) {
    let el = tiles.get(id);
    if (!el) {
      el = parked.get(id);
      if (el) parked.delete(id);          // reuse: image already decoded
      else el = makeTile(id, pos.idx);
      inner.appendChild(el);
      tiles.set(id, el);
    }
    el.style.width = tile + 'px';
    el.style.height = tile + 'px';
    el.style.transform = `translate(${pos.x}px, ${pos.y}px)`;
    el.classList.toggle('sel', S.sel.has(id));
    el.dataset.idx = pos.idx;
  }
}

function makeTile(id, idx) {
  const L = S.list, f = L.fl[idx];
  const el = document.createElement('div');
  el.className = 'tile pending';
  el.dataset.id = id;
  el.dataset.idx = idx;

  const kind = K.kind(f);
  const badges = [];
  if (kind === 2) badges.push('<span class="tag-pill">RAW</span>');
  if (kind === 3) badges.push('<span class="tag-pill">PSD</span>');
  if (K.err(f)) badges.push('<span class="tag-pill" style="background:color-mix(in srgb,#F2564D 78%,transparent)">!</span>');
  const expiry = S.view === 'bin' ? binExpiry(L.ts[idx]) : '';

  el.innerHTML = `<img alt="" decoding="async">
    <div class="veil"></div>
    <div class="check">${I('check', 11, 3)}</div>
    <div class="badge">${badges.join('')}</div>
    <span class="fav ${K.fav(f) ? 'on' : ''}">${I('heart', 14, 2)}</span>
    ${kind === 1 ? `<span class="dur">${fmtDur(L.dur[idx])}</span>` : ''}
    ${expiry ? `<span class="expiry">${expiry}</span>` : ''}`;

  const img = el.firstElementChild;
  img.onload = () => { el.classList.remove('pending'); pending.delete(id); };
  img.onerror = () => { pending.add(id); };
  img.src = '/api/thumb/' + id;
  return el;
}

function binExpiry(ts) {
  const days = +(S.settings.bin_days || 30);
  if (!days) return '';
  const left = Math.ceil((ts + days * 86400 - Date.now() / 1000) / 86400);
  return left <= 0 ? 'expiring' : left + 'd left';
}

function emptyState() {
  const d = document.createElement('div');
  d.className = 'empty';
  const noLibs = !S.server || !S.server.libraries.length;
  d.innerHTML = `<div class="in">
    <div class="ring">${I(noLibs ? 'folder' : 'search', 28, 1.6)}</div>
    <h2>${noLibs ? 'Add a folder to begin' : 'Nothing matches'}</h2>
    <p>${noLibs
      ? 'Point Speckle at a folder of photos. Every sub-folder inside it is included, and nothing on disk is moved or renamed.'
      : S.smart ? 'No photos look like that. Try different words, or check that photo understanding has finished in Settings.'
                : 'Try clearing the search or the filters above.'}</p>
    ${noLibs ? `<button class="btn solid" data-act="add-lib">${I('plus', 15)} Choose a folder</button>` : ''}
  </div>`;
  return d;
}

function updateStatusbar() {
  const sb = $('#statusbar');
  if (!sb || !S.server) return;
  const c = S.server.counts || {}, j = S.server.job || {}, ml = S.server.ml || {};
  sb.innerHTML = `<span class="dot" style="background:${j.running ? 'var(--warn)' : 'var(--good)'}"></span>
    <span>${j.running ? 'Indexing…' : `${fmtCount(c.total)} items cached`}</span>
    <span>${fmtBytes(c.bytes)}</span>
    ${c.errors ? `<span style="color:var(--danger)">${c.errors} unreadable</span>` : ''}
    ${c.missing ? `<span style="color:var(--warn)">${c.missing} missing</span>` : ''}
    <div class="right">
      ${ml.ready ? `<span>${fmtCount(ml.tagged)} tagged · ${fmtCount(ml.faces)} faces</span>` : ''}
      <span>${S.server.ffmpeg ? 'ffmpeg ready' : 'no ffmpeg'}</span>
      <span>port ${S.server.port}</span>
    </div>`;
}

// ============================================================== events =====

let chromeWired = false;
function wireChrome() {
  // render() runs more than once (before and after server state arrives) and
  // #app survives each pass, so binding here every time would stack handlers:
  // a twisty would toggle twice and net to nothing, and destructive actions
  // would fire twice.
  if (chromeWired) return;
  chromeWired = true;
  $('#app').addEventListener('click', e => {
    const tw = e.target.closest('[data-twist]');
    if (tw) {
      e.stopPropagation();
      const k = tw.dataset.twist;
      S.expanded.has(k) ? S.expanded.delete(k) : S.expanded.add(k);
      renderSide();
      return;
    }
    const t = e.target.closest('[data-view],[data-lib],[data-coll],[data-act]');
    if (!t) return;
    if (t.dataset.lib) {
      S.lib = +t.dataset.lib; S.sub = t.dataset.sub || null;
      S.coll = null; S.person = null; S.view = 'grid'; S.sel.clear(); go();
    } else if (t.dataset.coll) {
      S.coll = +t.dataset.coll; S.lib = null; S.sub = null; S.person = null;
      S.view = 'coll'; S.sel.clear(); go();
    } else if (t.dataset.view) {
      S.view = t.dataset.view; S.lib = null; S.sub = null; S.coll = null; S.person = null;
      S.sel.clear(); S.sideOpen = false; go();
    } else {
      action(t.dataset.act, t);
    }
  });
}

function wireMain() {
  const scroll = $('#scroll');
  if (scroll) {
    let ticking = false;
    scroll.addEventListener('scroll', () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => { paint(); ticking = false; });
    }, { passive: true });

    scroll.addEventListener('click', e => {
      const g = e.target.closest('[data-group]');
      if (g) {
        const [start, n] = g.dataset.group.split(':').map(Number);
        for (let i = start; i < start + n; i++) S.sel.add(S.list.ids[i]);
        S.selMode = true; renderMain(); return;
      }
      const t = e.target.closest('.tile');
      if (!t) return;
      const id = +t.dataset.id, idx = +t.dataset.idx;
      if (S.selMode || e.ctrlKey || e.metaKey || e.shiftKey) {
        if (e.shiftKey && S.lastClicked >= 0) {
          const [a, b] = [Math.min(S.lastClicked, idx), Math.max(S.lastClicked, idx)];
          for (let i = a; i <= b; i++) S.sel.add(S.list.ids[i]);
        } else {
          S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id);
        }
        S.lastClicked = idx; S.selMode = true; renderMain();
      } else openViewer(idx);
    });
  }

  const q = $('#q');
  if (q) {
    let t;
    q.addEventListener('input', () => {
      clearTimeout(t);
      t = setTimeout(() => { S.q = q.value.trim(); loadList(); }, S.smart ? 400 : 220);
    });
  }
  const z = $('#zoomer');
  if (z) z.addEventListener('input', () => {
    S.cols = +z.value; saveSetting('cols', S.cols);
    z.nextElementSibling.textContent = S.cols;
    layout(); paint();
  });
  const so = $('#sortSel');
  if (so) so.addEventListener('change', () => { S.sort = so.value; saveSetting('sort', S.sort); loadList(); });

  $$('[data-filter]').forEach(b => b.addEventListener('click', () => {
    S.filter = b.dataset.filter || null; S.sel.clear(); loadList();
  }));

  if (window.wirePage) window.wirePage();
}

function go() { renderSide(); renderMain(); loadList(); }
function renderSide() {
  const old = $('#side');
  if (old) old.outerHTML = sidebar();
  $$('.tab').forEach(t => t.setAttribute('aria-current', String(t.dataset.view === S.view)));
}

async function loadColls() {
  try { S.colls = (await api('/api/collections')).collections; } catch (e) { S.colls = []; }
}

async function action(a, el) {
  switch (a) {
    case 'menu': S.sideOpen = !S.sideOpen; renderSide(); break;
    case 'add-lib': window.pickFolder(); break;
    case 'rescan': await post('/api/rescan', {}); toast('Rescanning folders'); break;
    case 'selmode': S.selMode = !S.selMode; if (!S.selMode) S.sel.clear(); renderMain(); break;
    case 'clear-sel': S.sel.clear(); S.selMode = false; renderMain(); break;
    case 'hide-nudge': saveSetting('hid_nudge', true); renderMain(); break;
    case 'smart': {
      const ml = (S.server && S.server.ml) || {};
      if (!ml.ready) { toast('Turn on photo understanding in Settings first', 'err'); return; }
      S.smart = !S.smart; renderMain(); if (S.q) loadList();
      break;
    }

    case 'fav-sel':
      await Promise.all([...S.sel].map(id => post(`/api/media/${id}/meta`, { fav: true })));
      toast(`${S.sel.size} marked as favourite`);
      S.sel.clear(); await loadList(); break;

    case 'download-sel':
      [...S.sel].slice(0, 20).forEach((id, i) =>
        setTimeout(() => { const a2 = document.createElement('a'); a2.href = '/api/download/' + id; a2.download = ''; a2.click(); }, i * 250));
      break;

    case 'bin-sel': {
      const ids = [...S.sel];
      const r = await post('/api/bin', { ids });
      S.sel.clear(); S.selMode = false;
      await refreshServer(); await loadList();
      toast(`${r.moved} moved to the bin`, 'ok', { label: 'Undo', fn: async () => { await post('/api/restore', { ids }); await refreshServer(); await loadList(); } });
      break;
    }
    case 'restore-sel': {
      const r = await post('/api/restore', { ids: [...S.sel] });
      S.sel.clear(); await refreshServer(); await loadList(); toast(`${r.restored} restored`); break;
    }
    case 'purge-sel': {
      if (!confirm(`Permanently delete ${S.sel.size} files?\n\nThis cannot be undone.`)) return;
      const r = await post('/api/purge', { ids: [...S.sel] });
      S.sel.clear(); await refreshServer(); await loadList();
      toast(`Deleted ${r.purged} files, freed ${fmtBytes(r.freed)}`); break;
    }
    case 'restore-all': {
      const r = await post('/api/restore', { ids: S.list.ids });
      await refreshServer(); await loadList(); toast(`${r.restored} restored`); break;
    }
    case 'empty-bin': {
      const c = (S.server.counts || {}).bin || 0;
      if (!confirm(`Permanently delete all ${c} files in the bin?\n\nThis cannot be undone.`)) return;
      const r = await post('/api/purge', { all: true });
      await refreshServer(); await loadList();
      toast(`Emptied the bin — freed ${fmtBytes(r.freed)}`); break;
    }

    // ---- collections ----
    case 'new-coll': {
      const name = prompt('Name this collection');
      if (!name) return;
      await post('/api/collections', { name });
      await loadColls(); renderSide(); toast(`Created “${name}”`); break;
    }
    case 'add-to-coll': window.addToCollection([...S.sel]); break;
    case 'remove-from-coll': {
      const ids = [...S.sel];
      await post(`/api/collections/${S.coll}/items`, { ids, remove: true });
      S.sel.clear(); await loadColls(); renderSide(); await loadList();
      toast(`${ids.length} removed from the collection`); break;
    }
    case 'export-coll':
      toast('Building the archive — the download starts when it is ready');
      location.href = `/api/collections/${S.coll}/export?originals=1`;
      break;
    case 'rename-coll': {
      const c = S.colls.find(x => x.id === S.coll);
      const name = prompt('Rename collection', c ? c.name : '');
      if (!name) return;
      await post(`/api/collections/${S.coll}`, { name });
      await loadColls(); renderSide(); renderMain(); break;
    }
    case 'delete-coll': {
      const c = S.colls.find(x => x.id === S.coll);
      if (!confirm(`Delete the collection “${c ? c.name : ''}”?\n\nThe photos themselves are not touched.`)) return;
      await del(`/api/collections/${S.coll}`);
      S.coll = null; S.view = 'grid';
      await loadColls(); go(); toast('Collection deleted'); break;
    }
    case 'rename-person': {
      const p = (S.people || []).find(x => x.id === S.person);
      const name = prompt('Who is this?', p && p.name ? p.name : '');
      if (name == null) return;
      await post(`/api/people/${S.person}`, { name });
      if (window.loadPeople) await window.loadPeople();
      renderMain(); break;
    }

    case 'find-dupes':
      S.view = 'duplicates'; renderSide(); renderMain();
      toast('Hashing files to find exact duplicates — this runs in the background');
      post('/api/hash', {}).catch(() => {});
      if (window.loadDupes) window.loadDupes();
      break;

    default:
      if (window.pageAction) window.pageAction(a, el);
  }
}

// ============================================================== viewer =====

let V = null, idleTimer = null;

function fitBox(w, h) {
  const stage = V ? V.querySelector('.vstage') : null;
  const narrow = window.innerWidth <= 860;
  const sw = (stage ? stage.clientWidth : window.innerWidth) - (narrow ? 8 : 76);
  const sh = (stage ? stage.clientHeight : window.innerHeight) - (narrow ? 150 : 128);
  if (!w || !h) return { w: Math.round(sw), h: Math.round(sh) };
  const s = Math.min(sw / w, sh / h);
  return { w: Math.max(40, Math.round(w * s)), h: Math.max(40, Math.round(h * s)) };
}

/* The frame is sized from dimensions we already have, before the picture is
   requested. That is the whole trick: the box never changes size when the image
   arrives, so nothing on screen moves — and a video reloading its source for a
   seek keeps its footprint instead of collapsing to nothing. */
function sizeViewer() {
  if (!V || !S.list) return;
  const frame = V.querySelector('.vframe');
  if (!frame) return;
  const box = fitBox(S.list.w ? S.list.w[S.cur] : 0, S.list.h ? S.list.h[S.cur] : 0);
  frame.style.width = box.w + 'px';
  frame.style.height = box.h + 'px';
}

async function openViewer(idx) {
  if (!S.list || !S.list.ids.length) return;
  S.cur = clamp(idx, 0, S.list.ids.length - 1);
  if (!V) {
    V = document.createElement('div');
    V.className = 'viewer';
    V.innerHTML = `<div class="vstage">
        <div class="vbg"></div>
        <div class="vframe"><img class="vmedia" id="vimg" alt=""></div>
        <div class="vtop glass" id="vtop"></div>
        <button class="vnav prev glass desktop-only" data-vact="prev" style="transform:translateY(-50%) rotate(180deg)">${I('chev', 18, 2.2)}</button>
        <button class="vnav next glass desktop-only" data-vact="next">${I('chev', 18, 2.2)}</button>
        <div class="vbot glass" id="vbot"></div>
      </div><div id="vinfo"></div>`;
    document.body.appendChild(V);
    V.addEventListener('click', onViewerClick);
    V.addEventListener('mousemove', bumpIdle);
    V.addEventListener('touchstart', bumpIdle, { passive: true });
    setupSwipe(V);
  }
  await showCurrent();
}

function closeViewer() {
  if (!V) return;
  V.remove(); V = null; S.cur = -1;
  clearTimeout(idleTimer);
}

function bumpIdle() {
  if (!V) return;
  V.classList.remove('idle');
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => V && V.classList.add('idle'), 2600);
}

let showToken = 0;
async function showCurrent() {
  const token = ++showToken;
  const id = S.list.ids[S.cur];
  const isVideo = K.kind(S.list.fl[S.cur]) === 1;

  // Size, then swap the media element, before anything is fetched.
  sizeViewer();
  const frame = V.querySelector('.vframe');
  frame.classList.remove('buffering');
  let media = frame.firstElementChild;
  const wantTag = isVideo ? 'VIDEO' : 'IMG';
  if (!media || media.tagName !== wantTag) {
    frame.innerHTML = isVideo
      ? '<video class="vmedia" id="vvid" playsinline autoplay></video>'
      : '<img class="vmedia" id="vimg" alt="">';
    media = frame.firstElementChild;
  }
  V.querySelector('.vbg').style.backgroundImage = `url('/api/thumb/${id}')`;

  if (!isVideo) {
    // The preview is already prefetched so it paints immediately; the true
    // original then swaps in underneath without disturbing the layout.
    media.src = '/api/preview/' + id;
    const full = new Image();
    full.onload = () => { if (V && token === showToken) media.src = full.src; };
    full.src = '/api/full/' + id;
  }

  let item;
  try { item = await api('/api/media/' + id); }
  catch (e) { toast('Could not open this item', 'err'); return; }
  if (token !== showToken) return;

  V.querySelector('#vtop').innerHTML = topBar(item);
  const bot = V.querySelector('#vbot');
  bot.innerHTML = isVideo ? '' : stripInner();
  bot.classList.toggle('hidden', isVideo);
  V.querySelector('#vinfo').innerHTML = S.info ? infoPanel(item) : '';

  const old = V.querySelector('.vtrans');
  if (old) old.remove();
  if (isVideo) {
    V.querySelector('.vstage').insertAdjacentHTML('beforeend', transportMarkup(item));
    setupVideo(item, media);
  }
  prefetch();
  bumpIdle();
}

function topBar(item) {
  const isVideo = item.kind === 1;
  return `<button class="gbtn icon" data-vact="close">${I('close', 17)}</button>
    <div class="vtitle"><b>${esc(item.name)}</b>
      <span>${S.cur + 1} of ${fmtCount(S.list.total)} · ${fmtBytes(item.bytes)}${item.w ? ` · ${item.w}×${item.h}` : ''}${item.taken ? ' · ' + dateSub(item.taken) : ''}</span>
    </div>
    <span class="grow"></span>
    <button class="gbtn icon ${item.fav ? 'on' : ''}" data-vact="fav" title="Favourite (F)">${I('heart', 16)}</button>
    <button class="gbtn icon" data-vact="collect" title="Add to a collection">${I('stack', 16)}</button>
    ${!isVideo && item.state !== 2 ? `<button class="gbtn" data-vact="edit" title="Edit (E)">${I('edit', 16)} Edit</button>` : ''}
    ${isLocal() ? `<button class="gbtn icon" data-vact="reveal" title="Show in Explorer">${I('external', 16)}</button>` : ''}
    <button class="gbtn icon" data-vact="download" title="Download">${I('download', 16)}</button>
    <button class="gbtn icon warn" data-vact="bin" title="Move to bin (Del)">${I('trash', 16)}</button>
    <button class="gbtn ${S.info ? 'on' : ''}" data-vact="info" title="Info (I)">${I('info', 16)}</button>`;
}

function stripInner() {
  const L = S.list;
  const a = Math.max(0, S.cur - 8), b = Math.min(L.ids.length, S.cur + 9);
  let out = '';
  for (let i = a; i < b; i++) {
    out += `<div class="sthumb ${i === S.cur ? 'cur' : ''}" data-jump="${i}"><img src="/api/thumb/${L.ids[i]}" alt=""></div>`;
  }
  return `<button class="gbtn icon desktop-only" data-vact="prev" style="transform:rotate(180deg)">${I('chev', 16, 2.2)}</button>
    <div class="strip">${out}</div>
    <button class="gbtn icon desktop-only" data-vact="next">${I('chev', 16, 2.2)}</button>`;
}

function infoPanel(m) {
  const row = (k, v) => v == null || v === '' ? '' : `<dl class="kv"><dt>${k}</dt><dd>${esc(v)}</dd></dl>`;
  const exposure = [
    m.fnum ? 'f/' + (+m.fnum).toFixed(1) : null, m.expo || null, m.iso ? 'ISO ' + m.iso : null,
  ].filter(Boolean).join(' · ');
  const tags = (m.tags || []).map(t =>
    `<button class="tag" data-tagsearch="${esc(t.name)}">${esc(t.name)}</button>`).join('');
  const faces = (m.faces || []).map(f =>
    `<button class="tag" data-person="${f.cluster || ''}">${I('face', 12)} ${esc(f.name || 'Unnamed')}</button>`).join('');
  const colls = (m.collections || []).map(c =>
    `<button class="tag" data-gocoll="${c.id}">${I('stack', 12)} ${esc(c.name)}</button>`).join('');

  return `<aside class="info">
    ${m.camera || exposure ? `<div class="grp"><h4>Capture</h4>
      ${row('Camera', m.camera)}${row('Lens', m.lens)}
      ${row('Settings', exposure)}${row('Focal', m.focal ? (+m.focal).toFixed(0) + ' mm' : null)}
    </div>` : ''}
    <div class="grp"><h4>File</h4>
      ${row('Name', m.name)}${row('Size', fmtBytes(m.bytes))}
      ${row('Dimensions', m.w ? `${m.w} × ${m.h}` : null)}
      ${row('Kind', m.ext.toUpperCase() + (m.vcodec ? ` · ${m.vcodec}${m.acodec ? '/' + m.acodec : ''}` : ''))}
      ${row('Duration', m.dur ? fmtDur(m.dur) : null)}
      ${row('Taken', m.taken ? new Date(m.taken * 1000).toLocaleString() : null)}
      ${row('Library', m.library)}${row('Folder', m.sub || '(root)')}
      ${m.err ? row('Problem', m.err) : ''}
      <h4 style="padding-top:8px">Location on disk</h4>
      <div class="pathbox"><span class="mono" id="mpath">${esc(m.path)}</span></div>
      <div class="pathacts">
        <button class="btn outline" data-vact="copypath">${I('copy', 14)} Copy path</button>
        ${isLocal() ? `<button class="btn outline" data-vact="reveal">${I('external', 14)} Show in Explorer</button>` : ''}
      </div>
    </div>
    ${tags ? `<div class="grp"><h4>What Speckle sees</h4><div class="tags">${tags}</div></div>` : ''}
    ${faces ? `<div class="grp"><h4>People</h4><div class="tags">${faces}</div></div>` : ''}
    <div><h4>Collections</h4><div class="tags">${colls}
      <button class="tag add" data-vact="collect">+ add</button></div></div>
  </aside>`;
}

function prefetch() {
  const L = S.list;
  for (let d = -3; d <= 3; d++) {
    const i = S.cur + d;
    if (i < 0 || i >= L.ids.length || d === 0) continue;
    if (K.kind(L.fl[i]) === 1) continue;
    const im = new Image();
    im.src = '/api/preview/' + L.ids[i];
  }
}

function step(n) {
  const next = S.cur + n;
  if (next < 0 || next >= S.list.ids.length) return;
  S.cur = next;
  showCurrent();
}

async function onViewerClick(e) {
  const j = e.target.closest('[data-jump]');
  if (j) { S.cur = +j.dataset.jump; showCurrent(); return; }
  const ts = e.target.closest('[data-tagsearch]');
  if (ts) { closeViewer(); S.q = ts.dataset.tagsearch; S.smart = false; S.view = 'grid'; go(); return; }
  const gp = e.target.closest('[data-person]');
  if (gp && gp.dataset.person) { closeViewer(); S.person = +gp.dataset.person; S.view = 'person'; go(); return; }
  const gc = e.target.closest('[data-gocoll]');
  if (gc) { closeViewer(); S.coll = +gc.dataset.gocoll; S.view = 'coll'; go(); return; }

  const t = e.target.closest('[data-vact]');
  if (!t) return;
  const id = S.list.ids[S.cur];
  switch (t.dataset.vact) {
    case 'close': closeViewer(); break;
    case 'prev': step(-1); break;
    case 'next': step(1); break;
    case 'info': S.info = !S.info; saveSetting('info', S.info); showCurrent(); break;
    case 'collect': window.addToCollection([id]); break;
    case 'copypath': {
      const p = $('#mpath', V);
      if (p) navigator.clipboard.writeText(p.textContent).then(() => toast('Path copied'));
      break;
    }
    case 'reveal':
      try { await post('/api/reveal', { id }); } catch (err) { toast(err.message, 'err'); }
      break;
    case 'fav': {
      const on = !t.classList.contains('on');
      await post(`/api/media/${id}/meta`, { fav: on });
      S.list.fl[S.cur] = on ? S.list.fl[S.cur] | 4 : S.list.fl[S.cur] & ~4;
      t.classList.toggle('on', on);
      const tile = tiles.get(id);
      if (tile) tile.querySelector('.fav').classList.toggle('on', on);
      break;
    }
    case 'download': { const a = document.createElement('a'); a.href = '/api/download/' + id; a.download = ''; a.click(); break; }
    case 'edit': {
      const item = await api('/api/media/' + id);
      closeViewer();
      window.SpeckleEditor.open(item, async () => {
        toast('Saved');
        await refreshServer(); await loadList();
      });
      break;
    }
    case 'bin': {
      await post('/api/bin', { ids: [id] });
      const at = S.cur;
      ['ids', 'ts', 'fl', 'dur', 'w', 'h'].forEach(k => { if (S.list[k]) S.list[k].splice(at, 1); });
      S.list.total--;
      buildGroups(); dropRendered(); layout(); paint();
      await refreshServer(); renderSide();
      toast('Moved to the bin', 'ok', { label: 'Undo', fn: async () => { await post('/api/restore', { ids: [id] }); await refreshServer(); await loadList(); } });
      if (!S.list.ids.length) closeViewer();
      else { S.cur = Math.min(at, S.list.ids.length - 1); showCurrent(); }
      break;
    }
  }
}

function setupSwipe(el) {
  let x0 = null, y0 = null;
  el.addEventListener('touchstart', e => { x0 = e.touches[0].clientX; y0 = e.touches[0].clientY; }, { passive: true });
  el.addEventListener('touchend', e => {
    if (x0 == null) return;
    const dx = e.changedTouches[0].clientX - x0, dy = e.changedTouches[0].clientY - y0;
    if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy)) step(dx < 0 ? 1 : -1);
    else if (dy > 90 && Math.abs(dy) > Math.abs(dx)) closeViewer();
    else if (dy < -90 && Math.abs(dy) > Math.abs(dx)) { S.info = !S.info; showCurrent(); }
    x0 = y0 = null;
  }, { passive: true });
}

// ======================================================== video player =====

function transportMarkup(m) {
  return `<div class="vtrans glass" id="vtrans">
    <div class="scrub" id="scrub"><span class="buf"></span><span class="play"></span><span class="knob"></span></div>
    <div class="trow">
      <button class="pbtn main" data-vid="toggle">${I('play', 15, 2)}</button>
      <button class="pbtn" data-vid="back" title="Back 10s">${I('back10', 15)}</button>
      <button class="pbtn" data-vid="fwd" title="Forward 10s">${I('fwd10', 15)}</button>
      <span class="tt" id="vtime">0:00 / ${fmtDur(m.dur)}</span>
      <span class="grow"></span>
      ${!m.native ? `<span class="tag-pill" style="height:20px"
         title="This clip is ${esc(m.vcodec || '')}${m.acodec ? '/' + esc(m.acodec) : ''}, which browsers cannot play">LIVE TRANSCODE</span>` : ''}
      <button class="gbtn" data-vid="speed">1.0×</button>
      <button class="pbtn" data-vid="mute">${I('vol', 15)}</button>
      <input class="vol" type="range" min="0" max="1" step="0.02" value="1" id="vvol" aria-label="Volume">
      <button class="pbtn" data-vid="full">${I('expand', 15)}</button>
    </div>
  </div>`;
}

function setupVideo(m, v) {
  const trans = $('#vtrans', V), scrub = $('#scrub', V);
  if (!v || !trans) return;

  // A transcoded stream has no index to seek within, so time is tracked as an
  // offset from wherever ffmpeg was told to start.
  const transcoded = !m.native;
  let base = 0;
  v.src = transcoded ? `/api/transcode/${m.id}?t=0` : `/api/video/${m.id}`;

  const duration = () => (transcoded ? (m.dur || 0) : (v.duration || m.dur || 0));
  const now = () => base + (v.currentTime || 0);
  const busy = on => { const f = V && V.querySelector('.vframe'); if (f) f.classList.toggle('buffering', on); };

  const draw = () => {
    const d = duration() || 1, t = now();
    const pct = clamp(t / d * 100, 0, 100);
    $('.play', scrub).style.width = pct + '%';
    $('.knob', scrub).style.left = pct + '%';
    if (v.buffered.length) {
      $('.buf', scrub).style.width = clamp((base + v.buffered.end(v.buffered.length - 1)) / d * 100, 0, 100) + '%';
    }
    const lbl = $('#vtime', V);
    if (lbl) lbl.textContent = `${fmtDur(t)} / ${fmtDur(d)}`;
    const tog = $('[data-vid="toggle"]', V);
    if (tog) tog.innerHTML = v.paused ? I('play', 15, 2) : I('pause', 15, 2.4);
  };
  ['timeupdate', 'play', 'pause', 'progress', 'loadedmetadata'].forEach(ev => v.addEventListener(ev, draw));
  v.addEventListener('error', () => {
    if (!v.getAttribute('src')) return;
    toast(S.server.ffmpeg ? 'This clip could not be played' : 'ffmpeg is not installed, so this format cannot play', 'err');
    busy(false);
  });
  // Reloading the source blanks the element for a moment. The frame keeps its
  // size regardless; a spinner explains why nothing is moving yet.
  v.addEventListener('waiting', () => busy(true));
  v.addEventListener('playing', () => busy(false));
  v.addEventListener('canplay', () => busy(false));
  v.addEventListener('loadeddata', () => busy(false));

  const seek = to => {
    const d = duration();
    const t = clamp(to, 0, Math.max(0, d - 0.2));
    if (transcoded) {
      busy(true);
      base = t;
      v.src = `/api/transcode/${m.id}?t=${t.toFixed(2)}`;
      v.play().catch(() => {});
    } else v.currentTime = t;
    draw();
  };

  scrub.addEventListener('pointerdown', e => {
    const r = scrub.getBoundingClientRect();
    seek((e.clientX - r.left) / r.width * duration());
  });

  trans.addEventListener('click', e => {
    const b = e.target.closest('[data-vid]');
    if (!b) return;
    switch (b.dataset.vid) {
      case 'toggle': v.paused ? v.play() : v.pause(); break;
      case 'back': seek(now() - 10); break;
      case 'fwd': seek(now() + 10); break;
      case 'mute': v.muted = !v.muted; b.innerHTML = I(v.muted ? 'mute' : 'vol', 15); break;
      case 'speed': {
        const rates = [0.5, 1, 1.25, 1.5, 2];
        const i = (rates.indexOf(v.playbackRate) + 1) % rates.length;
        v.playbackRate = rates[i];
        b.textContent = (Number.isInteger(rates[i]) ? rates[i].toFixed(1) : String(rates[i])) + '×';
        break;
      }
      case 'full': (v.requestFullscreen || v.webkitEnterFullscreen || (() => {})).call(v); break;
    }
  });
  const vol = $('#vvol', V);
  if (vol) vol.addEventListener('input', e => { v.volume = +e.target.value; v.muted = false; });
  v.addEventListener('click', () => { v.paused ? v.play() : v.pause(); });
  draw();
}

// ============================================================ keyboard =====

document.addEventListener('keydown', async e => {
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName);
  if (document.querySelector('.editor')) return;

  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault(); const q = $('#q'); if (q) { q.focus(); q.select(); } return;
  }
  if (typing) { if (e.key === 'Escape') document.activeElement.blur(); return; }

  if (V) {
    switch (e.key) {
      case 'Escape': closeViewer(); return;
      case 'ArrowRight': case 'j': step(1); return;
      case 'ArrowLeft': case 'k': step(-1); return;
      case 'i': S.info = !S.info; saveSetting('info', S.info); showCurrent(); return;
      case 'f': { const b = $('[data-vact="fav"]', V); if (b) b.click(); return; }
      case 'e': { const t = $('[data-vact="edit"]', V); if (t) t.click(); return; }
      case 'Delete': case 'Backspace': { const t = $('[data-vact="bin"]', V); if (t) t.click(); return; }
      case ' ': { const v = $('#vvid', V); if (v) { e.preventDefault(); v.paused ? v.play() : v.pause(); } return; }
    }
    return;
  }

  if (!isGridView()) return;
  switch (e.key) {
    case 'Escape': if (S.sel.size) { S.sel.clear(); S.selMode = false; renderMain(); } break;
    case '+': case '=': S.cols = clamp(S.cols - 1, 3, 14); saveSetting('cols', S.cols); renderMain(); break;
    case '-': case '_': S.cols = clamp(S.cols + 1, 3, 14); saveSetting('cols', S.cols); renderMain(); break;
    case 'a': if (e.ctrlKey || e.metaKey) { e.preventDefault(); S.list.ids.forEach(i => S.sel.add(i)); S.selMode = true; renderMain(); } break;
    case 'Delete': if (S.sel.size) action('bin-sel'); break;
  }
});

// =============================================================== poll ======

let lastJob = '';
function poll() {
  // Polling is paced by what is happening, and stops entirely when the window
  // is hidden — a background server with nobody watching should cost nothing.
  let timer = null;
  const tick = async () => {
    const snap = o => JSON.stringify([o.job, o.counts, o.task, o.ml]);
    const before = S.server ? snap(S.server) : '';
    await refreshServer();
    if (before !== snap(S.server)) {
      renderSide();
      updateStatusbar();
      if (window.onServerTick) window.onServerTick();
      if (pending.size) {
        for (const id of [...pending]) {
          const el = tiles.get(id);
          if (el) { pending.delete(id); el.querySelector('img').src = '/api/thumb/' + id + '?r=' + Date.now(); }
        }
      }
    }
    const j = S.server.job || {};
    const key = j.running + '|' + j.phase;
    if (lastJob && lastJob !== key && !j.running) {
      await loadList();
      if (window.loadStorage) window.loadStorage();
    }
    lastJob = key;

    const busy = (S.server.job && S.server.job.running)
              || (S.server.task && S.server.task.running);
    timer = setTimeout(tick, document.hidden ? 30000 : busy ? 1200 : 6000);
  };

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) { clearTimeout(timer); tick(); }
  });
  tick();
  if (window.loadStorage) window.loadStorage();
}

boot();
