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
- Heredoc grande (`cat <<'EOF'`) no Git Bash do Windows falha com "unexpected EOF", e replace por script Python com aspas/barras escapadas falha em silêncio: crie/edite código com Write/Edit.
- Clippy 1.99 (`-D warnings`): `collapsible_if` pede let-chains (`if a && let Some(x) = y`); `result_large_err` → `Box<toml::de::Error>` no enum de erro; `field_reassign_with_default` → `X { campo, ..Default::default() }`.
- IP de socket dual-stack chega como `::ffff:a.b.c.d`: compare com `IpAddr::to_canonical()`.
- `u8::from_str_radix` aceita `+` inicial: valide `is_ascii_hexdigit` antes de parsear hex.
- Buffer de tempo real (jitter etc.): além de perda/reordenação, teste deriva de relógio, pausa do remetente (DTX/mudo) e ressincronização; simule pausa com salto de timestamp (senão o estimador sobe o alvo) e calcule a retomada com `target_delay_ms()`.
- QUIC/quinn: `accept_bi` do outro lado só retorna quando chega dado no stream → escreva um frame logo após `open_bi`.
- Diretório temporário de teste no Windows: relógio tem baixa resolução (nanos repetem) → nome com contador atômico.
- Helper de teste `wait_for` que descarta eventos não casados esconde contagens: drene e conte antes.
- cpal 0.18 difere do que o modelo lembra (SampleRate=u32, nome via `description()?.name()`, `device_by_id`; importar trait `cpal::Sample`): consulte `~/.cargo/registry/src/*/cpal-0.18.*/examples`. Build inicial opus+cpal ≈ 2 min.
- Scripts que escrevem em pastas de entrega não podem apagar pastas onde o usuário já roda coisas: monte em diretório temporário.

## Atalhos / padrões que funcionaram
- Controle negativo em todo teste de lógica crítica; repetir testes de rede/concorrência N vezes para pegar flakiness.
- Lógica pura isolada em funções/structs testáveis (sem I/O) em vez de testar via rede/UI.
- Código que só o hardware valida (áudio, GPU): logs com o motivo de cada transição.
- Declare desvios do pedido; não esconda.
- Saída do clippy/cargo: filtre com `grep -E "^error|^warning|-->|Finished|^test result"` para economizar tokens.
- Caminho temporário do Git Bash para programa Windows: `$(cygpath -w "$T")`.
- Processo em background no Git Bash: `$!` não é o PID Windows; encerre com `taskkill //F //IM sala-core.exe`.
- Capturar logs `tracing` em teste tokio multi-thread exige `set_global_default` (um `#[test]` por arquivo); `set_default` é por thread.
- Duas instâncias que se discam em teste: porta livre via `UdpSocket::bind("127.0.0.1:0")`, ler e soltar.

## Convenções do projeto (Sala)
- Spec: `ideia/sala-handoff/PLANO-CLAUDE-CODE.md`; design: `ideia/sala-handoff/design/`.
- Workspace Cargo em `sala/` (`crates/protocol`, `crates/core` → bin `sala-core`, depois `crates/ui`).
- Código, identificadores e logs em inglês; textos de interface em pt-BR.
- Pronto = `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` passando.
- Dependências mínimas; crate pesada nova = justificar no relatório (o sênior registra em `ontologia/decisoes.md`).
- Nunca logar conteúdo de chat (`inv-chat-never-on-disk`).
