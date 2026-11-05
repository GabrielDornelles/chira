'use strict';
/* Analytics + Alarms tabs. Charts follow the dataviz rules: thin marks, 2px gaps between stacked segments,
   4px rounded data ends on a square baseline, hairline gridlines, legend for >=2 series, hover tooltips,
   table view. Class colours = the app-wide validated palette; zone lines use three other validated slots. */

const ZONE_COLORS = ['#9085e9', '#008300', '#e66767'];
const RANGES = { '15m': [15 * 60e3, 15e3, '15 min'], '1h': [3600e3, 60e3, '1 hour'], '6h': [6 * 3600e3, 360e3, '6 hours'], '24h': [24 * 3600e3, 24 * 60e3, '24 hours'] };
const AN = { range: '15m', cam: 'all', table: false, data: null, busy: false };
const DWELL_EDGES = [0, 2, 4, 6, 10, 20, Infinity], DWELL_LABELS = ['< 2 s', '2–4 s', '4–6 s', '6–10 s', '10–20 s', '> 20 s'];

async function fetchEvents(rangeMs) {
  const r = await fetch(`/api/events?since=${Date.now() - rangeMs - 60e3}`); return r.json();
}

/* ───────── helpers ───────── */
function niceMax(v) { if (v <= 4) return 4; const p = Math.pow(10, Math.floor(Math.log10(v))), f = v / p; return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * p; }
function ticksFor(max) { const n = 4, s = max / n; return Array.from({ length: n + 1 }, (_, i) => Math.round(i * s * 100) / 100); }
function topRounded(x, y, w, h, r) { r = Math.min(r, w / 2, h); return `M${x},${y + h}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y + h}Z`; }
function rightRounded(x, y, w, h, r) { r = Math.min(r, h / 2, w); return `M${x},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y + h - r}Q${x + w},${y + h} ${x + w - r},${y + h}H${x}Z`; }
const hm = t => { const d = new Date(t); return `${pad(d.getHours())}:${pad(d.getMinutes())}`; };
const hms = hhmmss;
function showTip(chart, html, x, y) { let tip = $('.tip', chart); if (!tip) { tip = document.createElement('div'); tip.className = 'tip'; chart.appendChild(tip); } tip.replaceChildren(html); tip.classList.add('on'); const w = tip.offsetWidth; tip.style.left = Math.max(0, Math.min(chart.clientWidth - w, x + 14)) + 'px'; tip.style.top = Math.max(0, y - 10) + 'px'; }
function hideTip(chart) { $('.tip', chart)?.classList.remove('on'); }
function el(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
function tipRow(color, label, value, line = true) { const r = el('div', 'tr'); const k = el('i'); k.style.setProperty('--c', color); if (!line) { k.style.height = '10px'; k.style.width = '10px'; k.style.borderRadius = '3px'; } r.append(k, el('span', '', label), el('b', '', value)); return r; }

/* ───────── aggregation ───────── */
function aggregate() {
  const { events, occ, now } = AN.data, [span, bin] = RANGES[AN.range], t0 = now - span, N = Math.round(span / bin);
  const keep = x => AN.cam === 'all' || x.cam === AN.cam;
  const idx = t => Math.max(0, Math.min(N - 1, Math.floor((t - t0) / bin)));
  const flow = CLASS_LIST.map(() => new Array(N).fill(0));
  const mix = {}; let crossings = 0, alarms = 0, dwellSum = 0, dwellN = 0, lastMin = 0;
  const dwell = new Array(DWELL_LABELS.length).fill(0);
  for (const e of events) {
    if (e.t < t0 || !keep(e)) continue;
    if (e.kind === 'cross') { const ci = CLASS_LIST.indexOf(e.cls); if (ci >= 0) flow[ci][idx(e.t)]++; mix[e.cls] = (mix[e.cls] || 0) + 1; crossings++; if (e.t > now - 60e3) lastMin++; }
    else if (e.kind === 'alarm') alarms++;
    else if (e.kind === 'exit' && e.dwell > 0) { dwellSum += e.dwell; dwellN++; dwell[DWELL_EDGES.findIndex((lo, i) => e.dwell >= lo && e.dwell < DWELL_EDGES[i + 1])]++; }
  }
  const series = new Map(); let peak = 0;
  for (const o of occ) {
    if (o.t < t0 || !keep(o)) continue;
    const name = AN.cam === 'all' ? `${o.cam} · ${o.zone}` : o.zone, pct = 100 * o.n / Math.max(1, o.cap);
    if (!series.has(name)) series.set(name, new Array(N).fill(null));
    const a = series.get(name), i = idx(o.t); a[i] = Math.max(a[i] ?? 0, pct); peak = Math.max(peak, pct);
  }
  return { N, t0, bin, flow, mix, crossings, alarms, avgDwell: dwellN ? dwellSum / dwellN : 0, dwellN, dwell, series, peak, lastMin };
}

/* ───────── charts ───────── */
function chartFlow(host, A) {
  const W = Math.max(320, host.clientWidth), H = 230, L = 34, R = 6, T = 8, B = 24, pw = W - L - R, ph = H - T - B, slot = pw / A.N, bw = Math.max(2, Math.min(24, slot - 2));
  const tot = Array.from({ length: A.N }, (_, i) => A.flow.reduce((s, f) => s + f[i], 0)), max = niceMax(Math.max(1, ...tot)), y = v => T + ph - v / max * ph;
  let g = '';
  for (const v of ticksFor(max)) g += `<line x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}" stroke="#2a2833"/><text x="${L - 8}" y="${y(v) + 4}" text-anchor="end">${v}</text>`;
  for (let k = 0; k <= 4; k++) { const i = Math.round(k * (A.N - 1) / 4); g += `<text x="${L + (i + .5) * slot}" y="${H - 6}" text-anchor="${k === 0 ? 'start' : k === 4 ? 'end' : 'middle'}">${hm(A.t0 + i * A.bin)}</text>`; }
  let bars = '';
  for (let i = 0; i < A.N; i++) {
    let acc = 0; const x = L + i * slot + (slot - bw) / 2, segs = CLASS_LIST.map((c, ci) => [c, A.flow[ci][i]]).filter(s => s[1] > 0);
    segs.forEach(([c, v], si) => {
      const top = acc + v, y1 = y(top), h = y(acc) - y1, last = si === segs.length - 1, gap = si ? 2 : 0;
      const hh = Math.max(1, h - gap);
      bars += `<path d="${last ? topRounded(x, y1, bw, hh, 4) : `M${x},${y1}h${bw}v${hh}h-${bw}z`}" fill="${COLORS[c]}"/>`; acc = top;
    });
  }
  host.innerHTML = `<svg viewBox="0 0 ${W} ${H}" height="${H}"><g class="grid">${g}</g><g>${bars}</g><rect id="hv" y="${T}" height="${ph}" width="${slot}" fill="rgba(255,255,255,.05)" style="display:none"/><rect id="hit" x="${L}" y="${T}" width="${pw}" height="${ph}" fill="transparent"/></svg>`;
  const svg = $('svg', host), hv = $('#hv', host), hit = $('#hit', host);
  hit.addEventListener('pointermove', e => {
    const r = svg.getBoundingClientRect(), x = (e.clientX - r.left) * W / r.width, i = Math.max(0, Math.min(A.N - 1, Math.floor((x - L) / slot)));
    hv.style.display = ''; hv.setAttribute('x', L + i * slot);
    const f = el('div'); f.append(el('div', 'tt', `${hms(A.t0 + i * A.bin)} – ${hms(A.t0 + (i + 1) * A.bin)}`), tipRow('#ebe8f4', 'Total', tot[i]));
    CLASS_LIST.forEach((c, ci) => { if (A.flow[ci][i]) f.append(tipRow(COLORS[c], cap(c), A.flow[ci][i], false)); });
    showTip(host, f, e.clientX - host.getBoundingClientRect().left, e.clientY - host.getBoundingClientRect().top);
  });
  hit.addEventListener('pointerleave', () => { hv.style.display = 'none'; hideTip(host); });
}

function chartMix(host, A) {
  const rows = CLASS_LIST.map(c => [c, A.mix[c] || 0]).filter(r => r[1] > 0).sort((a, b) => b[1] - a[1]), total = rows.reduce((s, r) => s + r[1], 0);
  if (!rows.length) { host.innerHTML = '<div class="nodata">No crossings in this window</div>'; return; }
  const W = Math.max(260, host.clientWidth), rh = 38, H = rows.length * rh + 4, L = 82, R = 86, pw = W - L - R, max = rows[0][1];
  let s = '';
  rows.forEach(([c, v], i) => {
    const y = 4 + i * rh, w = Math.max(4, v / max * pw);
    s += `<text x="0" y="${y + 17}" style="fill:#ebe8f4;font-size:12px">${cap(c)}</text><path d="${rightRounded(L, y + 6, w, 16, 4)}" fill="${COLORS[c]}"/><text x="${L + w + 10}" y="${y + 18}" style="fill:#ebe8f4;font-size:12px">${v} <tspan style="fill:#8f8c9e">· ${Math.round(100 * v / total)}%</tspan></text>`;
  });
  host.innerHTML = `<svg viewBox="0 0 ${W} ${H}" height="${H}">${s}</svg>`;
}

function chartOcc(host, A) {
  const names = [...A.series.keys()];
  if (!names.length) { host.innerHTML = '<div class="nodata">No zone data in this window</div>'; return; }
  const W = Math.max(320, host.clientWidth), H = 230, L = 40, R = 14, T = 10, B = 24, pw = W - L - R, ph = H - T - B, max = Math.max(100, Math.ceil(A.peak / 25) * 25), x = i => L + (i + .5) * pw / A.N, y = v => T + ph - v / max * ph;
  let g = '';
  for (let v = 0; v <= max; v += 25) g += `<line x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}" stroke="#2a2833"/><text x="${L - 8}" y="${y(v) + 4}" text-anchor="end">${Math.round(v)}%</text>`;
  g += `<line x1="${L}" x2="${W - R}" y1="${y(100)}" y2="${y(100)}" stroke="#5e5b6e"/><text x="${W - R}" y="${y(100) - 5}" text-anchor="end" style="fill:#c3c2b7">full</text>`;
  for (let k = 0; k <= 4; k++) { const i = Math.round(k * (A.N - 1) / 4); g += `<text x="${x(i)}" y="${H - 6}" text-anchor="${k === 0 ? 'start' : k === 4 ? 'end' : 'middle'}">${hm(A.t0 + i * A.bin)}</text>`; }
  let lines = '', ends = '';
  names.forEach((n, si) => {
    const col = ZONE_COLORS[si % 3]; let d = '', pen = false, last = -1;
    A.series.get(n).forEach((v, i) => { if (v == null) { pen = false; return; } d += `${pen ? 'L' : 'M'}${x(i)},${y(v)}`; pen = true; last = i; });
    lines += `<path d="${d}" fill="none" stroke="${col}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`;
    if (last >= 0) ends += `<circle cx="${x(last)}" cy="${y(A.series.get(n)[last])}" r="4" fill="${col}" stroke="#121118" stroke-width="2"/>`;
  });
  host.innerHTML = `<svg viewBox="0 0 ${W} ${H}" height="${H}"><g class="grid">${g}</g>${lines}${ends}<line id="xh" y1="${T}" y2="${T + ph}" stroke="#8f8c9e" style="display:none"/><rect id="hit" x="${L}" y="${T}" width="${pw}" height="${ph}" fill="transparent"/></svg>`;
  const svg = $('svg', host), xh = $('#xh', host);
  $('#hit', host).addEventListener('pointermove', e => {
    const r = svg.getBoundingClientRect(), px = (e.clientX - r.left) * W / r.width, i = Math.max(0, Math.min(A.N - 1, Math.floor((px - L) / (pw / A.N))));
    xh.style.display = ''; xh.setAttribute('x1', x(i)); xh.setAttribute('x2', x(i));
    const f = el('div'); f.append(el('div', 'tt', hms(A.t0 + i * A.bin)));
    names.forEach((n, si) => { const v = A.series.get(n)[i]; f.append(tipRow(ZONE_COLORS[si % 3], n, v == null ? '–' : Math.round(v) + '%')); });
    showTip(host, f, e.clientX - host.getBoundingClientRect().left, e.clientY - host.getBoundingClientRect().top);
  });
  $('#hit', host).addEventListener('pointerleave', () => { xh.style.display = 'none'; hideTip(host); });
}

function chartDwell(host, A) {
  if (!A.dwellN) { host.innerHTML = '<div class="nodata">No completed zone visits yet</div>'; return; }
  const W = Math.max(260, host.clientWidth), H = 190, L = 6, R = 6, T = 22, B = 26, pw = W - L - R, ph = H - T - B, n = A.dwell.length, slot = pw / n, bw = Math.min(24, slot - 8), max = Math.max(...A.dwell), y = v => T + ph - v / max * ph;
  let s = `<line x1="${L}" x2="${W - R}" y1="${T + ph}" y2="${T + ph}" stroke="#363341"/>`;
  A.dwell.forEach((v, i) => { const x = L + i * slot + (slot - bw) / 2; if (v) s += `<path d="${topRounded(x, y(v), bw, T + ph - y(v), 4)}" fill="#9085e9"/><text x="${x + bw / 2}" y="${y(v) - 6}" text-anchor="middle" style="fill:#ebe8f4">${v}</text>`; s += `<text x="${L + (i + .5) * slot}" y="${H - 8}" text-anchor="middle">${DWELL_LABELS[i]}</text>`; });
  host.innerHTML = `<svg viewBox="0 0 ${W} ${H}" height="${H}">${s}</svg>`;
}

/* ───────── analytics page ───────── */
function legendHtml(items) { return `<div class="legend">${items.map(([c, l, ln]) => `<span><i${ln ? ' class="ln"' : ''} style="--c:${c}"></i>${esc(l)}</span>`).join('')}</div>`; }

async function renderAnalytics(force) {
  const root = $('#view-analytics');
  if (AN.busy) return; AN.busy = true;
  try {
    AN.data = await fetchEvents(RANGES[AN.range][0]);
    if (force || !$('.kpis', root)) buildAnalytics(root);
    const A = aggregate(), cams = [...new Set([...AN.data.events.map(e => e.cam), ...lastCams.map(c => c.name)])].sort();
    const sel = $('#an-cam'); if (sel.options.length !== cams.length + 1) sel.innerHTML = '<option value="all">All cameras</option>' + cams.map(c => `<option value="${esc(c)}">${esc(c)}</option>`).join(''); sel.value = AN.cam;
    const kp = (id, v, unit = '', d = '') => { $('#' + id + ' b').innerHTML = `${v}<small>${unit}</small>`; $('#' + id + ' .d').textContent = d; };
    kp('k-total', A.crossings, '', `in the last ${RANGES[AN.range][2]}`);
    kp('k-rate', A.lastMin, ' / min', 'last 60 seconds');
    kp('k-peak', A.series.size ? Math.round(A.peak) : '–', A.series.size ? '%' : '', 'busiest zone moment');
    kp('k-dwell', A.dwellN ? fmt(A.avgDwell, 1) : '–', A.dwellN ? ' s' : '', `${A.dwellN} completed visits`);
    const incs = INC.list.filter(i => i.last_t >= A.t0 && (AN.cam === 'all' || i.cam === AN.cam) && SEV[i.severity] >= 1); kp('k-alarms', incs.length, '', incs.filter(i => !i.acked_t).length ? `${incs.filter(i => !i.acked_t).length} not acknowledged` : 'all handled');
    $('#lg-flow').innerHTML = legendHtml(CLASS_LIST.filter((c, i) => A.flow[i].some(v => v)).map(c => [COLORS[c], cap(c)]));
    $('#lg-occ').innerHTML = A.series.size > 1 ? legendHtml([...A.series.keys()].map((n, i) => [ZONE_COLORS[i % 3], n, true])) : '';
    $('#flow-sub').textContent = `Objects crossing counting lines · ${RANGES[AN.range][1] / 1000 >= 60 ? RANGES[AN.range][1] / 60000 + ' min' : RANGES[AN.range][1] / 1000 + ' s'} bins`;
    const flowHost = $('#c-flow'); flowHost.classList.toggle('hide', AN.table);
    $('#t-flow').classList.toggle('hide', !AN.table); $('#tbl-link').textContent = AN.table ? 'Show chart' : 'Show table';
    if (AN.table) tableFlow($('#t-flow'), A); else chartFlow(flowHost, A);
    chartMix($('#c-mix'), A); chartOcc($('#c-occ'), A); chartDwell($('#c-dwell'), A);
  } catch (e) { console.error(e); } finally { AN.busy = false; }
}

function tableFlow(host, A) {
  const rows = [], used = CLASS_LIST.map((c, ci) => A.flow[ci].some(v => v));   // only classes that actually crossed
  for (let i = A.N - 1; i >= 0; i--) { const v = CLASS_LIST.map((c, ci) => A.flow[ci][i]).filter((x, ci) => used[ci]), t = v.reduce((a, b) => a + b, 0); if (t) rows.push(`<tr><td>${hms(A.t0 + i * A.bin)}</td>${v.map(x => `<td>${x || ''}</td>`).join('')}<td>${t}</td></tr>`); }
  host.innerHTML = `<div class="tbl"><table><thead class="micro"><tr><th>Time</th>${CLASS_LIST.filter((c, ci) => used[ci]).map(c => `<th>${c}</th>`).join('')}<th>Total</th></tr></thead><tbody>${rows.join('') || '<tr><td colspan="7" class="nodata">No crossings</td></tr>'}</tbody></table></div>`;
}

function buildAnalytics(root) {
  root.innerHTML = `
    <h1>Analytics</h1>
    <div class="filters"><select id="an-cam"></select><div class="segbtn" id="an-range">${Object.entries(RANGES).map(([k, v]) => `<button data-r="${k}"${k === AN.range ? ' class="on"' : ''}>${v[2]}</button>`).join('')}</div><span class="micro" id="an-live" style="margin-left:auto">auto-refresh · 5 s</span></div>
    <div class="kpis">${[['k-total', 'Objects counted'], ['k-rate', 'Current rate'], ['k-peak', 'Peak zone occupancy'], ['k-dwell', 'Average dwell time'], ['k-alarms', 'Alarm incidents']].map(([id, l]) => `<div class="kpi" id="${id}"><span class="micro">${l}</span><b>–</b><div class="d"></div></div>`).join('')}</div>
    <div class="panels">
      <div class="panel w8"><div class="head"><div><h3>Traffic flow</h3><div class="sub" id="flow-sub"></div></div><a class="tablelink" id="tbl-link">Show table</a></div><div id="lg-flow"></div><div class="chart" id="c-flow"></div><div id="t-flow" class="hide"></div></div>
      <div class="panel w4"><h3>Class mix</h3><div class="sub">Share of counted objects</div><div class="chart" id="c-mix"></div></div>
      <div class="panel w8"><h3>Zone occupancy</h3><div class="sub">Peak % of capacity per interval</div><div id="lg-occ"></div><div class="chart" id="c-occ"></div></div>
      <div class="panel w4"><h3>Dwell time</h3><div class="sub">How long objects stayed in zones</div><div class="chart" id="c-dwell"></div></div>
    </div>`;
  $('#an-cam').onchange = e => { AN.cam = e.target.value; renderAnalytics(); };
  $('#an-range').onclick = e => { const b = e.target.closest('button'); if (!b) return; AN.range = b.dataset.r; $$('#an-range button').forEach(x => x.classList.toggle('on', x === b)); renderAnalytics(); };
  $('#tbl-link').onclick = () => { AN.table = !AN.table; renderAnalytics(); };
}
setInterval(() => { if (tab === 'analytics') renderAnalytics(); else if (tab === 'alarms') renderAlarms(); }, 5000);
addEventListener('resize', () => { if (tab === 'analytics' && AN.data) renderAnalytics(); });
