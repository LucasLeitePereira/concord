// Teste de state.js sem DOM: node sala/crates/ui/tests/state.test.mjs
import assert from 'node:assert/strict';
import { initialState, reduce, setLink, friendPresence, callResultText, roomAction } from '../web/js/state.js';

// Botão principal: ninguém na sala → chama os online; alguém na sala → só entra
{
  const r = initialState();
  r.config = { friends: [{ name: 'ana' }, { name: 'bia' }, { name: 'caio' }] };
  assert.deepEqual(roomAction(r), { kind: 'join', friends: [] }); // ninguém online
  reduce(r, { type: 'PeerConnected', friend: 'ana' });
  reduce(r, { type: 'PeerStatus', friend: 'bia', in_room: false });
  assert.deepEqual(roomAction(r), { kind: 'call', friends: ['ana', 'bia'] }); // caio offline fica de fora
  reduce(r, { type: 'PeerStatus', friend: 'bia', in_room: true });
  assert.deepEqual(roomAction(r), { kind: 'join', friends: [] });
}

const s = initialState();
assert.deepEqual(s.calls, { incoming: [], outgoing: [] });

// Calls snapshot
assert.equal(reduce(s, { type: 'Calls', incoming: ['ana'], outgoing: ['bia'] }), 'full');
assert.deepEqual(s.calls, { incoming: ['ana'], outgoing: ['bia'] });
reduce(s, { type: 'Calls' }); // campos ausentes não quebram
assert.deepEqual(s.calls, { incoming: [], outgoing: [] });

// Resultado vira mensagem de sistema no chat, e sobrevive ao ChatHistory
assert.equal(reduce(s, { type: 'CallResult', friend: 'ana', result: 'declined' }), 'chat');
assert.equal(s.chat.at(-1).text, 'ana recusou a chamada');
assert.equal(s.chat.at(-1).system, true);
reduce(s, { type: 'CallResult', friend: 'ana', result: 'timeout' });
assert.equal(s.chat.at(-1).text, 'ana não atendeu');
reduce(s, { type: 'ChatHistory', entries: [{ id: 1, from_self: true, text: 'oi', sent_at: 1 }] });
assert.equal(s.chat.length, 3);
assert.equal(new Set(s.chat.map((e) => e.id)).size, 3);

// Variantes e resultados desconhecidos são ignorados
assert.equal(reduce(s, { type: 'CallResult', friend: 'ana', result: 'novo' }), 'none');
assert.equal(reduce(s, { type: 'Algo', x: 1 }), 'none');
assert.equal(callResultText('x', '???'), null);

// Presença: tocando vence "Online"; controle: sem toque pode chamar
const t = initialState();
reduce(t, { type: 'PeerConnected', friend: 'bia' });
assert.equal(friendPresence(t, 'bia').canCall, true);
reduce(t, { type: 'Calls', incoming: [], outgoing: ['bia'] });
const p = friendPresence(t, 'bia');
assert.equal(p.text, 'Chamando…');
assert.equal(p.canCall, false);
assert.equal(p.ringing, true);

// Reconectar ao núcleo zera as chamadas (o núcleo reenvia)
setLink(t, 'connected');
assert.deepEqual(t.calls, { incoming: [], outgoing: [] });

console.log('state.test.mjs: ok');
