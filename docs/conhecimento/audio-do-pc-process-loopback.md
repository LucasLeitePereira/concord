---
titulo: Áudio do PC na transmissão usa process loopback, sem fallback
tipo: decisao
gerado_por: claude-opus-5-5
commit: cb7b11f
arquivos: sala/crates/core/src/loopback.rs, sala/crates/core/src/share_audio.rs
revisado_por: claude-sonnet-5-5
revisado_em: 2026-10-30T00:43:49Z
parecer: Conferido no código: `loopback.rs` usa `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` e `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`, e o handler tem `#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]`.; `windows-core` é dependência direta no `Cargo.toml` do core. O `PROPVARIANT` usa `ManuallyDrop`, como o documento diz.; Em `app.rs`, `ShareState.audio` vem de `with_audio`. Isso bate com a afirmação de que a transmissão segue só com vídeo quando a captura falha. A D-028 existe em `ontologia/decisoes.md` e registra a ausência de fallback.; O exemplo `loopback_probe` existe em `examples/`. Os ids de conceito citados (`screen-share`, `media-datagram`, `voice-frame`) existem em `dominio.yaml`.; Explica o porquê (eco da voz dos amigos), a alternativa descartada (loopback comum) e as armadilhas de implementação. A D-028 só cobre a decisão; as armadilhas e o procedimento de verificação são conhecimento novo. O texto é conciso e não tem segredos.; O documento admite que a exclusão da voz numa chamada real ainda depende de teste manual. Essa ressalva é honesta e deve ficar.
---

# Áudio do PC na transmissão usa process loopback, sem fallback

Conceitos: `screen-share`, `media-datagram`, `voice-frame`.

O loopback comum do WASAPI captura tudo o que o PC toca, inclusive a voz dos amigos que o próprio
Concord reproduz: ela voltaria para eles como eco. Por isso a captura usa
`ActivateAudioInterfaceAsync` com `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` e o PID do
próprio processo (Windows 10 2004+). Detalhes na D-028.

Se a ativação falhar, a transmissão segue só com vídeo e `ShareState.audio` vem `false`.
Alternativa descartada: cair para o loopback comum (eco garantido).

Armadilhas de implementação com a crate `windows`:
- o handler precisa de `#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]` e de
  `windows-core` como dependência direta;
- o `PROPVARIANT` aponta para a stack: use `ManuallyDrop`.

Verificar: `cargo run -p sala-core --example loopback_probe` tocando um som do Windows mostra pico
acima de -20 dBFS. A exclusão da voz numa chamada real ainda depende de teste manual.
