"""Testes de coletar.py (stdlib). Rodar da raiz do repo: python -m unittest metricas/test_coletar.py -v"""
import contextlib, io, json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import coletar  # noqa: E402

MODELO = "claude-sonnet-5-5"


def uso(i=0, o=0, cw=0, cr=0):
    return {"input_tokens": i, "output_tokens": o,
            "cache_creation_input_tokens": cw, "cache_read_input_tokens": cr}


def tok(u):
    return sum(u.values())


def linhas_msg(mid, blocos, u, rid=None, ts="2026-10-22T10:00:00Z"):
    """Uma linha JSONL por bloco, repetindo id/requestId/usage como no transcript real."""
    return [{"type": "assistant", "timestamp": ts, "requestId": rid or "req_" + mid,
             "message": {"id": mid, "model": MODELO, "usage": u, "content": [b]}} for b in blocos]


def tu(nome, **inp):
    return {"type": "tool_use", "name": nome, "input": inp}


TEXTO = {"type": "text", "text": "ok"}


def escrever(caminho, linhas):
    os.makedirs(os.path.dirname(caminho), exist_ok=True)
    with open(caminho, "w", encoding="utf-8") as f:
        for l in linhas:
            f.write(json.dumps(l) + "\n")


def capturar(fn, *args):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        fn(*args)
    return buf.getvalue()


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.old = (coletar.PROJ, coletar.TAREFAS)
        coletar.PROJ = os.path.join(self.tmp.name, "proj")
        coletar.TAREFAS = os.path.join(self.tmp.name, "tarefas.jsonl")
        self.addCleanup(lambda: (setattr(coletar, "PROJ", self.old[0]), setattr(coletar, "TAREFAS", self.old[1])))

    def agente(self, agent_id, linhas, tipo="pleno"):
        arq = os.path.join(coletar.PROJ, "sess1", "subagents", f"agent-{agent_id}.jsonl")
        escrever(arq, linhas)
        with open(arq.replace(".jsonl", ".meta.json"), "w", encoding="utf-8") as f:
            json.dump({"agentType": tipo}, f)
        return arq

    def tarefas(self, linhas):
        with open(coletar.TAREFAS, "w", encoding="utf-8") as f:
            for l in linhas:
                f.write(json.dumps(l) + "\n")


U = {k: uso(i=10 * (n + 1), o=5 * (n + 1)) for n, k in enumerate(["m1", "m2", "m3", "m4", "m5", "m6", "m7"])}


def transcrito_base(edicao_path="src/novo.rs"):
    l = []
    l += linhas_msg("m1", [TEXTO, tu("Read", file_path="ontologia\\dominio.yaml")], U["m1"])
    l += linhas_msg("m2", [tu("Edit", file_path="ontologia/dominio.yaml", old_string="a", new_string="b")], U["m2"])
    l += linhas_msg("m3", [tu("Grep", pattern="x")], U["m3"])
    l += linhas_msg("m4", [tu("Bash", command="grep -r foo src"), tu("Bash", command="ls")], U["m4"])
    l += linhas_msg("m5", [tu("Read", file_path="src/a.rs"), tu("Read", file_path="src/a.rs"),
                           tu("Read", file_path="src/b.rs")], U["m5"])
    l += linhas_msg("m6", [TEXTO, tu("Write", file_path=edicao_path, content="x")], U["m6"])
    l += linhas_msg("m7", [tu("Bash", command="cargo test")], U["m7"])
    return l


class TestMedir(Base):
    def test_dedup_usage_e_controle_negativo(self):
        arq = self.agente("a1", transcrito_base())
        m = coletar.medir(arq)
        esperado = sum(tok(u) for u in U.values())
        total = sum(v["in"] + v["out"] + v["cw"] + v["cr"] for v in m["modelos"].values())
        self.assertEqual(total, esperado)
        # controle negativo: sem dedup (soma de todas as linhas) o total seria outro
        ingenuo = sum(tok(d["message"]["usage"]) for d in coletar._linhas(arq))
        self.assertNotEqual(ingenuo, esperado)

    def test_ate_1a_edicao_ignora_ontologia(self):
        m = coletar.medir(self.agente("a1", transcrito_base()))
        # antes do Write: Read, Edit(onto), Grep, Bash, Bash, Read x3 = 8 chamadas
        self.assertEqual(m["chamadas_ate_1a_edicao"], 8)
        self.assertEqual(m["tokens_ate_1a_edicao"], sum(tok(U[k]) for k in ["m1", "m2", "m3", "m4", "m5", "m6"]))
        self.assertEqual(m["tool_calls"], 10)
        self.assertEqual(m["arquivos_lidos_distintos"], 3)  # ontologia\dominio.yaml, a.rs, b.rs
        self.assertEqual(m["buscas"], 2)  # Grep + Bash grep

    def test_edicao_em_metricas_nao_conta(self):
        m = coletar.medir(self.agente("a1", transcrito_base(edicao_path="D:\\TI\\concord\\metricas\\x.py")))
        self.assertIsNone(m["chamadas_ate_1a_edicao"])
        self.assertIsNone(m["tokens_ate_1a_edicao"])

    def test_sem_edicao_none(self):
        l = linhas_msg("m1", [tu("Read", file_path="a")], U["m1"])
        m = coletar.medir(self.agente("a1", l))
        self.assertIsNone(m["chamadas_ate_1a_edicao"])
        self.assertIsNone(m["tokens_ate_1a_edicao"])

    def test_edicao_windows_path_fora_da_ontologia_conta(self):
        l = linhas_msg("m1", [tu("Edit", file_path="D:\\TI\\concord\\src\\a.rs")], U["m1"])
        self.assertEqual(coletar.medir(self.agente("a1", l))["chamadas_ate_1a_edicao"], 0)

    def test_leituras_ontologia_barra_invertida(self):
        l = linhas_msg("m1", [tu("Read", file_path="ontologia\\dominio.yaml"),
                              tu("Bash", command="cat ontologia/README.md"),
                              tu("Read", file_path="src/a.rs")], U["m1"])
        self.assertEqual(coletar.medir(self.agente("a1", l))["leituras_ontologia"], 2)

    def test_conteudo_editado_que_cita_ontologia_nao_e_leitura(self):
        # regressão: Write de código que contém a string "ontologia/" invalidaria braços CTRL
        l = linhas_msg("m1", [tu("Write", file_path="metricas/x.py", content="p = 'ontologia/dominio.yaml'"),
                              tu("Edit", file_path="src/a.rs", new_string="// ver ontologia/"),
                              tu("Bash", command="cat > x.py <<'EOF'\np = 'ontologia/a'\nEOF"),
                              tu("SubagentHandback", message="não li ontologia/"),
                              tu("Grep", pattern="id:", path="ontologia/dominio.yaml")], U["m1"])
        self.assertEqual(coletar.medir(self.agente("a1", l))["leituras_ontologia"], 1)

    def test_grep_na_pasta_ontologia_sem_barra_final(self):
        # regressão: Grep com path "...\\ontologia" (sem barra) não era contado
        l = linhas_msg("m1", [tu("Grep", pattern="id: x", path="D:\\TI\\concord\\ab\\T-003-onto\\ontologia"),
                              tu("Read", file_path="sala/src/ontologiaX.rs")], U["m1"])
        self.assertEqual(coletar.medir(self.agente("a1", l))["leituras_ontologia"], 1)

    def test_powershell_busca(self):
        l = linhas_msg("m1", [tu("PowerShell", command="Select-String foo *.rs"),
                              tu("PowerShell", command="Get-Date")], U["m1"])
        self.assertEqual(coletar.medir(self.agente("a1", l))["buscas"], 1)


class TestOrquestrador(Base):
    def test_custo_ontologia(self):
        l = (linhas_msg("m1", [TEXTO, tu("Write", file_path="ontologia/a.yaml", content="x")], U["m1"])
             + linhas_msg("m2", [tu("Edit", file_path="src/a.rs")], U["m2"])
             + linhas_msg("m3", [tu("Edit", file_path="D:\\r\\ontologia\\b.yaml")], U["m3"]))
        arq = os.path.join(coletar.PROJ, "sessX.jsonl")
        escrever(arq, l)
        c = coletar.custo_ontologia(arq)
        self.assertEqual(c["n_edicoes_ontologia"], 2)
        self.assertEqual(c["tokens"], tok(U["m1"]) + tok(U["m3"]))  # m1 contado uma vez só
        esperado = sum(coletar.custo(MODELO, U[k]) for k in ("m1", "m3"))
        self.assertAlmostEqual(c["usd"], esperado, places=4)
        out = capturar(coletar.main, ["orquestrador", "sessX"])
        self.assertEqual(json.loads(out)["custo_ontologia"]["n_edicoes_ontologia"], 2)


class TestEstatistica(unittest.TestCase):
    def test_cinco_favoraveis(self):
        self.assertAlmostEqual(coletar.p_sinais(5, 0), 0.0625)
        self.assertAlmostEqual(coletar.p_wilcoxon([-1, -2, -3, -4, -5]), 0.0625)

    def test_zero_descartado(self):
        self.assertAlmostEqual(coletar.p_sinais(4, 0), 0.125)
        self.assertAlmostEqual(coletar.p_wilcoxon([1, 1, 0, 2, 3]), 0.125)

    def test_misto(self):
        self.assertAlmostEqual(coletar.p_sinais(4, 1), 0.375)
        self.assertAlmostEqual(coletar.p_wilcoxon([-1, 2, 3, 4, 5]), 0.125)

    def test_simetrico_p1(self):
        self.assertEqual(coletar.p_wilcoxon([1, -1]), 1.0)
        self.assertEqual(coletar.p_wilcoxon([0, 0]), 1.0)

    def test_empates_ranks_medios(self):
        # |d| = 1,1,1 -> postos 2,2,2; todas positivas: extremos 2/8
        self.assertAlmostEqual(coletar.p_wilcoxon([1, 1, 1]), 0.25)


def par(i, braco, custo, **kw):
    base = {"id": f"T-{i}", "par": f"P{i}", "braco": braco, "custo_usd": custo, "duracao_s": 10,
            "tool_calls": 5, "chamadas_ate_1a_edicao": 3, "tokens_ate_1a_edicao": 100,
            "arquivos_lidos_distintos": 2, "buscas": 1, "bugs_na_revisao": 0, "violacoes": 0,
            "ocultos_ok": 8, "ocultos_total": 10, "fase": "F1", "agente": "pleno", "tipo": "dev"}
    base.update(kw)
    return base


class TestAB(Base):
    def test_cinco_pares_favoraveis(self):
        ls = []
        for i in range(1, 6):
            ls.append(par(i, "onto", 1.0, chamadas_ate_1a_edicao=2, ocultos_ok=10))
            ls.append(par(i, "ctrl", 1.0 + 0.1 * i, chamadas_ate_1a_edicao=3 + i))
        self.tarefas(ls)
        out = capturar(coletar.cmd_ab, None)
        # agregado: n, mediana, melhor, pior, empate, p sinais, p wilcoxon
        agg = [l for l in out.split("### Agregado")[1].splitlines() if l.startswith("| custo_usd")][0]
        c = [x.strip() for x in agg.split("|")[2:-1]]
        self.assertEqual(c[0], "5")
        self.assertEqual(c[2:5], ["5", "0", "0"])
        self.assertEqual(c[5:], ["0.0625", "0.0625"])
        # %ocultos: onto 100% vs ctrl 80% -> maior e melhor
        agg = [l for l in out.split("### Agregado")[1].splitlines() if l.startswith("| pct_ocultos")][0]
        self.assertEqual([x.strip() for x in agg.split("|")[2:-1]][2:5], ["5", "0", "0"])
        # empate em duracao_s (todos 0)
        agg = [l for l in out.split("### Agregado")[1].splitlines() if l.startswith("| duracao_s")][0]
        self.assertEqual([x.strip() for x in agg.split("|")[2:-1]][2:5], ["0", "0", "5"])
        self.assertIn("### Par P1", out)

    def test_zero_e_invalido_e_aa(self):
        ls = []
        for i in range(1, 5):
            ls.append(par(i, "onto", 1.0))
            ls.append(par(i, "ctrl", 1.0 if i == 1 else 2.0))
        ls.append(par(1, "aa", 1.5))
        ls.append(par(9, "onto", 0.1))
        ls.append(par(9, "ctrl", 9.0, invalido=True))  # invalido: par 9 incompleto, ignorado
        self.tarefas(ls)
        out = capturar(coletar.cmd_ab, None)
        self.assertNotIn("Par P9", out)
        self.assertIn("Agregado (4 pares)", out)
        agg = [l for l in out.split("### Agregado")[1].splitlines() if l.startswith("| custo_usd")][0]
        c = [x.strip() for x in agg.split("|")[2:-1]]
        self.assertEqual(c[2:5], ["3", "0", "1"])
        self.assertEqual(c[5:], ["0.2500", "0.2500"])  # n efetivo 3 -> 2/8 (sinais), 2/8 (wilcoxon)
        self.assertIn("ruído", out)  # par 1 tem aa
        self.assertIn("| custo_usd | 1 | 1 | 0 | 1.5 | 0.5 |", out)

    def test_linha_antiga_compat(self):
        antiga = {"id": "T-000", "fase": "setup", "agente": "junior", "custo_usd": 0.5, "duracao_s": 60,
                  "tool_calls": 3, "leituras_ontologia": 0, "tentativas": 1, "bugs_na_revisao": 0,
                  "ontologia_ajudou": "na", "divergencias_codigo_ontologia": 0}
        self.tarefas([antiga, par(1, "onto", 1.0), par(1, "ctrl", 2.0)])
        out = capturar(coletar.cmd_ab, None)
        self.assertIn("Agregado (1 pares)", out)
        res = capturar(coletar.main, ["resumo"])
        self.assertIn("junior", res)

    def test_resumo_medicao_separado_e_breakeven(self):
        ls = [par(1, "onto", 1.0), par(1, "ctrl", 3.0), par(2, "onto", 1.0), par(2, "ctrl", 2.0),
              par(3, "onto", 1.0), par(3, "ctrl", 5.0),
              par(4, "na", 100.0, tipo="medicao")]
        self.tarefas(ls)
        out = capturar(coletar.main, ["resumo", "--custo-ontologia", "8"])
        self.assertIn("Custo de desenvolvimento: US$ 13.000", out)  # sem os 100 da medicao
        self.assertIn("Overhead de medição (fora do custo de dev): US$ 100.000", out)
        self.assertIn("Break-even: 4.0 tarefas", out)  # mediana(2,1,4)=2 -> 8/2
        # controle negativo: sem economia
        self.tarefas([par(1, "onto", 3.0), par(1, "ctrl", 1.0)])
        self.assertIn("sem economia", capturar(coletar.main, ["resumo", "--custo-ontologia", "8"]))


class TestRegistrar(Base):
    def test_campos_novos(self):
        self.agente("abc", transcrito_base())
        capturar(coletar.main, ["registrar", "abc", "--tarefa", "T-003", "--fase", "F1", "--descricao", "d",
                                "--braco", "onto", "--ordem", "onto-primeiro", "--ocultos-ok", "7",
                                "--ocultos-total", "9", "--violacoes", "2", "--tipo", "dev"])
        capturar(coletar.main, ["registrar", "abc", "--tarefa", "T-004", "--fase", "F1", "--descricao", "d",
                                "--par", "T-003", "--braco", "ctrl", "--invalido", "--tipo", "medicao"])
        with open(coletar.TAREFAS, encoding="utf-8") as f:
            a, b = [json.loads(l) for l in f]
        self.assertEqual((a["braco"], a["par"], a["ordem"], a["ocultos_ok"], a["ocultos_total"], a["violacoes"],
                          a["invalido"], a["tipo"]), ("onto", "T-003", "onto-primeiro", 7, 9, 2, False, "dev"))
        self.assertEqual((a["chamadas_ate_1a_edicao"], a["arquivos_lidos_distintos"], a["buscas"]), (8, 3, 2))
        self.assertEqual(a["tokens_ate_1a_edicao"], sum(tok(U[k]) for k in ["m1", "m2", "m3", "m4", "m5", "m6"]))
        self.assertEqual((b["par"], b["invalido"], b["tipo"], b["ocultos_ok"]), ("T-003", True, "medicao", None))

    def test_defaults(self):
        self.agente("abc", transcrito_base())
        capturar(coletar.main, ["registrar", "abc", "--tarefa", "T-005", "--fase", "F1", "--descricao", "d"])
        with open(coletar.TAREFAS, encoding="utf-8") as f:
            a = json.loads(f.readline())
        self.assertEqual((a["braco"], a["par"], a["violacoes"], a["invalido"], a["tipo"]),
                         ("na", "T-005", 0, False, "dev"))


class TestAmbiente(unittest.TestCase):
    def test_proj_derivado_da_raiz(self):
        import re
        self.assertEqual(re.sub(r"[^A-Za-z0-9]", "-", "D:\\TI\\concord"), "D--TI-concord")


if __name__ == "__main__":
    unittest.main()
