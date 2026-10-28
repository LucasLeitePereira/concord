// Aba Amigos: lista, rede Tailscale e "Adicionar pelo IP". Só envia comandos; o estado vem do núcleo (D-010).

import { friends, friendPresence, avatarIndex, initial, isTailscaleIp } from './state.js';

export function initFriends({ state, send, h, icon }) {
  const $ = (id) => document.getElementById(id);
  let confirming = null; // { kind: 'remove' | 'reset', name }
  let pendingIp = null; // nome enviado pelo formulário de IP, aguardando o Config novo
  let pendingTn = null; // apelido enviado pela lista da rede
  const nicks = {}; // endereço -> apelido digitado (sobrevive às re-renderizações)
  let tnSignature = '';

  const linked = () => state.link === 'connected';
  const address = (f) => (f.port ? `${f.address}:${f.port}` : f.address);

  // Recria `box` e devolve o foco ao elemento (por data-key) que o tinha.
  function replaceKeepingFocus(box, children) {
    const active = document.activeElement;
    const key = active && active.dataset && box.contains(active) ? active.dataset.key : null;
    box.replaceChildren(...children);
    if (key) {
      const again = box.querySelector(`[data-key="${CSS.escape(key)}"]`);
      if (again && again !== active) again.focus();
    }
  }

  function setError(input, box, message) {
    box.replaceChildren(...(message ? [icon('i-info', 14), message] : []));
    if (message) input.setAttribute('aria-invalid', 'true');
    else input.removeAttribute('aria-invalid');
  }

  function setOk(box, message) {
    box.replaceChildren(...(message ? [icon('i-check', 15), message] : []));
  }

  // ---------- Seus amigos ----------

  function ask(kind, name) {
    confirming = { kind, name };
    renderFriendsView();
    const btn = document.querySelector('[data-key="confirm-yes"]');
    if (btn) btn.focus();
  }

  function cancel(returnKey) {
    confirming = null;
    renderFriendsView();
    const back = document.querySelector(`[data-key="${CSS.escape(returnKey)}"]`);
    if (back) back.focus();
  }

  function confirmBox(f) {
    const remove = confirming.kind === 'remove';
    const question = remove
      ? `Remover ${f.name} da sua lista de amigos?`
      : `Resetar a identidade de ${f.name}? Use se o amigo reinstalou ou trocou de PC; a próxima conexão confiará na identidade nova.`;
    return h('div', { class: 'confirm', role: 'group', 'aria-label': 'Confirmação' },
      h('span', {}, question),
      h('button', {
        type: 'button', class: `btn ${remove ? 'danger' : ''}`, 'data-key': 'confirm-yes',
        onclick: () => {
          confirming = null;
          send(remove ? { type: 'RemoveFriend', name: f.name } : { type: 'ResetFingerprint', name: f.name });
          renderFriendsView();
        },
      }, remove ? 'Remover' : 'Resetar'),
      h('button', {
        type: 'button', class: 'btn',
        onclick: () => cancel(`${remove ? 'rm' : 'reset'}:${f.name}`),
      }, 'Cancelar'));
  }

  function presencePill(pres) {
    if (pres.danger) return h('span', { class: 'pill danger' }, pres.text);
    const cls = pres.key === 'sala' ? 'pill live' : pres.key === 'online' ? 'pill' : 'pill off';
    return h('span', { class: cls }, h('span', { class: `dot${pres.key === 'off' ? ' off' : ''}`, 'aria-hidden': 'true' }), pres.text);
  }

  function friendRow(f) {
    const pres = friendPresence(state, f.name);
    const peer = state.peers[f.name];
    const outside = state.outsideTailnet[f.name] && !(peer && peer.connected);
    const on = pres.key !== 'off';
    const mine = confirming && confirming.name === f.name;
    const seal = f.verified
      ? h('span', { class: 'pill small' }, icon('i-shield', 13), 'Identidade verificada')
      : h('span', { class: 'pill small off' }, 'Ainda não conectou');
    return h('div', { class: 'row', role: 'listitem' },
      h('div', { class: `avatar lg av-${avatarIndex(f.name)}${on ? '' : ' off'}`, 'aria-hidden': 'true' },
        initial(f.name), h('span', { class: `badge${on ? ' on' : ''}` })),
      h('div', { class: 'row-body' },
        h('div', { class: 'row-line' },
          h('span', { class: `row-title${on ? '' : ' dim'}` }, f.name), seal),
        h('span', { class: 'row-sub mono' }, address(f)),
        outside && h('div', { class: 'notice' }, icon('i-alert', 15),
          h('span', {}, h('strong', {}, 'Fora da sua rede Tailscale.'), ' ',
            h('span', { class: 'muted' }, 'Para conectar, vocês precisam estar na mesma tailnet, ou ele precisa compartilhar a máquina com você.')))),
      presencePill(pres),
      h('div', { class: 'row-actions' },
        f.verified && h('button', {
          type: 'button', class: 'btn-text', 'data-key': `reset:${f.name}`,
          title: 'Use se o amigo reinstalou ou trocou de PC',
          'aria-label': `Resetar identidade de ${f.name}. Use se o amigo reinstalou ou trocou de PC`,
          onclick: () => ask('reset', f.name),
        }, 'Resetar identidade'),
        h('button', {
          type: 'button', class: 'btn-text danger', 'data-key': `rm:${f.name}`,
          'aria-label': `Remover ${f.name}`,
          onclick: () => ask('remove', f.name),
        }, 'Remover')),
      mine && confirmBox(f));
  }

  function renderMain() {
    const list = friends(state);
    if (confirming && !list.some((f) => f.name === confirming.name)) confirming = null;
    let node;
    if (!state.config) {
      node = null;
    } else if (list.length === 0) {
      node = h('section', { class: 'empty big', 'aria-labelledby': 'sec-vazio' },
        h('div', { class: 'state-icon', 'aria-hidden': 'true' }, icon('i-users', 24)),
        h('h2', { class: 'empty-title', id: 'sec-vazio' }, 'Nenhum amigo ainda'),
        h('p', { class: 'empty-text' }, 'O Concord liga seu PC direto ao dos seus amigos usando o Tailscale. Para adicionar alguém, vocês dois precisam estar na mesma rede do Tailscale (a tailnet). Quem estiver nela aparece logo abaixo. Você também pode adicionar pelo IP.'));
    } else {
      node = h('section', { class: 'panel', 'aria-labelledby': 'sec-seus' },
        h('div', { class: 'panel-head inline' },
          h('h2', { id: 'sec-seus' }, 'Seus amigos'),
          h('span', { class: 'sub' }, list.length === 1 ? '1 amigo' : `${list.length} amigos`)),
        h('div', { class: 'list', role: 'list', 'aria-labelledby': 'sec-seus' }, list.map(friendRow)));
    }
    replaceKeepingFocus($('friends-main'), node ? [node] : []);
  }

  // ---------- Na sua rede Tailscale ----------

  function requestTailnet() {
    if (!linked()) return;
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
    const err = $('tn-error');
    setOk($('tn-ok'), '');
    if (!nick) {
      err.replaceChildren(icon('i-info', 14), 'Dê um apelido para esse amigo.');
      return;
    }
    if (friends(state).some((f) => f.name === nick)) {
      err.replaceChildren(icon('i-info', 14), `Já existe um amigo chamado ${nick}. Escolha outro apelido.`);
      return;
    }
    err.replaceChildren();
    pendingTn = nick;
    send({ type: 'AddFriend', name: nick, address: p.address });
  }

  function tailnetRow(p, friend) {
    const owner = p.owner || (p.mine ? 'você' : null);
    const sub = [];
    if (owner) sub.push(`de ${owner}`, h('span', { 'aria-hidden': 'true' }, '·'));
    sub.push(h('span', { class: 'mono' }, p.address), h('span', { 'aria-hidden': 'true' }, '·'),
      h('span', { style: 'display:inline-flex;align-items:center;gap:6px' },
        h('span', { class: `dot${p.online ? '' : ' off'}`, 'aria-hidden': 'true' }), p.online ? 'Online' : 'Offline'));
    const nickId = `apelido-${p.address.replace(/[^0-9a-z]/gi, '-')}`;
    const action = friend
      ? h('span', { class: 'row-static' }, icon('i-check', 15), 'Já é seu amigo')
      : h('div', { class: 'row-add' },
        h('div', { class: 'field' },
          h('label', { for: nickId }, 'Apelido'),
          h('input', {
            class: 'input', type: 'text', id: nickId, maxlength: '40', 'data-key': `tn-nick:${p.address}`,
            value: nicks[p.address] ?? suggestedNick(p),
            oninput: (e) => { nicks[p.address] = e.target.value; },
            onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); addFromTailnet(p); } },
          })),
        h('button', {
          type: 'button', class: 'btn', 'data-key': `tn-add:${p.address}`,
          'aria-label': `Adicionar ${p.name} como amigo`,
          disabled: !linked(),
          onclick: () => addFromTailnet(p),
        }, 'Adicionar'));
    return h('div', { class: 'row', role: 'listitem' },
      h('div', { class: 'row-icon', 'aria-hidden': 'true' }, icon('i-screen', 20)),
      h('div', { class: 'row-body' },
        h('span', { class: 'row-title mono' }, p.name),
        h('span', { class: 'row-sub' }, sub)),
      action);
  }

  function skeletonRow() {
    return h('div', { class: 'skeleton', 'aria-hidden': 'true' },
      h('i', { class: 'sk-icon' }), h('div', { class: 'sk-lines' }, h('i', { class: 'sk-1' }), h('i', { class: 'sk-2' })));
  }

  // Só recria a lista quando algo dela muda: os campos de apelido não perdem o foco nem o texto.
  function renderTailnet() {
    const tn = state.tailnet;
    const list = friends(state);
    const sig = JSON.stringify([tn, list.map((f) => [f.name, f.address]), state.link]);
    if (sig === tnSignature) return;
    tnSignature = sig;
    const loading = !tn || !!tn.loading;
    $('tn-scan').disabled = !linked() || loading;
    $('sec-rede-box').setAttribute('aria-busy', String(loading));
    let rows;
    if (loading) {
      rows = [h('div', { class: 'list' },
        h('p', { class: 'hint-text', role: 'status', style: 'font-size:13px' }, 'Procurando dispositivos na sua rede…'),
        skeletonRow(), skeletonRow(), skeletonRow())];
    } else if (!tn.running) {
      rows = [h('div', { class: 'empty error', role: 'alert' },
        h('div', { class: 'state-icon', 'aria-hidden': 'true' }, icon('i-alert', 22)),
        h('span', { class: 'empty-title' }, 'Tailscale desligado ou não encontrado'),
        h('span', { class: 'empty-text' }, 'Abra o Tailscale e confira se você está conectado. Sem ele, ninguém consegue entrar na sua sala.'),
        h('button', { type: 'button', class: 'btn', onclick: requestTailnet, disabled: !linked() }, 'Tentar de novo'))];
    } else if (!tn.peers.length) {
      rows = [h('div', { class: 'empty' },
        h('div', { class: 'state-icon', 'aria-hidden': 'true' }, icon('i-screen', 22)),
        h('span', { class: 'empty-title' }, 'Nenhum outro dispositivo na sua rede Tailscale'),
        h('span', { class: 'empty-text' }, 'Peça para seu amigo entrar na mesma tailnet ou compartilhar a máquina dele com você. Depois, clique em Atualizar.'))];
    } else {
      rows = [h('div', { class: 'list', role: 'list' },
        tn.peers.map((p) => tailnetRow(p, list.find((f) => f.address === p.address))))];
    }
    replaceKeepingFocus($('tn-list'), rows);
  }

  $('tn-scan').addEventListener('click', requestTailnet);

  // ---------- Adicionar pelo IP ----------

  const nome = $('ip-nome');
  const addr = $('ip-addr');
  nome.addEventListener('input', () => { setError(nome, $('ip-nome-erro'), ''); setOk($('ip-ok'), ''); });
  addr.addEventListener('input', () => { setError(addr, $('ip-addr-erro'), ''); setOk($('ip-ok'), ''); });

  $('friend-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const name = nome.value.trim();
    const ip = addr.value.trim();
    let nameErr = '';
    let ipErr = '';
    if (!name) nameErr = 'Dê um nome para esse amigo.';
    else if (friends(state).some((f) => f.name === name)) nameErr = `Já existe um amigo chamado ${name}. Escolha outro nome.`;
    if (!ip) ipErr = 'Digite o IP do Tailscale do seu amigo.';
    else if (!isTailscaleIp(ip)) ipErr = 'Esse IP não é da rede Tailscale. Os endereços do Tailscale começam com 100.';
    setError(nome, $('ip-nome-erro'), nameErr);
    setError(addr, $('ip-addr-erro'), ipErr);
    setOk($('ip-ok'), '');
    if (nameErr || ipErr) {
      (nameErr ? nome : addr).focus();
      return;
    }
    pendingIp = name;
    send({ type: 'AddFriend', name, address: ip });
  });

  function renderFriendsView() {
    const list = friends(state);
    if (pendingIp && list.some((f) => f.name === pendingIp)) {
      setOk($('ip-ok'), `${pendingIp} foi adicionado aos seus amigos.`);
      pendingIp = null;
      nome.value = '';
      addr.value = '';
    }
    if (pendingTn && list.some((f) => f.name === pendingTn)) {
      setOk($('tn-ok'), `${pendingTn} foi adicionado aos seus amigos.`);
      pendingTn = null;
    }
    $('fr-add').disabled = !linked();
    renderMain();
    renderTailnet();
  }

  return { renderFriendsView, requestTailnet };
}
