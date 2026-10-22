#!/usr/bin/env python
"""Aplica os testes ocultos de um par num worktree e conta aprovados.

  python metricas/ab/aplicar_ocultos.py T-003 ab/T-003-onto

Copia metricas/ab/<par>/hidden_*.rs para o crate indicado na 1ª linha "Copiar para ..."
de cada arquivo, roda `cargo test --test <nome>` e imprime JSON {ok, total, falhas, clippy_ok}.
"""
import json, os, re, shutil, subprocess, sys

RAIZ = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CARGO = os.path.expanduser("~/.cargo/bin/cargo")


def main(par, worktree):
    wt = os.path.join(RAIZ, worktree)
    pasta = os.path.join(RAIZ, "metricas", "ab", par)
    ok = total = 0
    falhas = []
    for arq in sorted(f for f in os.listdir(pasta) if f.startswith("hidden_") and f.endswith(".rs")):
        src = os.path.join(pasta, arq)
        destino = re.search(r"Copiar para (\S+\.rs)", open(src, encoding="utf-8").read()).group(1)
        dst = os.path.join(wt, destino)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy(src, dst)
        crate_dir = os.path.dirname(os.path.dirname(dst))
        nome = arq[:-3]
        r = subprocess.run([CARGO, "test", "--test", nome, "--", "--test-threads=1"], cwd=crate_dir,
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        saida = r.stdout + r.stderr
        res = re.findall(r"^test (\S+) \.\.\. (ok|FAILED)", saida, re.M)
        if not res:  # não compilou: todos os testes do arquivo contam como falha
            n = len(re.findall(r"#\[test\]", open(src, encoding="utf-8").read()))
            total += n
            falhas.append({"arquivo": arq, "erro_compilacao": saida[-1500:]})
            continue
        total += len(res)
        ok += sum(1 for _, s in res if s == "ok")
        falhas += [t for t, s in res if s == "FAILED"]
    clippy = subprocess.run([CARGO, "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
                            cwd=os.path.join(wt, "sala"), capture_output=True, text=True,
                            encoding="utf-8", errors="replace")
    print(json.dumps({"par": par, "worktree": worktree, "ok": ok, "total": total, "falhas": falhas,
                      "clippy_ok": clippy.returncode == 0}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main(*sys.argv[1:3])
