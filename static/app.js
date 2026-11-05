'use strict';
/* ───────── shared state & helpers (also used by editor.js / analytics.js) ───────── */
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const cams = new Map();                       // id -> card state
// object classes: person plus the vehicle classes use validated categorical slots; same colour everywhere (boxes, chips, charts)
const COLORS = {
  person: '#2cc4d8', car: '#3987e5', truck: '#d95926', bus: '#199e70', motorcycle: '#c98500', bicycle: '#d55181', train: '#8a6fe0', boat: '#5aa5c9',
  dog: '#c99a5b', cat: '#e59ab8', bird: '#9ccc4a', horse: '#a8774c', backpack: '#8d9ab5', handbag: '#a58fb8', suitcase: '#7f8aa0', other: '#cfcfd4' };
const CLASS_LIST = ['person', 'car', 'truck', 'bus', 'motorcycle', 'bicycle', 'train', 'boat', 'dog', 'cat', 'bird', 'horse', 'backpack', 'handbag', 'suitcase'];   // same order as src/config.rs
const ACCENT = '#bda7ff', ACCENT_HI = '#efe9ff';
const STATUS = { ok: '#d9d5e8', warn: '#fab219', crit: '#d03b3b' };
const DEFAULTS = { boxes: true, labels: false, trails: true, lines: true, zones: true, hud: true };
let engineReady = false, DEV = false, srcRate = 30, hovered = null, focusedId = null, tab = 'live';

const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmt = (n, d = 0) => Number.isFinite(n) ? n.toFixed(d) : '–';
const pad = n => String(n).padStart(2, '0');
const hhmmss = t => { const d = new Date(t); return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`; };
const cap = s => s ? s[0].toUpperCase() + s.slice(1) : s;
function toast(msg) { const t = $('#toast'); t.textContent = msg; t.classList.add('show'); clearTimeout(toast.t); toast.t = setTimeout(() => t.classList.remove('show'), 2600); }
const load = k => { try { return { ...DEFAULTS, ...JSON.parse(localStorage.getItem('chira.' + k) || '{}') }; } catch { return { ...DEFAULTS }; } };
const save = (k, v) => { try { localStorage.setItem('chira.' + k, JSON.stringify(v)); } catch {} };
const pctColor = p => p >= .85 ? STATUS.crit : p >= .6 ? STATUS.warn : STATUS.ok;

const I = {
  boxes:  '<svg viewBox="0 0 24 24"><path d="M4 9V5h4M20 9V5h-4M4 15v4h4M20 15v4h-4"/></svg>',
  labels: '<svg viewBox="0 0 24 24"><path d="M3.5 12.5V5.5h7l9 9-7 7-9-9z"/><circle cx="7.8" cy="9.8" r="1"/></svg>',
  trails: '<svg viewBox="0 0 24 24"><path d="M3 19c4 0 4-7 8-7s3-7 10-7"/><circle cx="21" cy="5" r="1.4"/></svg>',
  lines:  '<svg viewBox="0 0 24 24"><path d="M3 12h18"/><path d="M8 7l-3 5 3 5M16 7l3 5-3 5" opacity=".5"/></svg>',
  zones:  '<svg viewBox="0 0 24 24"><path d="M5 7l9-3 5 6-3 9-10-2z"/></svg>',
  hud:    '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M3 15h18"/></svg>',
  select: '<svg viewBox="0 0 24 24"><path d="M5 4l14 7-6 2-2 6z"/></svg>',
  cfg:    '<svg viewBox="0 0 24 24"><path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/></svg>',
  focus:  '<svg viewBox="0 0 24 24"><path d="M9 4H4v5M15 4h5v5M9 20H4v-5M15 20h5v-5"/></svg>',
  close:  '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  bell:   '<svg viewBox="0 0 24 24"><path d="M6 16V11a6 6 0 0112 0v5l1.5 2h-15zM10 20a2 2 0 004 0"/></svg>',
};
const TOOLS = [['boxes', 'Boxes', 'B'], ['labels', 'Labels', 'L'], ['trails', 'Trails', 'T'], ['lines', 'Counting lines', 'C'], ['zones', 'Zones', 'Z'], ['hud', 'Stats', 'H']];

/* ───────── cameras ───────── */
async function postCamera(body) {
  if (!engineReady) { toast('The engine is still starting'); return false; }
  const r = await fetch('/api/cameras', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  if (!r.ok) { toast((await r.json().catch(() => ({}))).error || 'Could not add the camera'); return false; }
  poll(); return true;
}
async function removeCam(id) { await fetch('/api/cameras/' + id, { method: 'DELETE' }); if (focusedId === id) setFocus(null); poll(); }

function buildCard(c) {
  const el = document.createElement('article');
  el.className = 'card';
  const key = c.key;
  const o = { id: c.id, name: c.name, setup: c.setup, key, el, tg: load(key), latest: null, busy: false, trails: new Map(), counts: {}, bump: {}, flash: {}, zm: new Map(), src: c.src_fps, meta: null, frame: 0 };
  el.innerHTML = `
    <div class="stage">
      <canvas class="base" width="960" height="540"></canvas>
      <canvas class="over"></canvas>
      <div class="scrim-t"></div><div class="scrim-b"></div>
      <div class="top"><span class="name">${esc(c.name)}</span><span class="live"><i></i><span class="lv">Live</span></span><button class="pip" data-a="pip" data-tip="Open alarms">${I.bell}<span></span></button>
        <div class="tools">${TOOLS.map(([k, n, h]) => `<button data-k="${k}" data-tip="${n} · ${h}">${I[k]}</button>`).join('')}<span class="sep"></span>
          <button data-a="cfg" data-tip="Configure · E">${I.cfg}</button><button data-a="focus" data-tip="Focus · F">${I.focus}</button><button data-a="close" data-tip="Remove camera">${I.close}</button></div></div>
      <div class="lost"><div><span class="lost-t">Reconnecting to the camera…</span><span class="lost-s">It keeps trying on its own.</span><button class="btn ghost" data-a="remove">Remove camera</button></div></div>
      <div class="hud"><div class="counters"><div class="cl" style="display:flex;gap:34px;flex-wrap:wrap"></div><div class="cz" style="display:flex;gap:0;flex-wrap:wrap"></div></div>
        <div class="rate dev-only"><div><b data-k="fps">–</b><span class="micro">fps</span></div><div><b data-k="lat">–</b><span class="micro">latency</span></div><div><b data-k="drop">–</b><span class="micro">dropped</span></div></div></div>
    </div>`;
  o.base = $('canvas.base', el); o.bctx = o.base.getContext('2d', { alpha: false, desynchronized: true });
  o.over = $('canvas.over', el); o.octx = o.over.getContext('2d', { desynchronized: true });
  o.stage = $('.stage', el);
  applyToggles(o);
  el.addEventListener('mouseenter', () => hovered = o.id);
  el.addEventListener('mouseleave', () => { if (hovered === o.id) hovered = null; });
  el.addEventListener('mousemove', () => { el.classList.remove('idle'); clearTimeout(o.idleT); o.idleT = setTimeout(() => el.classList.add('idle'), 2600); });
  el.addEventListener('dblclick', e => { if (!e.target.closest('.tools')) setFocus(focusedId === o.id ? null : o.id); });
  $('.tools', el).addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.dataset.k) toggle(o, b.dataset.k);
    else if (b.dataset.a === 'cfg') openEditor(o);
    else if (b.dataset.a === 'focus') setFocus(focusedId === o.id ? null : o.id);
    else if (b.dataset.a === 'close') removeCam(o.id);
  });
  $('.pip', el).onclick = () => openAlarms(o.name);
  $('[data-a=remove]', el).onclick = () => removeCam(o.id);
  new ResizeObserver(() => sizeOverlay(o)).observe(o.stage);
  $('#grid').appendChild(el);

  const ws = new WebSocket(`ws://${location.host}/ws/${c.id}`);
  ws.binaryType = 'arraybuffer';
  ws.onmessage = ev => {
    if (document.hidden || tab !== 'live') return;
    const buf = ev.data, n = new DataView(buf).getUint32(0);
    o.latest = { meta: JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, n))), jpg: new Blob([new Uint8Array(buf, 4 + n)], { type: 'image/jpeg' }) };
    if (!o.busy) pump(o);
  };
  o.ws = ws;
  return o;
}

function applyToggles(o) {
  for (const [k] of TOOLS) $(`.tools [data-k="${k}"]`, o.el).classList.toggle('on', !!o.tg[k]);
  o.el.classList.toggle('nohud', !o.tg.hud);
}
function toggle(o, k) { o.tg[k] = !o.tg[k]; save(o.key, o.tg); applyToggles(o); if (!o.tg.trails) o.trails.clear(); if (o.meta) drawOverlay(o); }

function sizeOverlay(o) {
  const r = o.stage.getBoundingClientRect(), d = Math.min(window.devicePixelRatio || 1, 2);
  o.over.width = Math.round(r.width * d); o.over.height = Math.round(r.height * d); o.dpr = d;
  if (o.meta) drawOverlay(o);
}

async function pump(o) {                      // always render the newest frame; never build a backlog
  o.busy = true;
  while (o.latest) {
    const m = o.latest; o.latest = null;
    let bmp; try { bmp = await createImageBitmap(m.jpg); } catch { continue; }
    o.bctx.drawImage(bmp, 0, 0, 960, 540); bmp.close();
    o.meta = m.meta; o.frame++;
    ingest(o, m.meta);
    drawOverlay(o);
  }
  o.busy = false;
}

/* counters, zone meters, trails: DOM is only touched when a value changes */
function ingest(o, meta) {
  const now = performance.now();
  let html = '';
  for (const c of meta.counts) {
    const tot = c.fwd + c.rev, prev = o.counts[c.name] ?? tot;
    if (tot > prev) o.bump[c.name] = now;
    o.counts[c.name] = tot;
    const cls = Object.entries(c.by_class).sort((a, b) => b[1] - a[1]).map(([k, v]) => `<u style="--c:${COLORS[k] || COLORS.other}"><em>${v}</em> ${k}</u>`).join('');
    html += `<div class="counter"><div class="lbl"><i${now - (o.bump[c.name] || -1e9) < 450 ? ' class="hit"' : ''}></i><span class="micro">${esc(c.name)}</span></div><div class="num">${tot}</div><div class="sub">${cls || '<span>waiting for traffic</span>'}</div></div>`;
  }
  if (o._html !== html) { $('.cl', o.el).innerHTML = html; o._html = html; }
  updateZoneMeters(o, meta.zones || []);
  if (o.tg.trails) {
    const seen = new Set();
    for (const b of meta.boxes) {
      seen.add(b.id);
      let t = o.trails.get(b.id); if (!t) o.trails.set(b.id, t = { p: [], c: b.c, miss: 0 });
      t.p.push((b.b[0] + b.b[2]) / 2, b.b[3]); t.c = b.c; t.miss = 0;
      if (t.p.length > 64) t.p.splice(0, 2);
    }
    for (const [id, t] of o.trails) if (!seen.has(id) && ++t.miss > 8) o.trails.delete(id);
  }
}

function updateZoneMeters(o, zones) {
  const box = $('.cz', o.el), keep = new Set();
  for (const z of zones) {
    keep.add(z.id);
    let m = o.zm.get(z.id);
    if (!m) {
      const el = document.createElement('div'); el.className = 'zmeter';
      el.innerHTML = '<div class="lbl"><span class="micro"></span></div><div class="num"><span class="v">0</span><small>%</small></div><div class="zbar"><div></div></div><div class="sub"></div>';
      box.appendChild(el); o.zm.set(z.id, m = { el, v: $('.v', el), bar: $('.zbar div', el), sub: $('.sub', el), name: $('.micro', el), last: '' });
    }
    const p = z.cap ? z.n / z.cap : 0, sig = `${z.name}|${z.n}|${z.cap}`;
    if (sig !== m.last) {
      m.last = sig; m.name.textContent = z.name; m.v.textContent = Math.round(p * 100);
      m.bar.style.width = Math.min(100, p * 100) + '%'; m.sub.textContent = `${z.n} of ${z.cap} objects`;
      m.el.className = 'zmeter' + (p >= .85 ? ' crit' : p >= .6 ? ' warn' : '');
    }
  }
  for (const [id, m] of o.zm) if (!keep.has(id)) { m.el.remove(); o.zm.delete(id); }
  box.style.display = o.tg.zones ? 'flex' : 'none';
}

function polyPath(pts, W, H) { const p = new Path2D(); pts.forEach((q, i) => i ? p.lineTo(q[0] * W, q[1] * H) : p.moveTo(q[0] * W, q[1] * H)); p.closePath(); return p; }

function drawOverlay(o) {
  const ctx = o.octx, meta = o.meta, W = o.over.width, H = o.over.height;
  ctx.clearRect(0, 0, W, H);
  if (!meta || !W) return;
  const sx = W / meta.w, sy = H / meta.h, d = o.dpr || 1, now = performance.now();
  ctx.font = `500 ${11 * d}px Inter, system-ui, sans-serif`; ctx.textBaseline = 'middle';

  // zones: tinted by how full they are, flashing on alarms
  if (o.tg.zones) {
    for (const z of meta.zones || []) {
      const p = z.cap ? z.n / z.cap : 0, col = pctColor(p), path = polyPath(z.points, W, H), fk = Math.max(0, 1 - (now - (o.flash[z.name] || -1e9)) / 1400);
      ctx.globalAlpha = .13; ctx.fillStyle = col; ctx.fill(path);
      if (fk > 0) { ctx.globalAlpha = .28 * fk; ctx.fillStyle = STATUS.crit; ctx.fill(path); }
      ctx.globalAlpha = .85; ctx.lineWidth = 1.5 * d; ctx.lineJoin = 'round'; ctx.strokeStyle = col; ctx.stroke(path);
      if (fk > 0) { ctx.globalAlpha = fk; ctx.strokeStyle = STATUS.crit; ctx.stroke(path); }
      ctx.globalAlpha = 1;
      const top = z.points.reduce((a, q) => q[1] < a[1] ? q : a, z.points[0]), cx = z.points.reduce((s, q) => s + q[0], 0) / z.points.length;
      const label = `${z.name}  ${z.n}/${z.cap}`, tw = ctx.measureText(label).width + 18 * d, mx = cx * W, my = top[1] * H + 16 * d;
      ctx.fillStyle = 'rgba(10,10,12,.72)'; rr(ctx, mx - tw / 2, my - 10 * d, tw, 20 * d, 10 * d); ctx.fill();
      ctx.fillStyle = col; ctx.textAlign = 'center'; ctx.fillText(label, mx, my + .5 * d);
    }
  }

  // counting lines: constant size; a crossing only brightens the line
  if (o.tg.lines) {
    ctx.lineCap = 'round';
    for (const l of meta.lines) {
      const x1 = l.x1 * W, y1 = l.y1 * H, x2 = l.x2 * W, y2 = l.y2 * H, k = Math.max(0, 1 - (now - (o.bump[l.name] || -1e9)) / 700);
      ctx.strokeStyle = ACCENT; ctx.lineWidth = 1.6 * d; ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke();
      if (k > 0) { ctx.globalAlpha = k; ctx.strokeStyle = ACCENT_HI; ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke(); ctx.globalAlpha = 1; }
      ctx.fillStyle = k > 0 ? ACCENT_HI : ACCENT; for (const [x, y] of [[x1, y1], [x2, y2]]) { ctx.beginPath(); ctx.arc(x, y, 3.2 * d, 0, 6.3); ctx.fill(); }
      const label = `${l.name}  ${o.counts[l.name] ?? 0}`, tw = ctx.measureText(label).width + 18 * d, mx = (x1 + x2) / 2, my = (y1 + y2) / 2 - 16 * d;
      ctx.fillStyle = 'rgba(10,10,12,.72)'; rr(ctx, mx - tw / 2, my - 10 * d, tw, 20 * d, 10 * d); ctx.fill();
      ctx.fillStyle = k > 0 ? ACCENT_HI : ACCENT; ctx.textAlign = 'center'; ctx.fillText(label, mx, my + .5 * d);
    }
  }

  // trails (two paths per class: faint tail, brighter head)
  if (o.tg.trails && o.trails.size) {
    ctx.lineWidth = 1.5 * d; ctx.lineJoin = 'round'; ctx.lineCap = 'round';
    const tails = {}, heads = {};
    for (const t of o.trails.values()) {
      const n = t.p.length / 2; if (n < 3) continue;
      const mid = Math.max(0, n - 18);
      const T = tails[t.c] ||= new Path2D(), Hd = heads[t.c] ||= new Path2D();
      T.moveTo(t.p[0] * sx, t.p[1] * sy); for (let i = 1; i <= mid; i++) T.lineTo(t.p[i * 2] * sx, t.p[i * 2 + 1] * sy);
      Hd.moveTo(t.p[mid * 2] * sx, t.p[mid * 2 + 1] * sy); for (let i = mid + 1; i < n; i++) Hd.lineTo(t.p[i * 2] * sx, t.p[i * 2 + 1] * sy);
    }
    for (const c in tails) { ctx.strokeStyle = COLORS[c] || COLORS.other; ctx.globalAlpha = .25; ctx.stroke(tails[c]); ctx.globalAlpha = .8; ctx.stroke(heads[c]); }
    ctx.globalAlpha = 1;
  }

  // boxes: corner brackets + whisper-thin fill, batched per class
  if (o.tg.boxes && meta.boxes.length) {
    const br = {}, fl = {};
    for (const b of meta.boxes) {
      const x1 = b.b[0] * sx, y1 = b.b[1] * sy, x2 = b.b[2] * sx, y2 = b.b[3] * sy, w = x2 - x1, h = y2 - y1;
      const l = Math.min(Math.max(5 * d, Math.min(w, h) * .24), 18 * d);
      const P = br[b.c] ||= new Path2D(), F = fl[b.c] ||= new Path2D();
      P.moveTo(x1, y1 + l); P.lineTo(x1, y1); P.lineTo(x1 + l, y1); P.moveTo(x2 - l, y1); P.lineTo(x2, y1); P.lineTo(x2, y1 + l);
      P.moveTo(x1, y2 - l); P.lineTo(x1, y2); P.lineTo(x1 + l, y2); P.moveTo(x2 - l, y2); P.lineTo(x2, y2); P.lineTo(x2, y2 - l);
      F.rect(x1, y1, w, h);
    }
    ctx.lineWidth = 1.5 * d; ctx.lineJoin = 'miter'; ctx.lineCap = 'butt';
    for (const c in br) { const col = COLORS[c] || COLORS.other; ctx.globalAlpha = .08; ctx.fillStyle = col; ctx.fill(fl[c]); ctx.globalAlpha = .95; ctx.strokeStyle = col; ctx.stroke(br[c]); }
    ctx.globalAlpha = 1;
  }

  // labels
  if (o.tg.labels && meta.boxes.length) {
    ctx.font = `500 ${10.5 * d}px Inter, system-ui, sans-serif`; ctx.textAlign = 'left';
    for (const b of meta.boxes) {
      const t = `${b.c}  ${b.id}`, w = ctx.measureText(t).width + 12 * d, x = b.b[0] * sx, y = b.b[1] * sy - 19 * d;
      ctx.fillStyle = 'rgba(8,8,10,.7)'; rr(ctx, x, y, w, 17 * d, 8.5 * d); ctx.fill();
      ctx.fillStyle = '#ebe8f4'; ctx.fillText(t, x + 6 * d, y + 9 * d);
    }
  }
}
function rr(ctx, x, y, w, h, r) { ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(x, y, w, h, r); else ctx.rect(x, y, w, h); }

/* ───────── alarms: incidents, not notifications ─────────
   A rule firing repeatedly is ONE incident (server-side grouping). Only the first firing of a new incident can
   interrupt you, and only if its severity says so:  info -> log only · warning -> bell + camera marker · critical -> toast.
   Everything else waits quietly in the bell and the Alarms page until someone acknowledges it. */
const INC = { list: [], snoozes: [], groupMs: 120000, loaded: false };
const SEV = { info: 0, warning: 1, critical: 2 };
const SEV_LABEL = { info: 'Info', warning: 'Warning', critical: 'Critical' };
const SEV_ICON = { info: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="M12 11v5M12 8v.01"/></svg>', warning: '<svg viewBox="0 0 24 24"><path d="M12 4l9 16H3z"/><path d="M12 10v4.5M12 17.5v.01"/></svg>', critical: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v5.5M12 16.5v.01"/></svg>' };
let soundOn = (() => { try { return localStorage.getItem('chira.sound') !== '0'; } catch { return true; } })();
let notifyMode = (() => { try { return localStorage.getItem('chira.notify') || 'quiet'; } catch { return 'quiet'; } })();   // quiet | all | off
const setNotify = m => { notifyMode = m; try { localStorage.setItem('chira.notify', m); } catch {} };
const setSound = on => { soundOn = on; try { localStorage.setItem('chira.sound', on ? '1' : '0'); } catch {} };
const incState = i => i.acked_t ? 'acked' : (Date.now() - i.last_t < INC.groupMs ? 'active' : 'cleared');
const needsAttention = i => !i.acked_t && SEV[i.severity] >= 1;
const ago = t => { const s = Math.max(0, (Date.now() - t) / 1000); return s < 45 ? 'just now' : s < 3600 ? Math.round(s / 60) + ' min ago' : s < 86400 ? Math.round(s / 3600) + ' h ago' : new Date(t).toLocaleDateString(); };

async function loadIncidents() {
  try { const d = await (await fetch('/api/incidents')).json(); INC.list = d.incidents; INC.snoozes = d.snoozes; INC.groupMs = d.group_ms; INC.loaded = true; refreshAlarmUI(); } catch {}
}
function connectAlerts() {
  const ws = new WebSocket(`ws://${location.host}/ws/alerts`);
  ws.onmessage = e => { try { onAlertMsg(JSON.parse(e.data)); } catch {} };
  ws.onclose = () => setTimeout(connectAlerts, 2000);
}
function onAlertMsg(m) {
  if (m.type === 'incident') {
    const inc = m.incident, i = INC.list.findIndex(x => x.id === inc.id);
    if (i >= 0) INC.list[i] = inc; else INC.list.unshift(inc);
    if (inc.severity !== 'info') for (const o of cams.values()) if (o.name === inc.cam) o.flash[inc.target] = performance.now();
    if (m.new) notifyNew(inc);
  } else if (m.type === 'ack') {
    for (const i of INC.list) if (m.ids.includes(i.id)) i.acked_t = m.t;
  } else if (m.type === 'snooze') { loadIncidents(); return; }
  refreshAlarmUI();
}
function notifyNew(inc) {
  const sev = SEV[inc.severity];
  if (sev >= 1) { const b = $('#bell'); b.classList.remove('ring'); void b.offsetWidth; b.classList.add('ring'); }
  const loud = notifyMode === 'all' ? sev >= 1 : notifyMode === 'quiet' ? sev >= 2 : false;
  if (!loud) return;
  toastIncident(inc);
  if (soundOn && performance.now() - (ping.last || -1e9) > 1500) { ping.last = performance.now(); ping(); }
}
function toastIncident(a) {
  const el = document.createElement('div'); el.className = 'alert';
  el.innerHTML = `<div class="ic">${I.bell}</div><b></b><span class="t"></span><div class="m"></div><div class="act"><button class="btn sm" data-a="ack">Acknowledge</button><button class="btn ghost sm" data-a="view">View</button></div>`;
  $('b', el).textContent = a.rule; $('.t', el).textContent = hhmmss(a.last_t); $('.m', el).textContent = `${a.msg} · ${a.cam}`;
  const close = () => { el.classList.add('out'); setTimeout(() => el.remove(), 320); };
  el.onclick = e => { const b = e.target.closest('button'); if (!b) return; if (b.dataset.a === 'ack') ackIncident(a.id); else openAlarms(a.cam); close(); };
  const box = $('#toasts'); box.prepend(el); setTimeout(close, 9000);
  while (box.children.length > 3) box.lastChild.remove();
}
async function ackIncident(id) { await fetch('/api/incidents/ack', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id }) }); }
async function ackAll(cam) { await fetch('/api/incidents/ack', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(cam ? { cam } : {}) }); }
async function snoozeRule(cam, rule_id, minutes) { await fetch('/api/snooze', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ cam, rule_id, minutes }) }); loadIncidents(); }
function openAlarms(cam) { if (typeof AL !== 'undefined') { AL.cam = cam || 'all'; AL.view = 'attention'; } setTab('alarms'); }

/* bell badge, camera markers, inbox: all derived from INC.list */
function refreshAlarmUI() {
  const pending = INC.list.filter(needsAttention), crit = pending.some(i => i.severity === 'critical');
  const bb = $('#alarmbadge'); bb.textContent = pending.length > 99 ? '99+' : pending.length; bb.classList.toggle('show', pending.length > 0); bb.classList.toggle('warn', !crit);
  for (const o of cams.values()) {
    const mine = pending.filter(i => i.cam === o.name), pip = $('.pip', o.el);
    if (!pip) continue;
    const top = mine.reduce((m, i) => SEV[i.severity] > SEV[m] ? i.severity : m, 'info');
    pip.className = 'pip' + (mine.length ? ' show ' + top : '');
    $('span', pip).textContent = mine.length ? `${mine.length} alarm${mine.length > 1 ? 's' : ''}` : '';
  }
  if ($('#bellmenu').classList.contains('open')) renderInbox();
  if (tab === 'alarms' && typeof renderAlarms === 'function') renderAlarms();
}
function renderInbox() {
  const pending = INC.list.filter(needsAttention).sort((a, b) => SEV[b.severity] - SEV[a.severity] || b.last_t - a.last_t).slice(0, 8), box = $('#inbox');
  box.innerHTML = `<div class="ib-head"><b>Needs attention</b><span class="micro">${pending.length ? INC.list.filter(needsAttention).length + ' open' : ''}</span></div>` +
    (pending.length ? `<div class="ib-list">${pending.map(i => `<div class="ib-row" data-id="${i.id}"><span class="sevdot ${i.severity}"></span><span class="t1">${esc(i.rule)}${i.count > 1 ? ` <span class="cnt">×${i.count}</span>` : ''}</span><button class="btn ghost sm ack" data-ack="${i.id}">Acknowledge</button><span class="t2">${esc(i.cam)} · ${ago(i.last_t)}</span></div>`).join('')}</div>
      <div class="ib-foot"><button class="btn ghost sm" id="ib-all">Acknowledge all</button><button class="btn ghost sm" id="ib-open">Open alarms</button></div>`
      : `<div class="ib-empty"><div>All clear</div><div class="micro" style="margin-top:6px">nothing needs your attention</div></div><div class="ib-foot"><span></span><button class="btn ghost sm" id="ib-open">Open alarms</button></div>`);
  box.onclick = e => {
    e.stopPropagation();
    const a = e.target.closest('[data-ack]'); if (a) return ackIncident(+a.dataset.ack);
    if (e.target.closest('#ib-all')) return ackAll();
    if (e.target.closest('#ib-open')) { $('#bellmenu').classList.remove('open'); return openAlarms(); }
    const r = e.target.closest('.ib-row'); if (r) { const i = INC.list.find(x => x.id === +r.dataset.id); $('#bellmenu').classList.remove('open'); openAlarms(i?.cam); }
  };
}
$('#bell').onclick = e => { e.stopPropagation(); const m = $('#bellmenu'); m.classList.toggle('open'); if (m.classList.contains('open')) renderInbox(); };
setInterval(() => { if (INC.loaded) refreshAlarmUI(); }, 15000);   // "x min ago", active -> cleared
let actx;
function ping() {
  try {
    actx ||= new (window.AudioContext || window.webkitAudioContext)();
    const t = actx.currentTime, o = actx.createOscillator(), g = actx.createGain();
    o.type = 'sine'; o.frequency.setValueAtTime(880, t); o.frequency.exponentialRampToValueAtTime(1320, t + .09);
    g.gain.setValueAtTime(.0001, t); g.gain.exponentialRampToValueAtTime(.08, t + .02); g.gain.exponentialRampToValueAtTime(.0001, t + .45);
    o.connect(g).connect(actx.destination); o.start(t); o.stop(t + .5);
  } catch {}
}

/* ───────── tabs, focus, keys ───────── */
function setTab(t) {
  tab = t;
  $('#view-live').classList.toggle('hide', t !== 'live'); $('#view-analytics').classList.toggle('hide', t !== 'analytics'); $('#view-alarms').classList.toggle('hide', t !== 'alarms');
  $$('#tabs button').forEach(b => b.classList.toggle('on', b.dataset.tab === t));
  $('#hint').classList.toggle('hide', t !== 'live');
  history.replaceState(null, '', t === 'live' ? location.pathname : '#' + t);
  if (t === 'analytics') renderAnalytics(true);
  if (t === 'alarms') renderAlarms(true);
}
$('#tabs').onclick = e => { const b = e.target.closest('button'); if (b) setTab(b.dataset.tab); };

function setFocus(id) {
  focusedId = id; $('#grid').classList.toggle('focus', id != null);
  let others = 0; for (const o of cams.values()) if (o.id !== id) others++;
  for (const o of cams.values()) { const f = o.id === id; o.el.classList.toggle('focused', f); o.el.style.gridRow = f ? `1 / span ${Math.max(1, others)}` : ''; }
}
addEventListener('keydown', e => {
  if (e.metaKey || e.ctrlKey || e.altKey || /^(INPUT|SELECT|TEXTAREA)$/.test(e.target.tagName)) return;
  if ($('#editor-root').firstChild) return;           // the editor handles its own keys
  const k = e.key.toLowerCase();
  if (k === 'p') return DEV && togglePerf();
  if (k === 'escape') { setFocus(null); $('#bellmenu').classList.remove('open'); return; }
  if (tab !== 'live') return;
  const o = cams.get(hovered ?? focusedId) ?? (cams.size === 1 ? [...cams.values()][0] : null);
  if (!o) return;
  const map = { b: 'boxes', l: 'labels', t: 'trails', c: 'lines', z: 'zones', h: 'hud' };
  if (map[k]) toggle(o, map[k]); else if (k === 'f') setFocus(focusedId === o.id ? null : o.id); else if (k === 'e') openEditor(o);
});
function togglePerf() { $('#drawer').classList.toggle('open'); }

$('#perfbtn').onclick = togglePerf;
$('#addbtn').onclick = () => openAddDialog();
addEventListener('click', e => {
  if (!e.target.closest('#bellmenu')) $('#bellmenu').classList.remove('open');
  const o = e.target.closest('[data-open-add]'); if (o) openAddDialog(o.dataset.openAdd || undefined);
});

/* ───────── polling ───────── */
const phases = new Map(); let phaseSig = '', phaseSince = 0;
function recordPhase(s) {
  const run = s.cameras.filter(c => c.status === 'running' && c.fps > 0), n = run.length, sig = n + '@' + (run[0]?.src_fps || 0);
  if (sig !== phaseSig) { phaseSig = sig; phaseSince = performance.now(); }
  if (!n || performance.now() - phaseSince < 3000) return;
  if (!phases.has(sig)) phases.set(sig, { n, src: run[0].src_fps, fps: [], drop: [], lat: [] });
  const p = phases.get(sig), avg = f => run.reduce((a, c) => a + f(c), 0) / n;
  p.fps.push(avg(c => c.fps)); p.lat.push(avg(c => c.lat_ms)); p.drop.push(avg(c => { const t = c.processed + c.dropped; return t ? 100 * c.dropped / t : 0; }));
  for (const k of ['fps', 'drop', 'lat']) if (p[k].length > 240) p[k].shift();
  const A = a => a.reduce((x, y) => x + y, 0) / a.length;
  $('#phases').innerHTML = [...phases.values()].sort((a, b) => a.n - b.n || a.src - b.src).map(p =>
    `<tr><td>${p.n}</td><td>${p.src} fps</td><td>${fmt(A(p.fps), 1)}</td><td>${fmt(A(p.fps) * p.n)}</td><td>${fmt(A(p.drop), 1)}%</td><td>${fmt(A(p.lat))} ms</td></tr>`).join('');
}

let lastCams = [];
async function poll() {
  let s; try { s = await (await fetch('/api/stats')).json(); } catch { return; }
  const e = s.engine; engineReady = e.ready; if (DEV !== s.dev) { DEV = s.dev; document.body.classList.toggle('dev', DEV); }
  $('#eng').className = 'pill eng ' + (e.error ? 'bad' : e.ready ? 'ok' : 'busy');
  $('#engtxt').innerHTML = e.error ? 'Engine error' : e.ready ? (DEV ? `<b>${esc(e.model.replace(/_\d+x\d+.*/, '').toUpperCase())}</b> · ${e.ep === 'trt' ? 'TensorRT' : 'CUDA'}` : 'Ready') : 'Preparing engine';
  $('#banner').className = 'banner' + (e.ready ? '' : ' show') + (e.error ? ' err' : '');
  if (!e.ready) { $('#bantitle').textContent = e.error ? 'The engine failed to start' : e.message || 'Starting'; $('#bantext').textContent = e.error || 'The first start compiles an optimised GPU engine for your card. This happens once; later starts take seconds.'; }
  $('#addbtn').disabled = !e.ready;
  $('#gpu').textContent = fmt(s.gpu.util) + '%'; $('#gpubar').style.width = s.gpu.util + '%';

  lastCams = s.cameras;
  const ids = new Set(s.cameras.map(c => c.id));
  for (const [id, o] of cams) if (!ids.has(id)) { o.ws.close(); o.el.remove(); cams.delete(id); if (focusedId === id) setFocus(null); }
  for (const c of s.cameras) {
    if (!cams.has(c.id)) cams.set(c.id, buildCard(c));
    const o = cams.get(c.id), q = k => $(`[data-k="${k}"]`, o.el);
    o.src = c.src_fps;
    $('.live', o.el).classList.toggle('off', c.status !== 'running');
    const lost = c.status === 'reconnecting' || c.status.startsWith('error');
    o.el.classList.toggle('reconnecting', lost);
    $('.lost-t', o.el).textContent = lost && c.status.startsWith('error') ? 'This camera is not responding' : 'Reconnecting to the camera…';
    $('.lv', o.el).textContent = c.status === 'running' ? (DEV ? `Live · ${c.src_fps} fps` : 'Live') : c.status.startsWith('error') ? 'Offline' : cap(c.status.split(' ')[0]);
    q('fps').textContent = fmt(c.fps, 1); q('lat').innerHTML = fmt(c.lat_ms) + '<small>ms</small>';
    const t = c.processed + c.dropped, dp = t ? 100 * c.dropped / t : 0, dd = q('drop');
    dd.innerHTML = fmt(dp, 1) + '<small>%</small>'; dd.className = dp > 15 ? 'bad' : dp > 4 ? 'warn' : '';
  }
  const n = s.cameras.length; $('#grid').dataset.n = Math.min(n, 8); $('#empty').style.display = n ? 'none' : '';
  const run = s.cameras.filter(c => c.status === 'running');
  $('#p-total').textContent = fmt(run.reduce((a, c) => a + c.fps, 0)); $('#p-batch').textContent = fmt(s.batch.avg, 1) + ' / ' + s.batch.max;
  $('#p-ms').textContent = fmt(s.batch.ms, 1); $('#p-cpu').textContent = fmt(s.cpu_cores, 1);
  recordPhase(s);
}
setInterval(poll, 500); poll();
connectAlerts(); loadIncidents();
