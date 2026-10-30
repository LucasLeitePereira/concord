---
titulo: Rodada completa de testes e o f2_game_mode instável
tipo: operacao
gerado_por: claude-opus-5-5
commit: cb7b11f
arquivos: sala/crates/core/tests/f2_game_mode.rs, sala/crates/core/tests/f3_settings.rs
revisado_por: claude-sonnet-5-5
revisado_em: 2026-10-30T00:44:47Z
parecer: O código contradiz a faixa de 260–380 s. `TEST_TIMEOUT` em `f2_game_mode.rs:27` é de 180 s por volta. O `Elapsed(())` aparece quando uma volta passa de 180 s, e o teste não chega a 260–380 s.; O documento diz que o teste leva 170 s na rodada. Isso fica perto do limite de 180 s, e esse é o motivo real da instabilidade, mas o texto não explica isso.; O resto vale: o `F2_ROUNDS` existe (`f2_game_mode.rs:276`) e o procedimento de repetir o teste isolado é útil. Não há duplicata nem segredos.
---

# Rodada completa de testes e o f2_game_mode instável

Conceitos: `game-mode`, `core-process`.

Na máquina de desenvolvimento (i5-10210U, 8 GB), `cargo test --workspace` compila por ~15 min a frio (Tauri, wgpu, FFmpeg). O `f2_game_mode_end_to_end` leva ~38 s isolado e ~170 s na rodada completa. O limite por volta (`TEST_TIMEOUT` em `f2_game_mode.rs`) é de 180 s. Sob carga o teste passa desse limite e falha com `test timed out: Elapsed(())`.

Isso não é regressão por si só: rode de novo isolado (`cargo test -p sala-core --test f2_game_mode`). Se passar, é a instabilidade conhecida. O mesmo vale, com menos frequência, para `f3_settings` e `f1_integration`, que também têm timeouts fixos.

Dicas:
- use `--no-fail-fast` para ver o total real; sem ele o cargo para no primeiro binário que falha;
- `F2_ROUNDS=3` repete as voltas do f2 para caçar instabilidade (cada volta tem o próprio limite de 180 s);
- clippy e testes em sequência, nunca dois builds pesados no mesmo `target/`.
