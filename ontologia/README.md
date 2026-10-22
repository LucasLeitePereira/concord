# Ontologia do projeto

Modelo do domínio usado pela equipe para falar a mesma língua e achar código rápido.

| Arquivo | Conteúdo |
|---|---|
| `dominio.yaml` | conceitos, relações e invariantes (regras que o código deve garantir) |
| `mapa-codigo.yaml` | conceito → arquivos/módulos/testes que o implementam |
| `decisoes.md` | decisões de arquitetura (ADR curto) ligadas a conceitos |

## Regras
- Quem altera: **o sênior** (sessão principal). Pleno/sênior-revisor **propõem** mudanças no relatório.
- Agentes leem só o recorte citado no pedido: `grep -n -A15 'id: <conceito>' dominio.yaml`.
- Nomes de código seguem os `id`/`nome` dos conceitos.
- Divergência código × ontologia é registrada em `metricas/tarefas.jsonl` e resolvida pelo sênior (corrige um ou outro).
- Toda tarefa de código que crie/mova arquivos atualiza `mapa-codigo.yaml`.
