// Tela Ajustes: "Seu endereço", "Seu nome", "Atalhos" e "Amigos". Só envia comandos; o estado vem do `Config` do núcleo (D-010).

import { friends, initial } from './state.js';

const NAMED = {
  Space: 'Space', Enter: 'Enter', NumpadEnter: 'Enter', Tab: 'Tab', Backspace: 'Backspace',
  Insert: 'Insert', Delete: 'Delete', Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown',
  ArrowLeft: 'Left', ArrowUp: 'Up', ArrowRight: 'Right', ArrowDown: 'Down',
  NumpadMultiply: 'NumpadMultiply', NumpadAdd: 'NumpadAdd', NumpadSubtract: 'NumpadSubtract',
  NumpadDecimal: 'NumpadDecimal', NumpadDivide: 'NumpadDivide',
};

// Nome da tecla no formato do parser do núcleo (`hotkey.rs`), ou null se não for suportada.
function keyToken(code) {
  let m;
  if ((m = /^Key([A-Z])$/.exec(code))) return m[1];
  if ((m = /^Digit([0-9])$/.exec(code))) return m[1];
  if ((m = /^F([0-9]{1,2})$/.exec(code)) && Number(m[1]) >= 1 && Number(m[1]) <= 24) return `F${m[1]}`;
  if ((m = /^Numpad([0-9])$/.exec(code))) return `Numpad${m[1]}`;
  return NAMED[code] || null;
}

function modifierPrefix(e) {
  return [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Win'].filter(Boolean);
}

// Combinação a partir de um `keydown`: 'cancel' (Esc), null (só modificador / tecla não suportada) ou a string.
export function comboFromKeyEvent(e) {
  if (e.code === 'Escape') return 'cancel';
  if (/^(Control|Alt|Shift|Meta|OS)(Left|Right)$/.test(e.code)) return null;
  const key = keyToken(e.code);
  return key ? [...modifierPrefix(e), key].join('+') : null;
}

// Combinação a partir de um `mousedown`: só os botões laterais (3 = Mouse4, 4 = Mouse5).
export function comboFromMouseEvent(e) {
  if (e.button !== 3 && e.button !== 4) return null;
  return [...modifierPrefix(e), e.button === 3 ? 'Mouse4' : 'Mouse5'].join('+');
}

// Atalho global sem modificador e sem ser F1..F24 / botão lateral atrapalharia a digitação.
export function needsModifier(combo) {
  return /^[A-Z0-9]$|^(Space|Enter|Tab|Backspace)$/.test(combo);
}

const HOTKEYS = [
  { field: 'toggle_mute', label: 'Mutar-se/Desmutar-se' },
  { field: 'toggle_deafen', label: 'Silenciar/Voltar a ouvir' },
  { field: 'toggle_game_mode', label: 'Modo jogo' },
];

export function initSettings({ state, send, h, icon }) {
  const $ = (id) => document.getElementById(id);
  let confirming = null; // { kind: 'remove' | 'reset', name }
  let pendingAdd = null; // nome enviado, aguardando o Config novo
  let shownName = null; // último Config.name colocado no campo
  let volumeDragging = false;
  let thresholdDragging = false;

  const address = (f) => (f.port ? `${f.address}:${f.port}` : f.address);

  function ask(kind, name) {
    confirming = { kind, name };
    renderSettings();
    const btn = document.querySelector('[data-key="confirm-yes"]');
    if (btn) btn.focus();
  }

  function cancel(returnKey) {
    confirming = null;
    renderSettings();
    const back = document.querySelector(`[data-key="${CSS.escape(returnKey)}"]`);
    if (back) back.focus();
  }

  function confirmRow(f) {
    const remove = confirming.kind === 'remove';
    const question = remove
      ? `Remover ${f.name} da sua lista de amigos?`
      : `Resetar a identidade de ${f.name}? Use se o amigo reinstalou ou trocou de PC; a próxima conexão confiará na identidade nova.`;
    return h('div', { class: 'f-confirm', role: 'group', 'aria-label': 'Confirmação' },
      h('span', {}, question),
      h('button', {
        type: 'button', class: `btn ${remove ? 'danger' : 'neutral'}`, 'data-key': 'confirm-yes',
        onclick: () => {
          confirming = null;
          send(remove ? { type: 'RemoveFriend', name: f.name } : { type: 'ResetFingerprint', name: f.name });
          renderSettings();
        },
      }, remove ? 'Remover' : 'Resetar'),
      h('button', {
        type: 'button', class: 'btn neutral',
        onclick: () => cancel(`${remove ? 'rm' : 'reset'}:${f.name}`),
      }, 'Cancelar'));
  }

  function friendRow(f) {
    const mine = confirming && confirming.name === f.name;
    const seal = f.verified
      ? h('span', { class: 'seal ok' }, icon('i-check', 12), 'Identidade verificada')
      : h('span', { class: 'seal pending' }, 'Ainda não conectou');
    const actions = h('div', { class: 'f-actions' },
      f.verified && h('button', {
        type: 'button', class: 'link-btn', 'data-key': `reset:${f.name}`,
        title: 'Use se o amigo reinstalou ou trocou de PC',
        'aria-label': `Resetar identidade de ${f.name}. Use se o amigo reinstalou ou trocou de PC`,
        onclick: () => ask('reset', f.name),
      }, 'Resetar identidade'),
      h('button', {
        type: 'button', class: 'link-btn danger', 'data-key': `rm:${f.name}`,
        'aria-label': `Remover ${f.name}`,
        onclick: () => ask('remove', f.name),
      }, 'Remover'));
    return h('div', { role: 'listitem', class: 'f-row' },
      h('div', { class: 'avatar' }, initial(f.name)),
      h('span', { class: 'f-name' }, f.name),
      h('span', { class: 'f-addr' }, address(f)),
      seal,
      actions,
      mine && confirmRow(f));
  }

  let copiedTimer = null;
  let copied = false;

  function renderAddress(addr) {
    const row = $('my-address');
    if (row.dataset.addr === String(addr)) return; // não recriar (preserva foco e "Copiado")
    row.dataset.addr = String(addr);
    if (!addr) {
      row.replaceChildren(h('span', {}, 'Tailscale não encontrado.'));
      $('my-address-hint').textContent = 'Instale em tailscale.com/download e entre na mesma rede do seu amigo.';
      return;
    }
    const btn = h('button', {
      type: 'button', class: 'btn neutral', 'data-key': 'copy-addr',
      onclick: async () => {
        try {
          await navigator.clipboard.writeText(addr);
          copied = true;
        } catch {
          copied = false;
          $('fr-error').textContent = 'Não consegui copiar; selecione o endereço e copie com Ctrl+C.';
          return;
        }
        btn.textContent = 'Copiado';
        clearTimeout(copiedTimer);
        copiedTimer = setTimeout(() => { btn.textContent = 'Copiar'; copied = false; }, 2000);
      },
    }, 'Copiar');
    row.replaceChildren(h('span', { class: 'addr-value' }, addr), btn);
    $('my-address-hint').textContent = 'Mande para o seu amigo colar em Adicionar amigo.';
  }

  // ---------- Atalhos ----------

  let capturing = null; // campo de `hotkeys` sendo capturado
  let hkSignature = '';

  function stopCapture(sendOff = true) {
    if (!capturing) return;
    capturing = null;
    if (sendOff) send({ type: 'SetHotkeyCapture', on: false });
    renderHotkeys();
  }

  function startCapture(field) {
    if (capturing === field) {
      stopCapture();
      return;
    }
    const wasCapturing = capturing !== null;
    capturing = field;
    $('hk-error').textContent = '';
    if (!wasCapturing) send({ type: 'SetHotkeyCapture', on: true });
    renderHotkeys();
  }

  function finishCapture(combo) {
    const field = capturing;
    if (combo === 'cancel') {
      stopCapture();
      return;
    }
    if (field !== 'push_to_talk' && needsModifier(combo)) {
      $('hk-error').textContent = 'Use Ctrl, Alt ou Shift junto com a tecla (ou uma tecla F1 a F24).';
      return;
    }
    capturing = null;
    const hotkeys = { ...state.config.hotkeys, [field]: combo };
    send({ type: 'UpdateConfig', hotkeys });
    send({ type: 'SetHotkeyCapture', on: false });
    renderHotkeys();
  }

  function onCaptureKey(e) {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    const combo = comboFromKeyEvent(e);
    if (combo) finishCapture(combo);
  }

  function onCaptureMouse(e) {
    if (!capturing) return;
    const combo = comboFromMouseEvent(e);
    if (!combo) return;
    e.preventDefault();
    e.stopPropagation();
    finishCapture(combo);
  }

  window.addEventListener('keydown', onCaptureKey, true);
  window.addEventListener('mousedown', onCaptureMouse, true);
  window.addEventListener('mouseup', (e) => { if (capturing && (e.button === 3 || e.button === 4)) e.preventDefault(); }, true);
  window.addEventListener('blur', () => stopCapture());

  function hotkeyRow(field, label, value) {
    const on = capturing === field;
    return h('div', { class: 'hk-row' },
      h('span', { class: 'hk-label' }, label),
      h('span', { class: 'kbd' }, on ? 'Pressione a combinação… Esc cancela' : (value || 'Nenhum')),
      h('button', {
        type: 'button', class: `link-btn${on ? ' capturing' : ''}`, 'data-key': `hk:${field}`,
        'aria-label': `Alterar atalho: ${label}`, disabled: state.link !== 'connected',
        onclick: () => startCapture(field),
      }, on ? 'Cancelar' : 'Alterar'));
  }

  function renderHotkeys() {
    const cfg = state.config;
    const connected = state.link === 'connected' && !!cfg;
    if (capturing && !connected) capturing = null;
    const mode = cfg ? cfg.audio.mode : null;
    const noiseSuppression = cfg ? cfg.audio.noise_suppression : false;
    const sig = JSON.stringify([cfg && cfg.hotkeys, mode, noiseSuppression, capturing, connected]);
    if (sig === hkSignature) return;
    hkSignature = sig;
    for (const [id, m] of [['mode-vad', 'voice_activity'], ['mode-ptt', 'push_to_talk']]) {
      const b = $(id);
      b.setAttribute('aria-checked', String(mode === m));
      b.disabled = !connected;
    }
    $('noise-suppression').checked = noiseSuppression;
    $('noise-suppression').disabled = !connected;
    const rows = cfg ? HOTKEYS.map(({ field, label }) => hotkeyRow(field, label, cfg.hotkeys[field])) : [];
    if (cfg && mode === 'push_to_talk') {
      rows.push(hotkeyRow('push_to_talk', 'Tecla para falar', cfg.hotkeys.push_to_talk));
    }
    const active = document.activeElement;
    const key = active && active.dataset ? active.dataset.key : null;
    $('hotkey-list').replaceChildren(...rows);
    if (key) {
      const again = document.querySelector(`#hotkey-list [data-key="${CSS.escape(key)}"]`);
      if (again) again.focus();
    }
  }

  function renderMicMeter() {
    const cfg = state.config;
    const db = Number.isFinite(state.mic_level_db) ? state.mic_level_db : -120;
    const threshold = Number($('vad-threshold').value);
    const levelPercent = Math.max(0, Math.min(100, (db + 80) / 80 * 100));
    const thresholdPercent = Math.max(0, Math.min(100, (threshold + 80) / 80 * 100));
    const meter = $('mic-meter');
    $('mic-meter-fill').style.width = `${levelPercent}%`;
    $('mic-meter-fill').classList.toggle('transmitting', db >= threshold);
    meter.setAttribute('aria-valuenow', String(Math.max(-80, Math.min(0, db))));
    $('mic-threshold-line').style.left = `${thresholdPercent}%`;
    $('mic-sensitivity').hidden = !cfg || cfg.audio.mode !== 'voice_activity';
  }

  function renderAudioControls() {
    const cfg = state.config;
    const connected = state.link === 'connected' && !!cfg;
    const volume = cfg ? cfg.audio.input_volume : 100;
    const threshold = cfg ? cfg.audio.vad_threshold : -50;
    if (!volumeDragging) {
      $('mic-volume').value = String(volume);
      $('mic-volume-value').value = `${volume}%`;
    }
    if (!thresholdDragging) {
      $('vad-threshold').value = String(Math.max(-80, Math.min(-10, threshold)));
      $('vad-threshold-value').value = `${threshold} dB`;
    }
    $('mic-volume').disabled = !connected;
    $('vad-threshold').disabled = !connected;
    renderDeviceSelect('input-device', cfg && cfg.audio.input_device, state.audio_devices.inputs, connected);
    renderDeviceSelect('output-device', cfg && cfg.audio.output_device, state.audio_devices.outputs, connected);
    renderMicMeter();
  }

  function renderDeviceSelect(id, configured, available, connected) {
    const select = $(id);
    const value = configured || 'default';
    const names = ['default', ...available.filter((name) => name !== 'default')];
    if (value !== 'default' && !names.some((name) => name.toLowerCase() === value.toLowerCase())) {
      names.push(value);
    }
    select.replaceChildren(...names.map((name) => h('option', {
      value: name,
      selected: name.toLowerCase() === value.toLowerCase(),
    }, name === 'default' ? 'Padrão do Windows' : name === value && !available.some((device) => device.toLowerCase() === value.toLowerCase())
      ? `${name} (desconectado)` : name)));
    select.value = value;
    select.disabled = !connected;
  }

  for (const [id, field] of [['input-device', 'input_device'], ['output-device', 'output_device']]) {
    $(id).addEventListener('change', () => {
      const cfg = state.config;
      const value = $(id).value;
      if (!cfg || cfg.audio[field] === value) return;
      send({ type: 'UpdateConfig', audio: { ...cfg.audio, [field]: value } });
    });
  }

  for (const [id, mode] of [['mode-vad', 'voice_activity'], ['mode-ptt', 'push_to_talk']]) {
    $(id).addEventListener('click', () => {
      const cfg = state.config;
      if (!cfg || cfg.audio.mode === mode) return;
      send({ type: 'UpdateConfig', audio: { ...cfg.audio, mode } });
    });
  }

  $('noise-suppression').addEventListener('change', () => {
    const cfg = state.config;
    if (!cfg || cfg.audio.noise_suppression === $('noise-suppression').checked) return;
    send({ type: 'UpdateConfig', audio: { ...cfg.audio, noise_suppression: $('noise-suppression').checked } });
  });

  $('mic-volume').addEventListener('input', () => {
    volumeDragging = true;
    $('mic-volume-value').value = `${$('mic-volume').value}%`;
  });
  $('mic-volume').addEventListener('change', () => {
    volumeDragging = false;
    const cfg = state.config;
    const input_volume = Number($('mic-volume').value);
    if (!cfg || cfg.audio.input_volume === input_volume) return;
    send({ type: 'UpdateConfig', audio: { ...cfg.audio, input_volume } });
  });

  $('vad-threshold').addEventListener('input', () => {
    thresholdDragging = true;
    $('vad-threshold-value').value = `${$('vad-threshold').value} dB`;
    renderMicMeter();
  });
  $('vad-threshold').addEventListener('change', () => {
    thresholdDragging = false;
    const cfg = state.config;
    const vad_threshold = Number($('vad-threshold').value);
    if (!cfg || cfg.audio.vad_threshold === vad_threshold) return;
    send({ type: 'UpdateConfig', audio: { ...cfg.audio, vad_threshold } });
  });

  function renderSettings() {
    const cfg = state.config;
    if (cfg) renderAddress(cfg.my_address || null);
    renderHotkeys();
    renderAudioControls();
    const input = $('name-input');
    if (cfg && cfg.name !== shownName) {
      shownName = cfg.name;
      input.value = cfg.name;
    }
    $('name-save').disabled = state.link !== 'connected' || !cfg;
    $('fr-add').disabled = state.link !== 'connected';

    const list = friends(state);
    if (confirming && !list.some((f) => f.name === confirming.name)) confirming = null;
    if (pendingAdd && list.some((f) => f.name === pendingAdd)) {
      pendingAdd = null;
      $('fr-name').value = '';
      $('fr-addr').value = '';
      $('fr-error').textContent = '';
    }
    const active = document.activeElement;
    const key = active && active.dataset ? active.dataset.key : null;
    $('friend-list').replaceChildren(...(list.length
      ? list.map(friendRow)
      : [h('div', { class: 'hint-text' }, 'Nenhum amigo ainda.')]));
    if (key) {
      const again = document.querySelector(`#friend-list [data-key="${CSS.escape(key)}"]`);
      if (again && again !== active) again.focus();
    }
    renderTailnet();
  }

  // ---------- Na sua rede Tailscale ----------

  const nicks = {}; // endereço -> apelido digitado (sobrevive às re-renderizações)
  let tnSignature = '';

  function requestTailnet() {
    if (state.link !== 'connected') return;
    state.tailnet = { loading: true };
    renderTailnet();
    send({ type: 'ListTailnetPeers' });
  }

  // Apelido sugerido: o primeiro nome do dono (PC de outra pessoa) ou o nome do PC.
  function suggestedNick(p) {
    if (!p.mine && p.owner) {
      const base = p.owner.includes('@') ? p.owner.split('@')[0] : p.owner.trim().split(/\s+/)[0];
      if (base) return base.slice(0, 40);
    }
    return p.name.slice(0, 40);
  }

  function addFromTailnet(p) {
    const nick = (nicks[p.address] ?? suggestedNick(p)).trim();
    const err = $('fr-error');
    if (!nick) {
      err.textContent = 'Dê um apelido para esse amigo.';
      return;
    }
    if (friends(state).some((f) => f.name === nick)) {
      err.textContent = `Já existe um amigo chamado ${nick}. Escolha outro apelido.`;
      return;
    }
    err.textContent = '';
    send({ type: 'AddFriend', name: nick, address: p.address });
  }

  function tailnetRow(p, friend) {
    const meta = [p.owner, p.address, p.os && p.os !== 'windows' ? p.os : null].filter(Boolean).join(' · ');
    const info = h('div', { class: 'tn-info' },
      h('span', { class: 'f-name' }, p.name),
      h('span', { class: 'tn-meta' },
        h('span', { class: `dot${p.online ? '' : ' off'}` }),
        `${p.online ? 'Online' : 'Offline'} · ${meta}`));
    const action = friend
      ? h('span', { class: 'seal ok' }, icon('i-check', 12), `Já é amigo: ${friend.name}`)
      : h('div', { class: 'tn-add' },
        h('input', {
          type: 'text', maxlength: '40', placeholder: 'Apelido',
          'aria-label': `Apelido para ${p.name}`, 'data-key': `tn-nick:${p.address}`,
          value: nicks[p.address] ?? suggestedNick(p),
          oninput: (e) => { nicks[p.address] = e.target.value; },
          onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); addFromTailnet(p); } },
        }),
        h('button', {
          type: 'button', class: 'btn primary', 'data-key': `tn-add:${p.address}`,
          disabled: state.link !== 'connected',
          onclick: () => addFromTailnet(p),
        }, 'Adicionar'));
    return h('div', { role: 'listitem', class: 'f-row' },
      h('div', { class: 'avatar' }, initial(p.name)), info, action);
  }

  // Só recria a lista quando algo dela muda: os campos de apelido não perdem o foco nem o texto.
  function renderTailnet() {
    const tn = state.tailnet;
    const list = friends(state);
    const sig = JSON.stringify([tn, list.map((f) => [f.name, f.address]), state.link]);
    if (sig === tnSignature) return;
    tnSignature = sig;
    $('tn-scan').disabled = state.link !== 'connected' || !!(tn && tn.loading);
    let rows;
    if (!tn || tn.loading) {
      rows = [h('div', { class: 'hint-text' }, 'Procurando dispositivos…')];
    } else if (!tn.running) {
      rows = [h('div', { class: 'hint-text' }, 'O Tailscale está desligado ou não foi encontrado.')];
    } else if (!tn.peers.length) {
      rows = [h('div', { class: 'hint-text' },
        'Nenhum outro dispositivo na sua rede. Convide seu amigo pelo painel do Tailscale ou compartilhem as máquinas.')];
    } else {
      rows = tn.peers.map((p) => tailnetRow(p, list.find((f) => f.address === p.address)));
    }
    const active = document.activeElement;
    const key = active && active.dataset ? active.dataset.key : null;
    $('tn-list').replaceChildren(...rows);
    if (key) {
      const again = document.querySelector(`#tn-list [data-key="${CSS.escape(key)}"]`);
      if (again && again !== active) again.focus();
    }
  }

  $('tn-scan').addEventListener('click', requestTailnet);

  $('name-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const name = $('name-input').value.trim();
    if (!name || name === shownName) return;
    send({ type: 'UpdateConfig', name });
  });

  $('friend-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const name = $('fr-name').value.trim();
    const addr = $('fr-addr').value.trim();
    const err = $('fr-error');
    if (!name) {
      err.textContent = 'Digite o nome do amigo.';
      $('fr-name').focus();
      return;
    }
    if (!addr) {
      err.textContent = 'Digite o IP do Tailscale do amigo (100.x.y.z).';
      $('fr-addr').focus();
      return;
    }
    err.textContent = '';
    pendingAdd = name;
    send({ type: 'AddFriend', name, address: addr });
  });

  function focusAddFriend() {
    $('fr-name').focus();
    $('friend-form').scrollIntoView({ block: 'center' });
  }

  return { renderSettings, renderMicMeter, focusAddFriend, requestTailnet };
}
