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

## D-003 Opus: crate `opus` (audiopus_sys), compilado com CMake
**Conceitos:** voice-frame
**Decisão:** usar a crate `opus`, que expõe FEC, DTX, bitrate e o modo VOIP. O libopus é compilado estaticamente pelo `audiopus_sys`, que exige CMake no toolchain.
**Alternativa descartada:** Opus em Rust puro (imaturo, sem FEC/DTX completos).
**Data:** 2026-10-22

## D-004 Desenvolvimento nativo no Windows
**Conceitos:** core-process
**Decisão:** construir e testar em Windows 11 nativo com toolchain MSVC. O produto depende de WGC, WASAPI, named pipes e da bandeja.
**Alternativa descartada:** WSL2 com cross-compile (não testa nenhuma API nativa).
**Data:** 2026-10-22
