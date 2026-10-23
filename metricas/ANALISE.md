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

### Par 4 — T-010 histerese do detector de jogo (F2, com 2º A/A)

Ordem sorteada: CTRL primeiro, depois ONTO e A/A. Os testes ocultos tinham 16 casos, 7 deles adversariais (nome de arquivo por sufixo, alternância, override manual, `set_config`). Rótulos da revisão cega: X = ONTO, Y = A/A, Z = CTRL.

| métrica | Δ ONTO − CTRL | ruído A/A (T-010) |
|---|---|---|
| custo US$ | −0,041 (0,307 × 0,348) | 0,006 |
| duração s | −67 | 127 |
| tool calls | 0 (7 × 7) | 0 |
| chamadas até a 1ª edição | 0 | 1 |
| tokens até a 1ª edição | +2,6 k | 57 k |
| bugs na revisão cega | +1 (baixo) | 1 |
| violações de invariante | 0 | 0 |
| % testes ocultos | 0 (100 × 100; A/A 100) | 0 |

- **Teto de novo, mesmo com os casos adversariais.** O esqueleto trazia o contrato inteiro de D-008 nos comentários de doc, então os dois braços tinham a mesma informação para os casos difíceis. Nesse desenho, o A/B mede onde a informação está, e não se ela existe.
- **Bug do ONTO:** uma entrada vazia em `games` casava com um `exe_path` vazio. É robustez que nenhuma spec cobre; o CTRL e o A/A filtraram entradas vazias. O revisor ranqueou CTRL > ONTO > A/A, com a mesma semântica nos três. Fez o merge o CTRL.
- **O Δ de custo deste par (−0,041) é maior que o ruído do próprio A/A (0,006), mas menor que o ruído do A/A da T-004 (0,054).** Duração, chamadas e tokens até a 1ª edição ficaram dentro do ruído.

**Agregado com 4 pares** (`coletar.py ab`):

| métrica | mediana do Δ | ONTO melhor/pior/empate | p (sinais) | p (Wilcoxon exato) |
|---|---|---|---|---|
| custo | −0,051 US$ | 4/0/0 | 0,125 | 0,125 |
| tool calls | −2 | 3/0/1 | 0,25 | 0,25 |
| bugs na revisão | +1 | 0/3/1 | 0,25 | 0,25 |
| % testes ocultos | 0 | 0/0/4 | 1,0 | 1,0 |

**Leitura atualizada:**
- **H1:** a direção se mantém (ONTO mais barato nos 4 pares, −12 % no par 4). Com n = 4 ainda não há significância. O ganho está todo no custo, não em chamadas até a 1ª edição.
- **H2:** segue não suportada. ONTO teve mais bugs na revisão em 3 de 4 pares. No par 4 a diferença (1) fica dentro do ruído do A/A, porque o A/A também diferiu do ONTO em 1 bug.
- **H3:**
  - Custo da ontologia: US$ 0,86 (F1) + US$ 0,33 (F2) ≈ **US$ 1,19**.
  - Com a economia mediana de US$ 0,051 por tarefa, o break-even fica em **≈ 23 tarefas**. Até aqui houve 14 tarefas de dev, 9 na F1 e 5 na F2.
  - O custo dos bugs extras do ONTO continua fora da conta.

### Ajustes para o par 5 (UI)
- **Esqueleto sem o contrato nos comentários.** O contrato fica só onde o braço o receberia: na ontologia para o ONTO, no trecho da spec para o CTRL. Assim o par testa a fonte da informação.
- **Testes ocultos que pegam robustez fora da spec**, como entradas vazias e valores-limite da config. Nos 4 pares, foi aí que o ONTO perdeu.

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

### F2 — núcleo residente e modo jogo (fechada em 2026-10-22)

**Entrega:**
- `game_detector` (regras por nome de arquivo, histerese 2/3, override manual, D-008).
- Camada Win32 sem hooks: bandeja, menu, atalhos, PTT, leitura da janela em 1º plano e notificação só com o remetente (D-009, D-011).
- IPC por named pipe com instância única e ciclo de vida da UI (D-010).
- Orquestrador do modo jogo (D-012), logs com rotação diária, `measure.ps1`.
- 101 testes de lib e 10 suítes de integração, incluindo `f2_game_mode` (UI encerrada antes do `Status`, voz continua, chat vira notificação sem texto). `clippy -D warnings` limpo.
- **Metas §8 no modo jogo**, medidas com áudio sintético e sink nulo:

| métrica | medido | meta |
|---|---|---|
| working set | 3,8 MB | < 50 MB |
| private bytes | 7,0 MB | < 50 MB |
| CPU | 0,87 % da máquina (8 threads), ≈ 1,2 % em 6 núcleos | < 2 % |
| processos da Sala | só `sala-core` | só `sala-core` |

**Tarefas fora do A/B** (observacionais):

| tarefa | US$ | min | tool calls | ajudou | bugs | divergências |
|---|---|---|---|---|---|---|
| T-012 Win32 | 1,55 | 13,6* | 37 | s | 1 (teste com corrida, achado e corrigido pelo sênior no fechamento) | 0 |
| T-011 IPC/UI | 0,47 | 5,7 | 17 | s | 0 | 0 |
| T-013 integração | 2,03 | 29,2 | 58 | s | 1 médio (revisão: `Connected` repetido zerava chat e par de voz) | 0 |
| T-014 medição | 0,64 | 13,2 | 24 | s | 0 | 0 |

\* A duração registrada da T-012 (146 min) inclui a espera pelo limite de uso da conta. Os 13,6 min são o tempo de trabalho: 12 min na 1ª rodada e 3 na retomada.

**Leitura:**
- **A hipótese da F1 se confirmou na prática: o valor está nas decisões.** As 5 decisões novas (D-008 a D-012) foram escritas *antes* de delegar e serviram de contrato. Os 4 relatórios citam as decisões, e não os conceitos, como o que guiou o desenho. Houve 0 divergências código×ontologia.
  - O que as decisões fizeram, um documento de design comum também faria. A parte "ontologia" propriamente dita (glossário, relações) quase não aparece nos relatórios.
- **Os bugs reais da fase vieram de interação entre módulos**, não de lacuna da ontologia:
  - um evento duplicado herdado da F1;
  - um teste com corrida de threads.
  - Os dois só apareceram na revisão e na suíte rodada pelo sênior. Rodar a suíte completa na revisão continua indispensável: o agente relatou "tudo verde" na T-013, e a corrida falhou na 1ª execução do sênior.
- **O custo de manter a ontologia caiu:** US$ 0,33 na F2 contra US$ 0,86 na F1 (que incluía a criação), com 4 turnos de edição contados. A conta é aproximada: parte das edições foi feita por scripts Python no scratchpad, e o coletor pode não ter contado todas.
- **Memória que não pegou:** "não edite código por heredoc ou Python" estava na memória do pleno desde a F1, e 4 das 5 tarefas da F2 bateram de novo nesse erro. A regra subiu para a definição do agente (`.claude/agents/pleno.md`). Na F3, medir se a recorrência cai.

**Totais da F2:**

| | US$ |
|---|---|
| Agentes, dev (inclui os 3 braços do par) | 5,65 |
| Medição (revisão cega) | 0,45 |
| Orquestrador (sessão principal) | 6,19 |

**Ajustes para a F3:**
- par A/B da UI com o esqueleto sem contrato nos comentários;
- testes ocultos de robustez fora da spec;
- medir a recorrência do erro de heredoc depois da mudança no agente;
- validar à mão a voz com dispositivo real e a medição de CPU com cpal real.
