# AGENTS.md — guia para qualquer agente/harness continuar o Concord

Este arquivo é a porta de entrada para qualquer assistente de código (Claude Code, Codex, Cursor, Aider, Gemini CLI, etc.). Leia-o inteiro antes de mexer. `CLAUDE.md` descreve o fluxo específico do Claude Code (subagentes e métricas) e é opcional fora dele.

## 1. O que é
**Concord** (nome antigo e interno: **Sala**) é um app desktop leve para Windows, de voz P2P entre amigos sobre o **Tailscale**, com chat temporário e "modo jogo". Rust + Tauri 2.
- Especificação original: `ideia/sala-handoff/PLANO-CLAUDE-CODE.md` (seções §1–§13).
- Design obrigatório das telas: `ideia/sala-handoff/design/` (`DESIGN.md` + `telas/*.dc.html`).
- Repositório: https://github.com/LucasLeitePereira/concord (branch `main`). Releases têm o instalador `.exe`.

## 2. Ponto de retorno seguro
A tag **`checkpoint-2026-10-23`** marca a última versão estável e testada (v0.1.1, instalador publicado). Se algo der muito errado:
```bash
git fetch --tags
git switch -c recuperacao checkpoint-2026-10-23   # inspecionar sem perder nada
# ou, para descartar tudo depois do ponto (destrutivo):
git reset --hard checkpoint-2026-10-23
```

## 3. Estrutura
```
sala/                      workspace Cargo (nomes internos ainda "sala-*")
  crates/protocol/         tipos de rede (bincode) e IPC (JSON por linha); cliente do pipe (feature ipc-client)
  crates/core/             bin concord.exe: núcleo residente (voz, rede QUIC, bandeja/atalhos Win32, modo jogo, IPC)
    src/app.rs             orquestrador único (Controller): atalho, menu, IPC e detector passam por aqui (D-012)
    src/session.rs         sessão com amigos, eventos, chat em memória
    src/voice.rs           captura/reprodução cpal, Opus, VAD, ganho, medidor, jitter buffer
    src/net.rs             QUIC (quinn) + TOFU de fingerprint
    src/settings.rs        Config <-> mensagens da UI, detecção do IP do Tailscale
    src/game_detector.rs   detector de jogo com histerese (D-008)
    src/platform/          Win32 (bandeja, atalhos globais, PTT, notificação, janela em foco) — D-009
    tests/                 integração: f1_integration, f2_game_mode, f3_settings, ipc, net
  crates/ui/               bin concord-ui.exe: Tauri 2, frontend HTML/CSS/JS puro em web/ (sem npm) — D-016
  installer/concord.iss    Inno Setup (firewall UDP 47800, atalhos, baixa o Tailscale se faltar)
  scripts/                 build-installer.ps1, measure*.ps1
ontologia/                 domínio (dominio.yaml), mapa conceito→código (mapa-codigo.yaml), decisões D-001..D-023 (decisoes.md)
metricas/                  experimento sobre o efeito da ontologia (coletar.py lê transcripts do Claude Code; opcional fora dele)
.claude/                   agentes e memória do Claude Code; .claude/agent-memory/pleno.md tem armadilhas úteis a qualquer agente
```

## 4. Build, testes e entrega
Pré-requisitos: Windows 11, Rust stable MSVC, CMake ≥ 3.x (para `audiopus_sys`), WebView2 (já vem no Win11), Inno Setup 6 para o instalador.
```bash
cd sala
cargo build --release                                  # gera target/release/concord.exe e concord-ui.exe
cargo clippy --workspace --all-targets -- -D warnings  # obrigatório limpo
cargo test --workspace                                 # obrigatório verde (rode separado do clippy: o build do Tauri é lento)
powershell -File scripts/build-installer.ps1           # gera target/installer/Concord-Setup-<versão>.exe
```
- **Pronto** = clippy limpo + testes verdes + teste manual do que for visual.
- `f2_game_mode_end_to_end`, `room_friends_and_call_stats_over_loopback` (f3_settings) e `f1_call_voice_chat_and_chat_never_on_disk` às vezes estouram o tempo com a máquina carregada; rode de novo isolado antes de concluir que quebrou.
- Teste manual com 2 instâncias no mesmo PC: `concord.exe --profile a` e `--profile b` (portas 47801/47802, já amigos um do outro em 127.0.0.1). `--synthetic-audio` usa um tom no lugar do microfone. Dados ficam em `%APPDATA%\Concord\[profiles\<p>\]` (config.toml, identidade, logs); na primeira resolução, `Sala` é renomeada para `Concord` quando o destino ainda não existe.
- Release: suba a versão em `installer/concord.iss` (`AppVersion`) e nos `Cargo.toml`, gere o instalador e rode `gh release create vX.Y.Z <setup.exe>`.

## 5. Regras do projeto (não negociáveis)
- **Chat nunca vai para disco nem para log** (`inv-chat-never-on-disk`). Notificação no modo jogo mostra só o remetente (D-011).
- Sem chamadas externas além do P2P entre amigos (exceção: o instalador baixa o Tailscale oficial).
- Sem hooks, injeção de DLL ou overlay em jogos. Atalhos usam `RegisterHotKey` e o PTT faz polling (D-009).
- Protocolo **só cresce por adição**. Campo novo em mensagem de rede bincode vai **no fim** e com desserialização tolerante a ausência. A UI ignora variantes IPC desconhecidas.
- No modo jogo **só o `concord.exe` roda**: a UI é encerrada e reaberta na saída (D-012).
- Código, identificadores e logs em inglês; textos de interface em pt-BR com os nomes Mutar-se/Desmutar-se e Silenciar/Voltar a ouvir (D-013).
- Toda lógica nova tem teste, de preferência com controle negativo (o teste falha sem a correção). Bug corrigido ganha teste de regressão.
- Antes de mudar arquitetura, protocolo ou formato persistido, leia `ontologia/decisoes.md` e registre uma decisão nova (D-0NN) curta.
- Edite código com a ferramenta de edição do harness, nunca com heredoc/sed/Python no Git Bash (arquivos CRLF; já corrompeu edições várias vezes).

## 6. Estado atual (2026-10-29)
> Esta seção é a **passagem de bastão** e o único lugar com o estado do projeto: atualize-a ao fechar cada tarefa e comece a próxima em sessão nova. Trabalhe em fatias pequenas (um assunto por tarefa), leia só o trecho da função (não arquivos grandes inteiros) e rode só o teste do que mudou; a rodada completa de clippy + testes é uma vez no fim.

Feito: F1 (rede, voz, identidade), F2 (núcleo residente, modo jogo, bandeja, IPC), F3 parcial (UI no design revisado no Claude Design em 2026-10-28: lateral única, Chamada, aba Amigos, Ajustes em 2 colunas — T-031), F6 parcial (instalador Inno Setup, release no GitHub).
Design atual: `ideia/sala-handoff/design/DESIGN.md` + `telas/` (inclui Compartilhar e Visualizador para o F4). Exceção pedida pelo usuário: o medidor de sensibilidade segue o visual antigo (linha branca + marcador redondo + valor em dB).
`--synthetic-audio` troca **só o microfone** por um tom e abre a saída real (D-024): dá para ouvir os sons e a voz do outro perfil. Os testes de integração usam `AudioSource::Synthetic` (sem nenhum dispositivo).

**Próximos passos, por prioridade (decididos pelo usuário):**
1. **F4 — compartilhamento de tela** (pedido do usuário em 2026-10-29: antes de "Ouvir minha voz"), em fatias, uma por sessão (plano: T-033a probe ✔ · T-033b captura WGC + encoder + fragmentação, sem rede · T-033c transmissão, presets e bitrate adaptativo · T-033d visualizador wgpu · T-033e UI Compartilhar/"Ao vivo" · T-033f áudio do sistema). Sem fallback `libx264` (GPL): sem encoder de hardware, variante `Compartilhar-sem-encoder`.
2. **Ajustes de áudio** (vira T-034): dispositivos de entrada e saída (`ListAudioDevices`), sensibilidade com linha arrastável sobre o medidor (`SetMicMeter`/`MicLevel`, `UpdateConfig{audio}`) e botão **"Ouvir minha voz"** (loopback local que passa pelo mesmo ganho e VAD do envio; IPC novo `SetMicTest{on}`), e "Adicionar jogo". Hoje o limiar fica em `config.toml` (`vad_threshold`; −49 ficou bom no microfone do usuário).
3. F5: mesa de sons. README e CI (GitHub Actions) também ficaram para depois.
Aviso do Windows (SmartScreen, "fornecedor desconhecido"): o instalador não é assinado. Para tirar o aviso é preciso um certificado de assinatura de código (por exemplo, Certum Open Source, para projeto público) e `signtool`/`SignTool=` no Inno Setup; a reputação no SmartScreen ainda leva algum tempo de downloads para se formar. A decisão e o custo são do usuário.

T-032 (chamada com toque, sons de aviso do Kenney CC0 em `crates/core/assets/cues/`, D-024) saiu na v0.1.8. Em janela pequena o botão "Desligar" da barra de baixo some (o usuário pediu para deixar assim por ora).

Pendências de teste manual: "Não perturbe" do Windows com chat no modo jogo e o instalador baixando o Tailscale (nunca executado de fato).

Amigos precisam estar na mesma rede Tailscale: a mesma conta, um convite para a tailnet, ou as máquinas compartilhadas nos dois sentidos. Contas diferentes sem isso não se alcançam, e o app mostra "Fora da sua rede Tailscale" (D-022). Em Ajustes → Amigos, os dispositivos da tailnet aparecem em lista para adicionar só com um apelido (D-023).

## 7. Armadilhas conhecidas
Ver `.claude/agent-memory/pleno.md` (vale para qualquer modelo). As principais:
- Git Bash converte `/F` e caminhos com `\\` em argumentos de `.exe`: use `taskkill //F` ou lance via `cmd //c`. Não use `taskkill /IM concord.exe` enquanto testes rodam, porque mata os processos dos testes.
- Nunca dois `cargo build` pesados ao mesmo tempo no mesmo `target/`.
- Nunca dois agentes ou harnesses trabalhando ao mesmo tempo no mesmo checkout: um commit ou build de um mistura o trabalho do outro (aconteceu na v0.1.3). Use `git worktree` se precisar de paralelismo. A máquina é um i5 de 4 núcleos com 8 GB.
- O terminal maximizado conta como jogo no teste manual de detecção. Os executáveis do próprio Concord já são excluídos no código.
- cpal 0.18 tem API diferente da que os modelos lembram: consulte os exemplos no registry local.
- `cargo build --example X` não compila o binário principal. Para o release completo, use `cargo build --release` sem filtro.

T-033a (não commitada): `scripts/fetch-ffmpeg.ps1` baixa FFmpeg n7.1.5 LGPL shared (BtbN) e libclang 18 em `sala/third_party/` (ignorado pelo git; rodar o script antes do primeiro build). Crate `sala-video`: `probe`, `choose_codec` (AV1 > HEVC > H.264), cache `capabilities_if_ready()`; `concord --probe`; o `Hello` leva as capacidades reais. Nesta máquina: encode só QSV (HEVC/H.264), NVENC falha com "Generic error in an external library" (investigar na T-033b); decode AV1/HEVC/H.264 via D3D11VA. Pendências: o `concord.exe` agora exige as DLLs do FFmpeg, e o `concord.iss` ainda não as inclui; o `f2_game_mode` leva ~85 s na rodada completa (limite 90 s). Próximo: T-033b. T-033b (não commitada): em `sala-video`, `fragment` (fragmenta/remonta, descarta deltas após perda, keyframe ≤1/500 ms), `capture` (windows-capture 2.0.1, monitores/janelas, BGRA com cópia GPU→CPU, TODO zero-copy), `encoder` (nvenc>amf>qsv, baixa latência, BGRA→NV12 via swscale, `force_keyframe`), exemplo `capture_encode`. NVENC: a MX110 não tem engine NVENC (limitação de hardware). Medido: h264_qsv 1080p a 16–18 fps (~15 ms por encode síncrono); com 2% de perda quase todo keyframe se perde (dezenas de fragmentos). A T-033c precisa de encode assíncrono e de FEC, retransmissão ou keyframes menores. O WGC só emite quadro quando a tela muda: reenviar o último. Rodada completa: clippy ok, 286 testes ok, `f2_game_mode` estourou o timeout de 90 s sob carga; agora 1 volta por padrão (`F2_ROUNDS=3` para caçar instabilidade) e limite de 180 s por volta (uma volta varia de ~20 a ~60 s). Próximo: T-033c. T-033c1 (não commitada, só `sala-video` + protocolo): `preset` (720p30/1080p30/1080p60/SharpText, faixas de bitrate), `abr` (perda >5% ⇒ ×0,7; <1% ⇒ +5%/s após 5 s do corte), `VideoEncoder::set_bitrate` (QSV reabre se >15% + keyframe), FEC XOR só em keyframes (`MediaDatagram::Video` ganhou `fec_group: u8`), `sender` (slot de último quadro, thread de encode, reenvio a 1 s em tela parada, canal descarta delta). Exemplo com 2% de perda: 8 de 9 keyframes sobrevivem com FEC (4 sem). Rodada completa: clippy ok, 309 testes ok (`f2_game_mode` ~106 s). Próximo: T-033c2 = integração no core: `ControlMessage::ReceiverReport` nova (gate por `protocol_version`, D-024), ShareStart/Stop criam/param o `Sender`, KeyframeRequest → `request_keyframe`, task que drena o canal e envia com `max_datagram` real, receptor mede perda via `Reassembler`, teste de integração com fonte sintética. Avaliar `idle_resend` de 500 ms. T-033c2 (não commitada antes deste commit): `ControlMessage::ReceiverReport` (variante no fim do enum), `PROTOCOL_VERSION` 3 / `VIDEO_PROTOCOL_VERSION` 3 (ShareStart e reports só a pares v3); `core/src/share.rs` (`ShareOut` thread `share-send` → `send_datagram`; `ShareIn` remonta, mede perda e reporta a cada 1 s; `PipeFactory` para fonte sintética); `Session::start_share/start_screen_share/stop_share/share_stats/take_video_frames` (canal de 32 quadros para o viewer); `idle_resend` 500 ms. Teste `core/tests/screen_share.rs`. Rodada completa: clippy ok, 318 testes ok; `f2_game_mode` 119 s, mas estourou (260 s) numa rodada sob carga — instável. Riscos: sem pacing de envio (keyframe grande em rajada), codec escolhido por quem chama (sem negociar com o Hello), `start_screen_share` real só compila, gate v2 não testado com par real. Próximo: T-033d (visualizador wgpu consumindo `take_video_frames`). T-033d (D-027): `sala-video` `decoder.rs` (D3D11VA → fallback software silencioso; nesta máquina o D3D11VA recusa o H.264 do QSV, então roda em software, custo de CPU em 1080p60 não medido) e `viewer.rs` (winit 0.30 + wgpu 24, último quadro só, F11, F3 = estatísticas no título, Esc fecha); exemplo `view_synthetic` (rodar com `cargo run`, por causa das DLLs). Core: `OpenViewer` abre a janela (thread `video-forward`), modo jogo fecha no passo 2, `Session::request_keyframe`. Rodada completa: clippy ok, 328 testes ok (`f2_game_mode` 171 s). Pendências: janela validada só por log (falta ver na tela), um decoder por viewer (dois compartilhando ao mesmo tempo = reabre a cada troca), `hw_decode` do Hello pode ser otimista. Próximo: T-033e (UI Compartilhar/Ao vivo), depois T-033f.
