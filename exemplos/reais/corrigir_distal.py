"""Poe a camada DISTAL do MLC dos decks do mestrado na posicao de um 10x10.

Existe porque a regra 1 do CLAUDE.md proibe editar deck na mao.

O problema (aula 13 §13.6b): nos decks validados do mestrado as laminas
abertas tem *TRCL x = +-2,55 nas DUAS camadas. A distal fica 7,5 cm mais longe
da fonte (tr8), entao o mesmo 2,55 projeta um campo menor: 8,97 cm a 1,4 cm de
profundidade, e o MCNP mediu 9,08. A distal e' que define o campo.

Muda UMA coisa: o x do *TRCL das celulas-lamina da camada distal (401-474)
que estao ABERTAS (|x| = 2,55). O valor novo vem de
gerar_deck_mcnp.trcl_x_da_lamina(5.0, banco, "dist") -- o mesmo numero que o
gerador escreve (+-2,8925 -> ponta em +-5,00 cm no isocentro). A proximal,
o tr8 (ja' em Y nestes decks) e todo o resto ficam intactos.
No fim, reconstroi o original a partir do corrigido e compara BYTE A BYTE.

    python corrigir_distal.py MCNP_Lat_10x10 MCNP_Lat_10x10_distal
"""
import os
import re
import sys

AQUI = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(AQUI, "..", "..", "01_MODELO_MCNP"))
from gerar_deck_mcnp import trcl_x_da_lamina   # noqa: E402

DISTAL = range(401, 475)       # celulas-lamina da camada distal (ORIGEM.md §2)
X_ABERTA = 2.55                # valor do mestrado nas laminas abertas
MEIO_CAMPO = 5.0               # cm no isocentro (10x10)

RE_CEL = re.compile(rb"^ {0,4}(\d+)\s")
RE_TRCL = re.compile(rb"(\*TRCL=\(\s*)(-?[0-9.]+)")


def corrigir(origem, destino):
    bruto = open(origem, "rb").read()
    fim = b"\r\n" if b"\r\n" in bruto else b"\n"
    linhas = bruto.split(fim)
    x_novo = {+1: trcl_x_da_lamina(MEIO_CAMPO, "A", "dist")[0],
              -1: trcl_x_da_lamina(-MEIO_CAMPO, "B", "dist")[0]}

    novas, mudou, cel = list(linhas), [], None
    for i, l in enumerate(linhas):
        if not l.strip():                     # fim do bloco de celulas
            if cel is not None:
                break
            continue
        if l.lstrip()[:1] in (b"c", b"C") and l[:5].strip().lower() in (b"c", b""):
            continue                          # comentario
        m = RE_CEL.match(l)
        if m:
            cel = int(m.group(1))
        if cel not in DISTAL:
            continue
        t = RE_TRCL.search(l)
        if not t:
            continue
        x = float(t.group(2))
        if abs(abs(x) - X_ABERTA) > 1e-9:
            continue                          # lamina fechada (x = 0): fica
        s = 1 if x > 0 else -1
        novo = ("%.4f" % x_novo[s]).encode()
        novas[i] = l[:t.start(2)] + novo + l[t.end(2):]
        mudou.append((i, cel, x, x_novo[s]))

    saida = fim.join(novas)
    os.makedirs(os.path.dirname(destino) or ".", exist_ok=True)
    open(destino, "wb").write(saida)

    # --- verificacao: desfaz as mudancas e exige o original de volta ---
    volta = saida.split(fim)
    for i, _, _, _ in mudou:
        volta[i] = linhas[i]
    if fim.join(volta) != bruto:
        raise SystemExit("ERRO: o corrigido nao reconstroi o original; nao use")

    print("%s -> %s" % (origem, destino))
    print("  x novo da distal: A %+.4f  B %+.4f  (ponta em +-%.2f cm no isocentro)"
          % (x_novo[1], x_novo[-1], MEIO_CAMPO))
    print("  linhas alteradas: %d" % len(mudou))
    for i, c, a, b in mudou:
        print("   L%-4d celula %d: %+.4f -> %+.4f" % (i + 1, c, a, b))
    print("  reconstrucao byte a byte do original: OK")
    return mudou


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    corrigir(sys.argv[1], sys.argv[2])
