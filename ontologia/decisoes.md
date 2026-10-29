# Decisões de arquitetura

Formato: `## D-NNN título` · **Conceitos:** ids · **Decisão** · **Alternativa descartada e porquê** · **Data**.

## D-001 Serialização: bincode no controle e na mídia, JSON por linha no IPC
**Conceitos:** control-message, media-datagram, ipc-message
**Decisão:** stream de controle com quadros `u32` big-endian de tamanho + payload `bincode` (serde). Datagramas também em `bincode`, porque o cabeçalho compacto cabe em 1200 bytes. IPC em JSON delimitado por linha, como manda a spec, porque facilita depurar a UI.
**Alternativa descartada:** JSON no controle (mais bytes e parse mais lento, sem ganho, já que só o núcleo lê) e postcard (menos difundido; bincode basta).
**Data:** 2026-10-22

## D-002 Runtime assíncrono: tokio
**Conceitos:** peer-connection, core-process
**Decisão:** `tokio` (multi-thread com poucos workers) no `sala-core`, porque o `quinn` depende dele. O áudio do `cpal` roda em thread própria e fala com o tokio por canais sem bloqueio.
**Alternativa descartada:** async-std/smol (o quinn é de primeira classe só no tokio).
**Data:** 2026-10-22

## D-003 Opus: crate `opus` 0.4 (opusic-sys), compilado com CMake
**Conceitos:** voice-frame
**Decisão:** usar a crate `opus`, que expõe FEC, DTX, bitrate e o modo VOIP. O libopus é compilado estaticamente pelo `opusic-sys`, que exige CMake no toolchain. Captura e reprodução com `cpal` 0.18.
**Alternativa descartada:** Opus em Rust puro (imaturo, sem FEC/DTX completos).
**Data:** 2026-10-22

## D-004 Desenvolvimento nativo no Windows
**Conceitos:** core-process
**Decisão:** construir e testar em Windows 11 nativo com toolchain MSVC. O produto depende de WGC, WASAPI, named pipes e da bandeja.
**Alternativa descartada:** WSL2 com cross-compile (não testa nenhuma API nativa).
**Data:** 2026-10-22

## D-005 Perfis de teste e porta opcional por amigo
**Conceitos:** profile, config, friend
**Decisão:**
- Sem `--profile`, os dados ficam em `%APPDATA%\Sala` e a porta é 47800.
- `--profile <nome>` usa `%APPDATA%\Sala\profiles\<nome>`. A porta padrão é `a`→47801, `b`→47802 e qualquer outro nome →47803. O nome aceita só `[a-z0-9-]`.
- `Friend` ganha o campo opcional `port` (padrão 47800). Sem ele, duas instâncias na mesma máquina não conseguem discar uma para a outra.
- Na 1ª execução, a config do perfil `a` vem com o amigo "b" em 127.0.0.1:47802, e vice-versa.
- A raiz `%APPDATA%` pode ser trocada pela variável `SALA_APPDATA` (usada em testes).
**Alternativa descartada:** porta única com IPs de loopback diferentes (127.0.0.2). No Windows isso exige configuração extra e não testa a porta configurável.
**Data:** 2026-10-22

## D-006 Jitter buffer: underrun re-bufferiza com PLC curto; DTX não abre buraco de seq
**Conceitos:** jitter-buffer, voice-frame, inv-jitter-40-120ms
**Decisão:**
- `seq` conta pacotes enviados. Pausa de DTX ou mudo não cria buraco de seq.
- `Lost` na vez é só para buraco conhecido: o seq da vez falta, mas existe seq maior no buffer.
- Underrun é o buffer vazio depois do início. Nele o `pop` não avança o seq e volta a bufferizar. Nos primeiros 3 ticks (60 ms) devolve `Lost`, para o PLC cobrir uma travada curta; depois devolve `Empty`, que é silêncio (fim de fala, DTX, mudo).
- A reprodução volta quando passa `target_delay_ms` desde a próxima chegada. Ela continua no próximo seq esperado, ou no menor seq bufferizado se ficou mais que a capacidade para trás. Pacote com seq já tocado é descartado como atrasado.
**Alternativas descartadas:**
- `Lost` indefinido no underrun (braço ONTO do T-004): avança o seq sem parar, então deriva de relógio ou pausa do remetente vira silêncio permanente.
- `Empty` imediato (braço CTRL): uma travada curta de rede corta o áudio seco, sem PLC.
**Data:** 2026-10-22

## D-007 Conexão QUIC: TLS mútuo com verificação própria, heartbeat 2 s, backoff 1→15 s
**Conceitos:** peer-connection, identity-fingerprint, friend, inv-only-friends-connect, inv-fingerprint-tofu, inv-simultaneous-conn-lowest-fp
**Decisão:**
- `quinn` 0.11 + `rustls` 0.23 com provedor `ring`, o mesmo do rcgen. ALPN `sala/1`.
- TLS mútuo: os dois lados apresentam o certificado autoassinado. Os verificadores próprios conferem só a assinatura do handshake. A confiança é decidida depois do handshake por `TrustStore::check_peer` (IP canônico + fingerprint do certificado do par). Recusa = fechar a conexão com código de aplicação.
- Entrada de IP desconhecido: recusar já no `Incoming` (`refuse()`), antes do handshake.
- Vida da conexão: keep-alive do QUIC a cada 2 s, `max_idle_timeout` de 6 s, e `ControlMessage::Heartbeat` a cada 2 s no stream de controle.
- Reconexão: quem discou volta a discar com backoff 1, 2, 4, 8, 15, 15… s, zerado ao conectar. A UI recebe `Reconnecting`.
- Stream de controle: um bidirecional, aberto por quem discou. A 1ª mensagem de cada lado é `Hello`.
**Alternativa descartada:** verificar o fingerprint dentro do verificador do rustls. O verificador não sabe qual amigo está do outro lado na entrada, e o TOFU precisa gravar estado.
**Data:** 2026-10-22

## D-008 Detector de jogo: casamento por nome de arquivo e override manual
**Conceitos:** game-detector, game-mode, inv-game-mode-hysteresis-2-3
**Decisão:**
- A amostra traz o caminho completo do executável (`QueryFullProcessImageNameW`). As listas `games`/`exclude` têm nomes de arquivo; o casamento é pelo nome inteiro do arquivo, sem diferenciar maiúsculas (nada de sufixo ou substring). Jogo listado é positivo mesmo se também estiver em `exclude`. Sem janela em primeiro plano = negativo.
- Entrada manual (atalho, bandeja, UI): liga na hora; a detecção automática nunca tira do modo jogo que foi ligado à mão.
- Saída manual: desliga na hora; a entrada automática fica suspensa até chegar uma leitura negativa, e só contam as positivas depois dela. Sem isso o jogo ainda em foco religaria o modo 4 s depois.
- Recarregar a config mantém estado e origem e zera as sequências. `auto_detect = false` ⇒ `tick` nunca muda o estado.
**Alternativa descartada:** desligar a detecção automática após qualquer ação manual (o usuário teria de religar à mão e esqueceria).
**Data:** 2026-10-22

## D-009 Camada Win32: windows-sys, uma thread de plataforma, sem hooks
**Conceitos:** core-process, game-detector, game-mode
**Decisão:**
- `windows-sys` 0.61 (já está no lock por dependência transitiva): FFI cru, sem WinRT. Nada de `tray-icon`/`muda`/`notify-rust`/`windows`.
- Uma thread de plataforma com janela oculta (não message-only, para receber `TaskbarCreated` e recolocar o ícone se o Explorer reiniciar) e laço `GetMessageW`. Ela recebe `WM_HOTKEY`, o callback da bandeja e comandos de outras threads (`PostMessageW` + canal). Eventos saem por canal para o núcleo.
- Atalhos de teclado: `RegisterHotKey` com `MOD_NOREPEAT`. Botões do mouse (`Mouse4`/`Mouse5`) e push-to-talk (precisa soltar a tecla): sondagem `GetAsyncKeyState` a cada 15 ms, só enquanto o modo PTT estiver ativo. Falha ao registrar (combinação ocupada) é logada e informada à UI.
- Janela em primeiro plano: `GetForegroundWindow` → PID → `QueryFullProcessImageNameW`; `covers_monitor` = retângulo da janela ⊇ `rcMonitor` de `MonitorFromWindow`. Desktop/shell (`Progman`, `WorkerW`, `Shell_TrayWnd`) = sem janela.
- Notificação: `Shell_NotifyIconW` com `NIF_INFO` (o Windows 10/11 mostra como toast), sem AUMID. Clique na notificação (`NIN_BALLOONUSERCLICK`) = "Abrir o chat" = saída manual do modo jogo.
- Menu da bandeja (`TrackPopupMenu`) na ordem de `ModoJogo.dc.html`: cabeçalho, linha da chamada, Mutar (Ctrl+Shift+M), Ensurdecer (Ctrl+Shift+D), Entrar/Sair do modo jogo (Ctrl+Shift+G), Abrir a Sala, separador, Fechar a Sala. Novo atalho de config `toggle_deafen = "Ctrl+Shift+D"`, que o design pede.
**Alternativas descartadas:** hooks de baixo nível `WH_KEYBOARD_LL`/`WH_MOUSE_LL` (a spec proíbe hooks, e um hook lento atrasa a entrada do sistema inteiro, inclusive do jogo); Raw Input com `RIDEV_INPUTSINK` (acorda o processo a cada movimento do mouse, até 1000×/s no jogo); toast WinRT (crate `windows` pesada e exige AUMID registrado por atalho no Menu Iniciar).
**Data:** 2026-10-22

**Adendo (T-012, 2026-10-22):** clique esquerdo na bandeja = Abrir a Sala; rótulos do menu dinâmicos (Mutar/Desmutar, Ensurdecer/Voltar a ouvir, Entrar/Sair do modo jogo; "Fora de chamada" sem chamada); `Mouse4`/`Mouse5` também servem como atalho de ação (sondagem só ligada enquanto houver PTT ou atalho de mouse); `set_ptt(None)` com a tecla pressionada emite `PttReleased`. Falha ao ler o exe com janela válida ⇒ amostra com `exe_path` vazio (jogo protegido em tela cheia ainda conta).

## D-010 IPC e ciclo de vida da UI
**Conceitos:** ipc-message, core-process, ui-process, inv-game-mode-only-core
**Decisão:**
- Pipe `\.\pipe\sala-core-<USERNAME>`; com `--profile p`, `\.\pipe\sala-core-<USERNAME>-p` (duas instâncias na mesma máquina). Servidor `tokio::net::windows::named_pipe` com `first_pipe_instance(true)` (detecta núcleo já rodando) e `reject_remote_clients(true)`.
- Vários clientes ao mesmo tempo; eventos `CoreToUi` vão para todos. Linha inválida ⇒ `CoreToUi::Error` e a conexão segue.
- O núcleo lança `sala-ui.exe` (ao lado de `sala-core.exe`, ou `SALA_UI_EXE`) passando `--pipe <nome>`. Fechar a UI: `Shutdown` pelo pipe, esperar o processo terminar por até 2 s e, se não terminar, matá-lo; só então o modo jogo é considerado ativo. Sem executável da UI: aviso no log e o núcleo segue (até a F3).
**Alternativa descartada:** TCP local (exposto a outros usuários e ao firewall; a spec pede named pipe).
**Data:** 2026-10-22

**Adendo (T-011, 2026-10-22):** `AlreadyRunning` = `PermissionDenied` ao criar a 1ª instância. Linha > 64 KiB desconecta só aquele cliente; `Error` de linha inválida tem texto fixo (nunca ecoa conteúdo). A UI pede o estado inicial com `GetState`.

## D-011 Notificação de chat no modo jogo sem o texto
**Conceitos:** chat-message, game-mode, inv-chat-never-on-disk
**Decisão:** a notificação mostra só o remetente ("Pedro" · "Nova mensagem no chat"). O Windows grava as notificações no histórico da Central de Ações (`wpndatabase.db`), então o texto iria a disco. Escolha do usuário em 2026-10-22, contra o design, que mostrava o texto.
**Alternativa descartada:** texto completo (fere a restrição) ou configurável (complexidade sem pedido).
**Data:** 2026-10-22

## D-012 Orquestração do núcleo e sequência do modo jogo
**Conceitos:** core-process, game-mode, game-detector, call, ipc-message, inv-game-mode-only-core, inv-deafen-implies-mute
**Decisão:**
- `Session` ganha uma API de controle (comandos por canal para o laço, que é dono do `VoiceEngine`): mudo, ensurdecer, PTT, modo jogo. Toda mudança de mudo/ensurdecer/modo jogo envia `Status` a todos os amigos conectados. Ela também emite eventos (`SessionEvent`) para a camada de cima: chat recebido, conectado, desconectado, reconectando, `Status` do amigo, fingerprint divergente.
- Um único orquestrador (`app.rs`) liga Session + IPC + UiProcess + Platform + GameDetector. Atalho, menu da bandeja e comando IPC passam pelo mesmo caminho. `main.rs` continua fino.
- Entrar no modo jogo, nesta ordem: (1) `Shutdown` à UI e confirmar término (D-010); (2) fechar o visualizador (F4; ponto de extensão); (3) parar a transmissão, salvo `keep_sharing_in_game_mode` (F4); (4) liberar memória: soltar caches e devolver o working set com `SetProcessWorkingSetSize(proc, usize::MAX, usize::MAX)`; (5) `Status` aos amigos; (6) bandeja em âmbar. Sair: lançar a UI, `Status`, bandeja normal.
- No modo jogo, chat recebido ⇒ notificação só com o remetente (D-011). Clicar na notificação ⇒ saída manual do modo jogo (D-008).
- Detector: `probe()` a cada `POLL_INTERVAL`, só com `auto_detect`. Transição do detector ou ação manual chama a mesma sequência.
- 2ª instância: `AlreadyRunning` no pipe ⇒ conectar como cliente, enviar `UiToCore::OpenUi` e sair com código 0. O núcleo existente sai do modo jogo (manual) e abre a UI.
- "Fechar a Sala": encerra a UI (D-010), a sessão (Bye/close), a plataforma e o processo.
**Alternativa descartada:** cada fonte (atalho, bandeja, IPC) mexer direto no estado; isso duplicaria a lógica e quebraria `inv-deafen-implies-mute` em algum caminho.
**Data:** 2026-10-22
**Adendo (T-013, 2026-10-22):** "chamada atual" = 1º amigo conectado (MVP de 2 pessoas; `GetChatHistory`/`SendChat` não levam amigo). `Call`/`HangUp`/`OpenViewer` respondem `Error` até F3/F4. `SendChat` ecoa `ChatReceived{from_self:true}` a quem enviou. Novos `CoreToUi`: `PeerConnected`, `PeerDisconnected`, `PeerReconnecting`, `PeerStatus`, `FingerprintMismatch`; novos `UiToCore`: `OpenUi`, `ExitGameMode`. A UI real deve ignorar variantes desconhecidas de `CoreToUi` (o protocolo cresce só por adição). Logs com `tracing-appender` (leve: crossbeam-channel, symlink, time; faz a retenção de 14 dias), em `<data_dir>/logs/`, diretório criado antes. `Connected` repetido (conexão substituta) não zera histórico nem par de voz; só reenvia Hello/Status.
**Adendo (T-015, 2026-10-22):** toda transição do modo jogo leva um `Trigger` (`auto (<exe>)`, `hotkey`, `menu`, `tray_click`, `notification`, `ipc`, `second_instance`), logado em "entering/leaving game mode trigger=…" e registrado como último passo (`Step::Cause`), sem mudar a ordem da sequência. Falha de atalho: um único `warn` (no orquestrador), com ação e atalho legíveis. Log em hora local com offset (`tracing-subscriber` `local-time`).

## D-013 Nomes de mudo e ensurdecer na interface
**Conceitos:** voice-activation, call, inv-deafen-implies-mute
**Decisão:** os textos para o usuário usam os nomes convencionais: **Mutar-se / Desmutar-se** (microfone) e **Silenciar / Voltar a ouvir** (áudio de entrada dos amigos; antes "Ensurdecer"). Estado: "mutado" / "silenciado". Vale para bandeja (menu e tooltip), UI (F3) e textos de ajuda; prevalece sobre `DESIGN.md`, `Ajustes.dc.html`, `Main.dc.html` e `ModoJogo.dc.html`, que ainda dizem "Ensurdecer". Identificadores de código, chaves de config (`toggle_deafen`) e campos do protocolo (`deafened`) não mudam.
**Alternativa descartada:** manter "Ensurdecer" do design (pouco usado em PT-BR; escolha do usuário em 2026-10-22).
**Data:** 2026-10-22

## D-014 Volume de entrada e limiar de voz ajustáveis
**Conceitos:** voice-activation, config
**Decisão:**
- Novo `[audio] input_volume` (%, 0–200, padrão 100): ganho de software aplicado ao PCM capturado **antes** da ativação por voz e do encoder, com corte em [-1, 1]. Não mexe no volume do dispositivo no Windows (seria global e afetaria outros apps).
- `vad_threshold` padrão passa de −45 para **−50 dB**: no teste manual com Blue Snowball, −45 exigia falar alto ou muito perto. Configs já criadas mantêm o valor que têm.
- O log periódico de `voice tx` traz o nível médio e o pico do microfone (dBFS, depois do ganho) para calibrar o limiar sem UI.
- F3: a tela Ajustes ganha o controle "Volume de entrada" ao lado do medidor "Nível do microfone" (que já mostra a linha do limiar), ambos aplicados ao vivo pelo núcleo via IPC.
- Implementação (T-015): `input_volume` > 200 é limitado a 200 com `warn`; o medidor de nível fica depois do ganho e do denoiser, logo antes da ativação; silêncio = −120 dB.
**Alternativa descartada:** `IAudioEndpointVolume` (volume do dispositivo; global, muda para todos os apps); AGC automático (bombeia ruído de fundo e briga com o limiar).
**Data:** 2026-10-22

## D-015 Sala sempre aberta (substituída em parte pela D-024: conectar não entra mais na sala; toque e aceitar existem)
**Conceitos:** call, peer-connection, ipc-message
**Decisão:** conectado = na sala, com voz automática (escolha do usuário, 2026-10-23). `HangUp` sai da sala: a voz para nos dois sentidos (só se envia e toca se os dois estão na sala), mas chat e status continuam. `Call{friend}` volta. O `Status` de rede ganha `in_room` (`serde(default = true)`); `PeerStatus` repassa. Sem toque/aceitar.
**Data:** 2026-10-23

## D-016 sala-ui: Tauri 2 sem npm
**Conceitos:** ui-process, ipc-message
**Decisão:** `crates/ui` (Tauri 2) com frontend estático HTML/CSS/JS puro em `web/` (sem bundler/npm; `withGlobalTauri`), compilado só com `cargo build`. Cliente do pipe e `pipe_name` movidos para `sala-protocol` (feature `ipc-client`) para a UI não depender do core. Sem `--pipe`, a UI inicia o `sala-core.exe` ao lado. Fontes OFL locais (fontsource). Ícone provisório.
**Data:** 2026-10-23

## D-017 — T-020: Tailscale ao iniciar, Concord fora do gatilho, atalhos nos Ajustes
- Ao iniciar, o núcleo liga o Tailscale se estiver instalado e parado (`tailscale.rs`), em segundo plano e sem bloquear.
- Os executáveis do próprio app (`OWN_EXES` em `game_detector.rs`) nunca contam como jogo.
- Captura de atalho na UI: `UiToCore::SetHotkeyCapture{on}` suspende os atalhos globais enquanto o usuário digita; atalho em uso → `Error`.

## D-018 — Tailscale desliga junto com o Concord; núcleo sem console
- Ao sair (Fechar a Sala ou Ctrl+C), o núcleo roda `tailscale down` e fecha o `tailscale-ipn.exe`, por pedido do usuário. Com `--profile` (testes), o Tailscale não é tocado, nem na entrada nem na saída.
- No release, o `sala-core.exe` usa o subsistema `windows` (sem janela de terminal); o log vai só para o arquivo. O build de debug mantém o console.
- Aviso do SmartScreen ("fornecedor desconhecido"): só some com assinatura de código. Ver AGENTS.md.

## D-019 — T-022: pasta de dados Concord com migração de Sala
- A raiz padrão passa a ser `%APPDATA%\Concord`; `SALA_APPDATA` continua definindo a raiz de teste. Perfis ficam em `Concord\profiles\<nome>`.
- Na resolução do perfil, se só `Sala` existir, renomeá-la inteira para `Concord`. Se `Concord` já existir, usá-la sem tocar em `Sala` (e avisar se ambas existirem). Se a renomeação falhar, avisar e usar `Sala` apenas nesta execução, tentando novamente no próximo início.
- Nunca apagar nem sobrescrever dados.
- **Alternativa descartada:** copiar arquivos e remover a pasta antiga, pois duplicaria estado e poderia perder dados em falha parcial; renomear a pasta preserva o conteúdo como uma unidade.
- **Data:** 2026-10-23

## D-020 — T-023: executáveis e nome visível Concord
- Os binários gerados e referências operacionais passam a concord.exe e concord-ui.exe; pacotes/crates, pipe, prefixo de log, variáveis SALA_* e identificadores internos permanecem com os nomes Sala.
- O instalador substitui binários antigos durante atualização e encerra processos antigos e novos antes de instalar.
- **Data:** 2026-10-23

## D-021 — T-024: supressão local com RNNoise
- O `sala-core` usa `nnnoiseless` 0.5.2 sem features opcionais de I/O; o denoiser processa cada frame mono 48 kHz de 20 ms em dois blocos de 480 amostras, entre ganho e medição/VAD.
- A opção persistida `audio.noise_suppression` instala o processador no início e o liga/desliga ao vivo em `apply_audio`; a UI só envia `UpdateConfig{audio}` e reflete o estado confirmado pelo core.
- **Data:** 2026-10-23

## D-022 — T-029: áudio nunca bloqueia a sessão; "Sair" sempre sai; amigo fora da tailnet
- Cada tentativa de abrir um stream cpal roda numa thread nova com prazo de 5 s (`StreamThread` supervisor). Reusar a thread deixava o COM dela inutilizável depois que o dispositivo voltava (`RPC_E_CHANGED_MODE`), e a ativação do WASAPI pode travar para sempre.
- A sessão abre e reabre dispositivos via `spawn_blocking` e segue sem eles enquanto isso. O lado que faltar é tentado de novo a cada 5 s.
- "Sair": a parada da sessão tem prazo de 3 s, e o runtime é encerrado com `shutdown_timeout(1 s)`. Threads de áudio travadas não seguram o processo.
- Tailscale ao abrir: o estado é sondado até `Running` (`next_step`), em vez de uma decisão única.
- Contas Tailscale diferentes não se enxergam. Depois de 2 falhas de conexão, o core confere `tailscale status --json`. Se o IP do amigo (faixa 100.64/10) não estiver entre os peers, envia o IPC `PeerNotInTailnet`, e a UI mostra um aviso com a solução: convidar para a tailnet ou compartilhar as máquinas nos dois sentidos.
- **Data:** 2026-10-26

## D-023 — T-030: adicionar amigo pela lista da tailnet
- Ajustes → Amigos lista os dispositivos de `tailscale status --json` (IPC `ListTailnetPeers` → `TailnetPeers`), pedidos ao abrir Ajustes e no botão "Atualizar". Mostra nome do PC, dono, IP, online e "Já é amigo".
- Adicionar pede só um apelido, já sugerido (primeiro nome do dono, ou nome do PC). O nome do amigo continua sendo só um rótulo local: a conexão é casada pelo IP e o TOFU, pela identidade. O formulário manual por IP continua.
- O core passa a depender de `serde_json` (já no workspace) para ler o status.
- **Data:** 2026-10-26

## D-024 — T-032: chamada com toque e sons de aviso (substitui a D-015 no ponto "conectado = na sala")
- O app abre conectado e **fora da sala** (`in_room=false`, nada persistido). `JoinRoom` entra sem tocar; `Call{friend}` entra e toca no PC do amigo; ele escolhe `AcceptCall` (entra) ou `DeclineCall`. Prazo de 60 s contado por quem chama (`calls.rs`, `CallBook`); sem resposta, quem chamou continua na sala sozinho. Sair da sala cancela os toques de saida. Botão principal fora da sala (pedido do usuário em 2026-10-29): se algum amigo já está na sala, "Entrar na sala" (sem toque); senão "Chamar", que toca para todos os amigos online (`roomAction` em `state.js`).
- Protocolo so cresce por adicao: `ControlMessage::Ring/RingCancel/RingDecline` no fim do enum e `PROTOCOL_VERSION=2`. Um peer antigo derruba o stream de controle ao ver variante desconhecida, entao o `Ring` so vai a quem anunciou `protocol_version >= RING_PROTOCOL_VERSION` no Hello (senao `CallResult unsupported`). O decoder novo pula a variante desconhecida e segue.
- Sons de aviso (`cues.rs`, `CuePlayer` no `VoiceEngine`), misturados depois do silenciamento do "Silenciar": tocam mesmo ensurdecido. Volume proprio `audio.cue_volume` (0 a 100, padrao 60, 0 desliga). Notificacao de chat segue so com o remetente (D-011). Em 2026-10-29 os tons sintetizados (o usuário ouvia ruídos) viraram WAVs embutidos em `sala/crates/core/assets/cues/` (Kenney "Interface Sounds", CC0, licença junto), convertidos para 48 kHz mono 16-bit com bordas suavizadas; nada é baixado em tempo de execução.
- Modo jogo: toque, balao "X esta te chamando" (clique = entrar, sem sair do modo jogo) e itens da bandeja "Entrar na chamada de X"/"Recusar".
- `--synthetic-audio` troca so o microfone pelo tom e abre a saida real (`AudioSource::SyntheticMic`); os testes de integracao seguem sem dispositivo (`AudioSource::Synthetic`).
- **Data:** 2026-10-28

## D-025 — T-033a: FFmpeg LGPL e negociação de codec (F4)
- `ffmpeg-next =7.1.0` sobre FFmpeg n7.1.5 **LGPL shared** (BtbN), DLLs carregadas dinamicamente; `scripts/fetch-ffmpeg.ps1` baixa também o libclang 18.1.1 (o bindgen com LLVM 23 gera layouts quebrados). Tudo em `sala/third_party/`, fora do git; o instalador precisa levar as DLLs.
- **Sem fallback `libx264`** (GPL): sem encoder de hardware não há transmissão, e a UI mostra `Compartilhar-sem-encoder`.
- Invariante: codec escolhido ∈ encode local ∩ decode remoto, prioridade AV1 > HEVC > H.264 (`sala_video::choose_codec`). O probe abre de verdade cada encoder de hardware e decoder D3D11VA; só o `main` o aquece, e o `Hello` usa `capabilities_if_ready()` (nunca bloqueia o runtime; vazio se o probe ainda não terminou).
- **Data:** 2026-10-29

## D-026 — T-033b: captura WGC, encoder e fragmentação (F4)
- Captura com `windows-capture =2.0.1` (PLANO §5.5). Cópia GPU→CPU (BGRA) aceita no MVP; `TODO(zero-copy D3D11)` em `capture.rs`.
- Encoder: nvenc > amf > qsv, sem B-frames, GOP 20 s, CBR com VBV ~125 ms; BGRA→NV12 via swscale (feature `software-scaling`).
- O receptor descarta deltas após perda até um keyframe (`inv-drop-deltas-until-keyframe`). Problema aberto para a T-033c: com 2% de perda quase todo keyframe de 1080p se perde (FEC, retransmissão ou keyframe menor), e o encode síncrono fica em 16–18 fps.
- **Data:** 2026-10-29

## D-027 — T-033d: visualizador winit + wgpu no núcleo (F4)
- Visualizador em `sala-video/src/viewer.rs`: `winit 0.30` + `wgpu 24` (dx12), thread própria (`with_any_thread`), caixa de correio de 1 quadro (só o último), estatísticas no título (F3), F11 tela cheia.
- Decoder D3D11VA com fallback silencioso para o software do FFmpeg (LGPL): o probe só prova que o decoder abre; nesta máquina o D3D11VA recusa o H.264 do QSV.
- O viewer nunca decodifica delta antes de keyframe nem após buraco em `frame_id`; pede keyframe no máximo a cada 500 ms. `VideoFrame` carrega o codec do `ShareStart`.
- Thread `video-forward` encaminha `take_video_frames` ao viewer aberto e descarta sem viewer; modo jogo fecha o viewer no passo 2 (D-012).
- **Data:** 2026-10-29

## D-028 — T-033f: áudio do PC na transmissão (F4)
- Captura por WASAPI process loopback excluindo a árvore do próprio processo (Win10 2004+). Sem fallback para loopback comum (ecoaria a voz dos amigos): se falhar, transmite só vídeo e `ShareState.audio=false`.
- Opus estéreo 128 kbps 20 ms como `ShareAudio`, aos mesmos destinatários do vídeo; o receptor mistura em mono (saída é mono) com volume próprio, antes do "Silenciar", e toca mesmo fora da sala.
- **Data:** 2026-10-29
