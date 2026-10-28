"""Versao curta de um deck MCNP ja validado, so para colher um mctal REAL.

Existe porque a regra 1 do CLAUDE.md proibe editar .inp na mao: mesmo uma
mudanca de uma linha passa por script, para ficar auditavel e reproduzivel.

Muda UMA coisa: o cartao nps. O comentario com o valor original vai inline
($ do MCNP), entao o diff contra o deck validado e' de exatamente 1 linha.
No fim, reconstroi o original a partir do curto e compara BYTE A BYTE.

    python gerar_deck_curto.py MCNP_pdd_10x10 1e5 TESTES/TESTE_MCTAL_PDD_1e5.inp
"""
import os
import re
import sys

RE_NPS = re.compile(rb"^[ \t]*nps\b", re.IGNORECASE)
RE_TALLY = re.compile(rb"^[ \t]*([*+]?)[Ff](\d+)\s*:?", re.IGNORECASE)


def gerar(origem, nps, destino):
    bruto = open(origem, "rb").read()
    fim = b"\r\n" if b"\r\n" in bruto else b"\n"
    linhas = bruto.split(fim)

    achados = [i for i, l in enumerate(linhas) if RE_NPS.match(l)]
    if len(achados) != 1:
        raise SystemExit("esperava 1 cartao nps em %s, achei %d" % (origem, len(achados)))
    i = achados[0]
    antiga = linhas[i]
    nova = (b"nps " + nps.encode() + b"  $ curto para colher mctal; "
            b"original era '" + antiga.strip() + b"' em " + os.path.basename(origem).encode())
    linhas[i] = nova

    os.makedirs(os.path.dirname(destino) or ".", exist_ok=True)
    saida = fim.join(linhas)
    open(destino, "wb").write(saida)

    # --- verificacao: desfaz a unica mudanca e exige o original de volta ---
    volta = saida.split(fim)
    volta[i] = antiga
    if fim.join(volta) != bruto:
        raise SystemExit("ERRO: o curto nao reconstroi o original; nao use este deck")

    dif = [(n, a, b) for n, (a, b) in enumerate(zip(bruto.split(fim), saida.split(fim)), 1) if a != b]
    print("%s -> %s" % (origem, destino))
    print("  bytes %d -> %d   linhas %d -> %d" %
          (len(bruto), len(saida), len(bruto.split(fim)), len(saida.split(fim))))
    print("  linhas diferentes: %d" % len(dif))
    for n, a, b in dif:
        print("   - L%d  ANTES: %s" % (n, a.decode("latin-1")))
        print("     L%d  DEPOIS: %s" % (n, b.decode("latin-1")))
    print("  reconstrucao byte a byte do original: OK")
    tal = [l.decode("latin-1").strip() for l in linhas if RE_TALLY.match(l)]
    print("  tallies que o mctal vai trazer (%d):" % len(tal))
    for t in tal:
        print("     %s" % t)
    return destino


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    gerar(sys.argv[1], sys.argv[2], sys.argv[3])
