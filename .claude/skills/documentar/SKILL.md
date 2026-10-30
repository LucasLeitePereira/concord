---
name: documentar
description: Use quando descobrir uma decisão, armadilha, convenção ou procedimento que custou investigação e não está no código nem na documentação. Registra o conhecimento em _pendentes/ para o revisor aprovar.
---

# Documentar conhecimento do projeto

Documentação gerada por LLM que repete o que já existe piora os agentes e custa mais tokens.
Registre só o que não dá para tirar do código.

## O que documentar
- O porquê das decisões, as alternativas descartadas e por que foram descartadas.
- Armadilhas e seus sintomas (o erro que aparece, o que parecia ser a causa).
- Convenções não óbvias.
- Procedimentos de operação (como subir, migrar, recuperar).

## O que NÃO documentar
- Resumo do que o código faz, listas de arquivos ou funções.
- O que o `git log` ou um comentário já diz.
- O que fica óbvio ao ler o código.

## Antes de escrever
1. Use a tool `buscar` do MCP docserver. Se já existe documento sobre o assunto, escreva a nova
   versão em `docs/_pendentes/` com o mesmo nome de arquivo e acrescente ao front matter
   `substitui: conhecimento/<arquivo>.md`. Não crie duplicata.
2. Escreva sempre em `docs/_pendentes/<slug-curto>.md`. Nunca em `conhecimento/`.

## Front matter (obrigatório, formato plano `chave: valor`)
- `titulo`: título curto.
- `tipo`: `decisao`, `armadilha`, `convencao` ou `operacao`.
- `gerado_por`: o id do seu modelo.
- `commit`: saída de `git rev-parse --short HEAD`.
- `arquivos`: código a que o documento se refere, separado por vírgula.
- `substitui`: opcional, só ao atualizar um documento existente.

## Corpo
Um título `#` e no máximo ~30 linhas: o contexto em 1-2 frases, a decisão ou armadilha, o porquê
e como verificar. Sem segredos, tokens, senhas ou dados pessoais.

## Exemplo
```markdown
---
titulo: Por que a ingestão usa WAL
tipo: decisao
gerado_por: claude-opus-5-5
commit: a0ecf9e
arquivos: src/docserver/index.py
---

# Por que a ingestão usa WAL

O watcher grava no índice enquanto o servidor MCP lê o mesmo arquivo SQLite.
Sem WAL, a leitura falhava com "database is locked" durante a ingestão.
Alternativa descartada: um banco por processo, que divergia do índice.
Verificar: `PRAGMA journal_mode;` deve devolver `wal`.
```

## Depois
O revisor aprova, ajusta ou rejeita. Rejeitados vão para `docs/_rejeitados/` com
`motivos_rejeicao` no front matter: corrija e mova o arquivo de volta para `_pendentes/`.
