# Memória — sênior (curada pelo orquestrador; máx ~60 linhas)

## Ambiente
- Windows 11 nativo, repo em `D:\TI\concord`, Git Bash. `python` (não `python3`). Rust MSVC.

## Armadilhas (não repetir)
- Revisões com "toda/sempre" sem conferir cada ramo do código geram falsos positivos/negativos.

## Atalhos
- Comece pelos conceitos citados e por `ontologia/mapa-codigo.yaml`; não varra o repo.

## Convenções do projeto (Sala)
- Spec: `ideia/sala-handoff/PLANO-CLAUDE-CODE.md`. Invariantes em `ontologia/dominio.yaml` (`id: inv-...`).
- Em revisão cega A/B: julgue só o diff e a checklist recebidos; não tente inferir qual braço é qual.
