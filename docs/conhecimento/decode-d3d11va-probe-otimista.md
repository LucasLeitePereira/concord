---
titulo: D3D11VA no adaptador errado parecia "recusar" o stream
tipo: armadilha
gerado_por: claude-opus-5-5
commit: 4ad4407
arquivos: sala/crates/video/src/decoder.rs, sala/crates/video/src/lib.rs, sala/crates/video/examples/bench_decode.rs
revisado_por: claude-sonnet-5-5
revisado_em: 2026-10-30T18:20:35Z
parecer: O código confirma a solução. `adapter_order` (decoder.rs:59) tenta primeiro o adaptador que já funcionou (`GOOD_ADAPTER`) e depois os índices 0..3. `open_inner` passa o adaptador ao `HwDevice::d3d11va`. `switch_to_hardware` (a função em torno da linha 354) troca de adaptador quando o hardware não produziu quadro, e só depois vem o software.; `AV_CODEC_FLAG_LOW_DELAY` desliga o frame threading, e o código define `thread_count = 2` (decoder.rs:288-293). Isso bate com a afirmação sobre o software.; A D-030 existe em `ontologia/decisoes.md`. O diff desde o commit 4ad4407 só ajusta `EncoderConfig` (campo `ceiling_kbps`), `pub mod scale` e um comentário, e nada disso contradiz o documento.; O documento corrige o anterior, que atribuía a falha ao D3D11VA recusar o stream. A causa real é o adaptador DXGI 0 errado. O texto traz sintoma, causa, solução e alternativa, e não duplica nada.; Não consegui conferir pelo código o hardware da máquina (Intel UHD + MX110) nem os números do benchmark (90→235 fps no software, 99 fps no D3D11VA). São medições de campo e têm o comando de reprodução citado. Não há segredos nem dados pessoais.
---

# D3D11VA no adaptador errado parecia "recusar" o stream

Conceitos: `capabilities`, `screen-viewer`, `codec-negotiation`.

Sintoma: no notebook de desenvolvimento (Intel UHD + MX110), o probe dizia que o D3D11VA decodifica
H.264, mas o decoder real não produzia quadro ("Invalid data"/"No decoder device for codec found")
e o viewer caía para software, a 9–20 fps em 1080p60 com dois perfis no mesmo PC.

Causa: `av_hwdevice_ctx_create` sem dispositivo abre o adaptador DXGI 0, que nessa máquina não
decodifica o stream do `h264_qsv`. O adaptador 1 (Intel UHD) decodifica normalmente.

Solução (D-030): o `VideoDecoder` tenta primeiro o adaptador que já funcionou no processo, depois os
índices 0..3, e só então o software. No software, `AV_CODEC_FLAG_LOW_DELAY` desligava o frame
threading; agora são 2 threads de frame (mais threads não ganharam nada).

Números (release, 1080p60, 8 Mbps, `cargo run --release -p sala-video --example bench_decode`):
software 90 → 235 fps; D3D11VA no adaptador certo 99 fps, sem custo de CPU no decode.

Ainda vale: o probe só prova que o decoder abre; `hw_decode` no `Hello` pode ser otimista.
