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
- **H3, saldo: ainda negativo na F1.**
  - Custo de criar e manter a ontologia nesta sessão: **US$ 0,86**, medido por `coletar.py orquestrador`. São 11 turnos do orquestrador com edição em `ontologia/`, contando Edit/Write e escrita via shell.
  - O valor é aproximado. Conta o turno inteiro, mesmo quando ele também mexeu em outra coisa, e não conta o tempo de pensar sem editar.
  - Com a economia mediana de US$ 0,06 por tarefa, o break-even fica em **≈ 14 tarefas de código**. A F1 teve 9 tarefas de dev.
  - Esse cálculo ainda ignora o custo dos bugs a mais do braço ONTO. Na F1 eles custaram uma tarefa de correção (T-004b, US$ 0,28).

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

### F1 — workspace, protocolo, rede e voz (fechada em 2026-10-22)

**Entrega:**
- Pacotes `sala-protocol` e `sala-core`, com 64 testes unitários e 4 suítes de integração: aceite, `net` e `f1_integration`.
- `clippy -D warnings` limpo.
- Duas instâncias `--profile a/b --synthetic-audio` conectam com TOFU e trocam Hello/Status. A voz chega com ≈ 99,6 % dos quadros e 0 perdidos, e o chat comprovadamente não vai a disco nem a log.

**Tarefas fora do A/B** (observacionais; todas com a ontologia v2 no prompt):

| tarefa | US$ | min | tool calls | ajudou | bugs | divergências |
|---|---|---|---|---|---|---|
| T-005 config/perfis | 0,45 | 3,7 | 20 | s | 0 | 2 (tipos de `Friend`) |
| T-004b underrun D-006 | 0,28 | 1,8 | 16 | s | 0 | 0 |
| T-007 QUIC | 1,05 | 11,0 | 25 | s | 0 | 2 (limite real do datagrama; reconexão do mesmo lado) |
| T-008 voz | 1,03 | 9,5 | 28 | s | 0 | 0 |
| T-009 sessão/chat | 0,79 | 9,9 | 24 | s | 1 baixo | 0 |

**Leitura:**
- Nas tarefas de integração (T-007 a T-009), "ajudou" foi sempre "s", e os agentes citam as **decisões** (D-005/006/007) como o que mais poupou trabalho, mais que os conceitos.
- As divergências mudaram de tipo. Na F1 inicial eram lacunas da ontologia que viravam bug; agora são descobertas da implementação que viram atualização da ontologia, como o limite real do quinn abaixo de 1200 bytes.
- Hipótese para a F2: o valor está mais em **decisões e invariantes curadas** que no glossário de conceitos.

**Totais:**

| | valor |
|---|---|
| Custo dos agentes (dev) | US$ 5,99 |
| Medição (revisão cega e outros) | US$ 1,44 |
| Orquestrador (sessão inteira, incluindo a criação da ontologia e do experimento) | US$ 9,43 |

**Ajustes para a F2:**
- testes ocultos adversariais;
- 2º A/A;
- par A/B da histerese do modo jogo;
- par A/B de uma tela de UI, onde se espera pouco ganho da ontologia;
- medir se o custo da ontologia cai depois da curadoria inicial (US$/tarefa de manutenção).
