'use strict';
// The token stays in this browser's localStorage and travels only in the Authorization header.
const $ = (id) => document.getElementById(id);
let token = '';
try { token = localStorage.getItem('pctl-token') || ''; } catch (e) { /* private mode */ }
let current = null;
let timer = null;
const BLOCKED = 'Waiting for a human decision. Answer it at the computer.';

async function api(path, body) {
  const r = await fetch(path, {
    method: 'POST',
    headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
    body: JSON.stringify(body || {}),
  });
  if (r.status === 401) { showLogin('Token refused'); throw new Error('unauthorized'); }
  const v = await r.json();
  if (!r.ok) throw new Error(v.error || String(r.status));
  return v;
}

function stopTimer() { if (timer) { clearInterval(timer); timer = null; } }
function show(id) {
  for (const s of ['login', 'list', 'view']) $(s).hidden = s !== id;
  $('back').hidden = id !== 'view';
  if (id !== 'view') stopTimer();
}
function showLogin(msg) { show('login'); $('where').textContent = msg || ''; }

async function list() {
  show('list');
  $('where').textContent = '';
  const all = await api('/api/sessions');
  $('sessions').replaceChildren(...all.map((s) => {
    const li = document.createElement('li');
    const b = document.createElement('button');
    b.textContent = s.name;
    b.onclick = () => view(s.name);
    const st = document.createElement('span');
    st.className = 'st ' + s.status;
    st.textContent = s.status;
    const p = document.createElement('small');
    p.textContent = (s.project || '') + ' · ' + (s.harness || '-');
    li.append(b, st, p);
    return li;
  }));
}

async function refresh() {
  try {
    const v = await api('/api/screen', { name: current, lines: 60 });
    const pre = $('screen');
    const atEnd = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
    pre.textContent = v.text;
    if (atEnd) pre.scrollTop = pre.scrollHeight;
    $('status').textContent = current + ' · ' + v.status;
    $('status').className = 'st ' + v.status;
    const blocked = v.status === 'blocked';
    $('send').disabled = blocked;
    if (blocked) $('note').textContent = BLOCKED;
    else if ($('note').textContent === BLOCKED) $('note').textContent = '';
  } catch (e) {
    $('note').textContent = e.message;
  }
}

// Only the person switches sessions; the page never moves to another one by itself.
function view(name) {
  current = name;
  show('view');
  $('screen').textContent = '';
  $('note').textContent = '';
  refresh();
  timer = setInterval(refresh, 3000);
}

$('send').onclick = async () => {
  const text = $('text').value;
  if (!text.trim()) return;
  $('send').disabled = true;
  try {
    await api('/api/prompt', { name: current, text });
    $('text').value = '';
    $('note').textContent = 'Sent.';
  } catch (e) {
    $('note').textContent = e.message;
  }
  $('send').disabled = false;
  refresh();
};
$('save').onclick = () => {
  token = $('token').value.trim();
  $('token').value = '';
  try { localStorage.setItem('pctl-token', token); } catch (e) { /* kept for this page only */ }
  list().catch((e) => { $('where').textContent = e.message; });
};
$('forget').onclick = () => {
  token = '';
  try { localStorage.removeItem('pctl-token'); } catch (e) { /* nothing stored */ }
  showLogin('');
};
$('back').onclick = () => list().catch((e) => { $('where').textContent = e.message; });
$('reload').onclick = $('back').onclick;
document.addEventListener('visibilitychange', () => {
  if (document.hidden) stopTimer();
  else if (current && !$('view').hidden && !timer) { refresh(); timer = setInterval(refresh, 3000); }
});

if (token) list().catch((e) => { $('where').textContent = e.message; });
else showLogin('');
