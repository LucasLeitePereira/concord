# Memória — júnior (curada pelo orquestrador; máx ~60 linhas)

## Ambiente
- Windows 11 nativo, repo em `D:\TI\concord`. Shell Git Bash; PowerShell via `powershell -NoProfile -Command "..."`.
- Python é `python`; não existe `python3`. Instalações com `winget ... --accept-source-agreements --accept-package-agreements --silent`.
- Rust: se `cargo` não estiver no PATH da sessão, use `~/.cargo/bin/cargo`.
- Hardware: i5-10210U 4 núcleos/8 threads, ~8 GB RAM. Builds pesados um por vez; no máximo 2 cargo em paralelo e só de crates pequenas.
- CMake 4.4 do winget não está no PATH do Bash e há um CMake 3.22 do Android SDK: antes de compilar audiopus_sys, `export PATH="$HOME/.cargo/bin:/c/Users/Jose/AppData/Local/Microsoft/WinGet/Packages/Kitware.CMake_Microsoft.Winget.Source_8wekyb3d8bbwe/cmake-4.4.4-windows-x86_64/bin:$PATH"`.
- Após winget install, o PATH da sessão não muda: use caminhos completos.
- Builds com timeout longo (até 600000 ms); nunca dois builds pesados juntos.

## Armadilhas (não repetir)
- Ao resumir saídas de teste, mostre a lista COMPLETA de testes e o placar final (um `tail` curto já cortou testes).

## Atalhos / padrões que funcionaram
- Formato "comando → resultado" com timeout longo é o que o sênior quer.

## Convenções do projeto
- Workspace Cargo em `sala/`. Testes: `cargo test --workspace`; lint: `cargo clippy --workspace --all-targets -- -D warnings`.
