'use strict';
/* Add-camera dialog: paste a stream URL · find cameras on the network (ONVIF + open RTSP ports) · demo clips. */

const AC = { tab: 'url', found: null, scanning: false, open: null, streams: {}, busy: {} };
const post = (path, body) => fetch(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }).then(r => r.json());

function openAddDialog(tabName) {
  if (!engineReady) return toast('The engine is still starting');
  AC.tab = tabName || 'url';
  const root = $('#dialog-root');
  root.innerHTML = `<div class="dlg-back"><div class="dlg" role="dialog" aria-label="Add a camera">
    <div class="dlg-head"><h2>Add a camera</h2><button class="iconbtn" data-x aria-label="Close">${I.close}</button></div>
    <div class="dlg-tabs" id="ac-tabs"><button data-t="url">Stream URL</button><button data-t="net">Find on network</button><button data-t="demo">Demo clips</button></div>
    <div class="dlg-body" id="ac-body"></div></div></div>`;
  const close = () => { root.innerHTML = ''; removeEventListener('keydown', onKey, true); };
  const onKey = e => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  addEventListener('keydown', onKey, true);
  root.firstChild.addEventListener('mousedown', e => { if (e.target === root.firstChild) close(); });
  $('[data-x]', root).onclick = close;
  $('#ac-tabs').onclick = e => { const b = e.target.closest('button'); if (b) { AC.tab = b.dataset.t; renderAC(close); if (AC.tab === 'net' && !AC.found && !AC.scanning) scanNetwork(close); } };
  renderAC(close);
  if (AC.tab === 'net' && !AC.found && !AC.scanning) scanNetwork(close);
}

function renderAC(close) {
  $$('#ac-tabs button').forEach(b => b.classList.toggle('on', b.dataset.t === AC.tab));
  const body = $('#ac-body');
  if (AC.tab === 'url') {
    body.innerHTML = `
      <div class="hintline">Paste the address of your camera's stream. RTSP is the usual one: <code>rtsp://user:password@192.168.1.20:554/stream1</code>. RTMP, HTTP (MJPEG/HLS) and SRT work too. The address and password stay on this machine.</div>
      <div><label class="l">Stream address</label><input type="text" id="ac-url" placeholder="rtsp://user:password@192.168.1.20:554/stream1" spellcheck="false" autocomplete="off"></div>
      <div><label class="l">Name <span style="text-transform:none;letter-spacing:0">(optional)</span></label><input type="text" id="ac-name" placeholder="Garage" maxlength="32"></div>
      <div class="dlg-actions"><span class="res" id="ac-res"></span><button class="btn ghost" id="ac-test">Test connection</button><button class="btn" id="ac-add">Add camera</button></div>`;
    const url = $('#ac-url'); url.value = AC.prefill || ''; AC.prefill = ''; url.focus();
    $('#ac-test').onclick = async () => {
      const res = $('#ac-res'); res.className = 'res'; res.innerHTML = '<span class="spin"></span>Connecting…';
      const r = await post('/api/check', { url: url.value }); res.className = 'res ' + (r.ok ? 'ok' : 'bad'); res.textContent = r.ok ? `Connected · ${r.detail}` : r.error;
    };
    const add = async () => { if (await postCamera({ url: url.value, name: $('#ac-name').value })) close(); };
    $('#ac-add').onclick = add; url.onkeydown = e => { if (e.key === 'Enter') add(); };
  } else if (AC.tab === 'net') {
    renderNet(close);
  } else {
    body_demo(body, close);
  }
}

function body_demo(body, close) {
  body.innerHTML = `<div class="hintline">A highway clip streamed in real time, exactly like a camera: frames arrive at the source rate and are never sped up.</div>
    <button class="preset" data-setup="1"><b>Highway · North</b><span>One counting line, approach zone</span></button>
    <button class="preset" data-setup="2"><b>Highway · South</b><span>Two counting lines, merge zone</span></button>
    ${DEV ? `<div class="dlg-actions"><span class="res">Stream rate (developer)</span><div class="segbtn" id="rate"><button data-r="20">20</button><button data-r="30" class="on">30</button><button data-r="60">60</button></div></div>` : ''}`;
  $$('.preset', body).forEach(b => b.onclick = async () => { if (await postCamera({ setup: +b.dataset.setup, fps: srcRate })) close(); });
  const rate = $('#rate'); if (rate) rate.onclick = e => { const b = e.target.closest('button'); if (!b) return; srcRate = +b.dataset.r; $$('#rate button').forEach(x => x.classList.toggle('on', x === b)); };
}

async function scanNetwork(close) {
  AC.scanning = true; AC.found = null; renderNet(close);
  try { AC.found = (await post('/api/discover', {})).found; } catch { AC.found = []; }
  AC.scanning = false; if ($('#ac-body') && AC.tab === 'net') renderNet(close);
}

function renderNet(close) {
  const body = $('#ac-body'); if (!body) return;
  if (AC.scanning) { body.innerHTML = '<div class="hintline"><span class="spin"></span>Looking for cameras on your network (ONVIF and open RTSP ports)…</div>'; return; }
  if (!AC.found) { body.innerHTML = '<div class="dlg-actions"><span class="res">Scans your local network for cameras.</span><button class="btn" id="ac-scan">Scan network</button></div>'; $('#ac-scan').onclick = () => scanNetwork(close); return; }
  const rows = AC.found.map((f, i) => {
    const open = AC.open === f.ip, title = f.name || f.hardware || 'Camera or video device', st = AC.streams[f.ip];
    return `<div class="drow${open ? ' open' : ''}" data-ip="${esc(f.ip)}">
      <div><div class="t">${esc(title)} <span style="color:var(--muted);font-weight:400">${esc(f.ip)}</span></div><div class="chips2">${f.onvif ? '<span class="mini acc">ONVIF</span>' : ''}${f.rtsp ? '<span class="mini">RTSP open</span>' : ''}</div></div>
      ${open ? '' : `<button class="btn ghost sm" data-open="${esc(f.ip)}">Connect</button>`}
      ${open ? `<div class="two2"><div><label class="l">Username</label><input type="text" data-f="user" autocomplete="off" spellcheck="false"></div><div><label class="l">Password</label><input type="password" data-f="pass" autocomplete="off"></div></div>
        <div class="dlg-actions"><span class="res" id="res-${i}">${st === 'busy' ? '<span class="spin"></span>Asking the camera…' : st && st.length === 0 ? 'No stream found. Check the credentials, or paste the address yourself.' : ''}</span><button class="btn ghost sm" data-manual="${esc(f.ip)}">Enter address</button><button class="btn sm" data-find="${esc(f.ip)}">Find streams</button></div>
        ${Array.isArray(st) ? st.map(s => `<div class="drow" style="background:var(--surface)"><div><div class="t">${esc(s.label)} <span style="color:var(--muted);font-weight:400">${esc(s.detail)}</span></div><div class="s">${esc(s.url)}</div></div><button class="btn sm" data-add="${s.token}" data-name="${esc(title === 'Camera or video device' ? 'Camera ' + f.ip : title)}">Add</button></div>`).join('') : ''}` : ''}
    </div>`;
  }).join('');
  body.innerHTML = `<div class="hintline">${AC.found.length ? 'Found on your network. Pick one and sign in to see its streams.' : 'Nothing answered. Cameras on another subnet or with discovery turned off will not show up here: paste the stream address instead.'}</div>${rows}
    <div class="dlg-actions"><span class="res"></span><button class="btn ghost" id="ac-rescan">Scan again</button></div>`;
  $('#ac-rescan').onclick = () => { AC.streams = {}; AC.open = null; scanNetwork(close); };
  body.onclick = async e => {
    const q = a => e.target.closest(`[${a}]`);
    let b;
    if ((b = q('data-open'))) { AC.open = b.dataset.open; renderNet(close); }
    else if ((b = q('data-manual'))) { AC.prefill = `rtsp://${b.dataset.manual}:554/`; AC.tab = 'url'; renderAC(close); }
    else if ((b = q('data-find'))) {
      const ip = b.dataset.find, row = b.closest('.drow'), f = AC.found.find(x => x.ip === ip);
      const user = $('[data-f=user]', row).value, pass = $('[data-f=pass]', row).value;
      AC.streams[ip] = 'busy'; renderNet(close);
      const r = await post('/api/streams', { ip, onvif: f.onvif, user, pass });
      AC.streams[ip] = r.streams; renderNet(close);
      const nr = $(`.drow[data-ip="${CSS.escape(ip)}"]`); if (nr) { $('[data-f=user]', nr).value = user; $('[data-f=pass]', nr).value = pass; }
    } else if ((b = q('data-add'))) { if (await postCamera({ token: b.dataset.add, name: b.dataset.name })) close(); }
  };
}

{ const h = location.hash.slice(1); if (h === 'analytics' || h === 'alarms') setTab(h); }   // last script: everything is defined now
