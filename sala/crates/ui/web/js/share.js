// Diálogo Compartilhar tela. Só mostra o que o núcleo listou e envia comandos (D-010).
// O estado do diálogo fica em `state.shareDialog`; a lógica de validação, em state.js.

import {
  SHARE_RESOLUTIONS, SHARE_FPS, SHARE_UPLOAD_MBPS, shareView, formatMbps,
  encoderLabel, avatarIndex, initial, callPeers,
  SHARE_AUDIO_KEY, loadShareAudio, writePref,
} from './state.js';

function storage() {
  try { return window.localStorage; } catch { return null; }
}

export function initShare({ state, send, h, icon, linked }) {
  const d = state.shareDialog;
  const modal = document.getElementById('share-modal');
  let wasOpen = false;
  let opener = null;

  function openShare() {
    if (d.open) return;
    opener = document.activeElement;
    Object.assign(d, { open: true, tab: 'telas', sourceId: null, res: 1080, fps: 60, audio: loadShareAudio(storage()), pending: false, pendingName: null, pendingAudio: false, error: null });
    requestSources();
    renderShare();
  }

  function closeShare() {
    if (!d.open) return;
    d.open = false;
    d.pending = false;
    renderShare();
  }

  function requestSources() {
    if (linked()) send({ type: 'ListShareSources' });
  }

  function start(view) {
    if (!view.canStart || !linked()) return;
    d.error = null;
    d.pending = true;
    d.pendingName = view.source.name;
    d.pendingAudio = d.audio;
    send({ type: 'StartShare', source_id: view.source.id, resolution: view.res, fps: view.fps, audio: d.audio });
    renderShare();
  }

  function segment(group, options, current, pick, label) {
    return h('div', { class: `seg c${options.length}`, role: 'radiogroup', 'aria-labelledby': group },
      options.map((o) => h('button', {
        type: 'button', role: 'radio', 'aria-checked': String(o.value === current),
        'data-key': `${group}:${o.value}`, disabled: !o.enabled,
        onclick: () => pick(o.value),
      }, label(o.value))));
  }

  function sourceCard(s, view) {
    const win = d.tab === 'janelas';
    const checked = view.source && s.id === view.source.id;
    const meta = s.width > 0 && s.height > 0 ? `${s.width} × ${s.height}${s.primary ? ' · principal' : ''}` : (s.primary ? 'principal' : '');
    return h('button', {
      type: 'button', role: 'radio', class: 'src', 'aria-checked': String(!!checked),
      'data-key': `share-src:${s.id}`,
      onclick: () => { d.sourceId = s.id; d.error = null; renderShare(); },
    },
    h('div', { class: `src-thumb${win ? ' win' : ''}`, 'aria-hidden': 'true' },
      h('div', { class: 'bar' }),
      h('div', { class: 'body' }, !win && h('div', { class: 'side' }), h('div', { class: 'main' }))),
    h('div', { class: 'src-info' },
      win && h('span', { class: `app-mark av-${avatarIndex(s.name)}`, 'aria-hidden': 'true' }, initial(s.name)),
      h('div', { class: 'src-text' },
        h('span', { class: 'src-name' }, s.name),
        meta && h('span', { class: 'src-meta' }, meta))));
  }

  function sourcesPanel(view) {
    const win = d.tab === 'janelas';
    let body;
    if (!view.loaded) {
      body = h('p', { class: 'share-empty', role: 'status' },
        d.error ? '' : 'Procurando telas e janelas…');
    } else if (view.list.length === 0) {
      body = h('p', { class: 'share-empty' }, win ? 'Nenhuma janela disponível.' : 'Nenhuma tela encontrada.');
    } else {
      body = h('div', { class: `src-grid${win ? ' three' : ''}`, role: 'radiogroup', 'aria-label': win ? 'Janela para transmitir' : 'Tela para transmitir' },
        view.list.map((s) => sourceCard(s, view)));
    }
    return h('div', { class: 'share-panel', id: 'share-panel', role: 'tabpanel', 'aria-labelledby': `share-tab-${d.tab}` }, body);
  }

  function tabs() {
    return h('div', { class: 'seg tabs', role: 'tablist', 'aria-label': 'O que transmitir' },
      [['telas', 'Telas'], ['janelas', 'Janelas']].map(([id, text]) => h('button', {
        type: 'button', role: 'tab', id: `share-tab-${id}`, 'aria-selected': String(d.tab === id),
        'aria-controls': 'share-panel', 'data-key': `share-tab:${id}`,
        onclick: () => {
          if (d.tab === id) return;
          d.tab = id;
          d.sourceId = null;
          d.error = null;
          if (id === 'janelas') requestSources(); // a lista de janelas muda sozinha
          renderShare();
        },
      }, text)));
  }

  function build() {
    const view = shareView(state);
    const peers = callPeers(state);
    const names = peers.length ? peers.join(' e ') : 'Seus amigos';
    const resOpts = SHARE_RESOLUTIONS.map((r) => ({ value: r, enabled: view.allowedRes.includes(r) }));
    const fpsOpts = SHARE_FPS.map((f) => ({ value: f, enabled: view.allowedFps.includes(f) }));
    const pct = Math.max(1, (view.mbps / SHARE_UPLOAD_MBPS) * 100);
    const chip = encoderLabel(view.loaded ? state.shareSources.encoder : null);

    const dialog = h('div', {
      class: 'dialog', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'share-title', 'aria-describedby': 'share-desc',
    },
    h('div', { class: 'dialog-head' },
      h('div', { class: 'dialog-title' },
        h('h1', { id: 'share-title' }, 'Compartilhar tela'),
        h('span', { id: 'share-desc', class: 'sub' }, `${names} ${peers.length > 1 ? 'vão' : 'vai'} ver o que você escolher.`)),
      h('button', {
        type: 'button', class: 'btn-icon close', 'data-key': 'share-close', 'aria-label': 'Fechar', title: 'Fechar (Esc)',
        onclick: closeShare,
      }, icon('i-close', 18))),
    h('div', { class: 'share-sources' }, tabs(), sourcesPanel(view)),
    h('div', { class: 'share-opts' },
      h('div', { class: 'field' },
        h('span', { class: 'share-label', id: 'share-res' }, 'Resolução'),
        segment('share-res', resOpts, view.res, (v) => { d.res = v; renderShare(); }, (v) => `${v}p`),
        view.note && h('span', { class: 'hint-text note' }, icon('i-info', 13), view.note)),
      h('div', { class: 'field fps' },
        h('span', { class: 'share-label', id: 'share-fps' }, 'Quadros por segundo'),
        segment('share-fps', fpsOpts, view.fps, (v) => { d.fps = v; renderShare(); }, (v) => String(v)))),
    h('label', { class: 'check-row share-audio', for: 'share-audio' },
      h('span', {}, h('strong', {}, 'Transmitir o áudio do PC'),
        h('small', {}, 'Quem assiste ouve o som do seu computador, sem a voz da chamada.')),
      h('input', {
        type: 'checkbox', id: 'share-audio', 'data-key': 'share-audio', checked: d.audio,
        onchange: (e) => { d.audio = e.target.checked; writePref(storage(), SHARE_AUDIO_KEY, d.audio ? '1' : '0'); },
      })),
    h('div', { class: 'share-detail' },
      h('span', { class: 'mono big' }, `${view.res}p · ${view.fps} fps · ${formatMbps(view.mbps)} Mbps`),
      h('div', { class: 'row' }, h('span', {}, 'Uso do seu upload'),
        h('span', { class: 'mono' }, `${formatMbps(view.mbps)} de ~${SHARE_UPLOAD_MBPS} Mbps`)),
      h('div', { class: 'upload', role: 'img', 'aria-label': `Uso do upload: ${formatMbps(Math.round(pct * 10) / 10)}%` },
        h('div', { style: `width:${pct.toFixed(1)}%` }))),
    d.error && h('div', { class: 'field-error', role: 'alert', id: 'share-error' }, icon('i-alert', 14), d.error),
    h('div', { class: 'dialog-foot' },
      view.loaded && view.noEncoder
        ? h('span', { class: 'enc-warn', role: 'note' }, icon('i-alert', 16),
          'Sem codificação na placa de vídeo: limite de 720p a 30 fps')
        : (chip && h('span', { class: 'enc-info' }, h('span', { class: 'kbd encoder' }, chip), 'Codificação na placa de vídeo')),
      h('div', { class: 'dialog-actions' },
        !view.loaded && d.error && h('button', {
          type: 'button', class: 'btn', 'data-key': 'share-retry', onclick: () => { d.error = null; requestSources(); renderShare(); },
        }, 'Tentar de novo'),
        h('button', { type: 'button', class: 'btn', 'data-key': 'share-cancel', onclick: closeShare }, 'Cancelar'),
        h('button', {
          type: 'button', class: 'btn primary', 'data-key': 'share-start', disabled: !view.canStart || !linked(),
          onclick: () => start(view),
        }, d.pending ? 'Iniciando…' : 'Começar a transmitir'))));
    return dialog;
  }

  // Foco preso no diálogo; Esc fecha.
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && d.open) {
      e.preventDefault();
      closeShare();
    }
  });
  modal.addEventListener('keydown', (e) => {
    if (e.key !== 'Tab') return;
    const items = [...modal.querySelectorAll('button:not(:disabled), input:not(:disabled), [tabindex]:not([tabindex="-1"])')];
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && (document.activeElement === first || !modal.contains(document.activeElement))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (document.activeElement === last || !modal.contains(document.activeElement))) {
      e.preventDefault();
      first.focus();
    }
  });
  modal.addEventListener('mousedown', (e) => {
    if (e.target === modal) closeShare();
  });

  function renderShare() {
    if (!d.open) {
      modal.hidden = true;
      modal.replaceChildren();
      if (wasOpen) {
        wasOpen = false;
        if (opener && opener.isConnected && !opener.disabled) opener.focus();
        opener = null;
      }
      return;
    }
    const active = document.activeElement;
    const key = modal.contains(active) && active.dataset ? active.dataset.key : null;
    modal.replaceChildren(build());
    modal.hidden = false;
    const target = (key && modal.querySelector(`[data-key="${CSS.escape(key)}"]:not(:disabled)`))
      || modal.querySelector(`[data-key="share-tab:${d.tab}"]`);
    // Ao abrir, o foco começa na aba ativa; depois só volta ao que o tinha.
    if (!wasOpen || key) target.focus();
    wasOpen = true;
  }

  return { openShare, closeShare, renderShare, requestSources };
}
