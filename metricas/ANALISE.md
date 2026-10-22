# Impacto da ontologia — análise

Quem atualiza é o sênior, ao fim de cada fase, a partir de `tarefas.jsonl`. Comandos: `python metricas/coletar.py resumo`, `ab` e `orquestrador <sessão>`. O desenho do experimento está em `EXPERIMENTO.md`.

## Perguntas que esta análise responde
1. O custo (tokens/US$) e a duração por tarefa caem ao longo do projeto, por efeito da memória e da ontologia?
2. Em quantas tarefas a ontologia ajudou (s/parcial/n), e em que tipo de tarefa?
3. Quantos erros ela evitou (invariante que pegou bug) e quantos causou (ontologia desatualizada ou errada)?
4. Quanto retrabalho e quantos bugs a revisão achou por tarefa?
5. Quanto custa manter a ontologia (tarefas do tipo `ontologia`) frente à economia estimada?

## Fase 1 — experimento A/B (3 pares + 1 A/A)

**Desenho resumido:**
- Mesmo agente (`pleno`/sonnet), mesmo commit, mesmo esqueleto de API e mesmos testes ocultos.
- ONTO recebe ids de conceitos e invariantes; CTRL recebe o trecho literal da spec e não tem `ontologia/`.
- A ordem dos braços foi sorteada. A revisão cega foi feita pelo `senior`, com rótulos X/Y/Z.
- A/A = um segundo ONTO na T-004.

### Por par (Δ = ONTO − CTRL; negativo = ONTO melhor, exceto em `pct_ocultos`)

| métrica | T-003 protocolo | T-004 jitter | T-006 identidade | ruído A/A (T-004) |
|---|---|---|---|---|
| custo US$ | −0,060 | −0,100 | −0,023 | 0,054 |
| duração s | −57 | −89 | +15 | 36 |
| tool calls | −1 | −3 | −8 | 5 |
| chamadas até a 1ª edição | −1 | +1 | 0 | 0 |
| tokens até a 1ª edição | −43 k | +17 k | +3 k | 1,3 k |
| arquivos lidos | −1 | 0 | −1 | 0 |
| bugs na revisão cega | 0 | +1 | +2 | 0 |
| violações de invariante | 0 | −1 | 0 | 1 |
| % testes ocultos | 0 | 0 | 0 | 18 pp |

**Agregado:**

| métrica | mediana do Δ | ONTO melhor/pior/empate | p (sinais) | p (Wilcoxon exato) |
|---|---|---|---|---|
| custo | −0,060 US$ | 3/0/0 | 0,25 | 0,25 |
| tool calls | −3 | 3/0/0 | 0,25 | 0,25 |
| bugs na revisão | +1 | 0/2/1 | 0,50 | 0,50 |

### Leitura por hipótese (preliminar, n = 3)

- **H1, custo de exploração: tendência a favor, não confirmada.**
  - ONTO foi mais barato nos 3 pares (−6 a −23 % de US$) e usou menos tool calls nos 3. Com n = 3, o menor p possível é 0,25.
  - Na T-004 o Δ de custo (−0,10) é maior que o ruído A/A (0,054). Já o Δ de tool calls (−3) é menor que o ruído (5).
  - "Tokens até a 1ª edição" não melhorou. O prompt ONTO é curto, mas o agente lê o recorte da ontologia antes de editar.
- **H2, menos bugs e violações: não suportada.**
  - Os testes ocultos empataram em 100 % nos dois braços dos 3 pares (efeito teto). O A/A teve 82 %, então a variância de correção entre execuções do mesmo braço é grande.
  - Na revisão cega, CTRL teve menos bugs em T-004 e T-006, e o revisor preferiu CTRL em T-003 e T-006.
  - Causa provável: a spec literal traz contexto que a ontologia condensada perdeu. Exemplos: na T-004, a semântica de underrun e DTX; na T-006, IPv4 mapeado em IPv6 no socket dual-stack.
- **H4, divergência de nomes: inconclusiva.** Houve 1 divergência, no braço ONTO da T-004 (alvo de atraso que não se adapta). A revisão apontou 2 lacunas da própria ontologia (`opus_payload`×`payload`, `from_self`).
- **H3, saldo:**
  - Custo de criar e manter a ontologia nesta sessão: US$ 0,26 (turnos do orquestrador com edição em `ontologia/`, medido por `coletar.py orquestrador`).
  - Com a economia mediana de US$ 0,06 por tarefa, o break-even fica em ≈ 4–5 tarefas de código.
  - Esse cálculo ignora o custo dos bugs a mais, que na F1 custaram uma tarefa de correção (T-004b, ≈ US$ 0,2).

### Efeito colateral útil: o experimento melhorou a ontologia
A revisão cega achou lacunas, e todas viraram decisões ou invariantes (ontologia v2):
- D-006: underrun/DTX;
- IP canônico em `inv-only-friends-connect`;
- tipos de `Friend` e `payload`.

As tarefas seguintes que usaram a v2 (T-004b) relataram "ajudou: s", com 0 divergências. O próximo par A/B testa se a v2 fecha a diferença de correção.

### Ajustes para os próximos pares
- Os testes ocultos ficaram fáceis demais (teto). Faltam casos de deriva/pausa e dual-stack. A partir da F2, incluir ao menos 2 testes "adversariais" por par, tirados de bugs reais da revisão.
- Manter A/A em mais um par para estimar melhor o ruído de correção.

## Fases
_(F1 em andamento: T-007, T-008 e T-009 serão acrescentadas no fechamento da fase)_
