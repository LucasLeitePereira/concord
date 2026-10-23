# Projeto — fluxo da equipe

> Contexto do projeto, build, regras, estado e próximos passos: **leia `AGENTS.md` primeiro**. Este arquivo só descreve o fluxo de subagentes do Claude Code. Desde 2026-10-23 o projeto está em **modo econômico**: sem experimento A/B nem revisão `senior`, e as métricas se limitam a `coletar.py registrar`.

## Equipe
- **Sênior (sessão principal, Opus 5.5):** orquestra, decide, delega, revisa, cura memórias/ontologia, registra métricas, commita (se autorizado).
- `senior` (opus): segunda opinião/revisão independente — usar com parcimônia.
- `pleno` (sonnet): código e testes.
- `junior` (haiku): ambiente, instalações, clones, rodar build/testes.
Escolha sempre o agente mais barato que resolve. Prompts de delegação: objetivo, arquivos/conceitos da ontologia relevantes (por `id`), critério de pronto, limites. Não repetir no prompt o que já está na memória do agente.

## Ciclo pós-tarefa (obrigatório)
1. Revisar a entrega (rodar testes se for código).
2. `python metricas/coletar.py registrar <agentId> --tarefa T-NNN --fase F --descricao ... --tentativas --bugs --conceitos --ajudou --divergencias --licoes`.
3. Memória: filtrar "lições candidatas" → `.claude/agent-memory/<agente>.md` (só durável e não-óbvio; consolidar; remover obsoleto; ≤ ~60 linhas).
4. Ontologia: aplicar mudanças propostas aceitas em `ontologia/` (`mapa-codigo.yaml` sempre que arquivos forem criados/movidos).
5. Fim de fase: `coletar.py resumo` + `coletar.py orquestrador <sessão>` → atualizar `metricas/ANALISE.md`.

## Onde fica
- Agentes: `.claude/agents/` · Memória deles: `.claude/agent-memory/`
- Ontologia: `ontologia/` (ver README) · Métricas: `metricas/`
