---
titulo: DLLs do FFmpeg ao rodar exemplos e o concord.exe
tipo: operacao
gerado_por: claude-opus-5-5
commit: cb7b11f
arquivos: scripts/fetch-ffmpeg.ps1, sala/crates/video/build.rs, sala/.cargo/config.toml, sala/installer/concord.iss
revisado_por: claude-sonnet-5-5
revisado_em: 2026-10-30T00:44:14Z
parecer: Confere com o código. O build.rs copia as DLLs só para target/<perfil>/ e target/<perfil>/deps/, e não para examples/. O .cargo/config.toml define FFMPEG_DIR em third_party/ffmpeg. O third_party/ está no .gitignore. O scripts/fetch-ffmpeg.ps1 existe. O concord.iss empacota só concord.exe, concord-ui.exe e o ícone, sem DLLs, então a pendência é real.; Explica o sintoma (exit 127 por falta de swscale-8.dll ao rodar o exemplo direto) e como evitar. Não é resumo de código, não duplica nenhum documento aprovado e é conciso.; Não contém segredos nem dados pessoais.
---

# DLLs do FFmpeg ao rodar exemplos e o concord.exe

Conceitos: `ffmpeg-setup`, `hw-video-encoder`, `screen-viewer`.

O `sala-video` linka o FFmpeg LGPL dinamicamente (D-025). O `build.rs` copia as DLLs só para
`target/<perfil>/` e `target/<perfil>/deps/`; os exemplos ficam em `target/<perfil>/examples/`.

Sintoma: rodar `target/debug/examples/<x>.exe` direto sai com código 127 (falta `swscale-8.dll`),
sem mensagem útil no Git Bash.

Como evitar: rode exemplos com `cargo run -p sala-video --example <x>` (ou `-p sala-core`), ou
ponha `target/debug` e `sala/third_party/ffmpeg/bin` no PATH. Antes do primeiro build numa máquina
nova, rode `scripts/fetch-ffmpeg.ps1` (o `third_party/` não vai para o git).

Pendência conhecida: o `concord.iss` ainda não empacota as DLLs, então o instalador gera um
`concord.exe` que não abre o compartilhamento de tela.
