# Experimento: impacto da ontologia — pré-registro

Registrado em 2026-10-22, **antes de qualquer dado A/B**. Mudanças posteriores no desenho entram em "Desvios", com data e motivo.

## Pergunta
A ontologia (`ontologia/`: conceitos, relações, invariantes, mapa de código e decisões) melhora o desenvolvimento feito por agentes, comparada a entregar a spec crua, e compensa o custo de criá-la e mantê-la?

## Hipóteses
| # | Hipótese | Métrica primária | Direção esperada |
|---|---|---|---|
| H1 | Reduz o custo de exploração | `chamadas_ate_1a_edicao`, `tokens_ate_1a_edicao`, `arquivos_lidos_distintos`, `buscas` | ONTO < CTRL |
| H2 | Reduz bugs e violações de invariantes | % de testes ocultos aprovados; `violacoes` e `bugs` da revisão cega | ONTO ≥ CTRL em testes; ONTO < CTRL em violações |
| H3 | O saldo é positivo | Σ(economia por tarefa) × tarefas projetadas − (custo de criação + manutenção da ontologia) | > 0 |
| H4 | Reduz a divergência de nomes e conceitos | divergências código×ontologia apontadas na revisão | ONTO < CTRL |

Métricas secundárias: `custo_usd`, `duracao_s`, `tool_calls`, `tentativas`.

## Braços
Os dois braços usam o mesmo agente (`pleno`/sonnet), a mesma memória, o mesmo commit de partida e o mesmo esqueleto de API no prompt.
- **ONTO:** objetivo + ids dos conceitos e invariantes + esqueleto de API. O agente lê a ontologia, como no passo 2 do protocolo.
- **CTRL:** objetivo + trecho literal da spec que corresponde aos conceitos + o mesmo esqueleto. `ontologia/` é removida do worktree e a leitura é proibida. Se `leituras_ontologia > 0`, a execução é invalidada (`--invalido`) e repetida.
- **A/A:** o ONTO é repetido uma vez na T-004 para estimar o ruído natural do LLM.

## Procedimento por par
1. O sênior escreve o esqueleto de API e os testes ocultos (`metricas/ab/T-NNN/`) antes de delegar.
2. `git worktree add ab/T-NNN-onto` e `ab/T-NNN-ctrl` a partir do mesmo commit. A ordem é sorteada (`python -c "import random;print(random.choice(['onto-primeiro','ctrl-primeiro']))"`) e registrada em `--ordem`. Os braços rodam em sequência.
3. Os testes ocultos são aplicados nos dois worktrees: `cargo test` + `clippy`.
4. O agente `senior` faz a revisão cega: diffs rotulados X/Y (o rótulo é sorteado) e checklist de invariantes. Ele conta bugs, violações e divergências de nomes.
5. O merge fica com o melhor braço (testes ocultos > bugs > custo); em empate, vai o ONTO. O custo da revisão cega e dos testes ocultos é **overhead de medição** e não entra no custo de desenvolvimento.

## Amostra (8 pares, estratificada por tipo)
| Par | Fase | Tarefa | Tipo |
|---|---|---|---|
| 1 | F1 | T-003 protocolo (mensagens, framing, datagrama ≤ 1200) | tipos + invariante de tamanho |
| 2 | F1 | T-004 jitter buffer (+A/A) | lógica pura, muitas invariantes |
| 3 | F1 | T-006 identidade/pinning/allowlist/conexão simultânea | segurança, invariantes |
| 4 | F2 | histerese do detector de jogo | lógica pura |
| 5 | F3 | uma tela da UI | **pouca invariante** (controle de tipo) |
| 6 | F4 | fragmentação/remontagem de vídeo | lógica pura + invariantes |
| 7 | F4 | bitrate adaptativo | regra numérica |
| 8 | F5 | mixagem sem clipping | DSP simples |

## Análise
- Para cada par: Δ = ONTO − CTRL em cada métrica. Os resultados são mediana dos Δ, teste dos sinais e Wilcoxon exato (`coletar.py ab`).
- **Ruído:** |Δ| menor ou igual a |Δ A/A| conta como "sem efeito".
- **H3:** custo da ontologia = criação (Fase 0, turnos do sênior em `ontologia/`) + manutenção (`coletar.py orquestrador` → `custo_ontologia`). A economia por tarefa vem da mediana de Δ custo nos pares. O break-even é custo da ontologia ÷ economia mediana, em número de tarefas.
- **Limitações declaradas:** n = 8 (poder baixo, resultado indicativo), 1 execução por braço, um só modelo, o sênior é autor da ontologia e dos testes ocultos (risco de viés, mitigado pela revisão cega e por testes derivados da spec) e as tarefas não são independentes (o código mergeado de um par vira base do próximo).

## Desvios
- **2026-10-22 — concorrência entre pares.** Os pares T-003 e T-004 rodam em duas filas paralelas. Dentro de cada par, os braços continuam em sequência. Isso afeta `duracao_s` (métrica secundária), mas não tokens nem tool calls. Motivo: tempo de parede, numa máquina de 4 núcleos.
- **2026-10-22 — vazamento potencial.** `metricas/ab/` (os testes ocultos) entrou no commit base. Mitigação: `metricas/` e `ideia/` foram apagadas de todos os worktrees, e os transcripts são auditados à procura de acessos a "hidden"/"metricas".
- **2026-10-22 — F2, par T-010.** Ganha um 2º A/A (ONTO repetido), como previsto na ANALISE da F1. Os testes ocultos incluem casos adversariais (marcados `adv`), tirados das armadilhas reais da F1 e das regras de D-008. Os braços compartilham um `CARGO_TARGET_DIR` fora dos worktrees, aquecido antes do 1º braço, para que a compilação das dependências não pese só no braço que roda primeiro (afeta só `duracao_s`).
