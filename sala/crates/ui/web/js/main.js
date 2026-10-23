// Tela Principal. A interface só mostra o estado do núcleo e envia comandos (D-010).
// Chat nunca é persistido: fica só em `state.chat`, na memória (inv-chat-never-on-disk).

import {
  initialState, setLink, reduce, friends, inRoom, callPeers, chatPeer, callTarget,
  friendStatus, micText, formatDuration, callSeconds, formatClock, initial,
} from './state.js';
import { initSettings } from './settings.js';

const tauri = window.__TAURI__;
const state = initialState();
const $ = (id) => document.getElementById(id);

function send(msg) {
  return tauri.core.invoke('send', { msg }).catch(() => {});
}

// Cria elemento: h('div', { class: 'x', onclick: fn }, filhos...)
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v == null) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}

function icon(id, size) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'ico');
  svg.setAttribute('width', size);
  svg.setAttribute('height', size);
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#${id}`);
  svg.append(use);
  return svg;
}

const linked = () => state.link === 'connected';

const { renderSettings, renderMicMeter, focusAddFriend } = initSettings({ state, send, h, icon });
let micMeterOn = false;

function setMicMeter(on) {
  if (micMeterOn === on) return Promise.resolve();
  micMeterOn = on;
  return send({ type: 'SetMicMeter', on });
}

function syncMicMeter() {
  return setMicMeter(linked() && state.view === 'settings');
}

// ---------- Renderização ----------

function renderFriends() {
  const box = $('friends');
  const list = friends(state);
  const out = [];
  if (list.length === 0) {
    out.push(h('div', { class: 'empty-note' }, 'Nenhum amigo ainda.'));
  }
  for (const f of list) {
    const st = friendStatus(state, f.name);
    const avatar = h('div', { class: `avatar${st.online ? '' : ' off'}` }, initial(f.name),
      h('span', { class: `badge ${st.dot === 'off' ? '' : st.dot}` }));
    const text = h('div', { class: 'friend-text' },
      h('span', { class: 'friend-name' }, f.name),
      h('span', { class: `friend-status tone-${st.tone}` }, st.text));
    const cls = `friend${st.online ? '' : ' off'}`;
    // Fora da sala, clicar no amigo entra na sala (Call); dentro dela a linha é só informação.
    if (state.status && !inRoom(state)) {
      out.push(h('div', { role: 'listitem' }, h('button', {
        type: 'button', class: cls, 'data-key': `friend:${f.name}`,
        'aria-label': `Entrar na sala com ${f.name}. ${st.text}`,
        disabled: !linked(),
        onclick: () => send({ type: 'Call', friend: f.name }),
      }, avatar, text)));
    } else {
      out.push(h('div', { role: 'listitem', class: cls }, avatar, text));
    }
  }
  box.replaceChildren(...out);
}

function renderMe() {
  const name = (state.config && state.config.name) || 'Você';
  const ok = linked();
  $('me').replaceChildren(
    h('div', { class: 'avatar self' }, initial(name)),
    h('div', { class: 'me-text' },
      h('span', { class: 'me-name' }, name),
      h('span', { class: 'me-status' }, h('span', { class: `dot${ok ? '' : ' danger'}` }),
        ok ? 'Núcleo conectado' : 'Sem conexão')),
  );
}

function renderBanners() {
  const out = [];
  if (!linked()) {
    out.push(h('div', { class: 'banner danger', role: 'alert' },
      h('div', { class: 'banner-icon' }, icon('i-alert', 20)),
      h('div', { class: 'banner-text' },
        h('strong', {}, 'Sem conexão com o núcleo'),
        h('span', {}, 'Tentando reconectar…'))));
  }
  for (const name of Object.keys(state.mismatch)) {
    out.push(h('div', { class: 'banner danger', role: 'alert' },
      h('div', { class: 'banner-icon' }, icon('i-alert', 20)),
      h('div', { class: 'banner-text' },
        h('strong', {}, `A identidade de ${name} mudou`),
        h('span', {}, 'A conexão foi recusada por segurança. Se ele reinstalou o app, redefina a identidade nos Ajustes.')),
      h('button', { type: 'button', class: 'btn neutral', onclick: () => setView('settings') }, 'Abrir Ajustes')));
  }
  $('banners').replaceChildren(...out);
}

function statusCard(name, p) {
  const st = friendStatus(state, name);
  const mic = p.status ? micText(p.status) : { text: st.text, tone: st.tone };
  const vol = state.volumes[name] ?? 100;
  const slider = h('input', {
    type: 'range', min: '0', max: '200', step: '1', value: String(vol),
    'aria-label': `Volume de ${name}`, 'data-key': `vol:${name}`,
    disabled: !linked(),
    oninput: (e) => {
      const v = Number(e.target.value);
      state.volumes[name] = v;
      send({ type: 'SetPeerVolume', friend: name, volume: v / 100 });
    },
  });
  return h('div', { class: 'card' },
    h('div', { class: 'avatar big' }, initial(name)),
    h('div', { class: 'who' }, h('strong', {}, name), h('span', { class: `tone-${mic.tone}` }, mic.text)),
    h('label', { class: 'vol' }, 'Volume', slider));
}

function selfCard() {
  const s = state.status;
  const mic = s ? (s.deafened ? { text: 'Silenciado', tone: 'danger' } : micText(s)) : { text: '', tone: 'muted' };
  const a = state.config && state.config.audio;
  const detail = a
    ? `${a.mode === 'push_to_talk' ? 'Aperte para falar' : 'Ativação por voz'} · ${a.bitrate_kbps} kbps`
    : '';
  const name = (state.config && state.config.name) || 'Você';
  return h('div', { class: 'card' },
    h('div', { class: 'avatar self big' }, initial(name)),
    h('div', { class: 'who' }, h('strong', {}, 'Você'), h('span', { class: `tone-${mic.tone}` }, mic.text)),
    detail && h('span', { class: 'hint' }, detail));
}

function renderStage() {
  const cards = [];
  const list = friends(state);
  const peers = callPeers(state);
  if (list.length === 0) {
    cards.push(h('div', { class: 'card empty' },
      h('h2', {}, 'Nenhum amigo configurado'),
      h('p', {}, 'Adicione um amigo pelo nome e pelo IP do Tailscale para chamar. Abra os Ajustes para começar.'),
      h('button', { type: 'button', class: 'btn primary', onclick: () => setView('settings') }, 'Abrir Ajustes')));
  } else if (!state.status) {
    // ainda sem Status do núcleo: nada a mostrar
  } else if (!inRoom(state)) {
    cards.push(h('div', { class: 'card empty' },
      h('h2', {}, 'Você está fora da sala'),
      h('p', {}, 'Ninguém ouve você e você não ouve ninguém. Toque em Entrar, ou em um amigo, para voltar.')));
  } else if (peers.length === 0) {
    cards.push(h('div', { class: 'card empty' },
      h('h2', {}, 'Sala aberta'),
      h('p', {}, 'Aguardando amigos entrarem na sala.')));
    cards.push(selfCard());
  } else {
    for (const n of peers) cards.push(statusCard(n, state.peers[n]));
    cards.push(selfCard());
  }
  $('stage').replaceChildren(...cards);

  // Banner de transmissão do amigo (o núcleo só anuncia `sharing` quando existir).
  const sharer = callPeers(state).find((n) => state.peers[n].status.sharing);
  $('share-banner').replaceChildren(...(sharer ? [h('div', { class: 'banner' },
    h('div', { class: 'banner-icon' }, icon('i-screen', 20)),
    h('div', { class: 'banner-text' }, h('strong', {}, `${sharer} está transmitindo a tela`)),
    h('button', { type: 'button', class: 'btn primary', disabled: !linked(), onclick: () => send({ type: 'OpenViewer' }) }, 'Assistir'))] : []));
}

function renderHeader() {
  const peers = callPeers(state);
  let title = 'Sala';
  if (state.status) {
    if (!inRoom(state)) title = 'Fora da sala';
    else if (peers.length) title = `Chamada com ${peers.join(', ')}`;
    else title = 'Sala aberta';
  }
  $('title').textContent = title;

  const first = peers.find((n) => state.peers[n].stats) || peers[0];
  const pill = $('link-pill');
  const timer = $('timer');
  if (first) {
    const stats = state.peers[first].stats;
    pill.hidden = false;
    $('link-text').textContent = stats ? `Conexão direta · ${stats.rtt_ms} ms` : 'Medindo a conexão…';
    timer.hidden = !stats;
  } else {
    pill.hidden = true;
    timer.hidden = true;
  }
  tickTimer();

  const kbd = $('game-kbd');
  const hk = state.config && state.config.hotkeys && state.config.hotkeys.toggle_game_mode;
  kbd.hidden = !hk;
  kbd.textContent = hk || '';
}

function tickTimer() {
  const peers = callPeers(state);
  const first = peers.find((n) => state.peers[n].stats);
  if (first) $('timer').textContent = formatDuration(callSeconds(state.peers[first].stats));
}

function renderControls() {
  const s = state.status;
  const off = !linked() || !s;
  const muted = !!(s && s.muted);
  const deaf = !!(s && s.deafened);
  const btnMute = $('btn-mute');
  btnMute.setAttribute('aria-pressed', String(muted));
  btnMute.disabled = off;
  $('lbl-mute').textContent = muted ? 'Desmutar‑se' : 'Mutar-se'; // hífen sem quebra de linha
  $('use-mute').setAttribute('href', muted ? '#i-mic-off' : '#i-mic');
  const btnDeaf = $('btn-deaf');
  btnDeaf.setAttribute('aria-pressed', String(deaf));
  btnDeaf.disabled = off;
  $('lbl-deaf').textContent = deaf ? 'Voltar a ouvir' : 'Silenciar';
  $('btn-game').disabled = off;
  $('game-top').disabled = off;

  const room = $('btn-room');
  const inside = inRoom(state);
  room.classList.toggle('danger', inside);
  room.classList.toggle('enter', !inside);
  $('lbl-room').textContent = inside ? 'Desligar' : 'Entrar';
  $('use-room').setAttribute('href', inside ? '#i-hangup' : '#i-call');
  room.disabled = off || (!inside && !callTarget(state));
}

function chatLabel() {
  const p = chatPeer(state);
  return p ? `Mensagem para ${p}` : 'Mensagem';
}

function renderChat() {
  const log = $('chat-log');
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  const label = chatLabel();
  const input = $('chat-input');
  input.placeholder = label;
  input.setAttribute('aria-label', label);
  const can = linked() && !!chatPeer(state);
  input.disabled = !can;
  $('chat-send').disabled = !can;
  if (state.chat.length === 0) {
    log.replaceChildren(h('div', { class: 'chat-empty' },
      can ? 'Nenhuma mensagem ainda.' : 'O chat abre quando um amigo estiver conectado.'));
    return;
  }
  const name = chatPeer(state) || 'Amigo';
  log.replaceChildren(...state.chat.map((e) => h('div', { class: `msg${e.from_self ? ' self' : ''}` },
    h('div', { class: 'msg-head' }, h('b', {}, e.from_self ? 'Você' : name), h('time', {}, formatClock(e.sent_at))),
    h('span', { class: 'msg-text' }, e.text))));
  if (atBottom) log.scrollTop = log.scrollHeight;
}

function renderNav() {
  for (const el of document.querySelectorAll('[data-view]')) {
    if (el.classList.contains('nav-item')) {
      if (el.dataset.view === state.view) el.setAttribute('aria-current', 'page');
      else el.removeAttribute('aria-current');
    }
  }
  $('view-call').hidden = state.view !== 'call';
  $('chat').hidden = state.view !== 'call';
  $('view-settings').hidden = state.view !== 'settings';
}

function renderSplash() {
  $('splash').hidden = !(state.link === 'connecting');
}

// Re-renderiza e devolve o foco ao elemento que o tinha (a lista é recriada).
function render() {
  const active = document.activeElement;
  const key = active && active.dataset ? active.dataset.key : null;
  renderSplash();
  renderNav();
  renderFriends();
  renderMe();
  renderBanners();
  renderHeader();
  renderStage();
  renderControls();
  renderChat();
  renderSettings();
  if (key) {
    const again = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (again && again !== active) again.focus();
  }
}

function setView(v) {
  state.view = v;
  renderNav();
  syncMicMeter();
}

let toastTimer = null;
function toast(message) {
  const el = $('toast');
  el.textContent = message;
  el.classList.add('error');
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, 6000);
}

// ---------- Ligação com o núcleo ----------

function requestAll() {
  send({ type: 'GetState' });
  send({ type: 'GetConfig' });
  send({ type: 'GetChatHistory' });
}

function onLink(link) {
  const was = state.link;
  setLink(state, link === 'connected' ? 'connected' : 'disconnected');
  syncMicMeter();
  if (state.link === 'connected' && was !== 'connected') requestAll();
  render();
}

let lastRoom = null;
let lastChatPeer = null;
function onCore(msg) {
  const what = reduce(state, msg);
  if (what === 'error') {
    toast(`Núcleo: ${msg.message}`);
    return;
  }
  if (what === 'none') return;
  if (what === 'stats') {
    renderHeader();
    return;
  }
  if (what === 'mic') {
    if (state.view === 'settings') renderMicMeter();
    return;
  }
  if (what === 'chat') {
    renderChat();
    return;
  }
  render();
  // O chat é por chamada: pedir de novo quando a sala ou o amigo do chat mudam.
  const room = inRoom(state);
  const cp = chatPeer(state);
  if (room !== lastRoom || cp !== lastChatPeer) {
    lastRoom = room;
    lastChatPeer = cp;
    send({ type: 'GetChatHistory' });
  }
}

// ---------- Eventos da interface ----------

$('btn-mute').addEventListener('click', () => send({ type: 'SetMuted', muted: !(state.status && state.status.muted) }));
$('btn-deaf').addEventListener('click', () => send({ type: 'SetDeafened', deafened: !(state.status && state.status.deafened) }));
const enterGame = () => send({ type: 'EnterGameMode' });
$('btn-game').addEventListener('click', enterGame);
$('game-top').addEventListener('click', enterGame);
$('btn-room').addEventListener('click', () => {
  if (inRoom(state)) {
    send({ type: 'HangUp' });
  } else {
    const friend = callTarget(state);
    if (friend) send({ type: 'Call', friend });
  }
});
$('chat-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const input = $('chat-input');
  const text = input.value.trim();
  if (!text || input.disabled) return;
  send({ type: 'SendChat', text });
  input.value = '';
});
for (const el of document.querySelectorAll('[data-view]')) {
  el.addEventListener('click', () => setView(el.dataset.view));
}
$('add-friend').addEventListener('click', () => {
  setView('settings');
  focusAddFriend();
});

setInterval(() => { if (!document.hidden) tickTimer(); }, 1000);

async function start() {
  render();
  const appWindow = tauri.window.getCurrentWindow();
  await appWindow.onCloseRequested(async (event) => {
    if (!micMeterOn) return;
    event.preventDefault();
    await setMicMeter(false);
    await appWindow.close();
  });
  await tauri.event.listen('core', (e) => onCore(e.payload));
  await tauri.event.listen('link', (e) => onLink(e.payload));
  // O evento `link` pode ter saído antes de registrarmos o ouvinte.
  const now = await tauri.core.invoke('link_state');
  if (now === 'connected') onLink('connected');
}

start();
