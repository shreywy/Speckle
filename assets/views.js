/* Speckle — pages and modals. Loaded before app.js, which calls into
 * window.PAGES / window.wirePage / window.pageAction. */
'use strict';

// ============================================================== storage ====

async function loadStorage() {
  try { S.storage = await api('/api/storage'); } catch (e) { S.storage = null; }
  if (S.view === 'storage') renderMain();
}
window.loadStorage = loadStorage;

function pageStorage() {
  const st = S.storage;
  if (!st) { loadStorage(); return `<div class="page"><h2>Storage</h2><p class="lede">Measuring…</p></div>`; }

  // The bar shows where the library went. Free space is usually orders of
  // magnitude larger and would squash everything else into one invisible pixel,
  // so it is stated beside the bar rather than drawn to scale.
  const total = st.breakdown.reduce((a, b) => a + b.bytes, 0) || 1;
  const bar = st.breakdown.filter(b => b.bytes > 0)
    .map(b => `<i style="flex:${b.bytes};background:${b.color}" title="${b.label}: ${fmtBytes(b.bytes)}"></i>`).join('');
  const leg = st.breakdown.filter(b => b.bytes > 0).map(b =>
    `<span class="leg"><b style="background:${b.color}"></b>${b.label} <em>${fmtBytes(b.bytes)}</em>${b.count ? ` <s>${fmtCount(b.count)}</s>` : ''}</span>`).join('');

  const reclaimTotal = st.reclaim.reduce((a, b) => a + b.bytes, 0);
  const cards = st.reclaim.map(r => `<div class="rc" style="--accent:${r.color}">
      <span class="lab">${r.label}</span>
      <span class="amt">${fmtBytes(r.bytes)}</span>
      <span class="det">${esc(r.detail)}</span>
      <button class="btn outline go" data-recl="${r.id}">${
        r.id === 'bin' ? 'Empty bin' : r.id === 'duplicates' ? 'Review groups' : 'Recompress…'}</button>
    </div>`).join('');

  const c = S.compress;
  return `<div class="page">
    <h2>Storage &amp; cleanup</h2>
    <p class="lede">Where the drive went, and the things that will give the most of it back.
      Nothing here touches a file until you confirm — and recompression moves every original to the bin first,
      so a whole run stays undoable until you empty it.</p>

    <div class="card">
      <div style="display:flex;justify-content:space-between;align-items:baseline;margin-bottom:4px">
        <b style="font-size:15px;letter-spacing:-.01em">${fmtBytes(total)} indexed</b>
        <span class="count-label" style="margin:0">${fmtBytes(st.free)} free on the drive</span>
      </div>
      <div class="usebar">${bar}</div>
      <div class="legend">${leg}</div>
    </div>

    <div class="sec-h"><h3>Reclaimable</h3><span class="rule"></span>
      <span class="count-label">${fmtBytes(reclaimTotal)} total</span></div>
    <div class="reclaim">${cards}</div>

    <div class="sec-h"><h3>Recompress</h3><span class="rule"></span></div>
    <div class="job">
      <div class="card">
        <div class="field">
          <label for="cq">Quality — <span class="mono" id="cqv">${c.quality}</span></label>
          <input type="range" id="cq" min="55" max="98" value="${c.quality}">
          <div class="hint">Below about 78, banding starts to show on skin tones and gradients.
            82 is where most people stop seeing a difference.</div>
        </div>
        <div class="field">
          <label for="cscope">What to recompress</label>
          <select id="cscope">
            <option value="oversized" ${c.scope === 'oversized' ? 'selected' : ''}>Oversized photos</option>
            <option value="video" ${c.scope === 'video' ? 'selected' : ''}>Videos in inefficient codecs → HEVC</option>
          </select>
        </div>
        <div class="field">
          <label for="cmax">Longest edge</label>
          <select id="cmax">
            ${[[99999, 'Leave original size'], [4096, '4096 px'], [3072, '3072 px'], [2560, '2560 px'], [2048, '2048 px']]
              .map(([v, l]) => `<option value="${v}" ${c.max_edge === v ? 'selected' : ''}>${l}</option>`).join('')}
          </select>
        </div>
        <div class="toggle"><button class="tg" data-tog="keep_originals" aria-pressed="${c.keep_originals}" role="switch"></button>
          <span class="tx"><b>Move originals to the bin</b><span>Recoverable until you empty it. Turning this off deletes them outright.</span></span></div>
        <div class="warnbox">${I('alert', 16)}<span><b>This rewrites your files.</b>
          Recompression is lossy and cannot be undone once the bin is emptied. Measure a sample first if you are unsure.</span></div>
      </div>
      <div>
        <div class="est">
          <div style="font-size:11px;letter-spacing:.09em;text-transform:uppercase;color:var(--text-3);font-weight:650">Estimated saving</div>
          <div class="big" id="cEst">—</div>
          <div class="sm" id="cEstSub">Run a sample to measure this on your own files.</div>
          <button class="btn outline" id="cSample" style="width:100%;justify-content:center;margin-top:12px;padding:9px">
            ${I('zap', 15)} Measure on a sample</button>
          <button class="btn solid" id="cRun" style="width:100%;justify-content:center;margin-top:7px;padding:9px">
            Run recompression</button>
        </div>
      </div>
    </div>
  </div>`;
}

// =========================================================== duplicates ====

async function loadDupes() {
  try { S.dupes = await api('/api/duplicates'); } catch (e) { S.dupes = { groups: [] }; }
  if (S.view === 'duplicates') renderMain();
}
window.loadDupes = loadDupes;

function pageDuplicates() {
  const d = S.dupes;
  if (!d) { loadDupes(); return `<div class="page"><h2>Duplicates</h2><p class="lede">Comparing…</p></div>`; }
  const freed = d.groups.reduce((a, g) => a + g.freed, 0);

  return `<div class="page">
    <h2>Duplicates</h2>
    <p class="lede">Grouped by what the pictures actually look like, so the same photo re-saved at a different
      size or quality still matches. The largest copy in each group is kept; every path is shown so you can
      see exactly which file would go.</p>
    ${d.groups.length ? `<div class="strip-alert" style="background:var(--panel);border-color:var(--line)">
      <p><b>${d.groups.length} groups</b> · ${fmtBytes(freed)} recoverable</p>
      <button class="btn danger" style="margin-left:auto" id="binDupes">${I('trash', 15)} Bin every extra copy</button>
    </div>` : `<div class="card">No duplicates found. ${I('check', 15)}</div>`}
    ${d.groups.slice(0, 200).map(g => `<div class="dupgrp">
      <div class="dupthumbs">${g.items.map(it =>
        `<div class="dt ${it.keep ? 'keep' : 'drop'}" data-open-id="${it.id}" title="${esc(it.path)}">
           <img src="/api/thumb/${it.id}" alt=""></div>`).join('')}</div>
      <div class="dupmeta">
        ${g.items.map(it => `<div class="duprow ${it.keep ? 'keep' : ''}">
          <span class="tick ${it.keep ? 'keep' : 'drop'}">${it.keep ? 'keep' : 'remove'}</span>
          <span class="dp mono" title="${esc(it.path)}">${esc(it.path)}</span>
          <span class="ds mono">${fmtBytes(it.bytes)}${it.w ? ` · ${it.w}×${it.h}` : ''}</span>
          ${isLocal() ? `<button class="btn icon" data-revealpath="${esc(it.path)}" title="Show in Explorer">${I('external', 14)}</button>` : ''}
        </div>`).join('')}
      </div>
      <div class="act">
        <span class="count-label" style="margin:0">${fmtBytes(g.freed)} freed</span>
        <button class="btn outline" data-bin-group="${g.items.filter(i => !i.keep).map(i => i.id).join(',')}">Bin the extras</button>
      </div>
    </div>`).join('')}
  </div>`;
}

// ============================================================== people =====

async function loadPeople() {
  try { S.people = (await api('/api/people')).people; } catch (e) { S.people = []; }
  if (S.view === 'people') renderMain();
}
window.loadPeople = loadPeople;

function pagePeople() {
  const ml = (S.server && S.server.ml) || {};
  if (!ml.faces_ready) {
    return `<div class="page">
      <h2>People</h2>
      <p class="lede">Speckle can find faces in your photos and group them by person — entirely on this machine.
        Nothing is uploaded and no account is involved.</p>
      <div class="card">
        <b>Face recognition is off.</b>
        <p style="color:var(--text-2);font-size:12.5px;margin:6px 0 14px">
          It needs a one-off ${fmtBytes(288621354)} model download. After that, finding faces across a
          library runs at roughly 20–40 photos a second on this machine.</p>
        <button class="btn solid" data-act="enable-faces">${I('face', 15)} Download and turn on</button>
      </div>
    </div>`;
  }
  if (!S.people.length) { loadPeople(); }

  const named = S.people.filter(p => p.name);
  const unnamed = S.people.filter(p => !p.name);
  const sel = S.psel;
  const card = p => `<button class="person ${sel.has(p.id) ? 'sel' : ''}" data-person-card="${p.id}">
      <span class="pface">${p.cover ? `<img src="/api/face/${p.cover}" alt="">` : I('face', 24)}</span>
      <span class="ptick">${I('check', 12, 3)}</span>
      <b>${esc(p.name || 'Unnamed')}</b><span>${fmtCount(p.count)} photos</span>
    </button>`;

  // The person the merge keeps: a named one if exactly one is named, otherwise
  // whoever has the most photos.
  const chosen = mergeTarget();

  return `<div class="page">
    <h2>People</h2>
    <p class="lede">${fmtCount(ml.faces)} faces found across ${fmtCount(ml.people)} people.
      Click a face to see their photos, then give them a name. If the same person
      shows up twice, select both and merge them.</p>

    ${ml.faces_pending ? `<div class="banner"><span class="ic">${I('face', 15)}</span>
      <p>Still working through ${fmtCount(ml.faces_pending)} photos.</p></div>` : ''}

    <div class="strip-alert" style="background:var(--panel);border-color:${sel.size >= 2 ? 'var(--accent-line)' : 'var(--line)'}">
      <p>${sel.size
        ? `<b>${sel.size} selected.</b> ${sel.size >= 2
            ? `They will be merged into <b>${esc(chosen ? (chosen.name || 'the group with ' + chosen.count + ' photos') : '')}</b>.`
            : 'Select at least one more to merge.'}`
        : `<b>${S.pselMode ? 'Pick the people who are the same person.' : 'Same person listed twice?'}</b> ${
            S.pselMode ? '' : 'Turn on Select, tick each copy, then merge them into one.'}`}</p>
      <span class="grow" style="flex:1"></span>
      ${sel.size >= 2 ? `<button class="btn solid" data-act="merge-people">${I('face', 15)} Merge ${sel.size} into one</button>` : ''}
      <button class="btn ${S.pselMode ? 'on' : 'outline'}" data-act="people-select">${I('check', 15)} ${S.pselMode ? 'Done' : 'Select'}</button>
    </div>

    ${named.length ? `<div class="sec-h"><h3>Named</h3><span class="rule"></span></div>
      <div class="people">${named.map(card).join('')}</div>` : ''}
    ${unnamed.length ? `<div class="sec-h"><h3>Not named yet</h3><span class="rule"></span>
        <span class="count-label">${unnamed.length} groups</span></div>
      <div class="people">${unnamed.map(card).join('')}</div>` : ''}
    ${!S.people.length ? `<div class="card">No faces found yet.</div>` : ''}
  </div>`;
}

/** Which person a merge should keep. */
function mergeTarget() {
  const picked = (S.people || []).filter(p => S.psel.has(p.id));
  if (!picked.length) return null;
  const withName = picked.filter(p => p.name);
  if (withName.length === 1) return withName[0];
  return picked.slice().sort((a, b) => b.count - a.count)[0];
}

// ========================================================= collections =====

function pageCollections() {
  const cs = S.colls || [];
  return `<div class="page">
    <h2>Collections</h2>
    <p class="lede">Curated sets, like playlists. A photo can sit in as many as you like —
      nothing is copied or moved on disk until you export.</p>
    <div class="sec-h"><h3>Your collections</h3><span class="rule"></span>
      <button class="btn solid" data-act="new-coll">${I('plus', 15)} New collection</button></div>
    ${cs.length ? `<div class="collgrid">${cs.map(c => `
      <button class="collcard" data-coll="${c.id}">
        <span class="cover">${c.cover ? `<img src="/api/thumb/${c.cover}" alt="">` : I('stack', 26)}</span>
        <b>${esc(c.name)}</b>
        <span>${fmtCount(c.count)} items · ${fmtBytes(c.bytes)}</span>
      </button>`).join('')}</div>`
      : `<div class="card">None yet. Select some photos in the library and choose <b>Add to…</b></div>`}
  </div>`;
}

// ============================================================== upload =====

function pageUpload() {
  const dest = (S.settings.inbox || 'E:/Photos/Speckle').replace(/"/g, '');
  const org = S.settings.inbox_organize !== false;
  return `<div class="page">
    <h2>Upload</h2>
    <p class="lede">Files land in your upload folder and are indexed straight away.
      HEIC arrives exactly as your phone shot it — converting is a separate, deliberate choice.</p>

    <div class="card" id="drop" style="border-style:dashed;text-align:center;padding:38px 18px">
      <div class="ring" style="width:56px;height:56px;border-radius:16px;display:grid;place-items:center;margin:0 auto 14px;background:var(--accent-soft);color:var(--accent)">${I('upload', 26, 1.6)}</div>
      <h3 style="margin:0 0 6px;font-size:16px">Drop photos and videos here</h3>
      <p style="margin:0 0 16px;font-size:12.5px;color:var(--text-2)">or choose them from your device</p>
      <input type="file" id="fileInput" multiple accept="image/*,video/*" hidden>
      <button class="btn solid" id="pickFiles">${I('plus', 15)} Choose files</button>
    </div>
    <div id="upList" style="margin-top:14px"></div>

    <div class="sec-h"><h3>Where uploads go</h3><span class="rule"></span></div>
    <div class="card">
      <div class="field">
        <label for="inboxPath">Upload folder</label>
        <div style="display:flex;gap:8px">
          <input type="text" id="inboxPath" value="${esc(dest)}" spellcheck="false">
          <button class="btn outline" id="pickInbox" style="flex:none">${I('folder', 15)} Browse</button>
        </div>
        <div class="hint">Registered as a library automatically, so anything uploaded shows up in the grid.</div>
      </div>
      <div class="toggle"><button class="tg" data-setting="inbox_organize" aria-pressed="${org}" role="switch"></button>
        <span class="tx"><b>Sort into year-month folders</b>
          <span>A photo taken in June 2026 lands in <span class="mono">${esc(dest)}/2026-06</span>,
            using the date it was taken rather than the date it was uploaded.</span></span></div>
    </div>
  </div>`;
}

function wireUpload() {
  const input = $('#fileInput'), drop = $('#drop');
  if (!input) return;
  $('#pickFiles').onclick = () => input.click();
  input.onchange = () => doUpload([...input.files]);
  ['dragover', 'dragenter'].forEach(ev => drop.addEventListener(ev, e => {
    e.preventDefault(); drop.style.borderColor = 'var(--accent)';
  }));
  ['dragleave', 'drop'].forEach(ev => drop.addEventListener(ev, e => {
    e.preventDefault(); drop.style.borderColor = '';
  }));
  drop.addEventListener('drop', e => doUpload([...e.dataTransfer.files]));

  const ip = $('#inboxPath');
  if (ip) ip.addEventListener('change', () => saveSetting('inbox', ip.value.trim()));
  const pi = $('#pickInbox');
  if (pi) pi.onclick = () => pickFolder(p => { ip.value = p; saveSetting('inbox', p); }, 'Choose the upload folder');
}

async function doUpload(files) {
  if (!files.length) return;
  const list = $('#upList');
  const total = files.reduce((a, f) => a + f.size, 0);
  list.innerHTML = `<div class="card"><b>Uploading ${files.length} files · ${fmtBytes(total)}</b>
    <div class="bar" style="margin-top:10px"><i id="upBar" style="width:0%"></i></div>
    <div class="mono" id="upMsg" style="font-size:11.5px;color:var(--text-3);margin-top:6px">starting…</div></div>`;

  const fd = new FormData();
  files.forEach(f => fd.append('file', f, f.name));
  const xhr = new XMLHttpRequest();
  xhr.upload.onprogress = e => {
    if (!e.lengthComputable) return;
    const p = Math.round(e.loaded / e.total * 100);
    $('#upBar').style.width = p + '%';
    $('#upMsg').textContent = `${p}% · ${fmtBytes(e.loaded)} of ${fmtBytes(e.total)}`;
  };
  xhr.onload = async () => {
    try {
      const r = JSON.parse(xhr.responseText);
      $('#upMsg').textContent = `Saved ${r.saved} files into ${r.inbox}${r.failed ? ` · ${r.failed} failed` : ''}`;
      toast(`Uploaded ${r.saved} files`);
      await refreshServer(); renderSide();
    } catch (e) { $('#upMsg').textContent = 'Upload failed'; }
  };
  xhr.onerror = () => { $('#upMsg').textContent = 'Upload failed'; };
  xhr.open('POST', '/api/upload');
  xhr.send(fd);
}

// ============================================================ settings =====

function pageSettings() {
  const sv = S.server || {};
  const ml = sv.ml || {};
  const libs = (sv.libraries || []).map(l => `<div class="libcard">
      <span class="fdot" style="background:${l.color}22;color:${l.color}">${I('folder', 18)}</span>
      <span class="m"><b>${esc(l.name)}</b><span>${esc(l.path)} · ${fmtCount(l.count)} items · ${fmtBytes(l.bytes)}</span></span>
      <span class="st ${sv.job && sv.job.running ? 'run' : 'ok'}">${sv.job && sv.job.running ? 'Indexing' : 'Indexed'}</span>
      ${isLocal() ? `<button class="btn icon" data-revealpath="${esc(l.path)}" title="Show in Explorer">${I('external', 15)}</button>` : ''}
      <button class="btn icon danger" data-rmlib="${l.id}" title="Remove from Speckle">${I('close', 15)}</button>
    </div>`).join('');

  const accents = ['#4B79E4', '#3FA98A', '#E8A33D', '#E2617A', '#9B7BE8', '#DE6B3D', '#4BA3C7', '#7FB53B'];
  const cur = S.settings.accent || '#4B79E4';
  const icon = S.settings.icon || 'glimmer';
  const addrs = sv.addrs || [];

  return `<div class="page">
    <h2>Settings</h2>
    <p class="lede">Speckle never moves or renames anything inside a watched folder. It reads, and writes only to
      its own cache — except when you delete or recompress, which it always asks about first.</p>

    <div class="sec-h"><h3>Watched folders</h3><span class="rule"></span>
      <button class="btn outline" data-act="rescan">${I('refresh', 15)} Rescan</button>
      <button class="btn solid" data-act="add-lib">${I('plus', 15)} Add folder</button></div>
    ${libs || `<div class="card">No folders yet. Everything inside a folder you add — including every sub-folder — becomes part of that one library.</div>`}

    <div class="sec-h"><h3>Photo understanding</h3><span class="rule"></span></div>
    <div class="card">
      <p style="margin:0 0 12px;font-size:12.5px;color:var(--text-2)">
        Runs entirely on this machine. Speckle looks at each photo once and remembers what is in it, so you can
        search for <span class="mono">boats</span> or <span class="mono">a dog on the beach</span> and get
        results — no filenames or tags required.</p>
      ${ml.ready ? `<div class="mlstat">
          <span class="st ok">On</span>
          <span class="mono">${fmtCount(ml.tagged)} of ${fmtCount((sv.counts || {}).total)} photos understood</span>
          ${ml.pending ? `<span class="mono" style="color:var(--warn)">${fmtCount(ml.pending)} to go</span>` : ''}
        </div>
        <div style="display:flex;gap:8px;margin-top:12px;flex-wrap:wrap">
          <button class="btn outline" data-act="reindex-clip">${I('refresh', 15)} Re-analyse everything</button>
          <button class="btn outline danger" data-act="disable-clip">Turn off</button>
        </div>`
        : `<div class="mlstat"><span class="st">Off</span>
            <span class="mono">one-off download, about ${fmtBytes(155000000)}</span></div>
           <button class="btn solid" data-act="enable-clip" style="margin-top:12px">${I('sparkle', 15)} Download and turn on</button>`}
    </div>

    <div class="sec-h"><h3>Faces</h3><span class="rule"></span></div>
    <div class="card">
      <p style="margin:0 0 12px;font-size:12.5px;color:var(--text-2)">
        Finds faces and groups them by person, locally. Names you give are stored only in Speckle's own database.</p>
      ${ml.faces_ready ? `<div class="mlstat">
          <span class="st ok">On</span>
          <span class="mono">${fmtCount(ml.faces)} faces · ${fmtCount(ml.people)} people</span>
        </div>
        <div style="display:flex;gap:8px;margin-top:12px;flex-wrap:wrap">
          <button class="btn outline" data-act="recluster">${I('refresh', 15)} Rebuild groups from scratch</button>
          <button class="btn outline danger" data-act="disable-faces">Turn off</button>
        </div>`
        : `<div class="mlstat"><span class="st">Off</span>
            <span class="mono">one-off download, about ${fmtBytes(288621354)}</span></div>
           <button class="btn solid" data-act="enable-faces" style="margin-top:12px">${I('face', 15)} Download and turn on</button>`}
    </div>

    <div class="sec-h"><h3>Appearance</h3><span class="rule"></span></div>
    <div class="card">
      <div class="field"><label>App mark</label>
        <div class="marks">
          ${Object.entries(MARKS).map(([k, m]) => `<button class="markbtn" data-icon="${k}" aria-pressed="${icon === k}" title="${m.name}">
            <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"
                 stroke-linecap="round" stroke-linejoin="round">${m.svg}</svg>
            <span>${m.name}</span></button>`).join('')}
        </div>
      </div>
      <div class="field"><label>Accent colour</label>
        <div class="swatches">
          ${accents.map(a => `<button class="swatch" data-accent="${a}" style="background:${a}" aria-pressed="${cur.toLowerCase() === a.toLowerCase()}"></button>`).join('')}
          <input type="color" id="accentPick" value="${cur}" title="Pick any colour">
          <span class="mono" style="font-size:11.5px;color:var(--text-3)">${cur}</span>
        </div>
        <div class="hint">The greys take their hue from the accent, so the whole interface re-tunes rather than
          having a colour dropped onto it.</div>
      </div>
      <div class="toggle"><button class="tg" data-setting="theme_light" aria-pressed="${S.settings.theme === 'light'}" role="switch"></button>
        <span class="tx"><b>Light theme</b><span>Photos generally read better against a dark ground</span></span></div>
      <div class="toggle"><button class="tg" data-setting="info" aria-pressed="${!!S.settings.info}" role="switch"></button>
        <span class="tx"><b>Open the info panel by default</b><span>Off by default so the picture gets the whole window</span></span></div>
    </div>

    <div class="sec-h"><h3>Bin</h3><span class="rule"></span></div>
    <div class="card">
      <div class="field"><label for="binDays">Keep binned files for</label>
        <select id="binDays">${[[7, '7 days'], [30, '30 days'], [90, '90 days'], [0, 'Until I empty the bin']]
          .map(([v, l]) => `<option value="${v}" ${(+S.settings.bin_days || 30) === v ? 'selected' : ''}>${l}</option>`).join('')}</select>
        <div class="hint">Binned files live in a hidden <span class="mono">.speckle-bin</span> folder at the root of
          their own library, so moving them there is instant even for a 4 GB video.</div>
      </div>
    </div>

    <div class="sec-h"><h3>Remote access</h3><span class="rule"></span></div>
    <div class="card">
      <p style="margin:0 0 10px;font-size:12.5px;color:var(--text-2)">
        The server is already running on every interface. Open one of these on a phone that is on your tailnet,
        then use Share → Add to Home Screen and it behaves like an app:</p>
      ${addrs.length ? addrs.map(a => `<div class="urlbox">${I('cloud', 15)}<span>http://${a}:${sv.port}</span>
        <button class="btn icon" data-copy="http://${a}:${sv.port}" style="margin-left:auto">${I('copy', 14)}</button></div>`).join('')
        : `<div class="urlbox">${I('cloud', 15)}<span>http://&lt;this machine&gt;:${sv.port}</span></div>`}
      <div class="warnbox" style="margin-top:12px">${I('alert', 16)}<span>
        <b>There is no password.</b> Anything that can reach this machine on your tailnet can view, download and
        delete your photos. That was a deliberate choice for convenience — worth revisiting if you ever share the tailnet.</span></div>
    </div>

    <div class="sec-h"><h3>Cache</h3><span class="rule"></span></div>
    <div class="card">
      <dl class="kv" style="padding-left:0"><dt>Location</dt><dd>${esc(sv.data_dir || '')}</dd></dl>
      <dl class="kv" style="padding-left:0"><dt>Grid tier</dt><dd>320 px square JPEG</dd></dl>
      <dl class="kv" style="padding-left:0"><dt>Preview tier</dt><dd>1600 px, built when first opened</dd></dl>
      <dl class="kv" style="padding-left:0"><dt>ffmpeg</dt><dd>${sv.ffmpeg ? 'found' : 'NOT FOUND — video and HEIC unavailable'}</dd></dl>
      <div style="display:flex;gap:8px;margin-top:12px;flex-wrap:wrap">
        <button class="btn outline" data-act="rebuild">${I('refresh', 15)} Rebuild all thumbnails</button>
        <button class="btn outline" data-act="find-dupes">${I('copies', 15)} Scan for duplicates</button>
        ${isLocal() ? `<button class="btn outline" data-revealpath="${esc(sv.data_dir || '')}">${I('external', 15)} Open cache folder</button>` : ''}
      </div>
    </div>

    <div class="sec-h"><h3>Keyboard</h3><span class="rule"></span></div>
    <div class="card"><div class="kbd-grid">
      ${[['← →', 'Previous / next'], ['Esc', 'Back'], ['F', 'Toggle favourite'],
         ['E', 'Edit'], ['I', 'Info panel'], ['Del', 'Move to bin'],
         ['Space', 'Play / pause video'], ['Ctrl K', 'Search'],
         ['+ −', 'Grid size'], ['Ctrl A', 'Select all']]
        .map(([k, l]) => `<span class="kbd-row"><kbd>${k}</kbd> ${l}</span>`).join('')}
    </div></div>
  </div>`;
}

function wireSettings() {
  $$('[data-accent]').forEach(b => b.addEventListener('click', () => {
    setAccent(b.dataset.accent); saveSetting('accent', b.dataset.accent); applyFavicon(); renderMain(); renderSide();
  }));
  const pick = $('#accentPick');
  if (pick) pick.addEventListener('input', () => { setAccent(pick.value); saveSetting('accent', pick.value); applyFavicon(); });
  $$('[data-icon]').forEach(b => b.addEventListener('click', () => {
    saveSetting('icon', b.dataset.icon); applyFavicon(); renderMain(); renderSide();
  }));
  $$('[data-setting]').forEach(b => b.addEventListener('click', () => {
    const k = b.dataset.setting;
    if (k === 'theme_light') {
      const light = S.settings.theme !== 'light';
      document.documentElement.dataset.theme = light ? 'light' : 'dark';
      saveSetting('theme', light ? 'light' : 'dark');
    } else if (k === 'inbox_organize') {
      S.settings.inbox_organize = S.settings.inbox_organize === false;
      saveSetting('inbox_organize', S.settings.inbox_organize);
    } else {
      S.settings[k] = !S.settings[k];
      if (k === 'info') S.info = S.settings[k];
      saveSetting(k, S.settings[k]);
    }
    renderMain();
  }));
  const bd = $('#binDays');
  if (bd) bd.addEventListener('change', () => saveSetting('bin_days', +bd.value));
  $$('[data-rmlib]').forEach(b => b.addEventListener('click', async () => {
    if (!confirm('Remove this folder from Speckle?\n\nThe photos themselves are left exactly where they are.')) return;
    await del('/api/libraries/' + b.dataset.rmlib);
    await refreshServer(); renderSide(); renderMain(); loadList();
  }));
  $$('[data-copy]').forEach(b => b.addEventListener('click', () => {
    navigator.clipboard.writeText(b.dataset.copy).then(() => toast('Address copied'));
  }));
}

// =========================================================== page glue =====

window.PAGES = {
  storage: pageStorage,
  settings: pageSettings,
  duplicates: pageDuplicates,
  upload: pageUpload,
  people: pagePeople,
  collections: pageCollections,
};

window.wirePage = function () {
  if (S.view === 'storage') wireStorage();
  if (S.view === 'settings') wireSettings();
  if (S.view === 'upload') wireUpload();
  if (S.view === 'people' && !S.people.length) loadPeople();
  $$('[data-person-card]').forEach(b => b.addEventListener('click', e => {
    const id = +b.dataset.personCard;
    if (S.pselMode || e.ctrlKey || e.metaKey) {
      S.psel.has(id) ? S.psel.delete(id) : S.psel.add(id);
      S.pselMode = true;
      renderMain();
    } else {
      S.person = id; S.view = 'person'; go();
    }
  }));
};

window.pageAction = async function (a) {
  switch (a) {
    case 'rebuild':
      if (!confirm('Rebuild every thumbnail?\n\nNothing on disk changes, but this will take a while on a big library.')) return;
      await post('/api/rescan', { rethumb: true });
      toast('Rebuilding thumbnails');
      break;
    case 'enable-clip':
      if (!confirm('Download the vision model?\n\nAbout 155 MB from huggingface.co, once. After that everything runs offline.')) return;
      try { await post('/api/ml/enable', { what: 'clip' }); toast('Downloading — progress is in the sidebar'); }
      catch (e) { toast(e.message, 'err'); }
      break;
    case 'enable-faces':
      if (!confirm('Download the face model?\n\nAbout 275 MB from github.com, once. After that everything runs offline.')) return;
      try { await post('/api/ml/enable', { what: 'faces' }); toast('Downloading — progress is in the sidebar'); }
      catch (e) { toast(e.message, 'err'); }
      break;
    case 'disable-clip': await post('/api/ml/disable', { what: 'clip' }); await refreshServer(); renderMain(); break;
    case 'disable-faces': await post('/api/ml/disable', { what: 'faces' }); await refreshServer(); renderMain(); break;
    case 'reindex-clip': await post('/api/ml/reindex', { what: 'clip' }); toast('Re-analysing'); break;
    case 'recluster':
      if (!confirm('Rebuild every person group from scratch?\n\nNames you have typed and people you have merged by hand will be lost. Normal imports do not need this — new faces join existing people on their own.')) return;
      await post('/api/ml/recluster', {});
      toast('Rebuilding groups');
      await loadPeople();
      break;

    case 'people-select':
      S.pselMode = !S.pselMode;
      if (!S.pselMode) S.psel.clear();
      renderMain();
      break;

    case 'merge-people': {
      const target = mergeTarget();
      if (!target || S.psel.size < 2) return;
      const label = target.name || `the group with ${target.count} photos`;
      if (!confirm(`Merge ${S.psel.size} groups into ${label}?\n\nThis is permanent, but it only combines groups — no photos are changed, and later imports will add to the merged person rather than splitting them again.`)) return;
      try {
        const r = await post('/api/people/merge', { ids: [...S.psel], into: target.id });
        S.psel.clear(); S.pselMode = false;
        await loadPeople(); renderSide(); renderMain();
        toast(`Merged — ${r.moved} faces moved into one person`);
      } catch (e) { toast(e.message, 'err'); }
      break;
    }
  }
};

window.onServerTick = function () {
  if (S.view === 'people') {
    const ml = (S.server && S.server.ml) || {};
    if (ml.faces_ready && !S.people.length) loadPeople();
  }
};

// ------------------------------------------------------------- storage ----

function wireStorage() {
  const q = $('#cq');
  if (q) q.addEventListener('input', () => { S.compress.quality = +q.value; $('#cqv').textContent = q.value; });
  const sc = $('#cscope'); if (sc) sc.addEventListener('change', () => S.compress.scope = sc.value);
  const mx = $('#cmax'); if (mx) mx.addEventListener('change', () => S.compress.max_edge = +mx.value);
  $$('[data-tog]').forEach(b => b.addEventListener('click', () => {
    const k = b.dataset.tog;
    S.compress[k] = !S.compress[k];
    b.setAttribute('aria-pressed', String(S.compress[k]));
  }));
  const s = $('#cSample');
  if (s) s.addEventListener('click', async () => {
    s.disabled = true; $('#cEst').textContent = '…';
    $('#cEstSub').textContent = 'Compressing a sample…';
    try {
      const r = await post('/api/compress/preview', { ...S.compress });
      $('#cEst').textContent = fmtBytes(r.estimated_saving);
      $('#cEstSub').textContent = `${fmtCount(r.count)} files · about ${Math.round((1 - r.ratio) * 100)}% smaller, measured on ${r.sampled}`;
    } catch (e) { $('#cEst').textContent = '—'; $('#cEstSub').textContent = e.message; }
    s.disabled = false;
  });
  const r = $('#cRun');
  if (r) r.addEventListener('click', async () => {
    if (!confirm(`Recompress your ${S.compress.scope === 'video' ? 'videos' : 'oversized photos'} at quality ${S.compress.quality}?\n\n` +
      (S.compress.keep_originals ? 'Originals go to the bin and stay recoverable.' : 'Originals will be DELETED, not binned.'))) return;
    try { await post('/api/compress', S.compress); toast('Recompression started — progress is in the sidebar'); }
    catch (e) { toast(e.message, 'err'); }
  });
  $$('[data-recl]').forEach(b => b.addEventListener('click', () => {
    const id = b.dataset.recl;
    if (id === 'bin') { S.view = 'bin'; go(); }
    else if (id === 'duplicates') action('find-dupes');
    else { S.compress.scope = id === 'video' ? 'video' : 'oversized'; renderMain(); $('#cSample').click(); }
  }));
}

// ============================================================== modals =====

/** Server-side directory browser. Works from the desktop and the phone alike,
 *  which a native file dialog could not. */
async function pickFolder(onPick, title) {
  let cwd = '';
  const scrim = document.createElement('div');
  scrim.className = 'scrim';
  document.body.appendChild(scrim);

  const draw = async () => {
    let d;
    try { d = await api('/api/browse?path=' + encodeURIComponent(cwd)); }
    catch (e) { toast(e.message, 'err'); return; }
    const parts = cwd ? cwd.split('/').filter(Boolean) : [];
    scrim.innerHTML = `<div class="modal">
      <header><h3>${esc(title || 'Add a folder')}</h3>
        <p>Everything inside it, including every sub-folder, becomes one library.</p></header>
      <div class="body">
        <div class="crumbs">
          <button data-go="">Drives</button>
          ${parts.map((p, i) => `<span>/</span><button data-go="${esc(parts.slice(0, i + 1).join('/'))}">${esc(p)}</button>`).join('')}
        </div>
        <div class="dirlist">
          ${d.parent != null && cwd ? `<button class="dirrow" data-go="${esc(d.parent)}">${I('folder', 15)} ..</button>` : ''}
          ${d.entries.map(e => `<button class="dirrow" data-go="${esc(e.path)}">${I('folder', 15)} ${esc(e.name)}</button>`).join('')
            || '<div class="dirrow" style="color:var(--text-3)">No sub-folders here</div>'}
        </div>
      </div>
      <footer>
        <button class="btn outline" data-close>Cancel</button>
        <button class="btn solid" data-add ${cwd ? '' : 'disabled'}>Use ${cwd ? '“' + esc(cwd.split('/').pop() || cwd) + '”' : 'this folder'}</button>
      </footer>
    </div>`;
  };

  scrim.addEventListener('click', async e => {
    if (e.target === scrim || e.target.closest('[data-close]')) { scrim.remove(); return; }
    const g = e.target.closest('[data-go]');
    if (g) { cwd = g.dataset.go; draw(); return; }
    if (e.target.closest('[data-add]')) {
      scrim.remove();
      if (onPick) { onPick(cwd); return; }
      try {
        await post('/api/libraries', { path: cwd });
        toast('Indexing ' + cwd);
        await refreshServer(); renderSide(); loadList();
      } catch (err) { toast(err.message, 'err'); }
    }
  });
  draw();
}
window.pickFolder = pickFolder;

/** Pick (or create) a collection for a set of photos. */
async function addToCollection(ids) {
  if (!ids.length) return;
  await loadColls();
  const scrim = document.createElement('div');
  scrim.className = 'scrim';
  scrim.innerHTML = `<div class="modal">
    <header><h3>Add ${ids.length === 1 ? 'this photo' : ids.length + ' photos'} to…</h3>
      <p>Nothing is copied or moved — a collection is just a list.</p></header>
    <div class="body">
      <div class="dirlist">
        ${(S.colls || []).map(c => `<button class="dirrow" data-pick="${c.id}">
          ${I('stack', 15)} ${esc(c.name)} <span class="count-label" style="margin-left:auto">${fmtCount(c.count)}</span>
        </button>`).join('') || '<div class="dirrow" style="color:var(--text-3)">No collections yet</div>'}
      </div>
      <div class="field" style="margin-top:14px">
        <label for="newColl">Or make a new one</label>
        <div style="display:flex;gap:8px">
          <input type="text" id="newColl" placeholder="Collection name" spellcheck="false">
          <button class="btn solid" data-make style="flex:none">${I('plus', 15)} Create</button>
        </div>
      </div>
    </div>
    <footer><button class="btn outline" data-close>Cancel</button></footer>
  </div>`;
  document.body.appendChild(scrim);

  const add = async cid => {
    await post(`/api/collections/${cid}/items`, { ids });
    scrim.remove();
    await loadColls(); renderSide();
    const c = S.colls.find(x => x.id === +cid);
    toast(`Added to “${c ? c.name : 'collection'}”`);
    S.sel.clear(); S.selMode = false;
    if (isGridView()) renderMain();
  };

  scrim.addEventListener('click', async e => {
    if (e.target === scrim || e.target.closest('[data-close]')) { scrim.remove(); return; }
    const p = e.target.closest('[data-pick]');
    if (p) return add(p.dataset.pick);
    if (e.target.closest('[data-make]')) {
      const name = $('#newColl', scrim).value.trim();
      if (!name) return;
      const c = await post('/api/collections', { name });
      return add(c.id);
    }
  });
  scrim.addEventListener('keydown', e => { if (e.key === 'Enter') $('[data-make]', scrim).click(); });
}
window.addToCollection = addToCollection;

// -------------------------------------------------- delegated handlers ----

document.addEventListener('click', async e => {
  const rp = e.target.closest('[data-revealpath]');
  if (rp) {
    try { await post('/api/reveal', { path: rp.dataset.revealpath }); }
    catch (err) { toast(err.message, 'err'); }
    return;
  }
  const bg = e.target.closest('[data-bin-group]');
  if (bg) {
    const ids = bg.dataset.binGroup.split(',').map(Number).filter(Boolean);
    await post('/api/bin', { ids });
    toast(`${ids.length} moved to the bin`, 'ok', { label: 'Undo', fn: () => post('/api/restore', { ids }).then(loadDupes) });
    loadDupes(); refreshServer().then(renderSide);
    return;
  }
  if (e.target.closest('#binDupes')) {
    const ids = S.dupes.groups.flatMap(g => g.items.filter(i => !i.keep).map(i => i.id));
    if (!confirm(`Move ${ids.length} duplicate files to the bin?\n\nThe largest copy in each group is kept.`)) return;
    await post('/api/bin', { ids });
    toast(`${ids.length} moved to the bin`);
    loadDupes(); refreshServer().then(renderSide);
    return;
  }
  const oi = e.target.closest('[data-open-id]');
  if (oi && S.list) {
    const idx = S.list.ids.indexOf(+oi.dataset.openId);
    if (idx >= 0) openViewer(idx);
  }
});
