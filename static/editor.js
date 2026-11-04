'use strict';
/* Configuration editor: draw counting lines and zones on a frozen frame, pick the classes,
   set zone capacity and define alarm rules. Everything is stored normalised (0..1). */

let ED = null;
const KIND_LABEL = { enter: 'A vehicle enters the zone', exit: 'A vehicle leaves the zone', dwell: 'A vehicle stays in the zone for…', occupancy: 'The zone is more than … full', cross: 'A vehicle crosses a line' };
const uid = p => p + Math.random().toString(36).slice(2, 7);
const clamp01 = v => Math.max(0, Math.min(1, v));

async function openEditor(o, startTab = 'shapes') {
  if (ED) return;
  const r = await fetch(`/api/config/${o.id}`); if (!r.ok) return toast('Could not load the configuration');
  const j = await r.json();
  const snap = document.createElement('canvas'); snap.width = 960; snap.height = 540; snap.getContext('2d').drawImage(o.base, 0, 0);
  ED = { o, setup: j.setup, name: j.name, cfg: JSON.parse(JSON.stringify(j.config)), snap, tool: 'select', sel: null, draft: null, drag: null, hover: null, mouse: null, tab: startTab, dirty: false };
  const root = $('#editor-root');
  root.innerHTML = `
    <div class="editor">
      <div class="ed-head"><h2>Configure <span>· ${esc(j.name)}</span></h2><div class="spacer" style="flex:1"></div>
        <button class="btn ghost" id="ed-cancel">Cancel</button><button class="btn" id="ed-save">Save &amp; apply</button></div>
      <div class="ed-body">
        <div class="ed-stage"><canvas id="ed-cv"></canvas><div class="ed-hint" id="ed-hint"></div>
          <div class="ed-tools">
            <button data-t="select">${I.select}Select <kbd>V</kbd></button>
            <button data-t="line">${I.lines}Counting line <kbd>L</kbd></button>
            <button data-t="zone">${I.zones}Zone <kbd>Z</kbd></button></div></div>
        <div class="ed-panel"><div class="ed-tabs"><button data-p="shapes">Lines &amp; zones</button><button data-p="detect">Detect</button><button data-p="alarms">Alarms</button></div><div class="ed-scroll" id="ed-scroll"></div></div>
      </div>
    </div>`;
  ED.cv = $('#ed-cv'); ED.ctx = ED.cv.getContext('2d');
  new ResizeObserver(sizeEditor).observe($('.ed-stage'));
  $('#ed-cancel').onclick = closeEditor; $('#ed-save').onclick = saveEditor;
  $('.ed-tools').onclick = e => { const b = e.target.closest('button'); if (b) setTool(b.dataset.t); };
  $('.ed-tabs').onclick = e => { const b = e.target.closest('button'); if (b) { ED.tab = b.dataset.p; renderPanel(); } };
  const cv = ED.cv;
  cv.addEventListener('pointerdown', edDown); cv.addEventListener('pointermove', edMove); cv.addEventListener('pointerup', edUp); cv.addEventListener('dblclick', edDbl);
  cv.addEventListener('pointerleave', () => { ED.mouse = null; ED.hover = null; draw(); });
  document.addEventListener('keydown', edKey, true);
  sizeEditor(); renderPanel(); setTool('select');
}

function closeEditor() { document.removeEventListener('keydown', edKey, true); $('#editor-root').innerHTML = ''; ED = null; }

async function saveEditor() {
  const { o, cfg } = ED;
  const r = await fetch(`/api/config/${o.id}`, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(cfg) });
  if (!r.ok) return toast('Could not save the configuration');
  closeEditor(); toast('Configuration applied');
}

function sizeEditor() {
  if (!ED) return;
  const r = $('.ed-stage').getBoundingClientRect(), d = Math.min(window.devicePixelRatio || 1, 2);
  ED.cv.width = Math.round(r.width * d); ED.cv.height = Math.round(r.height * d); ED.dpr = d; draw();
}

function setTool(t) {
  ED.tool = t; ED.draft = null; if (t !== 'select') ED.sel = null;
  $$('.ed-tools button').forEach(b => b.classList.toggle('on', b.dataset.t === t));
  $('#ed-hint').textContent = { select: 'Click a shape to edit it · drag points to reshape · Delete removes', line: 'Click the start, then the end of the counting line', zone: 'Click to add corners · click the first point or press Enter to close' }[t];
  ED.cv.style.cursor = t === 'select' ? 'default' : 'crosshair'; renderPanel(); draw();
}

/* ───────── geometry ───────── */
const shapeOf = sel => sel && (sel.type === 'line' ? ED.cfg.lines : ED.cfg.zones).find(s => s.id === sel.id);
function pts(s, type) { return type === 'line' ? [[s.x1, s.y1], [s.x2, s.y2]] : s.points; }
function setPt(s, type, i, x, y) { if (type === 'line') { if (i === 0) { s.x1 = x; s.y1 = y; } else { s.x2 = x; s.y2 = y; } } else s.points[i] = [x, y]; }
function evPt(e) { const r = ED.cv.getBoundingClientRect(); return [clamp01((e.clientX - r.left) / r.width), clamp01((e.clientY - r.top) / r.height), r]; }
const dist = (ax, ay, bx, by) => Math.hypot(ax - bx, ay - by);
function segDist(px, py, ax, ay, bx, by) { const dx = bx - ax, dy = by - ay, t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy || 1))); return dist(px, py, ax + t * dx, ay + t * dy); }
function inPoly(poly, x, y) { let c = false; for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) { const a = poly[i], b = poly[j]; if ((a[1] > y) !== (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0]) c = !c; } return c; }

function hitTest(x, y, rect) {
  const W = rect.width, H = rect.height, R = 10;
  const all = [...ED.cfg.zones.map(s => ['zone', s]), ...ED.cfg.lines.map(s => ['line', s])];
  // vertices first (selected shape wins ties), then edges / interior
  for (const [type, s] of all.sort((a, b) => (b[1].id === ED.sel?.id) - (a[1].id === ED.sel?.id))) {
    const p = pts(s, type);
    for (let i = 0; i < p.length; i++) if (dist(x * W, y * H, p[i][0] * W, p[i][1] * H) < R) return { type, id: s.id, part: 'v', idx: i };
  }
  for (const [type, s] of all) {
    const p = pts(s, type);
    if (type === 'line' && segDist(x * W, y * H, p[0][0] * W, p[0][1] * H, p[1][0] * W, p[1][1] * H) < 9) return { type, id: s.id, part: 'body' };
    if (type === 'zone') {
      if (inPoly(p, x, y)) return { type, id: s.id, part: 'body' };
      for (let i = 0; i < p.length; i++) { const a = p[i], b = p[(i + 1) % p.length]; if (segDist(x * W, y * H, a[0] * W, a[1] * H, b[0] * W, b[1] * H) < 8) return { type, id: s.id, part: 'body' }; }
    }
  }
  return null;
}

/* ───────── pointer interaction ───────── */
function edDown(e) {
  const [x, y, rect] = evPt(e);
  if (ED.tool === 'select') {
    const h = hitTest(x, y, rect);
    ED.sel = h ? { type: h.type, id: h.id } : null;
    if (h) { ED.cv.setPointerCapture(e.pointerId); const s = shapeOf(ED.sel); ED.drag = h.part === 'v' ? { kind: 'v', idx: h.idx } : { kind: 'body', sx: x, sy: y, orig: JSON.parse(JSON.stringify(s)) }; }
    renderPanel(); draw(); return;
  }
  if (ED.tool === 'line') {
    if (!ED.draft) ED.draft = { type: 'line', a: [x, y] };
    else {
      const a = ED.draft.a; if (dist(a[0] * rect.width, a[1] * rect.height, x * rect.width, y * rect.height) > 14) {
        const n = ED.cfg.lines.length + 1, l = { id: uid('l'), name: freeName(ED.cfg.lines, 'Line'), x1: a[0], y1: a[1], x2: x, y2: y };
        ED.cfg.lines.push(l); ED.sel = { type: 'line', id: l.id }; ED.dirty = true; setTool('select'); ED.sel = { type: 'line', id: l.id }; renderPanel();
      }
    }
    draw(); return;
  }
  if (ED.tool === 'zone') {
    if (!ED.draft) ED.draft = { type: 'zone', pts: [] };
    const p = ED.draft.pts;
    if (p.length >= 3 && dist(x * rect.width, y * rect.height, p[0][0] * rect.width, p[0][1] * rect.height) < 14) return finishZone();
    p.push([x, y]); draw();
  }
}
function edMove(e) {
  const [x, y, rect] = evPt(e); ED.mouse = [x, y];
  if (ED.drag) {
    const s = shapeOf(ED.sel);
    if (ED.drag.kind === 'v') setPt(s, ED.sel.type, ED.drag.idx, x, y);
    else {
      const dx = x - ED.drag.sx, dy = y - ED.drag.sy, o = ED.drag.orig, p = pts(o, ED.sel.type);
      const minx = Math.min(...p.map(q => q[0])), maxx = Math.max(...p.map(q => q[0])), miny = Math.min(...p.map(q => q[1])), maxy = Math.max(...p.map(q => q[1]));
      const ddx = Math.max(-minx, Math.min(1 - maxx, dx)), ddy = Math.max(-miny, Math.min(1 - maxy, dy));
      if (ED.sel.type === 'line') { s.x1 = o.x1 + ddx; s.y1 = o.y1 + ddy; s.x2 = o.x2 + ddx; s.y2 = o.y2 + ddy; } else s.points = o.points.map(q => [q[0] + ddx, q[1] + ddy]);
    }
    ED.dirty = true;
  } else if (ED.tool === 'select') {
    const h = hitTest(x, y, rect); ED.hover = h; ED.cv.style.cursor = h ? (h.part === 'v' ? 'grab' : 'move') : 'default';
  }
  draw();
}
function edUp() { ED.drag = null; }
function edDbl() { if (ED.tool === 'zone' && ED.draft && ED.draft.pts.length >= 4) { ED.draft.pts.pop(); finishZone(); } }
function finishZone() {
  if (!ED.draft || ED.draft.pts.length < 3) return;
  const z = { id: uid('z'), name: freeName(ED.cfg.zones, 'Zone'), points: ED.draft.pts, capacity: 10, classes: [] };
  ED.cfg.zones.push(z); ED.dirty = true; setTool('select'); ED.sel = { type: 'zone', id: z.id }; renderPanel(); draw();
}
function freeName(list, base) { let n = list.length + 1; while (list.some(s => s.name === `${base} ${n}`)) n++; return `${base} ${n}`; }
function deleteSel() {
  if (!ED.sel) return;
  const { type, id } = ED.sel;
  if (type === 'line') ED.cfg.lines = ED.cfg.lines.filter(s => s.id !== id); else ED.cfg.zones = ED.cfg.zones.filter(s => s.id !== id);
  const before = ED.cfg.alarms.length; ED.cfg.alarms = ED.cfg.alarms.filter(a => a.target !== id);
  if (ED.cfg.alarms.length < before) toast('Alarms that used this shape were removed');
  ED.sel = null; ED.dirty = true; renderPanel(); draw();
}
function edKey(e) {
  if (/^(INPUT|SELECT|TEXTAREA)$/.test(e.target.tagName)) { if (e.key === 'Escape') e.target.blur(); return; }
  const k = e.key.toLowerCase();
  if (k === 'escape') { e.preventDefault(); e.stopPropagation(); if (ED.draft) { ED.draft = null; draw(); } else if (ED.tool !== 'select') setTool('select'); else closeEditor(); return; }
  if (k === 'enter' && ED.draft?.type === 'zone') { e.preventDefault(); finishZone(); return; }
  if ((k === 'backspace' || k === 'delete')) { e.preventDefault(); if (ED.draft?.type === 'zone' && ED.draft.pts.length) { ED.draft.pts.pop(); draw(); } else deleteSel(); return; }
  if (k === 'v') setTool('select'); else if (k === 'l') setTool('line'); else if (k === 'z') setTool('zone');
  e.stopPropagation();
}

/* ───────── drawing ───────── */
function draw() {
  if (!ED) return;
  const ctx = ED.ctx, W = ED.cv.width, H = ED.cv.height, d = ED.dpr || 1;
  ctx.clearRect(0, 0, W, H); ctx.drawImage(ED.snap, 0, 0, W, H);
  ctx.fillStyle = 'rgba(0,0,0,.32)'; ctx.fillRect(0, 0, W, H);
  ctx.font = `500 ${12 * d}px Inter, system-ui, sans-serif`; ctx.textBaseline = 'middle'; ctx.textAlign = 'center';
  const sel = ED.sel, hov = ED.hover;
  for (const z of ED.cfg.zones) {
    const on = sel?.id === z.id, hv = hov?.id === z.id, path = polyPath(z.points, W, H);
    ctx.globalAlpha = on ? .22 : hv ? .16 : .1; ctx.fillStyle = on ? GOLD : '#d6d2c8'; ctx.fill(path);
    ctx.globalAlpha = 1; ctx.lineWidth = (on ? 2 : 1.5) * d; ctx.lineJoin = 'round'; ctx.strokeStyle = on ? GOLD : 'rgba(236,232,223,.85)'; ctx.stroke(path);
    const cx = z.points.reduce((s, q) => s + q[0], 0) / z.points.length * W, cy = z.points.reduce((s, q) => s + q[1], 0) / z.points.length * H;
    pill(ctx, `${z.name}  ·  max ${z.capacity}`, cx, cy, on ? GOLD : '#ece8df', d);
    if (on) handles(ctx, z.points, W, H, d);
  }
  for (const l of ED.cfg.lines) {
    const on = sel?.id === l.id, hv = hov?.id === l.id, x1 = l.x1 * W, y1 = l.y1 * H, x2 = l.x2 * W, y2 = l.y2 * H;
    ctx.strokeStyle = on || hv ? GOLD_HI : GOLD; ctx.lineWidth = (on ? 2.4 : 1.8) * d; ctx.lineCap = 'round'; ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke();
    // arrow = the "forward" side (towards the right-hand side of start -> end)
    const mx = (x1 + x2) / 2, my = (y1 + y2) / 2, dx = x2 - x1, dy = y2 - y1, len = Math.hypot(dx, dy) || 1, nx = -dy / len, ny = dx / len, al = 26 * d;
    ctx.lineWidth = 1.6 * d; ctx.beginPath(); ctx.moveTo(mx, my); ctx.lineTo(mx + nx * al, my + ny * al); ctx.stroke();
    const tx = dx / len, ty = dy / len, tipx = mx + nx * al, tipy = my + ny * al;
    ctx.beginPath(); ctx.moveTo(tipx - nx * 7 * d + tx * 5 * d, tipy - ny * 7 * d + ty * 5 * d); ctx.lineTo(tipx, tipy); ctx.lineTo(tipx - nx * 7 * d - tx * 5 * d, tipy - ny * 7 * d - ty * 5 * d); ctx.stroke();
    pill(ctx, l.name, mx - nx * 18 * d, my - ny * 18 * d, GOLD, d);
    if (on) handles(ctx, [[l.x1, l.y1], [l.x2, l.y2]], W, H, d);
  }
  // drafts
  const m = ED.mouse;
  if (ED.draft?.type === 'line') {
    const a = ED.draft.a; ctx.setLineDash([6 * d, 5 * d]); ctx.strokeStyle = GOLD; ctx.lineWidth = 1.6 * d; ctx.beginPath(); ctx.moveTo(a[0] * W, a[1] * H); if (m) ctx.lineTo(m[0] * W, m[1] * H); ctx.stroke(); ctx.setLineDash([]);
    dot(ctx, a[0] * W, a[1] * H, d, GOLD);
  }
  if (ED.draft?.type === 'zone') {
    const p = ED.draft.pts; ctx.strokeStyle = GOLD; ctx.lineWidth = 1.8 * d; ctx.lineJoin = 'round'; ctx.beginPath(); p.forEach((q, i) => i ? ctx.lineTo(q[0] * W, q[1] * H) : ctx.moveTo(q[0] * W, q[1] * H));
    ctx.stroke(); if (m && p.length) { ctx.setLineDash([6 * d, 5 * d]); ctx.beginPath(); ctx.moveTo(p[p.length - 1][0] * W, p[p.length - 1][1] * H); ctx.lineTo(m[0] * W, m[1] * H); ctx.stroke(); ctx.setLineDash([]); }
    p.forEach((q, i) => dot(ctx, q[0] * W, q[1] * H, d, i === 0 && p.length >= 3 && m && dist(m[0] * W, m[1] * H, q[0] * W, q[1] * H) < 14 * d ? GOLD_HI : GOLD, i === 0 && p.length >= 3 ? 1.5 : 1));
  }
}
function pill(ctx, text, x, y, color, d) {
  const w = ctx.measureText(text).width + 18 * d; ctx.fillStyle = 'rgba(10,10,12,.78)'; rr(ctx, x - w / 2, y - 11 * d, w, 22 * d, 11 * d); ctx.fill(); ctx.fillStyle = color; ctx.fillText(text, x, y + .5 * d);
}
function dot(ctx, x, y, d, color, k = 1) { ctx.fillStyle = 'rgba(10,10,12,.9)'; ctx.beginPath(); ctx.arc(x, y, 7 * d * k, 0, 6.3); ctx.fill(); ctx.fillStyle = color; ctx.beginPath(); ctx.arc(x, y, 4.6 * d * k, 0, 6.3); ctx.fill(); }
function handles(ctx, p, W, H, d) { p.forEach(q => dot(ctx, q[0] * W, q[1] * H, d, GOLD_HI)); }

/* ───────── side panel ───────── */
function chipsHtml(selected, attr) { return CLASS_LIST.map(c => `<button class="chip${selected.includes(c) ? ' on' : ''}" style="--c:${COLORS[c]}" ${attr}="${c}">${c}</button>`).join(''); }

function renderPanel() {
  if (!ED) return;
  $$('.ed-tabs button').forEach(b => b.classList.toggle('on', b.dataset.p === ED.tab));
  const box = $('#ed-scroll'), cfg = ED.cfg;
  if (ED.tab === 'shapes') {
    const s = shapeOf(ED.sel);
    const item = (type, x) => {
      const on = ED.sel?.id === x.id;
      return `<div class="item${on ? ' sel' : ''}" data-sel="${type}:${x.id}"><div class="row"><b>${esc(x.name)}</b><span class="tag">${type === 'zone' ? `max ${x.capacity}` : 'line'}</span></div>${on ? editFields(type, x) : ''}</div>`;
    };
    box.innerHTML = `<div class="lead">Counting lines count each vehicle once as it crosses. Zones measure how full an area is (vehicles inside ÷ max) and drive dwell-time alarms.</div>
      ${cfg.zones.map(z => item('zone', z)).join('')}${cfg.lines.map(l => item('line', l)).join('')}
      ${!cfg.zones.length && !cfg.lines.length ? '<div class="empty-note">Nothing drawn yet.<br>Pick a tool below the picture and click on it.</div>' : ''}
      <div class="two"><button class="btn ghost sm" data-add="line">+ Counting line</button><button class="btn ghost sm" data-add="zone">+ Zone</button></div>`;
    $$('[data-sel]', box).forEach(el => el.addEventListener('click', e => { if (e.target.closest('input,button,select,.chip')) return; const [t, id] = el.dataset.sel.split(':'); ED.sel = ED.sel?.id === id ? null : { type: t, id }; renderPanel(); draw(); }));
    $$('[data-add]', box).forEach(b => b.onclick = () => setTool(b.dataset.add));
    bindShapeFields(box);
  } else if (ED.tab === 'detect') {
    box.innerHTML = `<div class="lead">Only these object types are detected, tracked, counted and can trigger alarms.</div><div class="chips" id="cls">${chipsHtml(cfg.classes, 'data-c')}</div>
      <div class="lead" style="margin-top:8px">Each zone can narrow this further under <b>Lines &amp; zones</b>.</div>`;
    $('#cls').onclick = e => { const b = e.target.closest('[data-c]'); if (!b) return; const c = b.dataset.c, i = cfg.classes.indexOf(c); if (i >= 0 && cfg.classes.length > 1) cfg.classes.splice(i, 1); else if (i < 0) cfg.classes.push(c); ED.dirty = true; renderPanel(); };
  } else renderAlarmsPanel(box);
}

function editFields(type, x) {
  if (type === 'line') return `<div class="field"><label>Name</label><input type="text" data-f="name" value="${esc(x.name)}" maxlength="24"></div>
    <div class="lead" style="margin-top:10px">The arrow shows the <b>forward</b> direction.</div>
    <div class="two" style="margin-top:6px"><button class="btn ghost sm" data-act="flip">Flip direction</button><button class="btn danger sm" data-act="del">Delete</button></div>`;
  return `<div class="field"><label>Name</label><input type="text" data-f="name" value="${esc(x.name)}" maxlength="24"></div>
    <div class="field"><label>Max vehicles (100% = full)</label><input type="number" data-f="capacity" min="1" max="999" value="${x.capacity}"></div>
    <div class="field"><label>Counts toward occupancy</label><div class="chips">${chipsHtml(x.classes, 'data-zc')}</div><div class="lead" style="margin:2px 0 0">${x.classes.length ? '' : 'All detected classes.'}</div></div>
    <div style="margin-top:12px"><button class="btn danger sm" data-act="del">Delete zone</button></div>`;
}
function bindShapeFields(box) {
  const s = shapeOf(ED.sel); if (!s) return;
  const isLine = ED.sel.type === 'line';
  $$('[data-f]', box).forEach(inp => inp.addEventListener('input', () => {
    if (inp.dataset.f === 'name') { s.name = inp.value; const t = inp.closest('.item').querySelector('.row b'); if (t) t.textContent = inp.value || ' '; }
    else s.capacity = Math.max(1, Math.min(999, +inp.value || 1));
    ED.dirty = true; draw();
  }));
  $$('[data-zc]', box).forEach(b => b.onclick = e => { e.stopPropagation(); const c = b.dataset.zc, i = s.classes.indexOf(c); i >= 0 ? s.classes.splice(i, 1) : s.classes.push(c); renderPanel(); });
  $$('[data-act]', box).forEach(b => b.onclick = e => {
    e.stopPropagation();
    if (b.dataset.act === 'del') deleteSel();
    else if (b.dataset.act === 'flip' && isLine) { [s.x1, s.x2] = [s.x2, s.x1]; [s.y1, s.y2] = [s.y2, s.y1]; draw(); }
  });
}

function renderAlarmsPanel(box) {
  const cfg = ED.cfg, zones = cfg.zones, lines = cfg.lines;
  const targets = kind => kind === 'cross' ? lines : zones;
  box.innerHTML = `<div class="lead">Get a notification (and a sound) when a vehicle does something in a zone. Alarms appear live and are kept in the Alarms tab.</div>` +
    cfg.alarms.map((a, i) => {
      const ts = targets(a.kind);
      return `<div class="item" style="cursor:default" data-i="${i}"><div class="row"><div class="sw${a.enabled ? ' on' : ''}" data-act="en"></div><input type="text" data-f="name" value="${esc(a.name)}" maxlength="32" style="flex:1"><button class="btn danger sm" data-act="del">×</button></div>
        <div class="field"><label>When</label><select data-f="kind">${Object.entries(KIND_LABEL).map(([k, l]) => `<option value="${k}"${k === a.kind ? ' selected' : ''}>${l}</option>`).join('')}</select></div>
        <div class="field"><label>${a.kind === 'cross' ? 'Line' : 'Zone'}</label><select data-f="target">${ts.map(t => `<option value="${t.id}"${t.id === a.target ? ' selected' : ''}>${esc(t.name)}</option>`).join('')}</select></div>
        ${a.kind === 'dwell' ? `<div class="field"><label>For at least (seconds)</label><input type="number" data-f="seconds" min="1" max="3600" value="${a.seconds || 10}"></div>` : ''}
        ${a.kind === 'occupancy' ? `<div class="field"><label>Full above (%)</label><input type="number" data-f="percent" min="1" max="200" value="${a.percent || 80}"></div>` : ''}
        ${a.kind === 'cross' ? `<div class="field"><label>Direction</label><select data-f="direction">${[['any', 'Either way'], ['fwd', 'Forward (arrow)'], ['rev', 'Backward']].map(([v, l]) => `<option value="${v}"${(a.direction || 'any') === v ? ' selected' : ''}>${l}</option>`).join('')}</select></div>` : ''}
        ${a.kind !== 'occupancy' ? `<div class="field"><label>Vehicle types ${a.classes.length ? '' : '(any)'}</label><div class="chips">${chipsHtml(a.classes, 'data-ac')}</div></div>` : ''}
      </div>`;
    }).join('') +
    (!zones.length && !lines.length ? '<div class="empty-note">Draw a zone or a counting line first, then add alarms for it.</div>' : '<button class="btn ghost sm" id="add-alarm">+ Add alarm</button>');
  const ab = $('#add-alarm'); if (ab) ab.onclick = () => { const kind = zones.length ? 'dwell' : 'cross'; cfg.alarms.push({ id: uid('a'), name: `Alarm ${cfg.alarms.length + 1}`, kind, target: (kind === 'cross' ? lines : zones)[0].id, classes: [], seconds: 10, percent: 80, direction: 'any', enabled: true }); ED.dirty = true; renderPanel(); box.scrollTop = box.scrollHeight; };
  $$('.item[data-i]', box).forEach(el => {
    const a = cfg.alarms[+el.dataset.i];
    $$('[data-f]', el).forEach(f => f.addEventListener(f.tagName === 'SELECT' ? 'change' : 'input', () => {
      const k = f.dataset.f, v = f.type === 'number' ? +f.value : f.value;
      a[k] = v;
      if (k === 'kind') { if (!targets(v).some(t => t.id === a.target)) a.target = targets(v)[0]?.id ?? ''; renderPanel(); }
    }));
    $$('[data-ac]', el).forEach(b => b.onclick = () => { const c = b.dataset.ac, i = a.classes.indexOf(c); i >= 0 ? a.classes.splice(i, 1) : a.classes.push(c); renderPanel(); });
    $$('[data-act]', el).forEach(b => b.onclick = () => { if (b.dataset.act === 'del') cfg.alarms.splice(+el.dataset.i, 1); else a.enabled = !a.enabled; ED.dirty = true; renderPanel(); });
  });
}
