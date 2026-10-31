# Estudo: travamentos da transmissão com mouse e jogos

**Status (2026-10-31):** este é o estudo, com as fontes. A execução e a ordem das fatias estão em `planos/jogos-pela-internet.md`, e o estado, no `AGENTS.md` §6. O usuário não tem cabo de rede, então o Wi-Fi do outro PC só pode ser isolado pelo LatencyMon. O teste com jogo pela internet está em "Resultado com jogo real".

## Resultado com jogo real (2026-10-30)

God of War 2 transmitido pelo amigo Lukeson, pela internet (Tailscale direto, 45 ms), visto neste notebook. Só o log de quem assiste; o log do transmissor não será pedido.

| Fase | Tempo | fps (média) | Incompletos/min | Descartados/min | Pacotes/s | Janelas sem perda |
|---|---|---|---|---|---|---|
| 1080p60, jogo sem limite (~200 fps; 80–110 transmitindo) | 5,7 min | 20,9 | 23,0 | 127,7 | 420 | 28% |
| 1080p60, jogo limitado a 60 | 22,2 min | 38,7 | 28,0 | 265,8 | 1.046 | 12% |
| 720p30, jogo limitado a 60 | 10,7 min | 27,3 de 30 | 5,1 | 5,8 | 487 | 84% |

- **Limitar o jogo quase dobrou o fps,** mas não reduziu a perda, e os descartes aumentaram junto com o volume.
- **720p a 30 fps ficou bom para jogo:** no mini chefe, 28–29 fps com mínimo de 25 e perda abaixo de 1%. Excluída a falha abaixo, é a configuração recomendada hoje.
- **Volume não explica tudo:** a primeira fase enviava 420 pacotes/s e perdia muito mais que a de 720p com 487. Keyframes maiores em 1080p e o jogo sem limite disputando a máquina são candidatos.
- **Falha sem causa conhecida:** às 19:04:10–19:04:25 a perda foi de 91–100%, por cerca de 10 s. Às 18:28 outra queda terminou em desconexão. Pode ser internet, PC ou a própria transmissão. Teste sugerido: jogar o mesmo trecho em chamada, sem transmitir.
- **Achado no código:** a FEC protege só keyframes (`SenderConfig`, `sala/crates/video/src/sender.rs:100`). Um pacote perdido em quadro comum faz os quadros seguintes serem descartados até o próximo keyframe. É o que transforma 2–3% de perda em 1 de cada 4 quadros descartados em 1080p.

## Aceleração de hardware (hipótese de um amigo do usuário)

Um amigo teve um problema parecido no Skype, e a causa era a aceleração de hardware. Situação do Concord:

- **Usa GPU:** captura (WGC), codificação (`h264_qsv`, obrigatória, sem modo por software), decodificação (D3D11VA, com plano B por software) e janela de quem assiste (wgpu).
- **Usa CPU:** cópia do quadro para a RAM e conversão de cor (swscale).
- **Não existe** opção no app para ligar ou desligar a aceleração.

A disputa pela GPU combina com o encode mais lento e com o Chrome travar mais que o terminal. Sozinha, não explica o swscale, que roda só na CPU, ficar até 5× mais lento.

**Teste sem código, ainda não feito:** no PC que transmite, desligar "Usar aceleração de hardware quando disponível" em `chrome://settings/system`, reiniciar o Chrome, repetir 1080p60 com vídeo e mouse, e religar depois. Se melhorar, ganham força a prioridade de GPU e a T-035f2.

## Contexto

No reteste da v0.1.10, vídeos (YouTube, vlogs, filmes) ficaram muito bons em 1080p60: qualidade e fps se mantiveram. O problema restante é específico: a transmissão trava quando o mouse se mexe muito no PC que transmite, e o comportamento com jogos ainda não foi medido.

Este documento é só estudo. Nenhum código foi alterado. A parte final propõe a ordem do que fazer, e cada fatia de código precisa da sua aprovação.

**Conclusão em uma frase:** não achei um caso idêntico ao nosso, mas os apps maduros resolvem isso de três formas, e duas delas o Concord ainda não usa: pipeline inteiro na GPU e fila curta com controle por atraso.

## São dois problemas, não um

| | Problema | Situação |
|---|---|---|
| 1 | **Por que o transmissor desacelera:** encode de 15 para 35–59 ms, swscale de 7 para 20–37 ms, captura de 4 para 10–16 ms, e erros no áudio ao mesmo tempo | Causa desconhecida. Afeta etapas diferentes de uma vez, então algo está tirando CPU do processo |
| 2 | **Por que uma desaceleração curta vira um travamento de segundos:** RTT de até 9,6 s sem perda | Causa de fila, com padrão de correção conhecido |

O problema 2 é o que você vê como "a live parou e voltou de repente". Dá para melhorá-lo mesmo sem resolver o 1.

## O que os apps maduros fazem

### 1. Pipeline inteiro na GPU (Sunshine)

Essa é a diferença mais clara em relação ao Concord. O [código do Sunshine](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_vram.cpp) copia a captura para uma textura compartilhada, converte a cor por shader, mistura o cursor na GPU e entrega ao encoder sem passar pela RAM.

O Concord copia 8 MB por quadro para a RAM e converte com swscale na CPU, justamente a etapa que ficou até 5× mais lenta nos travamentos. É a nossa T-035f2, agora validada como o caminho já estabelecido. O Sunshine também tem esse caminho com WGC (`display_wgc_vram_t`), a API de captura que usamos.

### 2. Prioridade de GPU (Sunshine, Looking Glass)

O [Sunshine](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_base.cpp) chama `D3DKMTSetProcessSchedulingPriorityClass` e `SetGPUThreadPriority(7)` para a captura e o encode não perderem a vez quando o jogo satura a GPU. O [Looking Glass](https://forum.level1techs.com/t/news-dxgi-capture-performance/160459) relatou zero quadros perdidos com GPU a 100% no teste do autor.

Ressalvas: pode exigir administrador, tem [bug conhecido com NVIDIA e HAGS](https://forums.developer.nvidia.com/t/nvenc-encoder-hangs-on-windows-when-using-d3d11-in-real-time-mode/357465), e a [API](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtsetprocessschedulingpriorityclass) é pouco documentada. Só ajuda quando a GPU está disputada, o que importa para jogos. No notebook medimos GPU a 7%, e o eunice ainda não foi medido.

### 3. Fila curta e controle por atraso (WebRTC)

- O WebRTC usa [controle híbrido de atraso e perda](https://stuff.mit.edu/afs/athena/reference/internet-drafts/draft-ietf-rmcat-gcc-02.xml): corta a taxa quando o atraso sobe, antes de haver perda.
- O [pacer tem limite de tempo de fila](https://chromium.googlesource.com/external/webrtc/+show/2b3a10e62d7e35525d2c7228ba4ad01d98042323/modules/pacing/g3doc/index.md). A regra escrita lá é que vale mais arriscar perda e recuperar com keyframe do que causar atraso severo.
- O encoder [pula quadros quando a fila prevista fica longa](https://codereview.webrtc.org/1412293003).

## Casos parecidos

- **Mais próximo: [Sunshine PR #164](https://github.com/LizardByte/Sunshine/pull/164) e [issue #227](https://github.com/loki-47-6F-64/sunshine/issues/227).** Mexer o mouse travava a transmissão inteira, e o travamento durava enquanto o mouse se movia. Esconder o cursor não resolvia. A correção foi chamar `DwmFlush()` antes de pegar o próximo quadro, alinhando a captura ao compositor. Prioridade, afinidade e MMCSS foram descartados por eles. **Limites:** é Desktop Duplication, não WGC, e a causa raiz nunca foi confirmada.
- **Wi-Fi: [draft BBR-realtime da IETF](https://www.ietf.org/archive/id/draft-huitema-ccwg-bbr-realtime-00.html).** Suspensões de 100–200 ms do Wi-Fi acumulam pacotes no driver, e o transmissor continua enchendo a fila até perceber. Encaixa no eunice, que está em Wi-Fi com RTT de segundos e sem perda, mas não está provado.
- **Latência de driver (DPC):** [guias](https://www.digitalcitizen.life/how-to-diagnose-and-fix-high-dpc-latency-in-windows/) descrevem exatamente este conjunto de sintomas: áudio com falhas, mouse engasgando e travamentos com CPU normal. Driver de [Wi-Fi](https://community.intel.com/t5/Wireless/Intel-WifiLink-4965-AGN-Driver-s-DPC-Latencies-causing-audio/m-p/203920) e [USB](https://macmyths.com/how-to-fix-wdf01000-sys-high-latency-issues/) são culpados comuns. Explicaria por que CPU, captura e áudio pioram juntos. Hipótese forte, sem medição.
- **Mouse com polling alto:** há [relatos de travamento](https://forums.tomshardware.com/threads/system-stutters-on-mouse-movement.3548244/?view=votes), mas a [Godot](https://godotengine.org/article/fixing-high-polling-rate-mice-on-windows/) diz que 1 kHz costuma ser inofensivo, salvo em CPU muito lenta por núcleo. Anedótico, mas o teste é barato.
- **Driver Intel:** [Sunshine #1614](https://github.com/LizardByte/Sunshine/issues/1614) registra regressão do Quick Sync em driver novo na UHD 770. Vale olhar a versão no eunice.
- **OBS e Discord:** com [encoder sobrecarregado](https://obsproject.com/forum/threads/encoding-overload.107689), a solução comum é limitar o fps do jogo. No Discord com iGPU, é [baixar resolução e fps](https://community.intel.com/t5/Intel-ARC-Graphics/Discord-Screen-Share-causes-FPS-loss-Update-workaround-solution/m-p/1678354). Seu teste em 720p vai na mesma direção.

**Sem resultado útil:** detalhes internos do Parsec, um caso exato de Windows + Quick Sync + mouse, e orientação específica para quinn.

## O que isso muda no diagnóstico do Concord

Conferi nosso código e encontrei três pontos:

1. **Correção do que eu disse antes:** falei que o ABR ignora atraso, e isso não é exato. RTT acima de 400 ms (`RTT_HOLD_MS`, `sala/crates/video/src/abr.rs:28`) bloqueia aumentos. O que falta é **cortar** a taxa por atraso.
2. **Buffer de datagramas:** `transport()` (`sala/crates/core/src/net.rs:317`) usa os padrões do quinn 0.11: controle Cubic e 1 MiB de buffer de datagramas. A 2 Mbit/s, isso são cerca de 4 s de fila. O Cubic não vê o atraso como sinal para o vídeo.
3. **Buffer do socket, nova suspeita:** pedimos 4 MiB de `SO_SNDBUF` (`SOCKET_BUFFER_BYTES`, `net.rs:330`), criado para o problema de recepção. A 2 Mbit/s são cerca de 17 s de fila possível. Ainda precisa de confirmação: o log mostra o valor concedido pelo Windows, e parte da fila pode estar no Tailscale ou no Wi-Fi.

## Próximos passos, em ordem

### Fatia agora: T-037c, limitar a fila (só código)

Não depende de você nem de cabo, porque ataca o problema 2. Delegar ao `pleno`; o teste com dois PCs fica para quando você voltar.

1. **Decisão D-0NN** em `ontologia/decisoes.md`, antes do código.
2. **Cortar por atraso:** em `Abr::on_report` (`sala/crates/video/src/abr.rs:83`), RTT acima de um limite em relatórios seguidos corta a taxa como a perda já corta, respeitando `DECREASE_COOLDOWN_MS`. Um pico isolado não corta.
3. **Filas menores:** `datagram_send_buffer_size` em `transport()` (`sala/crates/core/src/net.rs:317`) e `SO_SNDBUF` separado do `SO_RCVBUF`, que continua em 4 MiB. O tamanho precisa caber um keyframe.
4. **Contadores nos logs `share stats`:** descartes reais de envio e atraso de agendamento (uma thread dorme 1 ms e registra o quanto acordou atrasada). Esse segundo número aponta CPU ou driver travando a máquina sem precisar de LatencyMon em todo teste.
5. **Testes:** no ABR, com controle negativo (pico isolado não corta; RTT sustentado corta). Só os testes afetados pelo `pleno`; clippy e `cargo test --workspace` uma vez no fim, pelo `junior`.

**Risco a verificar:** o áudio usa a mesma conexão (`net.send_datagram`, `sala/crates/core/src/session.rs:947`), e o quinn descarta os datagramas mais antigos quando o buffer enche. Um buffer pequeno demais pode cortar áudio numa rajada de vídeo. O áudio hoje está bom e não pode piorar.

**Critério de pronto:** testes passando, commit local, sem push nem release. Depois, em nova queda no eunice: RTT abaixo de ~1 s e imagem de volta em menos de 1 s.

**Limite:** isso não deixa o transmissor mais rápido. O travamento longo vira uma queda curta de qualidade.

### Fatia nova: perda pela internet

FEC nos quadros comuns feita na T-038 (D-035). Se `incomplete` não cair à metade no teste real, avaliar keyframe intermediário (intra refresh) ou intercalar os grupos de FEC.

Também vale avaliar um aviso ou preset sugerindo 720p a 30 fps e limite de fps no jogo.

### Depois, com você: testes no eunice

Um de cada vez, em 1080p60, com vídeo e mouse:

1. **LatencyMon durante uma queda:** mostra se há pico de DPC e qual driver causou. Sem cabo, é o melhor substituto para isolar o Wi-Fi.
2. **CPU por processo durante uma queda:** mostra se outro processo compete com o Concord.
3. **Polling do mouse mais baixo,** se o mouse permitir.
4. **Versão do driver Intel** e especificações do eunice.

### Etapa 2: pipeline na GPU (T-035f2)

Só depois da T-037c, se os jogos justificarem. Reduz de verdade o consumo de CPU do transmissor, então é a parte do código que ataca o problema 1. É a maior mudança e precisa ser dividida em fatias: captura em textura, conversão por shader, entrega ao QSV. A prioridade de GPU entra aqui, com medição antes e depois.

### Etapa 3: teste com jogo real

Teste transmitindo do eunice, porque hoje não temos essa medição. `DwmFlush` e `MinUpdateInterval` ficam como opção só se o mouse continuar derrubando o fps. Sozinho, o mouse não travou (55 fps, encode de 19 ms), então não é prioridade.

### Registro

Ao sair do modo de plano: skill `documentar` em `docs/_pendentes/` com este estudo e o `AGENTS.md` §6 atualizado.

## Verificação

- Cada hipótese fica confirmada ou descartada por uma medição, sem correção por tentativa.
- Comparar os logs `share stats` de antes e depois: RTT máximo, duração das quedas, fps e descartes.
- Para vídeo, a qualidade atual não pode piorar: repetir o teste de YouTube em 1080p60 depois de cada mudança.
