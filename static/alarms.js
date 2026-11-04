'use strict';
/* Alarms page: a triage inbox, not a log.
   Incidents (grouped firings) are sorted by what needs a person first. Each one carries a snapshot with the
   object highlighted, a count, how long it lasted, and three actions: acknowledge · mute this rule for a while · edit it.
   Design rules taken from alarm-management practice (ISA-18.2 / EEMUA 191): few priority levels, an alarm must be
   actionable, acknowledged state is visible, nuisance rules can be shelved temporarily and always come back. */

const AL = { cam: 'all', view: 'attention', sev: 'all', cfgs: [], cfgAt: 0, menu: null, raf: 0 };
const SNOOZES = [[15, '15 minutes'], [60, '1 hour'], [480, '8 hours'], [1440, '24 hours']];

function ruleText(r, cfg) {
  const z = cfg.zones.find(z => z.id === r.target), l = cfg.lines.find(l => l.id === r.target), who = r.classes.length ? r.classes.map(cap).join(' / ') : 'Any vehicle';
  const tn = (z || l)?.name || '?';
  return { enter: `${who} enters <b>${esc(tn)}</b>`, exit: `${who} leaves <b>${esc(tn)}</b>`, dwell: `${who} stays in <b>${esc(tn)}</b> ≥ ${r.seconds} s`, occupancy: `<b>${esc(tn)}</b> is ≥ ${r.percent}% full`, cross: `${who} crosses <b>${esc(tn)}</b>${r.direction === 'fwd' ? ' (forward)' : r.direction === 'rev' ? ' (backward)' : ''}` }[r.kind] || r.kind;
}
const sevChip = s => `<span class="sev ${s}">${SEV_ICON[s]}${SEV_LABEL[s]}</span>`;
const dur = ms => { const s = Math.round(ms / 1000); return s < 90 ? s + ' s' : s < 5400 ? Math.round(s / 60) + ' min' : (s / 3600).toFixed(1) + ' h'; };
const dayKey = t => { const d = new Date(t), n = new Date(); const diff = Math.round((new Date(n.getFullYear(), n.getMonth(), n.getDate()) - new Date(d.getFullYear(), d.getMonth(), d.getDate())) / 864e5); return diff === 0 ? 'Today' : diff === 1 ? 'Yesterday' : d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'short' }); };

function thumbHtml(i) {
  if (!i.snap) return '<div class="thumb none">no snapshot</div>';
  const b = i.bbox, c = COLORS[i.cls] || COLORS.other;
  return `<div class="thumb" data-zoom="${i.id}"><img loading="lazy" src="/api/snapshot/${i.id}" alt="">${b ? `<span class="bx" style="--c:${c};left:${b[0] / 9.6}%;top:${b[1] / 5.4}%;width:${(b[2] - b[0]) / 9.6}%;height:${(b[3] - b[1]) / 5.4}%"></span>` : ''}</div>`;
}
function incHtml(i) {
  const st = incState(i), c = COLORS[i.cls] || COLORS.other;
  const stateTxt = st === 'acked' ? `Acknowledged ${ago(i.acked_t)}` : st === 'active' ? 'Ongoing' : `Ended ${ago(i.last_t)}`;
  const snz = INC.snoozes.some(s => s.cam === i.cam && s.rule_id === i.rule_id);
  return `<div class="inc ${i.severity === 'critical' && !i.acked_t ? 'crit' : ''} ${st === 'acked' ? 'acked' : ''}" data-id="${i.id}">
    ${thumbHtml(i)}
    <div class="mid">
      <div class="ttl">${sevChip(i.severity)}<b>${esc(i.rule)}</b>${i.count > 1 ? `<span class="cnt" title="${i.count} times in this incident">×${i.count}</span>` : ''}<span class="state ${st}">${stateTxt}</span></div>
      <div class="msg">${esc(i.msg)}</div>
      <div class="meta"><span>${esc(i.cam)}</span><span>${hhmmss(i.first_t)}${i.count > 1 ? ` · lasted ${dur(i.last_t - i.first_t)}` : ''}</span>${i.cls ? `<span class="legend" style="margin:0"><span><i style="--c:${c}"></i>${esc(i.cls)}</span></span>` : ''}${snz ? '<span>rule muted</span>' : ''}</div>
    </div>
    <div class="acts">
      ${i.acked_t ? '' : `<button class="btn sm" data-ack="${i.id}">Acknowledge</button>`}
      <button class="btn ghost sm" data-mute="${i.id}">Mute this rule ▾</button>
      <button class="btn ghost sm" data-edit="${i.id}">Edit rule</button>
    </div></div>`;
}

async function loadRuleCfgs(force) {
  if (!force && Date.now() - AL.cfgAt < 20000) return;
  AL.cfgAt = Date.now();
  AL.cfgs = await Promise.all([...cams.values()].map(o => fetch(`/api/config/${o.id}`).then(r => r.json()).then(j => ({ o, j }))));
}

function renderAlarms(force) {
  if (AL.raf) return;
  AL.raf = requestAnimationFrame(async () => {
    AL.raf = 0;
    if (tab !== 'alarms') return;
    if (AL.menu) return;                       // do not yank a menu away while it is open
    const root = $('#view-alarms');
    try { await loadRuleCfgs(force); } catch {}
    const all = INC.list, camNames = [...new Set([...all.map(i => i.cam), ...[...cams.values()].map(o => o.name)])].sort();
    if (AL.cam !== 'all' && !camNames.includes(AL.cam)) AL.cam = 'all';
    const pend = all.filter(needsAttention), crit = pend.filter(i => i.severity === 'critical').length;
    let list = all.filter(i => (AL.cam === 'all' || i.cam === AL.cam) && (AL.sev === 'all' || (AL.sev === 'critical' ? i.severity === 'critical' : SEV[i.severity] >= 1)));
    const counts = { attention: list.filter(needsAttention).length, all: list.length, acked: list.filter(i => i.acked_t).length };
    if (AL.view === 'attention') list = list.filter(needsAttention);
    else if (AL.view === 'acked') list = list.filter(i => i.acked_t);
    list.sort((a, b) => (!a.acked_t === !b.acked_t ? 0 : a.acked_t ? 1 : -1) || SEV[b.severity] - SEV[a.severity] || b.last_t - a.last_t);

    let body = '';
    if (!list.length) {
      body = AL.view === 'attention'
        ? `<div class="allclear"><svg viewBox="0 0 24 24"><path d="M12 0C13 8 16 11 24 12C16 13 13 16 12 24C11 16 8 13 0 12C8 11 11 8 12 0Z"/></svg><b>All clear</b>Nothing needs your attention.${all.length ? ' Handled alarms are under “All”.' : ' Alarms from your rules will show up here, grouped, with a snapshot.'}</div>`
        : '<div class="allclear"><b>Nothing here</b>No alarms match these filters.</div>';
    } else if (AL.view === 'attention') {
      body = `<div class="inc-list">${list.map(incHtml).join('')}</div>`;
    } else {
      const groups = new Map(); for (const i of list) { const k = i.acked_t ? dayKey(i.last_t) : 'Needs attention'; (groups.get(k) || groups.set(k, []).get(k)).push(i); }
      body = [...groups].map(([k, v]) => `<div class="inc-group"><span class="micro">${esc(k)}</span><div class="inc-list">${v.slice(0, 80).map(incHtml).join('')}</div></div>`).join('');
    }
    const snz = INC.snoozes.filter(s => s.until > Date.now());
    const notifyOpts = [['quiet', 'Critical only'], ['all', 'Warnings too'], ['off', 'Off']];
    root.innerHTML = `
      <div class="al-top"><h1>Alarms</h1><span class="sum">${pend.length ? `<b>${pend.length}</b> need${pend.length === 1 ? 's' : ''} attention${crit ? ` · <b style="color:#ff8d8d">${crit} critical</b>` : ''}` : 'All clear'}</span></div>
      <div class="al-bar">
        <div class="segbtn" id="al-view"><button data-v="attention"${AL.view === 'attention' ? ' class="on"' : ''}>Needs attention · ${counts.attention}</button><button data-v="all"${AL.view === 'all' ? ' class="on"' : ''}>All · ${counts.all}</button><button data-v="acked"${AL.view === 'acked' ? ' class="on"' : ''}>Acknowledged · ${counts.acked}</button></div>
        <select id="al-cam" style="width:auto;min-width:170px;border-radius:99px"><option value="all">All cameras</option>${camNames.map(c => `<option value="${esc(c)}"${c === AL.cam ? ' selected' : ''}>${esc(c)}</option>`).join('')}</select>
        <select id="al-sev" style="width:auto;min-width:150px;border-radius:99px"><option value="all">Every severity</option><option value="warning"${AL.sev === 'warning' ? ' selected' : ''}>Warning and above</option><option value="critical"${AL.sev === 'critical' ? ' selected' : ''}>Critical only</option></select>
        <span class="grow"></span>
        ${counts.attention ? '<button class="btn sm" id="al-ackall">Acknowledge all</button>' : ''}
      </div>
      ${snz.length ? `<div class="snoozed">${snz.map(s => { const r = AL.cfgs.find(c => c.o.name === s.cam)?.j.config.alarms.find(a => a.id === s.rule_id); return `<span class="snz"><span><b style="color:var(--text);font-weight:600">${esc(r?.name || s.rule_id)}</b> · ${esc(s.cam)} muted until ${hhmmss(s.until).slice(0, 5)}</span><button class="btn ghost sm" data-unmute="${esc(s.cam)}|${esc(s.rule_id)}">Resume</button></span>`; }).join('')}</div>` : ''}
      ${body}
      <div class="inc-group" style="margin-top:34px"><span class="micro">Notifications on this screen</span>
        <div class="al-bar" style="margin-top:10px"><div class="segbtn" id="al-notify">${notifyOpts.map(([k, l]) => `<button data-n="${k}"${notifyMode === k ? ' class="on"' : ''}>${l}</button>`).join('')}</div>
        <div class="segbtn" id="al-sound"><button data-s="1"${soundOn ? ' class="on"' : ''}>Sound on</button><button data-s="0"${!soundOn ? ' class="on"' : ''}>Sound off</button></div>
        <span class="hintline" style="color:var(--faint);font-size:12px">Only brand-new incidents can interrupt you. Repeats join the open incident silently.</span></div></div>
      <details class="rules"${AL.rulesOpen ? ' open' : ''}><summary><span class="micro">Alarm rules</span></summary>
        <div class="rulecards">${AL.cfgs.length ? AL.cfgs.map(({ o, j }) => `<div class="rcard"><div class="row"><b>${esc(j.name)}</b><button class="btn ghost sm" data-editcam="${o.id}">Edit rules</button></div><ul>${j.config.alarms.length ? j.config.alarms.map(r => `<li class="${r.enabled ? '' : 'off'}"><span class="sevdot ${r.severity || 'warning'}"></span><b>${esc(r.name)}</b><span>${ruleText(r, j.config)}</span></li>`).join('') : '<li>No rules yet</li>'}</ul></div>`).join('') : '<div class="rcard" style="color:var(--muted)">Add a camera to define alarm rules.</div>'}</div>
      </details>`;
    bindAlarms(root);
  });
}

function bindAlarms(root) {
  $('#al-view').onclick = e => { const b = e.target.closest('button'); if (b) { AL.view = b.dataset.v; renderAlarms(); } };
  $('#al-cam').onchange = e => { AL.cam = e.target.value; renderAlarms(); };
  $('#al-sev').onchange = e => { AL.sev = e.target.value; renderAlarms(); };
  const aa = $('#al-ackall'); if (aa) aa.onclick = () => ackAll(AL.cam === 'all' ? undefined : AL.cam);
  $('#al-notify').onclick = e => { const b = e.target.closest('button'); if (b) { setNotify(b.dataset.n); renderAlarms(); } };
  $('#al-sound').onclick = e => { const b = e.target.closest('button'); if (b) { setSound(b.dataset.s === '1'); if (soundOn) ping(); renderAlarms(); } };
  $('details.rules', root).ontoggle = e => { AL.rulesOpen = e.target.open; };
  root.onclick = e => {
    let b;
    if ((b = e.target.closest('[data-ack]'))) return ackIncident(+b.dataset.ack);
    if ((b = e.target.closest('[data-unmute]'))) { const [cam, rule] = b.dataset.unmute.split('|'); return snoozeRule(cam, rule, 0); }
    if ((b = e.target.closest('[data-edit]'))) { const i = INC.list.find(x => x.id === +b.dataset.edit), o = [...cams.values()].find(c => c.name === i?.cam); return o ? openEditor(o, 'alarms') : toast('That camera is not running'); }
    if ((b = e.target.closest('[data-editcam]'))) { const o = cams.get(+b.dataset.editcam); return o && openEditor(o, 'alarms'); }
    if ((b = e.target.closest('[data-zoom]'))) return zoomSnap(INC.list.find(x => x.id === +b.dataset.zoom));
    if ((b = e.target.closest('[data-mute]'))) return muteMenu(b, INC.list.find(x => x.id === +b.dataset.mute));
  };
}

function muteMenu(btn, inc) {
  closeMute();
  const m = document.createElement('div'); m.className = 'snzmenu';
  m.innerHTML = `<span class="micro" style="padding:6px 12px">Mute “${esc(inc.rule)}” for</span>` + SNOOZES.map(([min, l]) => `<button data-m="${min}">${l}</button>`).join('');
  const r = btn.getBoundingClientRect(); m.style.position = 'fixed'; m.style.left = Math.min(innerWidth - 190, r.left) + 'px'; m.style.top = (r.bottom + 4) + 'px'; m.style.right = 'auto';
  m.onclick = e => { const b = e.target.closest('button'); if (!b) return; snoozeRule(inc.cam, inc.rule_id, +b.dataset.m); toast(`Muted for ${SNOOZES.find(s => s[0] === +b.dataset.m)[1]}. It comes back by itself.`); closeMute(); };
  document.body.appendChild(m); AL.menu = m;
  setTimeout(() => addEventListener('click', closeMute, { once: true }), 0);
}
function closeMute() { if (AL.menu) { AL.menu.remove(); AL.menu = null; renderAlarms(); } }

function zoomSnap(i) {
  if (!i || !i.snap) return;
  const lb = $('#lightbox'), b = i.bbox, c = COLORS[i.cls] || COLORS.other;
  lb.innerHTML = `<div class="fr"><img src="/api/snapshot/${i.id}" alt="">${b ? `<span class="bx" style="--c:${c};left:${b[0] / 9.6}%;top:${b[1] / 5.4}%;width:${(b[2] - b[0]) / 9.6}%;height:${(b[3] - b[1]) / 5.4}%"></span>` : ''}</div>`;
  lb.classList.remove('hide'); lb.onclick = () => lb.classList.add('hide');
}
addEventListener('keydown', e => { if (e.key === 'Escape') $('#lightbox').classList.add('hide'); });
