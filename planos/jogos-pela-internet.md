# Plano: transmissão de jogos pela internet

## Contexto

Vídeos em 1080p60 já estão aprovados. O que falta é jogo: no teste com God of War 2 pela internet, 1080p60 ficou em 21–39 fps com muitos quadros descartados, enquanto 720p a 30 fps ficou bom. Este plano junta o que você pediu para considerar: a pesquisa na internet, a aceleração de hardware e essa análise. O estudo completo, com fontes, continua em `planos/travamentos-transmissao.md`.

**Objetivo:** transmitir jogo pela internet sem travamentos longos, mantendo vídeo e voz como estão.

**Mudança de ordem:** o estudo começava pela fila (T-037c). O teste com jogo mostrou que o maior ganho, e o mais barato, é a proteção dos quadros comuns, então ela vem primeiro.

## O que conferi no código

- **FEC:** o receptor já recupera qualquer quadro, porque o `fec_group` vem em cada pacote (`Reassembler`, `sala/crates/video/src/fragment.rs`). Só o envio limita a keyframes (`sala/crates/video/src/sender.rs:383`). Não precisa mudar o protocolo, e só quem transmite precisa atualizar.
- **Custo de cada perda:** ~28 quadros incompletos por minuto viram ~266 descartados, cerca de 9 por perda, e cada um pede um keyframe, ~9× maior que um quadro comum.
- **RTT do ABR:** já é medido no transmissor (`sala/crates/core/src/session.rs:1232`), o lugar certo.
- **Filas:** o Windows concedeu os 4 MiB de `SO_SNDBUF` pedidos (visto no log deste notebook). O maior quadro registrado aqui tem 244 KB, então 512 KiB cabem um keyframe.
- **Tela de transmissão:** abre em 1080p60 por padrão (`sala/crates/ui/web/js/state.js:35`).

## Fatias

Uma por vez, cada uma com commit local. O `pleno` implementa e roda só os testes afetados; o `junior` faz a rodada completa no fim de cada fatia.

### 1. T-038: FEC nos quadros comuns

- **Decisão D-035** em `ontologia/decisoes.md`, antes do código, com o custo de ~12,5% a mais de dados.
- Em `sender.rs:383`, aplicar `cfg.fec_group` a todos os quadros, e atualizar o comentário de `SenderConfig`.
- Teste com controle negativo, no modelo de `fec_saves_most_keyframes_under_two_percent_loss` (`fragment.rs:599`): com a mesma perda semeada, deltas com FEC sobrevivem mais que sem FEC.
- **Limite:** a perda real parece vir em rajadas, e o XOR recupera só um pacote por grupo. O ganho real sai da comparação entre `fec_recovered` e `incomplete` no log.

### 2. T-037c: limitar a fila

Ataca os travamentos de segundos, com RTT de até 9,6 s.

- **Decisão D-036.**
- **Corte por atraso:** em `Abr::on_report` (`sala/crates/video/src/abr.rs:83`), novo `RTT_DECREASE_MS = 800`: dois relatórios seguidos acima disso cortam a taxa como a perda, respeitando `DECREASE_COOLDOWN_MS`. Testes: pico isolado não corta; RTT sustentado corta.
- **Filas menores:** `datagram_send_buffer_size` de 512 KiB em `transport()` (`sala/crates/core/src/net.rs:317`) e `SO_SNDBUF` de 512 KiB em `tune_socket_buffers` (`net.rs:334`). `SO_RCVBUF` continua em 4 MiB.
- **Contadores em `share stats tx`:** descartes reais de envio, a partir de `datagrams` e `datagram_frames_tx`, e atraso de agendamento (thread que dorme 1 ms e registra quanto acordou atrasada).
- **Risco:** o áudio usa a mesma conexão, e o quinn descarta os datagramas mais antigos quando o buffer enche. Se a voz piorar no teste, o buffer volta a crescer.

### 3. T-039: orientação na tela de transmissão

Na janela de transmissão (`sala/crates/ui/web/js/share.js`), um aviso curto: para jogos, use 720p a 30 fps e limite o fps do jogo. O padrão de 1080p60 continua, porque vídeo está aprovado nele.

### 4. Teste real

Exige novo instalador. **Push e release só com seu OK na hora.**

Mesmo protocolo do teste anterior, olhando só o log de quem assiste:

| Cenário | Antes | Meta |
|---|---|---|
| 1080p60, jogo limitado a 60 | 28 incompletos e 266 descartados por minuto | metade ou menos |
| 720p a 30 fps, jogo limitado a 60 | 27 fps, 6 descartados por minuto | sem piora |
| YouTube em 1080p60 | aprovado | sem piora |
| Voz na chamada | sem cortes audíveis | sem piora |

No transmissor que eu puder ler (eunice): RTT abaixo de ~1 s e imagem de volta em menos de 1 s depois de uma queda.

### 5. Diagnósticos sem código

Não dependem das fatias e podem ser feitos a qualquer hora:

1. **Jogar sem transmitir:** ele joga o mesmo trecho só em chamada. Decide se as quedas de 10 s (19:04) e a desconexão (18:28) vêm do Concord.
2. **Aceleração de hardware do Chrome:** desligar no PC que transmite, repetir 1080p60 com vídeo e mouse, e religar depois.
3. **LatencyMon e CPU por processo** no eunice, durante uma queda. Substitui o teste de cabo.
4. **Polling do mouse e driver Intel** no eunice.

### 6. Só se ainda faltar, conforme a medição

- **Mais proteção contra perda:** se `incomplete` não cair à metade, avaliar intra refresh do QSV ou intercalar os grupos de FEC.
- **Espaçar o envio de keyframes:** se a perda continuar concentrada nos keyframes, espalhar o envio em `drain` (`sala/crates/core/src/share.rs:511`).
- **T-037d:** recuperação mais rápida do ABR, que leva ~2 min para voltar do piso.
- **T-035f2 e prioridade de GPU:** se 1080p60 seguir abaixo de ~45 fps sem perda, ou se o diagnóstico 2 apontar a GPU. É a maior mudança, em fatias: captura em textura, conversão por shader, entrega ao QSV.

**Fora deste plano:** `DwmFlush` e `MinUpdateInterval`, porque o mouse sozinho não travou (55 fps), e trocar o controle de congestionamento do quinn.

## Ao fechar cada fatia

`coletar.py registrar`, `ontologia/` (decisões e `mapa-codigo.yaml`), `AGENTS.md` §6 e `planos/travamentos-transmissao.md`. O conhecimento novo vai pela skill `documentar`, com `docserver revisar` e `ingest` no fim da sessão.

## Verificação

- **Testes:** os novos de cada fatia, com controle negativo, e depois clippy e `cargo test --workspace --no-fail-fast`, com `sala-video --lib` usando `--test-threads=1`.
- **Comparação real:** a tabela da fatia 4, usando os mesmos campos de `share stats rx`.
- **Uma mudança por vez:** cada fatia é medida antes da próxima sempre que houver teste real disponível.
