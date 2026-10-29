// Tela Principal. A interface só mostra o estado do núcleo e envia comandos (D-010).
// Chat nunca é persistido: fica só em `state.chat`, na memória (inv-chat-never-on-disk).

import {
  initialState, setLink, reduce, friends, inRoom, callPeers, chatPeer, roomAction,
  friendStatus, friendPresence, avatarIndex, micText, formatDuration, callSeconds, formatClock, initial,
  sharingPeers, shareConsumeError, encoderLabel,
  SHARE_VOLUME_KEY, loadShareVolume, writePref, clampVolumePct, shareVolumeMessage, shareAudioNote, makeThrottle,
} from './state.js';
import { initSettings } from './settings.js';
import { initFriends } from './friends.js';
import { initShare } from './share.js';

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

const { renderSettings, renderMicMeter } = initSettings({ state, send, h, icon });
const { renderFriendsView, requestTailnet } = initFriends({ state, send, h, icon });
const { openShare, renderShare } = initShare({ state, send, h, icon, linked });
let micMeterOn = false;

function setMicMeter(on) {
  if (micMeterOn === on) return Promise.resolve();
  micMeterOn = on;
  return send({ type: 'SetMicMeter', on });
}

function requestAudioDevices() {
  if (linked()) send({ type: 'ListAudioDevices' });
}

function syncMicMeter() {
  return setMicMeter(linked() && state.view === 'settings');
}

// "Mutado" para a interface inclui silenciado (silenciar também muta).
function selfFlags() {
  const s = state.status;
  return { muted: !!(s && (s.muted || s.deafened)), deafened: !!(s && s.deafened) };
}

function toggleMute() {
  const s = state.status;
  if (!s) return;
  if (s.deafened) {
    // Desmutar-se enquanto silenciado volta a ouvir.
    send({ type: 'SetDeafened', deafened: false });
    if (s.muted) send({ type: 'SetMuted', muted: false });
  } else {
    send({ type: 'SetMuted', muted: !s.muted });
  }
}

function toggleDeafen() {
  send({ type: 'SetDeafened', deafened: !(state.status && state.status.deafened) });
}

const muteLabel = (muted) => (muted ? 'Desmutar‑se' : 'Mutar-se'); // hífen sem quebra de linha
const deafLabel = (deaf) => (deaf ? 'Voltar a ouvir' : 'Silenciar');

function joinNames(names) {
  return names.length <= 1 ? names.join('') : `${names.slice(0, -1).join(', ')} e ${names[names.length - 1]}`;
}

function toneClass(tone) {
  return tone === 'accent' ? 'live' : tone === 'muted' ? '' : tone;
}

// ---------- Renderização ----------

function renderFriends() {
  const list = friends(state);
  const out = [];
  if (list.length === 0) {
    out.push(h('p', { class: 'empty-note' }, 'Nenhum amigo ainda.'));
  }
  for (const f of list) {
    const pres = friendPresence(state, f.name);
    const on = pres.key !== 'off';
    const cls = pres.danger ? 'danger' : pres.key === 'sala' ? 'live' : pres.key === 'online' ? '' : 'off';
    out.push(h('div', { class: `friend${on ? '' : ' off'}`, role: 'listitem' },
      h('div', { class: `avatar av-${avatarIndex(f.name)}${on ? '' : ' off'}`, 'aria-hidden': 'true' },
        initial(f.name), h('span', { class: `badge${on ? ' on' : ''}` })),
      h('div', { class: 'friend-text' },
        h('span', { class: 'friend-name' }, f.name),
        h('span', { class: `friend-status ${cls}` }, pres.text)),
      pres.ringing && h('button', {
        type: 'button', class: 'btn-text', 'data-key': `cancel:${f.name}`,
        disabled: !linked(), onclick: () => send({ type: 'HangUp' }),
      }, 'Cancelar'),
      pres.canCall && h('button', {
        type: 'button', class: 'btn-icon', 'data-key': `call:${f.name}`,
        'aria-label': `Chamar ${f.name} para a sala`, title: `Chamar ${f.name} para a sala`,
        disabled: !linked(),
        onclick: () => send({ type: 'Call', friend: f.name }),
      }, icon('i-call', 18))));
  }
  $('friends').replaceChildren(...out);
}

function renderMe() {
  const name = (state.config && state.config.name) || 'Você';
  const s = state.status;
  const ok = linked();
  const { muted, deafened } = selfFlags();
  const off = !ok || !s;
  let status = 'Microfone ligado';
  if (!ok) status = 'Sem conexão';
  else if (!s) status = 'Conectando…';
  else if (deafened) status = 'Silenciado';
  else if (muted) status = 'Mutado';
  const muteText = muteLabel(muted).replace('‑', '-');
  const deafText = deafLabel(deafened);
  $('me').replaceChildren(
    h('div', { class: 'avatar self', 'aria-hidden': 'true' }, initial(name)),
    h('div', { class: 'me-text' },
      h('span', { class: 'me-name' }, name),
      h('span', { class: `me-status${!ok || muted ? ' danger' : ''}` }, status)),
    h('button', {
      type: 'button', class: `btn-icon${muted ? ' on' : ''}`, 'data-key': 'me-mute',
      'aria-label': muteText, title: muteText, disabled: off, onclick: toggleMute,
    }, icon(muted ? 'i-mic-off' : 'i-mic', 18)),
    h('button', {
      type: 'button', class: `btn-icon${deafened ? ' on' : ''}`, 'data-key': 'me-deaf',
      'aria-label': deafText, title: deafText, disabled: off, onclick: toggleDeafen,
    }, icon(deafened ? 'i-headphones-off' : 'i-headphones', 18)));
}

function noticeBox(title, text) {
  return h('div', { class: 'notice' }, icon('i-alert', 16),
    h('span', {}, h('strong', {}, title), ' ', h('span', { class: 'muted' }, text)));
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
        h('span', {}, 'A conexão foi recusada por segurança. Se ele reinstalou o app, redefina a identidade na aba Amigos.')),
      h('button', { type: 'button', class: 'btn', onclick: () => setView('friends') }, 'Abrir Amigos')));
  }
  for (const name of Object.keys(state.outsideTailnet)) {
    if (state.peers[name] && state.peers[name].connected) continue;
    out.push(noticeBox(`${name} está fora da sua rede Tailscale.`,
      'Contas diferentes do Tailscale não se enxergam. Convide a pessoa para a sua rede ou compartilhem as máquinas um com o outro no painel do Tailscale.'));
  }
  // Chamadas recebidas: fixas no topo, em qualquer aba (D-024).
  const rings = state.calls.incoming.map((name) => h('div', { class: 'banner', role: 'alert' },
    h('div', { class: 'banner-icon' }, icon('i-call', 20)),
    h('div', { class: 'banner-text' }, h('strong', {}, `${name} está te chamando`)),
    h('button', {
      type: 'button', class: 'btn primary', 'data-key': `ring-accept:${name}`, disabled: !linked(),
      onclick: () => send({ type: 'AcceptCall', friend: name }),
    }, 'Entrar'),
    h('button', {
      type: 'button', class: 'btn', 'data-key': `ring-decline:${name}`, disabled: !linked(),
      onclick: () => send({ type: 'DeclineCall', friend: name }),
    }, 'Recusar')));
  $('banners').replaceChildren(...out);
  $('ring-banners').replaceChildren(...rings);
}

function micIcon(off) {
  return icon(off ? 'i-mic-off' : 'i-mic', 14);
}

function statusCard(name, p) {
  const st = friendStatus(state, name);
  const mic = p.status ? micText(p.status) : { text: st.text, tone: st.tone };
  const vol = state.volumes[name] ?? 100;
  const slider = h('input', {
    type: 'range', min: '0', max: '200', step: '1', value: String(vol),
    'aria-label': `Volume de ${name}`, 'aria-valuetext': `${vol}%`, 'data-key': `vol:${name}`,
    disabled: !linked(),
    oninput: (e) => {
      const v = Number(e.target.value);
      state.volumes[name] = v;
      e.target.setAttribute('aria-valuetext', `${v}%`);
      send({ type: 'SetPeerVolume', friend: name, volume: v / 100 });
    },
  });
  const micOff = !!(p.status && (p.status.muted || p.status.deafened));
  return h('div', { class: 'tile' },
    h('div', { class: 'tile-top' }, h('span', { class: 'tile-meta', 'data-rtt': name })),
    h('div', { class: 'tile-center' },
      h('div', { class: `avatar xl av-${avatarIndex(name)}`, 'aria-hidden': 'true' }, initial(name)),
      h('div', { class: 'who' },
        h('strong', {}, name),
        h('span', { class: `tile-status ${toneClass(mic.tone)}` }, p.status && micIcon(micOff), mic.text))),
    h('label', { class: 'tile-vol' }, 'Volume', slider));
}

function selfCard() {
  const s = state.status;
  const { muted, deafened } = selfFlags();
  const mic = s ? (deafened ? { text: 'Silenciado', tone: 'danger' } : micText(s)) : { text: '', tone: 'muted' };
  const a = state.config && state.config.audio;
  const detail = a
    ? `${a.mode === 'push_to_talk' ? 'Aperte para falar' : 'Ativação por voz'} · ${a.bitrate_kbps} kbps`
    : '';
  const name = (state.config && state.config.name) || 'Você';
  return h('div', { class: 'tile' },
    h('div', { class: 'tile-top' }),
    h('div', { class: 'tile-center' },
      h('div', { class: 'avatar xl self', 'aria-hidden': 'true' }, initial(name)),
      h('div', { class: 'who' },
        h('strong', {}, 'Você'),
        h('span', { class: `tile-status ${toneClass(mic.tone)}` }, s && micIcon(muted), mic.text))),
    h('div', { class: 'tile-self' }, detail));
}

function emptyState(title, text, ...actions) {
  return h('div', { class: 'empty' },
    h('div', { class: 'state-icon', 'aria-hidden': 'true' }, icon('i-users', 22)),
    h('h2', { class: 'empty-title' }, title),
    h('p', { class: 'empty-text' }, text),
    ...actions);
}

function roomActionLabel(act) {
  return act.kind === 'call' ? 'Chamar' : 'Entrar na sala';
}

// Fora da sala: entra direto se alguém já está lá; senão chama os amigos online.
function enterOrCall() {
  const act = roomAction(state);
  if (act.kind === 'call') {
    for (const friend of act.friends) send({ type: 'Call', friend });
  } else {
    send({ type: 'JoinRoom' });
  }
}

function renderStage() {
  const cards = [];
  const list = friends(state);
  const peers = callPeers(state);
  if (list.length === 0) {
    cards.push(emptyState('Nenhum amigo configurado',
      'Adicione um amigo pelo nome e pelo IP do Tailscale para chamar. Abra a aba Amigos para começar.',
      h('button', { type: 'button', class: 'btn primary', onclick: () => setView('friends') }, 'Adicionar amigo')));
  } else if (!state.status) {
    // ainda sem Status do núcleo: nada a mostrar
  } else if (!inRoom(state)) {
    const act = roomAction(state);
    cards.push(emptyState('Você não está na sala',
      act.kind === 'call'
        ? 'Ninguém está na sala. Chame seus amigos: o toque dura 1 minuto e você fica na sala esperando.'
        : 'Ninguém ouve você e você não ouve ninguém. Entre na sala para conversar.',
      h('button', {
        type: 'button', class: 'btn primary', 'data-key': 'join-room', disabled: !linked(),
        onclick: enterOrCall,
      }, roomActionLabel(act))));
  } else if (peers.length === 0) {
    const ringing = state.calls.outgoing;
    cards.push(emptyState(ringing.length ? `Chamando ${joinNames(ringing)}` : 'Sala aberta',
      ringing.length ? 'Aguardando a resposta.' : 'Aguardando amigos entrarem na sala.',
      ringing.length ? h('span', { class: 'pill live' }, 'Chamando…') : null,
      ringing.length ? h('button', {
        type: 'button', class: 'btn-text', 'data-key': 'cancel-call', disabled: !linked(),
        onclick: () => send({ type: 'HangUp' }),
      }, 'Cancelar') : null));
    cards.push(selfCard());
  } else {
    for (const n of peers) cards.push(statusCard(n, state.peers[n]));
    cards.push(selfCard());
  }
  $('stage').replaceChildren(...cards);
  updateTileMeta();

  renderShareBanners();
}

// Volume da transmissão do amigo (áudio que EU recebo): lembrado e reenviado quando o banner aparece.
function localStore() {
  try { return window.localStorage; } catch { return null; }
}
let shareVolumePct = loadShareVolume(localStore());
let shareVolumeSent = false;
const sendShareVolume = makeThrottle((pct) => send(shareVolumeMessage(pct)), 100);

// Banners de transmissão: a nossa ("você ao vivo") e a dos amigos. Nada abre sozinho.
function renderShareBanners() {
  const out = [];
  if (state.share.sharing) {
    const s = state.share;
    const enc = state.shareSources && encoderLabel(state.shareSources.encoder);
    const audioNote = shareAudioNote(s);
    const detail = [s.source, `${s.resolution}p · ${s.fps} fps`, enc, audioNote === 'com áudio' ? audioNote : null]
      .filter(Boolean).join(' · ');
    out.push(h('div', { class: 'banner live' },
      h('div', { class: 'banner-icon' }, h('span', { class: 'live-dot', 'aria-hidden': 'true' })),
      h('div', { class: 'banner-text' },
        h('strong', {}, 'Você está transmitindo'),
        h('span', { class: 'mono' }, detail),
        audioNote === 'Sem áudio do PC' && h('span', { class: 'share-note', role: 'status' }, audioNote)),
      h('button', {
        type: 'button', class: 'btn danger', 'data-key': 'share-stop', disabled: !linked(),
        onclick: () => send({ type: 'StopShare' }),
      }, 'Parar')));
  }
  const watched = sharingPeers(state);
  if (watched.length === 0) shareVolumeSent = false;
  else if (!shareVolumeSent && linked()) {
    shareVolumeSent = true;
    send(shareVolumeMessage(shareVolumePct));
  }
  for (const name of watched) {
    out.push(h('div', { class: 'banner' },
      h('div', { class: 'banner-icon' }, icon('i-screen', 20)),
      h('div', { class: 'banner-text' }, h('strong', {}, `${name} está transmitindo a tela`)),
      h('label', { class: 'share-vol' },
        h('span', {}, 'Volume da tela'),
        h('input', {
          type: 'range', class: 'range', min: '0', max: '100', step: '1', value: String(shareVolumePct),
          'data-key': `share-vol:${name}`, 'aria-label': 'Volume da tela', disabled: !linked(),
          oninput: (e) => {
            shareVolumePct = clampVolumePct(e.target.value);
            writePref(localStore(), SHARE_VOLUME_KEY, shareVolumePct);
            sendShareVolume(shareVolumePct);
          },
        })),
      h('button', {
        type: 'button', class: 'btn primary', 'data-key': `share-watch:${name}`, disabled: !linked(),
        onclick: () => send({ type: 'OpenViewer' }),
      }, 'Assistir')));
  }
  $('share-banner').replaceChildren(...out);
}

function renderShareControls() {
  const live = state.share.sharing;
  $('nav-share-live').hidden = !live;
  const btn = $('btn-share');
  btn.classList.toggle('tint', live);
  btn.setAttribute('aria-pressed', String(live));
  btn.disabled = !linked() || !state.status;
  $('nav-share').disabled = !linked();
}

// Item da lateral e botão "Tela": parados abrem o diálogo; ao vivo, o botão para a transmissão.
function onShareNav() {
  if (state.share.sharing) setView('call');
  else openShare();
}

function onShareButton() {
  if (state.share.sharing) send({ type: 'StopShare' });
  else openShare();
}

function updateTileMeta() {
  for (const el of document.querySelectorAll('[data-rtt]')) {
    const p = state.peers[el.dataset.rtt];
    const text = p && p.stats ? `${p.stats.rtt_ms} ms` : '';
    if (el.textContent !== text) el.textContent = text;
  }
}

function renderHeader() {
  const peers = callPeers(state);
  let title = 'Sala';
  if (state.status) {
    if (!inRoom(state)) title = 'Fora da sala';
    else if (peers.length) title = `Chamada com ${joinNames(peers)}`;
    else title = 'Sala aberta';
  }
  $('title').textContent = title;

  const first = peers.find((n) => state.peers[n].stats);
  const timer = $('timer');
  const count = $('room-count');
  timer.hidden = !first;
  const showCount = !!state.status && inRoom(state);
  count.hidden = !showCount;
  if (showCount) count.textContent = `${peers.length + 1} na sala`;
  $('meta-sep').hidden = timer.hidden || count.hidden;
  tickTimer();
  updateTileMeta();

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
  const { muted, deafened } = selfFlags();
  const btnMute = $('btn-mute');
  btnMute.classList.toggle('on', muted);
  btnMute.disabled = off;
  $('lbl-mute').textContent = muteLabel(muted);
  $('use-mute').setAttribute('href', muted ? '#i-mic-off' : '#i-mic');
  const btnDeaf = $('btn-deaf');
  btnDeaf.classList.toggle('on', deafened);
  btnDeaf.disabled = off;
  $('lbl-deaf').textContent = deafLabel(deafened);
  $('use-deaf').setAttribute('href', deafened ? '#i-headphones-off' : '#i-headphones');
  $('game-top').disabled = off;

  const room = $('btn-room');
  const inside = inRoom(state);
  room.classList.toggle('danger', inside);
  room.classList.toggle('enter', !inside);
  $('lbl-room').textContent = inside ? 'Desligar' : roomActionLabel(roomAction(state));
  $('use-room').setAttribute('href', inside ? '#i-hangup' : '#i-call');
  room.disabled = off;
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
  log.replaceChildren(...state.chat.map((e) => e.system ? h('div', { class: 'msg system' },
    h('span', { class: 'msg-text' }, e.text)) : h('div', { class: 'msg' },
    h('div', { class: 'msg-head' },
      h('b', { class: `fg-${e.from_self ? 2 : avatarIndex(name)}` }, e.from_self ? 'Você' : name),
      h('time', {}, formatClock(e.sent_at))),
    h('span', { class: 'msg-text' }, e.text))));
  if (atBottom) log.scrollTop = log.scrollHeight;
}

function renderNav() {
  for (const el of document.querySelectorAll('.nav-item[data-view]')) {
    if (el.dataset.view === state.view) el.setAttribute('aria-current', 'page');
    else el.removeAttribute('aria-current');
  }
  $('view-call').hidden = state.view !== 'call';
  $('chat').hidden = state.view !== 'call';
  $('view-friends').hidden = state.view !== 'friends';
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
  renderShareControls();
  renderChat();
  renderShare();
  renderFriendsView();
  renderSettings();
  if (key) {
    const again = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (again && again !== active) again.focus();
  }
}

function setView(v) {
  const entering = v !== state.view;
  state.view = v;
  renderNav();
  syncMicMeter();
  if (!entering) return;
  if (v === 'settings') requestAudioDevices();
  if (v === 'friends') requestTailnet();
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
  if (state.link === 'connected' && was !== 'connected') {
    requestAll();
    sendFocus();
  }
  if (state.link === 'connected' && state.view === 'settings') requestAudioDevices();
  render();
}

let lastRoom = null;
let lastChatPeer = null;
function onCore(msg) {
  const what = reduce(state, msg);
  if (what === 'error') {
    if (shareConsumeError(state, msg.message)) renderShare();
    else toast(`Núcleo: ${msg.message}`);
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

$('btn-mute').addEventListener('click', toggleMute);
$('btn-deaf').addEventListener('click', toggleDeafen);
$('game-top').addEventListener('click', () => send({ type: 'EnterGameMode' }));
$('nav-share').addEventListener('click', onShareNav);
$('btn-share').addEventListener('click', onShareButton);
$('btn-room').addEventListener('click', () => {
  if (inRoom(state)) {
    send({ type: 'HangUp' });
  } else {
    enterOrCall();
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
for (const el of document.querySelectorAll('.nav-item[data-view]')) {
  el.addEventListener('click', () => setView(el.dataset.view));
}
$('add-friend').addEventListener('click', () => setView('friends'));

// O núcleo toca o som de mensagem só quando a janela não está em foco (D-024).
function sendFocus() {
  if (linked()) send({ type: 'SetUiFocus', focused: document.hasFocus() });
}

window.addEventListener('focus', () => {
  sendFocus();
  if (state.view === 'settings') requestAudioDevices();
});
window.addEventListener('blur', sendFocus);

setInterval(() => { if (!document.hidden) tickTimer(); }, 1000);

async function start() {
  render();
  await tauri.event.listen('core', (e) => onCore(e.payload));
  await tauri.event.listen('link', (e) => onLink(e.payload));
  // O evento `link` pode ter saído antes de registrarmos o ouvinte.
  const now = await tauri.core.invoke('link_state');
  if (now === 'connected') onLink('connected');
}

start();
