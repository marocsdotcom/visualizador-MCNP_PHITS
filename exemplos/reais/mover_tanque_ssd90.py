"""Poe o tanque do deck dos PERFIS a SSD 90, como a medida e o deck do PDD.

Existe porque a regra 1 do CLAUDE.md proibe editar deck na mao.

O problema (2026-09-29): as larguras a 50 % do TPS/medida sao 10 x (90+d)/100
(9,15 cm a 1,4 cm; 12,12 a 29,7) -- SSD 90. O MCNP_Lat_10x10 do mestrado tem a
agua em z = 100 (SSD 100). O deck do PDD ja' esta' a SSD 90, e o jeito que ele
usa e' o que se copia aqui: so' o zmin da superficie 701 (100 -> 90), com o
valor antigo guardado no comentario, exatamente como no MCNP_pdd_10x10.

Muda:
  - superficie 701 (rpp do tanque): zmin 100 -> 90 (zmax 130 fica: 40 cm de agua)
  - tr21..tr25 (as 5 camadas de detectores): z - 10  (101,4 -> 91,4; ...)
Nao muda mais nada (as pz 300-449 so' servem ao f4 de segmentacao, que o
tradutor nao usa). No fim reconstroi o original e compara BYTE A BYTE.

    python mover_tanque_ssd90.py LONGA/MCNP_Lat_10x10_distal LONGA/MCNP_Lat_10x10_distal_ssd90
"""
import os
import re
import sys

DZ = -10.0
RE_701 = re.compile(rb"^(701\s+rpp\s+-25\s+25\s+-25\s+25\s+)(100)(\s+130)(.*)$")
RE_TR = re.compile(rb"^(tr2[1-5]\s+0\s+0\s+)([0-9.]+)(.*)$", re.I)


def mover(origem, destino):
    bruto = open(origem, "rb").read()
    fim = b"\r\n" if b"\r\n" in bruto else b"\n"
    linhas = bruto.split(fim)
    novas, mudou = list(linhas), []
    for i, l in enumerate(linhas):
        m = RE_701.match(l)
        if m:
            novas[i] = (m.group(1) + b"90" + m.group(3) + m.group(4).rstrip()
                        + b" -25 25 -25 25 100 130")
            mudou.append((i, l, novas[i]))
            continue
        m = RE_TR.match(l)
        if m:
            z = float(m.group(2))
            novo = ("%g" % (z + DZ)).encode()
            novas[i] = m.group(1) + novo + m.group(3)
            mudou.append((i, l, novas[i]))
    n701 = sum(1 for _, a, _ in mudou if a.startswith(b"701"))
    ntr = len(mudou) - n701
    if n701 != 1 or ntr != 5:
        raise SystemExit("ERRO: esperava 1 superficie 701 e 5 tr21-25; achei %d e %d"
                         % (n701, ntr))
    saida = fim.join(novas)
    os.makedirs(os.path.dirname(destino) or ".", exist_ok=True)
    open(destino, "wb").write(saida)

    volta = saida.split(fim)
    for i, a, _ in mudou:
        volta[i] = a
    if fim.join(volta) != bruto:
        raise SystemExit("ERRO: o novo nao reconstroi o original; nao use")

    print("%s -> %s" % (origem, destino))
    for i, a, b in mudou:
        print("   L%-4d %s" % (i + 1, a.decode("latin-1").strip()))
        print("      -> %s" % b.decode("latin-1").strip())
    print("  reconstrucao byte a byte do original: OK")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    mover(sys.argv[1], sys.argv[2])
