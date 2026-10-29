// Tela Ajustes: Áudio, Modo jogo, Atalhos e Você. Só envia comandos; o estado vem do `Config` do núcleo (D-010).
// A aba Amigos está em friends.js.

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
  { field: 'toggle_mute', label: 'Mutar-se' },
  { field: 'toggle_deafen', label: 'Silenciar' },
  { field: 'toggle_game_mode', label: 'Modo jogo' },
];
const PTT = { field: 'push_to_talk', label: 'Tecla para falar' };

const BITRATES = [
  [96, 'Alta · 96 kbps (recomendado)'],
  [128, 'Máxima · 128 kbps'],
  [64, 'Econômica · 64 kbps'],
];

// Limites do medidor/limite de sensibilidade (dB).
const DB_MIN = -80;
const DB_MAX = -10;

export function initSettings({ state, send, h, icon }) {
  const $ = (id) => document.getElementById(id);
  let shownName = null; // último Config.name colocado no campo
  let pendingName = null; // nome enviado, aguardando o Config novo
  let nameSaved = false;
  let volumeDragging = false;
  let cueDragging = false;
  let thresholdDragging = false;

  const linked = () => state.link === 'connected' && !!state.config;

  // ---------- Você ----------

  let copiedTimer = null;

  function renderAddress(addr) {
    const row = $('my-address');
    if (row.dataset.addr === String(addr)) return; // não recriar (preserva foco e "Copiado")
    row.dataset.addr = String(addr);
    if (!addr) {
      row.replaceChildren(h('div', { class: 'notice' }, icon('i-alert', 16),
        h('span', {}, h('strong', {}, 'Tailscale não encontrado.'), ' ',
          h('span', { class: 'muted' }, 'Instale em tailscale.com/download e entre na mesma rede do seu amigo.'))));
      $('my-address-hint').replaceChildren();
      return;
    }
    const btn = h('button', {
      type: 'button', class: 'btn-text', 'data-key': 'copy-addr', 'aria-label': 'Copiar seu endereço',
      onclick: async () => {
        try {
          await navigator.clipboard.writeText(addr);
        } catch {
          $('addr-error').replaceChildren(icon('i-info', 14), 'Não consegui copiar; selecione o endereço e copie com Ctrl+C.');
          return;
        }
        $('addr-error').replaceChildren();
        setCopyButton(btn, true);
        clearTimeout(copiedTimer);
        copiedTimer = setTimeout(() => setCopyButton(btn, false), 2000);
      },
    });
    setCopyButton(btn, false);
    row.replaceChildren(h('div', { class: 'input-row' }, h('span', { class: 'input readonly', 'aria-labelledby': 'lbl-end' }, addr), btn));
    $('my-address-hint').replaceChildren(h('div', { class: 'hint-text', style: 'display:flex;flex-wrap:wrap;align-items:center;gap:8px' },
      h('span', { class: 'pill live' }, h('span', { class: 'dot', 'aria-hidden': 'true' }), 'Tailscale conectado'),
      'Seus amigos usam este endereço para adicionar você.'));
  }

  function setCopyButton(btn, copied) {
    btn.setAttribute('aria-label', copied ? 'Endereço copiado' : 'Copiar seu endereço');
    btn.replaceChildren(icon(copied ? 'i-check' : 'i-copy', 15), copied ? 'Copiado' : 'Copiar');
  }

  $('name-input').addEventListener('input', () => {
    nameSaved = false;
    $('name-save').textContent = 'Salvar';
  });

  $('name-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const name = $('name-input').value.trim();
    if (!name || name === shownName) return;
    pendingName = name;
    send({ type: 'UpdateConfig', name });
  });

  // ---------- Atalhos ----------

  let capturing = null; // campo de `hotkeys` sendo capturado
  let hkError = null; // { field, combo, message } da captura em curso
  let hkSignature = '';

  function stopCapture(sendOff = true) {
    if (!capturing) return;
    capturing = null;
    hkError = null;
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
    hkError = null;
    if (!wasCapturing) send({ type: 'SetHotkeyCapture', on: true });
    renderHotkeys();
  }

  // Quem já usa a combinação (rótulo), ou null.
  function ownerOf(combo, field) {
    const hk = state.config && state.config.hotkeys;
    if (!hk) return null;
    const other = [...HOTKEYS, PTT].find((x) => x.field !== field && String(hk[x.field]).toLowerCase() === combo.toLowerCase());
    return other ? other.label : null;
  }

  function finishCapture(combo) {
    const field = capturing;
    if (combo === 'cancel') {
      stopCapture();
      return;
    }
    const owner = ownerOf(combo, field);
    if (owner) {
      hkError = { field, combo, message: `Esse atalho já está em uso por ${owner}.` };
      renderHotkeys();
      return;
    }
    if (field !== PTT.field && needsModifier(combo)) {
      hkError = { field, combo, message: 'Use Ctrl, Alt ou Shift junto com a tecla (ou uma tecla F1 a F24).' };
      renderHotkeys();
      return;
    }
    capturing = null;
    hkError = null;
    const hotkeys = { ...state.config.hotkeys, [field]: combo };
    send({ type: 'UpdateConfig', hotkeys });
    send({ type: 'SetHotkeyCapture', on: false });
    renderHotkeys();
  }

  function onCaptureKey(e) {
    if (!capturing) return;
    // Tab sai da captura sem prender o foco.
    if (e.code === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey) {
      stopCapture();
      return;
    }
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

  function hotkeyRow({ field, label }, value, extra = '') {
    const on = capturing === field;
    const err = on && hkError && hkError.field === field ? hkError : null;
    const errId = `hk-err-${field}`;
    const controls = on
      ? [
        h('button', {
          type: 'button', class: `capture${err ? ' conflict' : ''}`, 'data-key': `hk:${field}`,
          'aria-label': `Novo atalho para ${label}. Aperte as teclas. Esc cancela.`,
          'aria-describedby': err ? errId : false,
        }, err ? err.combo : 'Aperte a nova tecla…'),
        h('button', { type: 'button', class: 'btn-text', onclick: () => stopCapture() }, 'Cancelar'),
      ]
      : [
        h('span', { class: 'kbd' }, value || 'Nenhum'),
        h('button', {
          type: 'button', class: 'btn-text', 'data-key': `hk:${field}`,
          'aria-label': `Alterar atalho: ${label}`, disabled: !linked(),
          onclick: () => startCapture(field),
        }, 'Alterar'),
      ];
    return h('div', { class: `hk-item ${extra}` },
      h('div', { class: 'hk-row' }, h('span', { class: 'hk-label' }, label), controls),
      err && h('span', { class: 'hk-error', id: errId, role: 'alert' }, icon('i-info', 14), err.message));
  }

  function renderHotkeys() {
    const cfg = state.config;
    const connected = linked();
    if (capturing && !connected) capturing = null;
    const mode = cfg ? cfg.audio.mode : null;
    const sig = JSON.stringify([cfg && cfg.hotkeys, mode, capturing, hkError, connected]);
    if (sig === hkSignature) return;
    hkSignature = sig;
    const active = document.activeElement;
    const key = active && active.dataset ? active.dataset.key : null;
    $('hotkey-list').replaceChildren(...(cfg ? HOTKEYS.map((x) => hotkeyRow(x, cfg.hotkeys[x.field])) : []));
    $('ptt-row').replaceChildren(...(cfg && mode === 'push_to_talk' ? [hotkeyRow(PTT, cfg.hotkeys.push_to_talk, 'ptt-row')] : []));
    if (key) {
      const again = document.querySelector(`#view-settings [data-key="${CSS.escape(key)}"]`);
      if (again && again !== active) again.focus();
    }
  }

  // ---------- Áudio ----------

  function setText(el, text) {
    if (el.textContent !== text) el.textContent = text;
  }

  function renderMicMeter() {
    const cfg = state.config;
    const vad = !!cfg && cfg.audio.mode === 'voice_activity';
    const db = Number.isFinite(state.mic_level_db) ? state.mic_level_db : -120;
    const threshold = Number($('vad-threshold').value);
    const pct = Math.max(0, Math.min(100, (db - DB_MIN) / (DB_MAX - DB_MIN) * 100));
    const above = vad && db >= threshold;
    $('mic-meter-fill').style.width = `${pct}%`;
    $('mic-meter-fill').classList.toggle('transmitting', above);
    $('vad-threshold').hidden = !vad;
    $('vad-threshold').setAttribute('aria-valuetext', `${threshold} dB`);
    const line = $('mic-threshold-line');
    line.hidden = !vad;
    line.style.setProperty('--p', String((threshold - DB_MIN) / (DB_MAX - DB_MIN)));
    $('vad-threshold-value').hidden = !vad;
    setText($('vad-threshold-value'), `${threshold} dB`.replace('-', '−'));
    $('meter-state').classList.toggle('on', above);
    if (!cfg) {
      setText($('meter-state-text'), 'Abaixo do limite');
      return;
    }
    setText($('lbl-nivel'), vad ? 'Sensibilidade' : 'Nível do microfone');
    if (vad) {
      setText($('meter-state-text'), above ? 'Transmitindo' : 'Abaixo do limite');
      setText($('nivel-ajuda'), 'Arraste o marcador. Abaixo dele, sua voz não é enviada.');
    } else {
      setText($('meter-state-text'), `Segure ${cfg.hotkeys.push_to_talk} para falar`);
      setText($('nivel-ajuda'), 'No modo Aperte para falar, sua voz só sai enquanto você segura a tecla.');
    }
  }

  function renderBitrate(value, connected) {
    const select = $('bitrate');
    if (select.dataset.v !== String(value)) {
      select.dataset.v = String(value);
      const opts = BITRATES.map(([v, label]) => h('option', { value: String(v) }, label));
      if (!BITRATES.some(([v]) => v === value)) opts.push(h('option', { value: String(value) }, `Personalizada · ${value} kbps`));
      select.replaceChildren(...opts);
      select.value = String(value);
    }
    select.disabled = !connected;
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

  function renderAudioControls() {
    const cfg = state.config;
    const connected = linked();
    const a = cfg ? cfg.audio : null;
    const volume = a ? a.input_volume : 100;
    const threshold = a ? a.vad_threshold : -50;
    if (!volumeDragging) {
      $('mic-volume').value = String(volume);
      $('mic-volume-value').value = `${volume}%`;
    }
    if (!thresholdDragging) {
      $('vad-threshold').value = String(Math.max(DB_MIN, Math.min(DB_MAX, threshold)));
    }
    $('mic-volume').disabled = !connected;
    if (!cueDragging) {
      const cue = a && Number.isFinite(a.cue_volume) ? a.cue_volume : 60;
      $('cue-volume').value = String(cue);
      $('cue-volume-value').value = `${cue}%`;
    }
    $('cue-volume').disabled = !connected;
    $('vad-threshold').disabled = !connected;
    for (const [id, m] of [['mode-vad', 'voice_activity'], ['mode-ptt', 'push_to_talk']]) {
      const b = $(id);
      b.setAttribute('aria-checked', String(!!a && a.mode === m));
      b.disabled = !connected;
    }
    $('noise-suppression').checked = !!a && a.noise_suppression;
    $('noise-suppression').disabled = !connected;
    renderBitrate(a ? a.bitrate_kbps : 96, connected);
    renderDeviceSelect('input-device', a && a.input_device, state.audio_devices.inputs, connected);
    renderDeviceSelect('output-device', a && a.output_device, state.audio_devices.outputs, connected);
    renderMicMeter();
  }

  function updateAudio(patch) {
    const cfg = state.config;
    if (!cfg) return;
    send({ type: 'UpdateConfig', audio: { ...cfg.audio, ...patch } });
  }

  for (const [id, field] of [['input-device', 'input_device'], ['output-device', 'output_device']]) {
    $(id).addEventListener('change', () => {
      const cfg = state.config;
      const value = $(id).value;
      if (!cfg || cfg.audio[field] === value) return;
      updateAudio({ [field]: value });
    });
  }

  for (const [id, mode] of [['mode-vad', 'voice_activity'], ['mode-ptt', 'push_to_talk']]) {
    $(id).addEventListener('click', () => {
      const cfg = state.config;
      if (!cfg || cfg.audio.mode === mode) return;
      updateAudio({ mode });
    });
  }

  $('bitrate').addEventListener('change', () => {
    const cfg = state.config;
    const bitrate_kbps = Number($('bitrate').value);
    if (!cfg || cfg.audio.bitrate_kbps === bitrate_kbps) return;
    updateAudio({ bitrate_kbps });
  });

  $('noise-suppression').addEventListener('change', () => {
    const cfg = state.config;
    if (!cfg || cfg.audio.noise_suppression === $('noise-suppression').checked) return;
    updateAudio({ noise_suppression: $('noise-suppression').checked });
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
    updateAudio({ input_volume });
  });

  $('cue-volume').addEventListener('input', () => {
    cueDragging = true;
    $('cue-volume-value').value = `${$('cue-volume').value}%`;
  });
  $('cue-volume').addEventListener('change', () => {
    cueDragging = false;
    const cfg = state.config;
    const cue_volume = Number($('cue-volume').value);
    if (!cfg || cfg.audio.cue_volume === cue_volume) return;
    updateAudio({ cue_volume });
  });

  $('vad-threshold').addEventListener('input', () => {
    thresholdDragging = true;
    renderMicMeter();
  });
  $('vad-threshold').addEventListener('change', () => {
    thresholdDragging = false;
    const cfg = state.config;
    const vad_threshold = Number($('vad-threshold').value);
    if (!cfg || cfg.audio.vad_threshold === vad_threshold) return;
    updateAudio({ vad_threshold });
  });

  // ---------- Modo jogo ----------

  function updateGameMode(patch) {
    const cfg = state.config;
    if (!cfg) return;
    send({ type: 'UpdateConfig', game_mode: { ...cfg.game_mode, ...patch } });
  }

  $('game-auto').addEventListener('change', () => {
    const cfg = state.config;
    if (!cfg || cfg.game_mode.auto_detect === $('game-auto').checked) return;
    updateGameMode({ auto_detect: $('game-auto').checked });
  });
  $('game-keep').addEventListener('change', () => {
    const cfg = state.config;
    if (!cfg || cfg.game_mode.keep_sharing_in_game_mode === $('game-keep').checked) return;
    updateGameMode({ keep_sharing_in_game_mode: $('game-keep').checked });
  });

  function renderGameMode() {
    const cfg = state.config;
    const connected = linked();
    const gm = cfg ? cfg.game_mode : null;
    $('game-auto').checked = !!gm && gm.auto_detect;
    $('game-auto').disabled = !connected;
    $('game-keep').checked = !!gm && gm.keep_sharing_in_game_mode;
    $('game-keep').disabled = !connected;
    const games = gm ? gm.games : [];
    const list = $('game-list');
    const sig = JSON.stringify([games, connected]);
    if (list.dataset.sig === sig) return;
    list.dataset.sig = sig;
    list.replaceChildren(...(games.length
      ? games.map((g) => h('li', { class: 'row compact' },
        icon('i-gamepad', 16),
        h('span', { class: 'row-title mono', style: 'flex-grow:1' }, g),
        h('button', {
          type: 'button', class: 'btn-text danger', 'aria-label': `Remover ${g}`, disabled: !connected,
          onclick: () => updateGameMode({ games: state.config.game_mode.games.filter((x) => x !== g) }),
        }, 'Remover')))
      : [h('li', { class: 'hint-text' }, 'Nenhum jogo na lista.')]));
  }

  // ---------- Conjunto ----------

  function renderSettings() {
    const cfg = state.config;
    if (cfg) renderAddress(cfg.my_address || null);
    renderHotkeys();
    renderAudioControls();
    renderGameMode();
    const input = $('name-input');
    if (cfg && cfg.name !== shownName) {
      shownName = cfg.name;
      input.value = cfg.name;
    }
    if (cfg && pendingName && cfg.name === pendingName) {
      pendingName = null;
      nameSaved = true;
    }
    $('name-save').textContent = nameSaved ? 'Salvo' : 'Salvar';
    $('name-save').disabled = !linked();
  }

  return { renderSettings, renderMicMeter };
}
