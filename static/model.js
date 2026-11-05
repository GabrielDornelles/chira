'use strict';
/* Detection settings dialog: processing resolution (= which exported model), confidence, overlap merging, GPU batch size
   and the classes a new camera starts with. Confidence / overlap / classes apply immediately; a different model or batch size
   needs the engine to reload (one button). Everything has a default and a "Reset to defaults" button. */

const MD = { s: null, busy: false };
const mdApi = (method, path, body) => fetch(path, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) }).then(r => r.json());
const FAMILY = { yolo26s: ['YOLO26 S', 'Faster, a bit less accurate'], yolo26m: ['YOLO26 M', 'More accurate (default)'], yolox: ['YOLOX S', 'Older model, traffic classes only'] };
const famName = f => (FAMILY[f] || [f.toUpperCase()])[0];
const resWord = w => w <= 960 ? 'Standard' : w <= 1280 ? 'Sharper' : 'Maximum detail';
const resHint = w => w <= 960 ? 'Close or medium range. Lowest GPU use.' : w <= 1280 ? 'Cameras mounted higher or farther away.' : 'High poles, wide yards, small distant objects.';

async function openModelDialog() {
  const root = $('#dialog-root');
  root.innerHTML = `<div class="dlg-back"><div class="dlg" role="dialog" aria-label="Detection settings">
    <div class="dlg-head"><h2>Detection</h2><button class="iconbtn" data-x aria-label="Close">${I.close}</button></div>
    <div class="dlg-body" id="md-body"><div class="hintline"><span class="spin"></span>Loading…</div></div></div></div>`;
  const close = () => { root.innerHTML = ''; removeEventListener('keydown', onKey, true); };
  const onKey = e => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  addEventListener('keydown', onKey, true);
  root.firstChild.addEventListener('mousedown', e => { if (e.target === root.firstChild) close(); });
  $('[data-x]', root).onclick = close;
  MD.s = await mdApi('GET', '/api/model');
  renderModel(close);
}

function renderModel(close) {
  const body = $('#md-body'); if (!body || !MD.s) return;
  const S = MD.s, st = S.settings, d = S.defaults, act = S.active;
  const pct = v => Math.round(v * 100);
  const fams = [...new Set(S.models.map(m => m.family))];
  const cur = S.models.find(m => m.file === st.model), curFam = MD.fam && fams.includes(MD.fam) ? MD.fam : (cur ? cur.family : fams[0]);
  const pending = S.restart_needed;
  const isDefault = st.model === d.model && st.confidence === d.confidence && st.overlap === d.overlap && st.max_batch === d.max_batch && [...st.default_classes].sort().join() === [...d.default_classes].sort().join();
  const batchOpts = [0, 1, 2, 3, 4, 6, 8].map(n => `<option value="${n}"${st.max_batch === n ? ' selected' : ''}>${n ? n : `Automatic (${act.auto_batch})`}</option>`).join('');
  body.innerHTML = `
    ${pending ? `<div class="restartbar"><span>${st.model !== act.model ? 'A different model is selected.' : 'The batch size changed.'} The engine has to reload to use it. Your cameras come back by themselves.</span><button class="btn sm" id="md-restart">Reload engine</button></div>` : ''}
    <div class="hintline">Running now: <b style="color:var(--text);font-weight:600">${esc(act.model.split('/').pop().replace('.onnx', ''))}</b> · ${act.w} × ${act.h} · ${act.ep === 'trt' ? 'TensorRT' : 'CUDA'} · ${act.max_batch} per batch${act.from_env ? ' · model forced by the MODEL environment variable' : ''}</div>

    <div class="mdsec"><h3>Processing resolution</h3>
      <div class="hintline">Every camera frame is resized to this size before detection. <b style="color:var(--text);font-weight:600">If your camera is mounted high or far from the scene, objects are only a few pixels wide: raise it.</b> More pixels means more GPU work per frame.</div>
      ${fams.length > 1 ? `<div class="segbtn" id="md-fam" style="width:fit-content">${fams.map(f => `<button data-f="${esc(f)}"${f === curFam ? ' class="on"' : ''} title="${esc((FAMILY[f] || [])[1] || '')}">${esc(famName(f))}</button>`).join('')}</div>` : ''}
      <div class="res-grid">${S.models.filter(m => m.family === curFam).map(m => `
        <button class="rescard${m.file === st.model ? ' on' : ''}${m.file === act.model ? ' isrun' : ''}" data-m="${esc(m.file)}">
          <b>${resWord(m.w)}</b><span class="px">${m.w} × ${m.h}</span>
          <span>${resHint(m.w)}</span>
          <span class="cost">${m.cost <= 1.05 ? 'Baseline GPU use' : `≈ ${m.cost}× the GPU work`}${m.all_classes ? '' : ' · traffic classes only'}${m.file === act.model ? ' · running' : ''}</span>
        </button>`).join('') || '<div class="hintline">No models found in ./models.</div>'}</div>
      <div class="hintline" style="color:var(--faint)">More sizes: export one with <code>research/export_ultra_wrapped.py --imgsz H W</code> and drop it in <code>models/</code>.</div>
    </div>

    <div class="mdsec"><h3>Sensitivity</h3>
      <div class="rng"><label class="l">Confidence <b id="v-conf">${pct(st.confidence)}%</b><i>default ${pct(d.confidence)}%</i></label><input type="range" id="md-conf" min="10" max="95" step="1" value="${pct(st.confidence)}"></div>
      <div class="hintline">How sure the model must be before an object is tracked and counted. Lower finds more small or partly hidden things, with more false detections. Higher is stricter.</div>
      <div class="rng"><label class="l">Overlap merging <b id="v-ov">${pct(st.overlap)}%</b><i>default ${pct(d.overlap)}%</i></label><input type="range" id="md-ov" min="10" max="90" step="1" value="${pct(st.overlap)}"></div>
      <div class="hintline">Boxes of the same kind that overlap more than this are treated as one object. Lower merges close neighbours (a crowd may count short), higher keeps them apart (one object may be counted twice).</div>
    </div>

    <div class="mdsec"><h3>New cameras track</h3>
      <div class="chips" id="md-cls">${chipsHtml(st.default_classes, 'data-dc')}</div>
      <div class="hintline">The classes a camera starts with when you add it. Each camera keeps its own list: change it in the camera's Configure panel.</div>
    </div>

    <details class="mdsec"${MD.adv ? ' open' : ''} id="md-adv"><summary><h3 style="display:inline">Advanced</h3></summary>
      <div class="rng" style="margin-top:10px"><label class="l">Cameras per GPU batch</label><select id="md-batch" style="width:auto;min-width:190px;border-radius:99px">${batchOpts}</select></div>
      <div class="hintline">Frames from this many cameras are detected in one GPU call. More is more efficient but uses more GPU memory; the automatic value follows the resolution.</div>
    </details>

    <div class="dlg-actions"><span class="res" id="md-res"></span><button class="btn ghost" id="md-reset"${isDefault ? ' disabled' : ''}>Reset to defaults</button><button class="btn" id="md-done">Done</button></div>`;

  const save = async patch => { MD.s = await mdApi('PUT', '/api/model', patch); renderModel(close); };
  const flash = t => { const r = $('#md-res'); if (r) { r.textContent = t; r.className = 'res ok'; clearTimeout(flash.t); flash.t = setTimeout(() => { r.textContent = ''; }, 1800); } };
  const slider = (id, label, key) => {
    const el = $(id);
    el.oninput = () => { $(label).textContent = el.value + '%'; };
    el.onchange = async () => { MD.s = await mdApi('PUT', '/api/model', { [key]: el.value / 100 }); flash('Saved. Applies to the live cameras now.'); const r = $('#md-reset'); if (r) r.disabled = false; };
  };
  slider('#md-conf', '#v-conf', 'confidence'); slider('#md-ov', '#v-ov', 'overlap');
  const fam = $('#md-fam'); if (fam) fam.onclick = e => { const b = e.target.closest('button'); if (b) { MD.fam = b.dataset.f; renderModel(close); } };
  $$('.rescard', body).forEach(b => b.onclick = () => save({ model: b.dataset.m }));
  $('#md-cls').onclick = e => { const b = e.target.closest('[data-dc]'); if (!b) return; const set = new Set(st.default_classes); set.has(b.dataset.dc) ? set.delete(b.dataset.dc) : set.add(b.dataset.dc); if (!set.size) return flash('Keep at least one class.'); save({ default_classes: CLASS_LIST.filter(c => set.has(c)) }); };
  $('#md-batch').onchange = e => save({ max_batch: +e.target.value });
  $('#md-adv').ontoggle = e => { MD.adv = e.target.open; };
  $('#md-reset').onclick = async () => { MD.s = await mdApi('POST', '/api/model/reset'); MD.fam = null; renderModel(close); flash('Back to the defaults.'); };
  $('#md-done').onclick = () => { $('#dialog-root').innerHTML = ''; };
  const rs = $('#md-restart'); if (rs) rs.onclick = restartEngine;
}

/* reload the engine, then come back once it is ready */
async function restartEngine() {
  $('#dialog-root').innerHTML = `<div class="dlg-back"><div class="dlg" style="text-align:center;padding:38px 30px"><h2 style="font-weight:200;font-size:24px">Reloading the engine</h2><div class="hintline" style="margin-top:10px" id="rs-msg"><span class="spin"></span>Stopping the cameras…</div><div class="hintline" style="margin-top:6px;color:var(--faint)">A new resolution compiles once (a few minutes), then it is cached. Your cameras come back by themselves.</div></div></div>`;
  try { const r = await fetch('/api/restart', { method: 'POST' }); if (!r.ok) { const j = await r.json().catch(() => ({})); $('#rs-msg').textContent = j.error || 'The engine could not be reloaded.'; return; } } catch {}
  const msg = $('#rs-msg');
  await new Promise(r => setTimeout(r, 2500));
  for (let i = 0; i < 900; i++) {
    try {
      const s = await (await fetch('/api/stats', { cache: 'no-store' })).json();
      if (s.engine.ready) return location.reload();
      if (msg) msg.innerHTML = `<span class="spin"></span>${esc(s.engine.message || 'Starting…')}`;
    } catch { if (msg) msg.innerHTML = '<span class="spin"></span>Restarting…'; }
    await new Promise(r => setTimeout(r, 1500));
  }
  location.reload();
}

$('#detbtn').onclick = () => openModelDialog();
$('#eng').onclick = () => openModelDialog();
