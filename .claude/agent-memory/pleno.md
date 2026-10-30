# Memória — pleno (curada pelo orquestrador; máx ~60 linhas)

## Ambiente
- Windows 11 nativo, repo em `D:\TI\concord`. Shell Git Bash (POSIX); PowerShell via `powershell -NoProfile -Command "..."`.
- Python é `python` (3.14); não existe `python3`.
- Rust MSVC (`stable-x86_64-pc-windows-msvc`); se `cargo` não estiver no PATH, use `~/.cargo/bin/cargo`.
- Hardware: i5-10210U 4 núcleos/8 threads, ~8 GB RAM. Builds pesados um por vez; no máximo 2 cargo em paralelo e só de crates pequenas.
- CMake 4.4 do winget não está no PATH do Bash e há um CMake 3.22 do Android SDK: antes de compilar audiopus_sys, `export PATH="$HOME/.cargo/bin:/c/Users/Jose/AppData/Local/Microsoft/WinGet/Packages/Kitware.CMake_Microsoft.Winget.Source_8wekyb3d8bbwe/cmake-4.4.4-windows-x86_64/bin:$PATH"`.
- Após winget install, o PATH da sessão não muda: use caminhos completos.
- Nunca dois `cargo build` pesados ao mesmo tempo no mesmo `target/`.

## Armadilhas (não repetir)
- Teste que só passa nesta máquina (arquivo não versionado, .gitignore) é teste quebrado: relate como PRIORIDADE.
- Mudar formato de dados persistidos sem migração idempotente + backup + teste perde dados do usuário.
- NUNCA edite código por heredoc ou script Python no Git Bash ("unexpected EOF"; replace falha em silêncio em arquivo CRLF como `config.rs`): use Write (arquivo inteiro) ou Edit. Falhou de novo na F2 em 3 agentes.
- Clippy 1.99 (`-D warnings`): `collapsible_if` pede let-chains (`if a && let Some(x) = y`); `result_large_err` → `Box<toml::de::Error>` no enum de erro; `field_reassign_with_default` → `X { campo, ..Default::default() }`.
- IP de socket dual-stack chega como `::ffff:a.b.c.d`: compare com `IpAddr::to_canonical()`.
- `u8::from_str_radix` aceita `+` inicial: valide `is_ascii_hexdigit` antes de parsear hex.
- Buffer de tempo real (jitter etc.): além de perda/reordenação, teste deriva de relógio, pausa do remetente (DTX/mudo) e ressincronização; simule pausa com salto de timestamp (senão o estimador sobe o alvo) e calcule a retomada com `target_delay_ms()`.
- QUIC/quinn: `accept_bi` do outro lado só retorna quando chega dado no stream → escreva um frame logo após `open_bi`.
- Diretório temporário de teste no Windows: relógio tem baixa resolução (nanos repetem) → nome com contador atômico.
- Teste com duas threads/instâncias concorrentes: não presuma quem chega primeiro (ex.: dois `RegisterHotKey` iguais — a recusa sai em qualquer um dos lados). Rode testes de concorrência 5–8× antes de entregar.
- Helper de teste `wait_for` que descarta eventos não casados esconde contagens: drene e conte antes.
- cpal 0.18 difere do que o modelo lembra (SampleRate=u32, nome via `description()?.name()`, `device_by_id`; importar trait `cpal::Sample`): consulte `~/.cargo/registry/src/*/cpal-0.18.*/examples`. Build inicial opus+cpal ≈ 2 min.
- Scripts que escrevem em pastas de entrega não podem apagar pastas onde o usuário já roda coisas: monte em diretório temporário.

- Win32 (`windows-sys`): chamada que reentra no `wndproc` (`DestroyWindow`, `TrackPopupMenu`) quebra `RefCell` emprestado → `try_borrow_mut` e soltar o empréstimo antes; texto de menu vindo do usuário: escapar `&` como `&&`; constantes ausentes (`NIN_BALLOONUSERCLICK` = `WM_USER + 5`) definir à mão.
- Named pipe tokio: cliente pode levar `ERROR_PIPE_BUSY` (231) entre instâncias → retry curto. Teste que usa exe de example: sempre `cargo build --example X` no helper (incremental, barato) — exe velho quebra quando o protocolo muda.
- `SetForegroundWindow` de processo lançado pelo harness falha sem Alt sintético antes; `cargo test --lib` não recompila examples (`cargo build --example X` antes do teste `--ignored`).

- Teste manual de auto-detecção: o terminal do harness maximizado conta como jogo → ponha `windowsterminal.exe` no `exclude` do perfil de teste (e restaure), ou desligue `auto_detect` com backup do `config.toml`. `Start-Process -WindowStyle Hidden` cria `conhost` filho: filtre ao contar processos. Atalho global sintético: `keybd_event` via `Add-Type` no PowerShell funciona com `RegisterHotKey`.

## Atalhos / padrões que funcionaram
- Controle negativo em todo teste de lógica crítica (barato: mutar 1 linha com backup `cp`, rodar `cargo test -p sala-core --lib <mód>`, restaurar; mute com Edit e confira com `grep -n` que a mutação entrou — `sed -i` já falhou em silêncio); o caso de controle tem de ser negativo só pela regra testada (ex.: `explorer.exe` em tela cheia, não em janela); repetir testes de rede/concorrência N vezes para pegar flakiness.
- Lógica pura isolada em funções/structs testáveis (sem I/O) em vez de testar via rede/UI.
- Código que só o hardware valida (áudio, GPU): logs com o motivo de cada transição.
- Declare desvios do pedido; não esconda.
- Saída do clippy/cargo: filtre com `grep -E "^error|^warning|-->|Finished|^test result"` para economizar tokens.
- Caminho temporário do Git Bash para programa Windows: `$(cygpath -w "$T")`.
- Processo em background no Git Bash: `$!` não é o PID Windows; encerre com `taskkill //F //IM sala-core.exe`.
- Capturar logs `tracing` em teste tokio multi-thread exige `set_global_default` (um `#[test]` por arquivo); `set_default` é por thread.
- `Controller::steps()`: passos novos (ex. `Step::Cause`) vão no fim da sequência; testes exigem `UiShutdown` como passo 0 (D-012).
- Hora local no log: feature `local-time` do `tracing-subscriber` + `LocalTime::rfc_3339()` funciona no Windows/tokio.
- Duas instâncias que se discam em teste: porta livre via `UdpSocket::bind("127.0.0.1:0")`, ler e soltar; guarde as portas já entregues no processo (testes paralelos recebem a mesma → EADDRINUSE).
- Duas sessões que se discam ao mesmo tempo trocam a conexão duplicada ~1 s depois (`Disconnected`+`Connected`): teste que age logo após conectar fica flaky → `settle_connection` (~1,6 s) de `f3_settings.rs`.
- Variante nova de `ControlMessage`: peer antigo derruba o stream de controle no erro de decode → só enviar com gate por `Hello.protocol_version` (D-024).
- Desde a D-024 o app/engine começa com `in_room=false`: teste de voz precisa de `set_in_room(true)`/`JoinRoom` explícito.
- Antes do `cargo build --release`, confira se há `concord.exe` rodando ("failed to remove file"); não mate o do usuário.
- `cat > /dev/null` no Git Bash trava esperando stdin.

- sala-ui (Tauri 2 sem tauri-cli): frontend em `crates/ui/web/` usa `window.__TAURI__` (withGlobalTauri); registre o ouvinte antes de pedir `link_state`. Build frio ≈ 12 min: rode clippy e test separados ou em background.
- Pacotes: UI = `-p sala-ui` (bin `concord-ui`), núcleo = `-p sala-core` (bin `concord`). O `web/` é embutido no build: mudou o frontend, recompile. Arquivos do `web/` são LF (`.gitattributes`).
- JS sem infra de teste: `node --check` + um `.mjs` no scratchpad importando `state.js` por `file:///` (ele não usa DOM).
- `main.render()` recria a lateral e os tiles: todo elemento interativo precisa de `data-key` para devolver o foco; valores que mudam sem render completo (latência do `CallStats`) usam um atualizador próprio (`updateTileMeta`).

## Convenções do projeto (Sala)
- Spec: `ideia/sala-handoff/PLANO-CLAUDE-CODE.md`; design: `ideia/sala-handoff/design/`.
- Workspace Cargo em `sala/` (`crates/protocol`, `crates/core` → bin `sala-core`, depois `crates/ui`).
- Código, identificadores e logs em inglês; textos de interface em pt-BR.
- Pronto = `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` passando.
- Dependências mínimas; crate pesada nova = justificar no relatório (o sênior registra em `ontologia/decisoes.md`).
- Nunca logar conteúdo de chat (`inv-chat-never-on-disk`).
- FFmpeg (`sala-video`): `ffmpeg-sys-next` precisa do libclang 18 (LLVM 23 gera asserts de layout quebrados); `scripts/fetch-ffmpeg.ps1` baixa os dois em `sala/third_party/`. Crate `ffmpeg-next` com o mesmo major do FFmpeg (7.1.x), features `codec`+`format` juntas.
- `decoder::find(Id::AV1)` devolve libdav1d (software): use `find_by_name("av1")` para hwaccel D3D11VA.
- Nunca chamar `sala_video::capabilities()` (probe bloqueante, 2–4 s) de código async ou testes: use `capabilities_if_ready()`; só o `main` aquece o cache.
- `windows-capture` 2.x: API difere da memória (`start_free_threaded`, `Settings::new` com 8 args, `as_nopadding_buffer`); leia os fontes no registry após `cargo fetch`. WGC só emite quadro quando a tela muda: reenviar o último quadro.
- `ffmpeg-next`: swscale exige feature `software-scaling`; `rc_buffer_size` sem setter (escreva via `as_mut_ptr()` antes do `open`). Erro opaco de encoder: rode `third_party/ffmpeg/bin/ffmpeg.exe` com o mesmo encoder para ver o log real (a MX110 desta máquina não tem NVENC).
- `VideoEncoder` tem `unsafe impl Send` (swscale `*mut`): só mover para a thread, nunca compartilhar. Dimensão/bitrate em inteiros (float dá 1919.99).
- Protocolo bincode: variante nova de `ControlMessage` só no fim do enum (índice). Flag local marcado antes do `change_local` não transmite `Status` (before == after).
- Rodada completa: `cargo test --workspace --no-fail-fast` (sem isso para no 1º binário). `f2_game_mode` oscila 119–260 s sob carga.
- FFmpeg/DLLs, decode D3D11VA, áudio do PC (WASAPI) e testes lentos: estão no docserver (`buscar`), não aqui.
- wgpu 24: `request_adapter` → Option, `TexelCopyTextureInfo`; winit 0.30: `with_any_thread(true)` fora da main. Clippy 1.99: `to_*(&mut self)` → wrong_self_convention; `x % n == 0` → `is_multiple_of`; `as_chunks::<N>()` no lugar de `chunks_exact`.
- REGRA: edição só com Edit/Write, mesmo em arquivo LF (T-033f1 usou python/sed de novo).
- Fatias acima de US$ 2 (T-033d decoder+janela+core; T-033f1 COM/WASAPI+rede): separe crate de mídia/nativo da integração no core.
- T-035b custou US$ 5,61 (201 tool calls): diagnóstico + ABR + decoder + bench numa fatia só. Diagnóstico/medição vira fatia própria antes da correção.
- Examples do `sala-video` precisam das DLLs no PATH: `PATH=$PWD/target/release:$PATH target/release/examples/x.exe`. Bench com CPU saturada: `--frames 30` + timeout. swscale só usa threads via `sws_scale_frame` (o `sws_scale` ignora).
- QSV ao vivo: só `bit_rate`/`rc_max_rate`, abaixo do valor de abertura e sem tocar `rc_buffer_size`, senão EINVAL no `send_frame` seguinte (D-034). Sempre sai IDR. `--log 1` no bench revela o erro real. Mutação: uma por vez, uma mascara a outra. `AV_CODEC_FLAG_LOW_DELAY` desliga frame threading. `cargo test -p sala-video --lib` em paralelo dá 0xc0000005: use `--test-threads=1`.
