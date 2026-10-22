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
- Heredoc grande (`cat <<'EOF'`) no Git Bash do Windows falha com "unexpected EOF": crie/edite arquivos com Write/Edit, nunca via shell.
- Scripts que escrevem em pastas de entrega não podem apagar pastas onde o usuário já roda coisas: monte em diretório temporário.

## Atalhos / padrões que funcionaram
- Controle negativo em todo teste de lógica crítica; repetir testes de rede/concorrência N vezes para pegar flakiness.
- Lógica pura isolada em funções/structs testáveis (sem I/O) em vez de testar via rede/UI.
- Código que só o hardware valida (áudio, GPU): logs com o motivo de cada transição.
- Declare desvios do pedido; não esconda.

## Convenções do projeto (Sala)
- Spec: `ideia/sala-handoff/PLANO-CLAUDE-CODE.md`; design: `ideia/sala-handoff/design/`.
- Workspace Cargo em `sala/` (`crates/protocol`, `crates/core` → bin `sala-core`, depois `crates/ui`).
- Código, identificadores e logs em inglês; textos de interface em pt-BR.
- Pronto = `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` passando.
- Dependências mínimas; crate pesada nova = justificar no relatório (o sênior registra em `ontologia/decisoes.md`).
- Nunca logar conteúdo de chat (`inv-chat-never-on-disk`).
