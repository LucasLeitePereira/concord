---
titulo: Probe D3D11VA ok não garante decode do stream
tipo: armadilha
gerado_por: claude-opus-5-5
commit: cb7b11f
arquivos: sala/crates/video/src/lib.rs, sala/crates/video/src/decoder.rs
revisado_por: claude-sonnet-5-5
revisado_em: 2026-10-30T00:44:03Z
parecer: O código confirma o essencial. `try_open_hw_decoder` (video/src/lib.rs:195) só cria o device D3D11VA e abre o decoder, sem alimentar bitstream, e o próprio comentário admite que uma GPU sem o perfil pode passar no probe.; `VideoDecoder::switch_to_software` (decoder.rs:228) troca para software quando o hardware nunca produziu quadro. Depois `decode` chama a si mesmo com o mesmo keyframe, sem round trip ao remetente.; O teste `prime` e `decodes_what_the_encoder_produced` já tratam o fallback, e o nome do decoder muda depois dele (`h264+d3d11va` para `h264 (software)`).; `hw_decode` existe no protocolo e em `Capabilities`, então o aviso para não negociar codec só com ele é pertinente.; É uma armadilha com sintoma e causa, é conciso, não duplica nada e não tem segredos. Não consegui conferir pelo código o hardware da máquina (Intel UHD + MX110) nem a mensagem de erro do FFmpeg. São observações de campo plausíveis, e o documento já marca o custo de CPU do decode por software como não medido.
---

# Probe D3D11VA ok não garante decode do stream

Conceitos: `capabilities`, `screen-viewer`, `codec-negotiation`.

O probe (`sala_video::probe`) só prova que o decoder de hardware abre com um contexto pequeno. Na
máquina de desenvolvimento (Intel UHD + MX110) ele reporta H.264/HEVC/AV1 via D3D11VA, mas o
D3D11VA recusa o stream H.264 real do `h264_qsv` com "No decoder device for codec found".

Por isso o `VideoDecoder` troca em silêncio para o decoder de software do FFmpeg quando o hardware
nunca produziu quadro, e redecodifica o mesmo keyframe (sem pedir outro ao remetente).

Consequências:
- `hw_decode` no `Hello` pode ser otimista; não use isso sozinho para negociar codec.
- Teste de decoder precisa aceitar o fallback (o nome do decoder muda depois dele).
- O custo de CPU do decode por software em 1080p60 ainda não foi medido.
