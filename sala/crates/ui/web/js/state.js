// Estado da interface e regras de derivação. Sem DOM, sem lógica de negócio:
// só espelha o que o núcleo enviou (D-010).

export function initialState() {
  return {
    // 'connecting' (nunca conectou) | 'connected' | 'disconnected'
    link: 'connecting',
    everConnected: false,
    status: null, // { muted, deafened, game_mode, in_room }
    config: null, // CoreToUi::Config
    peers: {}, // nome -> { connected, reconnecting, status, stats }
    mismatch: {}, // nome -> true (FingerprintMismatch)
    chat: [],
    volumes: {}, // nome -> 0..200 (só da sessão da interface)
    view: 'call',
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
    case 'PeerConnected': {
      const p = peer(state, msg.friend);
      p.connected = true;
      p.reconnecting = null;
      delete state.mismatch[msg.friend];
      return 'full';
    }
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
    case 'ChatHistory':
      state.chat = Array.isArray(msg.entries) ? msg.entries.slice() : [];
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

// Quem receberá o `Call` do botão "Entrar".
export function callTarget(state) {
  return chatPeer(state) || (friends(state)[0] && friends(state)[0].name) || null;
}

// { text, tone: 'accent'|'game'|'danger'|'muted', dot: 'on'|'game'|'danger'|'off', online }
export function friendStatus(state, name) {
  const p = state.peers[name];
  if (state.mismatch[name]) return { text: 'Identidade diferente', tone: 'danger', dot: 'danger', online: false };
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

export function initial(name) {
  const t = (name || '?').trim();
  return t ? Array.from(t)[0].toUpperCase() : '?';
}
