# Projeto — fluxo da equipe

> Contexto do projeto, build, regras, estado e próximos passos: **leia `AGENTS.md` primeiro**. Este arquivo só descreve o fluxo de subagentes do Claude Code. Desde 2026-10-23 o projeto está em **modo econômico**: sem experimento A/B nem revisão `senior`, e as métricas se limitam a `coletar.py registrar`.

## Equipe
- **Sênior (sessão principal, Opus 5.5):** orquestra, decide, delega, revisa, cura memórias/ontologia, registra métricas, commita (se autorizado).
- `senior` (opus): segunda opinião/revisão independente — usar com parcimônia.
- `pleno` (sonnet): código e testes.
- `junior` (haiku): ambiente, instalações, clones, rodar build/testes.
Escolha sempre o agente mais barato que resolve. Prompts de delegação: objetivo, arquivos/conceitos da ontologia relevantes (por `id`), critério de pronto, limites. Não repetir no prompt o que já está na memória do agente.

## Economia de tokens (2026-10-29)
- **Fatias pequenas:** o sênior divide pedidos grandes em fatias de no máximo ~US$ 1,50 ou um assunto por tarefa (ex.: protocolo → núcleo → UI), cada uma com o próprio critério de pronto. Fatia que passar de ~US$ 2 no `coletar.py registrar` vira lição registrada.
- **Leitura dirigida:** localizar com Grep ou `ontologia/mapa-codigo.yaml` e ler só o trecho da função (Read com offset/limit). Arquivo inteiro só com menos de ~300 linhas ou quando for reescrito. Imagem só em tarefa visual, uma por vez.
- **Testes:** o pleno roda só o teste do que mudou (`cargo test -p <crate> --test <arquivo> <filtro>` ou `--lib <módulo>`). A rodada completa (clippy + `cargo test --workspace`) é feita uma vez no fim, de preferência pelo `junior`, que devolve só o resumo e as falhas.
- **Sessões curtas:** uma tarefa ou fatia por sessão. Ao fechar, atualize o `AGENTS.md` §6 (passagem de bastão) e comece a próxima em sessão nova ou com `/clear`.
- **Estado num lugar só:** o estado do projeto fica só no `AGENTS.md` §6. Memória dos agentes guarda só armadilhas e atalhos; a ontologia guarda conceito, relação, invariante e decisão.

## Ciclo pós-tarefa (obrigatório)
1. Revisar a entrega (rodada completa de clippy + testes uma vez, de preferência pelo `junior`).
2. `python metricas/coletar.py registrar <agentId> --tarefa T-NNN --fase F --descricao ... --tentativas --bugs --conceitos --ajudou --divergencias --licoes`.
3. Memória: filtrar "lições candidatas" → `.claude/agent-memory/<agente>.md` (só armadilhas e atalhos duráveis e não-óbvios, nada de estado do projeto; consolidar; remover obsoleto; ≤ ~60 linhas).
3b. Atualizar o `AGENTS.md` §6 (estado e próximo passo) antes de encerrar a sessão.
4. Ontologia: aplicar mudanças propostas aceitas em `ontologia/` (`mapa-codigo.yaml` sempre que arquivos forem criados/movidos).
5. Fim de fase: `coletar.py resumo` + `coletar.py orquestrador <sessão>` → atualizar `metricas/ANALISE.md`.

## Onde fica
- Agentes: `.claude/agents/` · Memória deles: `.claude/agent-memory/`
- Ontologia: `ontologia/` (ver README) · Métricas: `metricas/`
