// Estado da interface e regras de derivação. Sem DOM, sem lógica de negócio:
// só espelha o que o núcleo enviou (D-010).

export function initialState() {
  return {
    // 'connecting' (nunca conectou) | 'connected' | 'disconnected'
    link: 'connecting',
    everConnected: false,
    status: null, // { muted, deafened, game_mode, in_room }
    config: null, // CoreToUi::Config
    audio_devices: { inputs: ['default'], outputs: ['default'] },
    mic_level_db: -120, // CoreToUi::MicLevel
    peers: {}, // nome -> { connected, reconnecting, status, stats }
    mismatch: {}, // nome -> true (FingerprintMismatch)
    outsideTailnet: {}, // nome -> true (PeerNotInTailnet; some ao conectar)
    tailnet: null, // null (nunca pediu) | { loading: true } | { running, peers } (TailnetPeers)
    chat: [],
    calls: { incoming: [], outgoing: [] }, // CoreToUi::Calls (D-024)
    systemSeq: 0,
    volumes: {}, // nome -> 0..200 (só da sessão da interface)
    view: 'call',
    share: initialShare(), // CoreToUi::ShareState (a nossa transmissão)
    shareSources: null, // null (nunca chegou) | { encoder, monitors, windows } (ShareSources)
    shareDialog: initialShareDialog(), // só da interface: diálogo Compartilhar
  };
}

function initialShare() {
  // audio: o núcleo abriu a captura do áudio do PC; audioAsked: o usuário pediu.
  return { sharing: false, resolution: 0, fps: 0, source: null, audio: false, audioAsked: false };
}

function initialShareDialog() {
  return { open: false, tab: 'telas', sourceId: null, res: 1080, fps: 60, audio: true, pending: false, pendingName: null, pendingAudio: false, error: null };
}

// Preferências locais (só conveniência): ficam em localStorage, que pode faltar ou lançar.
export const SHARE_AUDIO_KEY = 'concord.share.audio';
export const SHARE_VOLUME_KEY = 'concord.share.volume';

export function readPref(storage, key) {
  try {
    const v = storage && storage.getItem(key);
    return v === undefined ? null : v;
  } catch {
    return null;
  }
}

export function writePref(storage, key, value) {
  try {
    if (storage) storage.setItem(key, String(value));
  } catch { /* sem armazenamento: segue sem lembrar */ }
}

// Marcado por padrão; só "0" desmarca.
export function loadShareAudio(storage) {
  return readPref(storage, SHARE_AUDIO_KEY) !== '0';
}

// Volume da tela do amigo, inteiro 0–100 (padrão 100).
export function clampVolumePct(v) {
  const n = Math.round(Number(v));
  return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : 100;
}

export function loadShareVolume(storage) {
  const raw = readPref(storage, SHARE_VOLUME_KEY);
  return raw === null ? 100 : clampVolumePct(raw);
}

// Mensagem do SetShareAudioVolume (volume 0..1).
export function shareVolumeMessage(pct) {
  return { type: 'SetShareAudioVolume', volume: clampVolumePct(pct) / 100 };
}

// Complemento do banner "você ao vivo": '' | 'com áudio' | 'Sem áudio do PC'.
export function shareAudioNote(share) {
  if (share.audio) return 'com áudio';
  return share.audioAsked ? 'Sem áudio do PC' : '';
}

// Throttle com borda final: envia já se passou `ms`; senão agenda o último valor.
export function makeThrottle(fn, ms, now = () => Date.now(), timers = { set: setTimeout, clear: clearTimeout }) {
  let last = -Infinity;
  let timer = null;
  let pendingArg;
  return (arg) => {
    const t = now();
    if (t - last >= ms) {
      if (timer !== null) { timers.clear(timer); timer = null; }
      last = t;
      fn(arg);
      return;
    }
    pendingArg = arg;
    if (timer === null) {
      timer = timers.set(() => {
        timer = null;
        last = now();
        fn(pendingArg);
      }, ms - (t - last));
    }
  };
}

function peer(state, name) {
  if (!state.peers[name]) {
    state.peers[name] = { connected: false, reconnecting: null, status: null, stats: null };
  }
  return state.peers[name];
}

export function setLink(state, link) {
  state.link = link;
  if (link === 'connected') {
    state.everConnected = true;
    // O núcleo reenvia tudo em GetState; não confiar no que havia antes da queda.
    state.peers = {};
    state.mismatch = {};
    state.calls = { incoming: [], outgoing: [] };
    state.share = initialShare();
    state.shareDialog.pending = false;
  }
}

// Texto do resultado de uma chamada que tocou (CallResult), ou null se não se mostra.
export function callResultText(friend, result) {
  switch (result) {
    case 'declined': return `${friend} recusou a chamada`;
    case 'timeout': return `${friend} não atendeu`;
    case 'cancelled': return `${friend} desistiu da chamada`;
    case 'unsupported': return `${friend} usa uma versão antiga do Concord e não recebe chamadas`;
    default: return null;
  }
}

// Aplica uma mensagem CoreToUi. Devolve o que mudou: 'full' | 'stats' | 'chat' | 'error' | 'none'.
// Variantes desconhecidas são ignoradas (D-012).
export function reduce(state, msg) {
  switch (msg && msg.type) {
    case 'Status':
      state.status = {
        muted: !!msg.muted,
        deafened: !!msg.deafened,
        game_mode: !!msg.game_mode,
        in_room: msg.in_room !== false,
      };
      return 'full';
    case 'Config':
      state.config = msg;
      return 'full';
    case 'AudioDevices':
      state.audio_devices = {
        inputs: Array.isArray(msg.inputs) ? msg.inputs : ['default'],
        outputs: Array.isArray(msg.outputs) ? msg.outputs : ['default'],
      };
      return 'full';
    case 'MicLevel':
      state.mic_level_db = Number.isFinite(msg.db) ? msg.db : -120;
      return 'mic';
    case 'PeerConnected': {
      const p = peer(state, msg.friend);
      p.connected = true;
      p.reconnecting = null;
      delete state.mismatch[msg.friend];
      delete state.outsideTailnet[msg.friend];
      return 'full';
    }
    case 'TailnetPeers':
      state.tailnet = { running: !!msg.running, peers: Array.isArray(msg.peers) ? msg.peers : [] };
      return 'full';
    case 'PeerNotInTailnet':
      state.outsideTailnet[msg.friend] = true;
      return 'full';
    case 'PeerDisconnected': {
      const p = peer(state, msg.friend);
      p.connected = false;
      p.reconnecting = null;
      p.status = null;
      p.stats = null;
      return 'full';
    }
    case 'PeerReconnecting': {
      const p = peer(state, msg.friend);
      p.connected = false;
      p.reconnecting = msg.attempt;
      p.stats = null;
      return 'full';
    }
    case 'PeerStatus': {
      const p = peer(state, msg.friend);
      p.connected = true;
      p.reconnecting = null;
      p.status = {
        muted: !!msg.muted,
        deafened: !!msg.deafened,
        game_mode: !!msg.game_mode,
        sharing: !!msg.sharing,
        in_room: msg.in_room !== false,
      };
      return 'full';
    }
    case 'CallStats': {
      const p = peer(state, msg.friend);
      p.stats = { rtt_ms: msg.rtt_ms, since_secs: msg.since_secs, at: Date.now() };
      return 'stats';
    }
    case 'FingerprintMismatch':
      state.mismatch[msg.friend] = true;
      return 'full';
    case 'Calls':
      state.calls = {
        incoming: Array.isArray(msg.incoming) ? msg.incoming.slice() : [],
        outgoing: Array.isArray(msg.outgoing) ? msg.outgoing.slice() : [],
      };
      return 'full';
    case 'CallResult': {
      const text = callResultText(msg.friend, msg.result);
      if (!text) return 'none';
      // Mensagem de sistema no chat (só em memória, como o resto do chat).
      state.systemSeq += 1;
      state.chat.push({ id: `sys-${state.systemSeq}`, system: true, from_self: false, text, sent_at: Date.now() });
      return 'chat';
    }
    case 'ShareSources': {
      const list = (v) => (Array.isArray(v) ? v.filter((s) => s && typeof s.id === 'string') : []);
      state.shareSources = {
        encoder: typeof msg.encoder === 'string' && msg.encoder ? msg.encoder : null,
        monitors: list(msg.monitors),
        windows: list(msg.windows),
      };
      state.shareDialog.error = null;
      return 'full';
    }
    case 'ShareState': {
      const d = state.shareDialog;
      state.share = {
        sharing: !!msg.sharing,
        resolution: Number(msg.resolution) || 0,
        fps: Number(msg.fps) || 0,
        source: msg.sharing ? (d.pendingName || state.share.source) : null,
        audio: !!msg.sharing && msg.audio === true,
        audioAsked: msg.sharing ? (d.pendingName ? d.pendingAudio : state.share.audioAsked) : false,
      };
      d.pending = false;
      d.pendingName = null;
      d.pendingAudio = false;
      if (msg.sharing) d.open = false; // começou: o diálogo cumpriu o papel
      return 'full';
    }
    case 'ChatHistory':
      state.chat =(Array.isArray(msg.entries) ? msg.entries.slice() : [])
        .concat(state.chat.filter((e) => e.system));
      return 'chat';
    case 'ChatReceived':
      if (msg.entry && !state.chat.some((e) => e.id === msg.entry.id && e.from_self === msg.entry.from_self)) {
        state.chat.push(msg.entry);
      }
      return 'chat';
    case 'Error':
      return 'error';
    default:
      return 'none';
  }
}

export function friends(state) {
  return (state.config && state.config.friends) || [];
}

export function inRoom(state) {
  return !!(state.status && state.status.in_room);
}

// Amigos conectados que também estão na sala (e a nossa sala está aberta para a chamada).
export function callPeers(state) {
  if (!inRoom(state)) return [];
  return friends(state)
    .map((f) => f.name)
    .filter((n) => {
      const p = state.peers[n];
      return p && p.connected && p.status && p.status.in_room;
    });
}

// O núcleo manda o chat para o primeiro amigo conectado (`current_friend`).
export function chatPeer(state) {
  return friends(state)
    .map((f) => f.name)
    .find((n) => state.peers[n] && state.peers[n].connected) || null;
}

// Botão principal fora da sala: se algum amigo já está na sala, só entra; senão chama
// todos os amigos online (sem ninguém online, entra sozinho). { kind: 'join'|'call', friends }.
export function roomAction(state) {
  const online = friends(state)
    .map((f) => f.name)
    .filter((n) => state.peers[n] && state.peers[n].connected && !state.mismatch[n]);
  if (online.some((n) => state.peers[n].status && state.peers[n].status.in_room)) {
    return { kind: 'join', friends: [] };
  }
  return online.length ? { kind: 'call', friends: online } : { kind: 'join', friends: [] };
}

export function isRinging(state, name) {
  return state.calls.outgoing.includes(name);
}

// { text, tone: 'accent'|'game'|'danger'|'muted', dot: 'on'|'game'|'danger'|'off', online }
export function friendStatus(state, name) {
  const p = state.peers[name];
  if (state.mismatch[name]) return { text: 'Identidade diferente', tone: 'danger', dot: 'danger', online: false };
  if (state.outsideTailnet[name] && !(p && p.connected)) {
    return { text: 'Fora da sua rede Tailscale', tone: 'danger', dot: 'danger', online: false };
  }
  if (p && p.reconnecting != null) return { text: 'Reconectando…', tone: 'muted', dot: 'off', online: false };
  if (!p || !p.connected) return { text: 'Offline', tone: 'muted', dot: 'off', online: false };
  const s = p.status;
  if (!s) return { text: 'Conectado', tone: 'muted', dot: 'on', online: true };
  if (s.game_mode) return { text: 'Em modo jogo', tone: 'game', dot: 'game', online: true };
  if (!s.in_room) return { text: 'Fora da sala', tone: 'muted', dot: 'off', online: true };
  return { text: 'Na sala', tone: 'accent', dot: 'on', online: true };
}

// Texto do estado de microfone de alguém (D-013: "mutado"/"silenciado").
export function micText(s) {
  if (!s) return { text: '', tone: 'muted' };
  if (s.deafened) return { text: 'Silenciado', tone: 'danger' };
  if (s.muted) return { text: 'Microfone mutado', tone: 'danger' };
  return { text: 'Microfone ligado', tone: 'muted' };
}

export function formatDuration(totalSecs) {
  const s = Math.max(0, Math.floor(totalSecs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const two = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${two(m)}:${two(sec)}` : `${two(m)}:${two(sec)}`;
}

// Segundos de chamada agora, contando desde o último CallStats.
export function callSeconds(stats, now = Date.now()) {
  if (!stats) return 0;
  return stats.since_secs + Math.max(0, (now - stats.at) / 1000);
}

export function formatClock(sentAt) {
  // `sent_at` vem em milissegundos desde a época; tolera segundos.
  const ms = sentAt < 1e11 ? sentAt * 1000 : sentAt;
  const d = new Date(ms);
  return d.toLocaleTimeString('pt-BR', { hour: '2-digit', minute: '2-digit' });
}

// Cor estável do avatar (1..5) a partir do nome; o fingerprint não chega à interface.
export function avatarIndex(name) {
  let hash = 0;
  for (const ch of String(name || '')) hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
  return (hash % 5) + 1;
}

// Presença na lateral e em Amigos: { key: 'sala'|'online'|'off', text, danger, canCall }.
export function friendPresence(state, name) {
  const p = state.peers[name];
  if (state.mismatch[name]) return { key: 'off', text: 'Identidade diferente', danger: true, canCall: false };
  if (p && p.reconnecting != null) return { key: 'off', text: 'Reconectando…', danger: false, canCall: false };
  if (!p || !p.connected) return { key: 'off', text: 'Offline', danger: false, canCall: false };
  if (isRinging(state, name)) return { key: 'online', text: 'Chamando…', danger: false, canCall: false, ringing: true };
  if (p.status && p.status.in_room) return { key: 'sala', text: 'Na sala', danger: false, canCall: false };
  return { key: 'online', text: 'Online', danger: false, canCall: true };
}

// Validação da faixa do Tailscale (100.64.0.0/10). Devolve true/false.
export function isTailscaleIp(ip) {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(String(ip).trim());
  if (!m) return false;
  const o = m.slice(1).map(Number);
  if (o.some((n) => n > 255)) return false;
  return o[0] === 100 && o[1] >= 64 && o[1] <= 127;
}

// ---------- Compartilhar tela ----------

export const SHARE_RESOLUTIONS = [480, 720, 1080, 1440];
export const SHARE_FPS = [30, 60];
const SHARE_DIMS = { 480: [854, 480], 720: [1280, 720], 1080: [1920, 1080], 1440: [2560, 1440] };
// Mbps a 30 / 60 fps (DESIGN.md).
const SHARE_MBPS = { 480: [1.5, 2.5], 720: [3, 5], 1080: [6, 8], 1440: [10, 14] };
export const SHARE_UPLOAD_MBPS = 250;

// Amigos (na chamada) que estão transmitindo a tela.
export function sharingPeers(state) {
  return callPeers(state).filter((n) => state.peers[n].status && state.peers[n].status.sharing);
}

export function shareBitrateMbps(res, fps) {
  const row = SHARE_MBPS[res];
  return row ? row[fps === 60 ? 1 : 0] : 0;
}

// Números com vírgula decimal: 1,5.
export function formatMbps(v) {
  return String(v).replace('.', ',');
}

// Rótulo do chip do encoder; `null` quando não há encoder em hardware.
export function encoderLabel(encoder) {
  if (!encoder) return null;
  const known = { qsv: 'QuickSync · H.264', nvenc: 'NVENC', amf: 'AMF' };
  return known[encoder] || String(encoder).toUpperCase();
}

// Resoluções que cabem na fonte (largura e altura 16:9); fonte 0x0 = desconhecida, não limita.
// Sem encoder em hardware o teto é 720p.
export function shareAllowedRes(source, noEncoder) {
  const w = source ? source.width : 0;
  const hgt = source ? source.height : 0;
  return SHARE_RESOLUTIONS.filter((r) => {
    if (noEncoder && r > 720) return false;
    const [rw, rh] = SHARE_DIMS[r];
    return !((hgt > 0 && rh > hgt) || (w > 0 && rw > w));
  });
}

// Tudo o que o diálogo precisa, já resolvido: fonte escolhida, resolução/fps válidos,
// notas e se dá para começar. A escolha inválida cai para a maior permitida.
export function shareView(state) {
  const d = state.shareDialog;
  const src = state.shareSources;
  const loaded = !!src;
  const noEncoder = loaded && !src.encoder;
  const list = loaded ? (d.tab === 'janelas' ? src.windows : src.monitors) : [];
  const source = list.find((s) => s.id === d.sourceId) || list[0] || null;
  const allowedRes = shareAllowedRes(source, noEncoder);
  const res = allowedRes.includes(d.res) ? d.res : (allowedRes[allowedRes.length - 1] || 480);
  const allowedFps = noEncoder ? [30] : SHARE_FPS;
  const fps = allowedFps.includes(d.fps) ? d.fps : allowedFps[allowedFps.length - 1];
  const notes = [];
  const known = source && source.width > 0 && source.height > 0;
  if (known && SHARE_RESOLUTIONS.some((r) => !shareAllowedRes(source, false).includes(r))) {
    notes.push(`${d.tab === 'janelas' ? 'A janela tem' : 'Sua tela é'} ${source.width} × ${source.height}`);
  }
  if (noEncoder) notes.push('acima de 720p a 30 fps precisa da placa de vídeo');
  return {
    loaded, noEncoder, list, source, allowedRes, res, allowedFps, fps,
    mbps: shareBitrateMbps(res, fps),
    note: notes.join(' · ').replace(/^./, (c) => c.toUpperCase()),
    canStart: loaded && !noEncoder && !!source && !d.pending,
  };
}

// Mensagem de erro mostrada no diálogo (o núcleo escreve em português, salvo o transitório).
export function shareErrorText(message) {
  const m = String(message || '');
  if (/still probing/i.test(m)) return 'O Concord ainda está detectando a placa de vídeo. Tente de novo em instantes.';
  return m || 'Não foi possível transmitir a tela.';
}

// Erro do núcleo com o diálogo aberto vai para o diálogo (e destrava o botão).
// Devolve true se consumiu; false = mostrar como aviso comum.
export function shareConsumeError(state, message) {
  const d = state.shareDialog;
  if (!d.open) return false;
  d.error = shareErrorText(message);
  d.pending = false;
  d.pendingName = null;
  return true;
}

export function initial(name) {
  const t = (name || '?').trim();
  return t ? Array.from(t)[0].toUpperCase() : '?';
}
