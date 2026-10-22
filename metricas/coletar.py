#!/usr/bin/env python3
"""Métricas da equipe de agentes.

  coletar.py agente <agentId>                     # tokens/custo/duração de um subagente
  coletar.py registrar <agentId> --tarefa T-001 --fase F1 --descricao "..." \
      [--tentativas 1] [--bugs 0] [--conceitos a,b] [--ajudou s|n|parcial|na] \
      [--divergencias 0] [--licoes 0] [--obs "..."] \
      [--braco onto|ctrl|aa|na] [--par T-NNN] [--ordem "..."] \
      [--ocultos-ok N --ocultos-total M] [--violacoes 0] [--invalido] \
      [--tipo dev|medicao|ontologia|setup]            # grava linha em tarefas.jsonl
  coletar.py orquestrador <sessionId>              # custo da sessão principal (sênior)
  coletar.py ab                                    # A/B ONTO x CTRL por par (sinais + Wilcoxon exato)
  coletar.py resumo [--custo-ontologia USD]        # tabelas por fase/agente + ontologia + break-even

Env: CONCORD_CLAUDE_PROJ sobrescreve a pasta de transcripts do projeto.
"""
import argparse, glob, json, os, re, sys
from datetime import datetime
from itertools import combinations
from math import comb
from statistics import median

AQUI = os.path.dirname(os.path.abspath(__file__))
RAIZ = os.path.dirname(AQUI)
PROJ = os.environ.get("CONCORD_CLAUDE_PROJ") or os.path.join(
    os.path.expanduser("~"), ".claude", "projects", re.sub(r"[^A-Za-z0-9]", "-", RAIZ))
TAREFAS = os.path.join(AQUI, "tarefas.jsonl")
with open(os.path.join(AQUI, "precos.json"), encoding="utf-8") as _f:
    PRECOS = json.load(_f)

EDITORES = ("Edit", "Write", "NotebookEdit")
TERMOS_BUSCA = ("grep", "rg ", "find ", "Select-String", "Get-ChildItem")
# "ontologia" como segmento de caminho/palavra (com ou sem barra final)
RE_ONTOLOGIA = re.compile(r"(?<![A-Za-z0-9_-])ontologia(?![A-Za-z0-9_-])")


def custo(modelo, u):
    p = PRECOS.get(modelo)
    if not p:
        return None
    cc = u.get("cache_creation") or {}
    w1h = cc.get("ephemeral_1h_input_tokens", 0)
    w5m = cc.get("ephemeral_5m_input_tokens", u.get("cache_creation_input_tokens", 0) - w1h)
    return (u.get("input_tokens", 0) * p["in"] + u.get("output_tokens", 0) * p["out"]
            + w5m * p["w5m"] + w1h * p["w1h"]
            + u.get("cache_read_input_tokens", 0) * p["read"]) / 1e6


def _em_pasta(caminho, pasta):
    return f"/{pasta}/" in "/" + str(caminho).replace("\\", "/").lstrip("/")


def _linhas(arquivo):
    with open(arquivo, encoding="utf-8") as f:
        for linha in f:
            try:
                d = json.loads(linha)
            except ValueError:
                continue
            if isinstance(d, dict):
                yield d


def _tokens(u):
    return (u.get("input_tokens", 0) + u.get("output_tokens", 0)
            + u.get("cache_creation_input_tokens", 0) + u.get("cache_read_input_tokens", 0))


def _edicao_ontologia(b):
    return (b.get("type") == "tool_use" and b.get("name") in ("Edit", "Write")
            and _em_pasta((b.get("input") or {}).get("file_path", ""), "ontologia"))


def medir(arquivo):
    """Soma usage (dedup por message.id+requestId), duração, uso da ontologia e custo de exploração."""
    vistos, tot, tempos = set(), {}, []
    leituras_onto = chamadas = buscas = 0
    lidos = set()
    ate_edicao = None  # (chamadas, tokens)
    tokens_acum = 0
    for d in _linhas(arquivo):
        if d.get("timestamp"):
            tempos.append(d["timestamp"])
        m = d.get("message") or {}
        if d.get("type") != "assistant" or not isinstance(m, dict):
            continue
        u, chave = m.get("usage"), (m.get("id"), d.get("requestId"))
        if u and chave not in vistos:
            vistos.add(chave)
            t = tot.setdefault(m.get("model", "?"), {"in": 0, "out": 0, "cw": 0, "cr": 0, "usd": 0.0})
            t["in"] += u.get("input_tokens", 0)
            t["out"] += u.get("output_tokens", 0)
            t["cw"] += u.get("cache_creation_input_tokens", 0)
            t["cr"] += u.get("cache_read_input_tokens", 0)
            t["usd"] += custo(m.get("model"), u) or 0
            tokens_acum += _tokens(u)
        for b in m.get("content") or []:
            if not (isinstance(b, dict) and b.get("type") == "tool_use"):
                continue
            nome, inp = b.get("name"), b.get("input") or {}
            if (nome in EDITORES and ate_edicao is None
                    and not any(_em_pasta(inp.get("file_path", ""), p) for p in ("ontologia", "metricas"))):
                ate_edicao = (chamadas, tokens_acum)
            chamadas += 1
            # só leitura/busca/shell: conteúdo escrito (Edit/Write, heredoc, relatório) que cita
            # "ontologia/" não é leitura; do comando shell vale só a 1ª linha
            if nome in ("Read", "Grep", "Glob"):
                alvo = json.dumps(inp)
            elif nome in ("Bash", "PowerShell"):
                alvo = str(inp.get("command", "")).split("\n", 1)[0]
            else:
                alvo = ""
            if RE_ONTOLOGIA.search(alvo.replace("\\\\", "/").replace("\\", "/")):
                leituras_onto += 1
            if nome == "Read" and inp.get("file_path"):
                lidos.add(inp["file_path"])
            if nome in ("Grep", "Glob"):
                buscas += 1
            elif nome in ("Bash", "PowerShell") and any(s in str(inp.get("command", "")) for s in TERMOS_BUSCA):
                buscas += 1
    dur = 0
    if tempos:
        f = lambda s: datetime.fromisoformat(s.replace("Z", "+00:00"))
        dur = round((f(max(tempos)) - f(min(tempos))).total_seconds())
    return {"modelos": tot, "duracao_s": dur, "tool_calls": chamadas, "leituras_ontologia": leituras_onto,
            "chamadas_ate_1a_edicao": ate_edicao[0] if ate_edicao else None,
            "tokens_ate_1a_edicao": ate_edicao[1] if ate_edicao else None,
            "arquivos_lidos_distintos": len(lidos), "buscas": buscas}


def custo_ontologia(arquivo):
    """Tokens/US$ das mensagens (dedup) com Edit/Write em ontologia/."""
    chaves, n = set(), 0
    for d in _linhas(arquivo):
        m = d.get("message") or {}
        if d.get("type") != "assistant" or not isinstance(m, dict):
            continue
        for b in m.get("content") or []:
            if isinstance(b, dict) and _edicao_ontologia(b):
                n += 1
                chaves.add((m.get("id"), d.get("requestId")))
    vistos, tokens, usd = set(), 0, 0.0
    for d in _linhas(arquivo):
        m = d.get("message") or {}
        if d.get("type") != "assistant" or not isinstance(m, dict):
            continue
        chave = (m.get("id"), d.get("requestId"))
        u = m.get("usage")
        if chave in chaves and u and chave not in vistos:
            vistos.add(chave)
            tokens += _tokens(u)
            usd += custo(m.get("model"), u) or 0
    return {"tokens": tokens, "usd": round(usd, 4), "n_edicoes_ontologia": n}


def achar_agente(agent_id):
    arqs = glob.glob(os.path.join(glob.escape(PROJ), "*", "subagents", f"agent-{agent_id}.jsonl"))
    if not arqs:
        sys.exit(f"transcript do agente {agent_id} não encontrado")
    meta = arqs[0].replace(".jsonl", ".meta.json")
    tipo = "?"
    if os.path.exists(meta):
        with open(meta, encoding="utf-8") as f:
            tipo = json.load(f).get("agentType", "?")
    return arqs[0], tipo


def cmd_agente(a):
    arq, tipo = achar_agente(a.agent_id)
    print(json.dumps({"agente": tipo, **medir(arq)}, indent=2, ensure_ascii=False))


def cmd_registrar(a):
    arq, tipo = achar_agente(a.agent_id)
    m = medir(arq)
    soma = lambda k: sum(v[k] for v in m["modelos"].values())
    linha = {
        "id": a.tarefa, "fase": a.fase, "data": datetime.now().isoformat(timespec="seconds"),
        "agente": tipo, "agent_id": a.agent_id, "modelo": ",".join(m["modelos"]),
        "descricao": a.descricao,
        "tokens_in": soma("in"), "tokens_out": soma("out"),
        "cache_write": soma("cw"), "cache_read": soma("cr"),
        "custo_usd": round(soma("usd"), 4), "duracao_s": m["duracao_s"],
        "tool_calls": m["tool_calls"], "leituras_ontologia": m["leituras_ontologia"],
        "chamadas_ate_1a_edicao": m["chamadas_ate_1a_edicao"],
        "tokens_ate_1a_edicao": m["tokens_ate_1a_edicao"],
        "arquivos_lidos_distintos": m["arquivos_lidos_distintos"], "buscas": m["buscas"],
        "tentativas": a.tentativas, "bugs_na_revisao": a.bugs,
        "conceitos_consultados": [c for c in a.conceitos.split(",") if c],
        "ontologia_ajudou": a.ajudou, "divergencias_codigo_ontologia": a.divergencias,
        "licoes_novas": a.licoes, "obs": a.obs,
        "braco": a.braco, "par": a.par or a.tarefa, "ordem": a.ordem,
        "ocultos_ok": a.ocultos_ok, "ocultos_total": a.ocultos_total,
        "violacoes": a.violacoes, "invalido": bool(a.invalido), "tipo": a.tipo,
    }
    with open(TAREFAS, "a", encoding="utf-8") as f:
        f.write(json.dumps(linha, ensure_ascii=False) + "\n")
    print(json.dumps(linha, ensure_ascii=False))


def cmd_orquestrador(a):
    arq = os.path.join(PROJ, f"{a.session_id}.jsonl")
    print(json.dumps({"agente": "senior-orquestrador", **medir(arq),
                      "custo_ontologia": custo_ontologia(arq)}, indent=2, ensure_ascii=False))


# ---------- estatística (stdlib) ----------

def p_sinais(melhor, pior):
    n = melhor + pior
    if n == 0:
        return 1.0
    k = min(melhor, pior)
    return min(1.0, 2 * sum(comb(n, i) for i in range(k + 1)) / 2 ** n)


def p_wilcoxon(deltas):
    """Wilcoxon signed-rank exato, bicaudal: enumera as 2^n atribuições de sinal (descarta Δ=0)."""
    ds = [d for d in deltas if d != 0]
    n = len(ds)
    if n == 0:
        return 1.0
    abs_ = sorted(abs(d) for d in ds)
    ranks2 = {}  # posto médio dobrado (inteiro)
    i = 0
    while i < n:
        j = i
        while j < n and abs_[j] == abs_[i]:
            j += 1
        ranks2[abs_[i]] = i + j + 1  # 2 * média de (i+1..j)
        i = j
    r2 = [ranks2[abs(d)] for d in ds]
    total2 = sum(r2)
    obs = sum(r for r, d in zip(r2, ds) if d > 0)
    dev_obs = abs(2 * obs - total2)
    cont = 0
    for k in range(n + 1):
        for sub in combinations(range(n), k):
            if abs(2 * sum(r2[i] for i in sub) - total2) >= dev_obs:
                cont += 1
    return cont / 2 ** n


# ---------- A/B ----------

METRICAS_AB = ["custo_usd", "duracao_s", "tool_calls", "chamadas_ate_1a_edicao", "tokens_ate_1a_edicao",
               "arquivos_lidos_distintos", "buscas", "bugs_na_revisao", "violacoes", "pct_ocultos"]


def _valor(l, k):
    if k == "pct_ocultos":
        ok, tot = l.get("ocultos_ok"), l.get("ocultos_total")
        return 100.0 * ok / tot if ok is not None and tot else None
    return l.get(k)


def _ler_linhas():
    if not os.path.exists(TAREFAS):
        return []
    with open(TAREFAS, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def pares_ab(linhas):
    pares = {}
    for l in linhas:
        if l.get("braco") in ("onto", "ctrl", "aa") and not l.get("invalido"):
            pares.setdefault(l.get("par") or l["id"], {})[l["braco"]] = l  # última linha do braço vence
    return {p: b for p, b in pares.items() if "onto" in b and "ctrl" in b}


def _f(v):
    return "-" if v is None else (f"{v:.4g}" if isinstance(v, float) else str(v))


def cmd_ab(_):
    pares = pares_ab(_ler_linhas())
    if not pares:
        return print("sem pares A/B (braco onto+ctrl, sem invalido)")
    deltas = {k: [] for k in METRICAS_AB}
    for par, b in sorted(pares.items()):
        print(f"\n### Par {par}\n")
        tem_aa = "aa" in b
        print("| métrica | onto | ctrl | Δ (onto−ctrl) |" + (" aa | \\|onto−aa\\| (ruído) |" if tem_aa else ""))
        print("|---|---|---|---|" + ("---|---|" if tem_aa else ""))
        for k in METRICAS_AB:
            o, c = _valor(b["onto"], k), _valor(b["ctrl"], k)
            d = None if o is None or c is None else o - c
            if d is not None:
                deltas[k].append(d)
            linha = f"| {k} | {_f(o)} | {_f(c)} | {_f(d)} |"
            if tem_aa:
                x = _valor(b["aa"], k)
                linha += f" {_f(x)} | {_f(None if x is None or o is None else abs(o - x))} |"
            print(linha)
    print(f"\n### Agregado ({len(pares)} pares)\n")
    print("| métrica | n | mediana Δ | onto melhor | onto pior | empate | p sinais | p Wilcoxon |")
    print("|---|---|---|---|---|---|---|---|")
    for k in METRICAS_AB:
        ds = deltas[k]
        if not ds:
            print(f"| {k} | 0 | - | - | - | - | - | - |")
            continue
        sinal = 1 if k == "pct_ocultos" else -1  # maior é melhor só em %ocultos
        mel = sum(1 for d in ds if d * sinal > 0)
        pio = sum(1 for d in ds if d * sinal < 0)
        print(f"| {k} | {len(ds)} | {_f(median(ds))} | {mel} | {pio} | {len(ds) - mel - pio} "
              f"| {p_sinais(mel, pio):.4f} | {p_wilcoxon(ds):.4f} |")


# ---------- resumo ----------

def cmd_resumo(a):
    linhas = _ler_linhas()
    if not linhas:
        return print("sem tarefas registradas")
    dev = [l for l in linhas if l.get("tipo") != "medicao"]
    med = [l for l in linhas if l.get("tipo") == "medicao"]
    grupos = {}
    for l in dev:
        g = grupos.setdefault((l["fase"], l["agente"]), {"n": 0, "usd": 0, "dur": 0, "tent": 0, "bugs": 0})
        g["n"] += 1; g["usd"] += l["custo_usd"]; g["dur"] += l["duracao_s"]
        g["tent"] += l.get("tentativas", 1); g["bugs"] += l.get("bugs_na_revisao", 0)
    print("| fase | agente | tarefas | US$ | US$/tarefa | min/tarefa | tentativas/tarefa | bugs |")
    print("|---|---|---|---|---|---|---|---|")
    for (fase, ag), g in sorted(grupos.items()):
        n = g["n"]
        print(f"| {fase} | {ag} | {n} | {g['usd']:.3f} | {g['usd']/n:.3f} | {g['dur']/n/60:.1f} "
              f"| {g['tent']/n:.1f} | {g['bugs']} |")
    print(f"\nCusto de desenvolvimento: US$ {sum(l['custo_usd'] for l in dev):.3f} ({len(dev)} linhas)")
    print(f"Overhead de medição (fora do custo de dev): US$ {sum(l['custo_usd'] for l in med):.3f} ({len(med)} linhas)")
    aj = {}
    for l in dev:
        k = l.get("ontologia_ajudou", "na")
        aj[k] = aj.get(k, 0) + 1
    print("\nOntologia ajudou:", ", ".join(f"{k}={v}" for k, v in sorted(aj.items())))
    print("Divergências código×ontologia:", sum(l.get("divergencias_codigo_ontologia", 0) for l in dev))
    print("Leituras da ontologia (tool calls):", sum(l.get("leituras_ontologia", 0) for l in dev))
    if a.custo_ontologia is not None:
        eco = [b["ctrl"]["custo_usd"] - b["onto"]["custo_usd"] for b in pares_ab(linhas).values()]
        if not eco:
            return print("\nBreak-even: sem pares A/B")
        med_eco = median(eco)
        if med_eco <= 0:
            print(f"\nBreak-even: sem economia (mediana ctrl−onto = US$ {med_eco:.4f})")
        else:
            print(f"\nBreak-even: {a.custo_ontologia / med_eco:.1f} tarefas "
                  f"(US$ {a.custo_ontologia} ÷ economia mediana US$ {med_eco:.4f}, {len(eco)} pares)")


def construir_parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    s = p.add_subparsers(dest="cmd", required=True)
    s.add_parser("agente").add_argument("agent_id")
    r = s.add_parser("registrar")
    r.add_argument("agent_id")
    r.add_argument("--tarefa", required=True)
    r.add_argument("--fase", required=True)
    r.add_argument("--descricao", required=True)
    r.add_argument("--tentativas", type=int, default=1)
    r.add_argument("--bugs", type=int, default=0)
    r.add_argument("--conceitos", default="")
    r.add_argument("--ajudou", choices=["s", "n", "parcial", "na"], default="na")
    r.add_argument("--divergencias", type=int, default=0)
    r.add_argument("--licoes", type=int, default=0)
    r.add_argument("--obs", default="")
    r.add_argument("--braco", choices=["onto", "ctrl", "aa", "na"], default="na")
    r.add_argument("--par", default=None)
    r.add_argument("--ordem", default="")
    r.add_argument("--ocultos-ok", type=int, default=None)
    r.add_argument("--ocultos-total", type=int, default=None)
    r.add_argument("--violacoes", type=int, default=0)
    r.add_argument("--invalido", action="store_true")
    r.add_argument("--tipo", choices=["dev", "medicao", "ontologia", "setup"], default="dev")
    s.add_parser("orquestrador").add_argument("session_id")
    s.add_parser("ab")
    rs = s.add_parser("resumo")
    rs.add_argument("--custo-ontologia", type=float, default=None)
    return p


def main(argv=None):
    a = construir_parser().parse_args(argv)
    {"agente": cmd_agente, "registrar": cmd_registrar, "orquestrador": cmd_orquestrador,
     "ab": cmd_ab, "resumo": cmd_resumo}[a.cmd](a)


if __name__ == "__main__":
    main()
