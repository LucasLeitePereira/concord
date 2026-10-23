// Tela Ajustes: "Seu nome" e "Amigos". Só envia comandos; o estado vem do `Config` do núcleo (D-010).

import { friends, initial } from './state.js';

export function initSettings({ state, send, h, icon }) {
  const $ = (id) => document.getElementById(id);
  let confirming = null; // { kind: 'remove' | 'reset', name }
  let pendingAdd = null; // nome enviado, aguardando o Config novo
  let shownName = null; // último Config.name colocado no campo

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

  function renderSettings() {
    const cfg = state.config;
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
  }

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

  return { renderSettings, focusAddFriend };
}
