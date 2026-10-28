"""Junta num deck PHITS so' as tallies de dois decks com a MESMA fisica.

Existe porque a regra 1 do CLAUDE.md proibe editar deck na mao.

Depois do tanque dos perfis ir para SSD 90 (ORIGEM.md §7.5), o deck do PDD e
o dos perfis ficaram com parametros, fonte, materiais, superficies e celulas
iguais: as duas rodadas fariam o MESMO transporte, e so' pontuariam coisas
diferentes. Uma rodada com as tallies dos dois economiza um transporte inteiro.

Regra: copia o deck BASE inteiro e insere, antes do [T-Gshow], as secoes de
tally do deck EXTRA ([T-Track], [T-Deposit], [T-Cross]). Recusa se qualquer
secao fisica ([Parameters], [Source], [Material], [Surface], [Cell]) diferir,
ignorando comentarios ($) e espacos. [Transform] pode diferir so' em
transformacoes que nenhuma superficie usa (tr das camadas de detector do
MCNP) -- as da BASE ficam. O [Multiplier] tem de ser o mesmo nos dois.

    python juntar_tallies.py BASE.inp EXTRA.inp SAIDA.inp
"""
import re
import sys

FISICA = ("parameters", "source", "material", "surface", "cell", "multiplier")
TALLY = ("t-track", "t-deposit", "t-cross")


def secoes(texto):
    """Lista de (nome, linhas) na ordem do arquivo."""
    out, nome, bloco = [], "_cabecalho", []
    for l in texto.splitlines():
        m = re.match(r"^\s*\[\s*([^\]]+?)\s*\]", l)
        if m:
            out.append((nome, bloco))
            nome, bloco = m.group(1).lower(), []
        bloco.append(l)
    out.append((nome, bloco))
    return out


def normaliza(linhas):
    r = []
    for l in linhas:
        l = l.split("$")[0].rstrip()
        if l.strip():
            r.append(" ".join(l.split()))
    return r


def trs_usadas(surface):
    usadas = set()
    for l in normaliza(surface[1:]):
        t = l.split()
        if len(t) > 2 and t[1].lstrip("-").isdigit() and not t[1].startswith("-"):
            try:
                float(t[2])
            except ValueError:
                usadas.add(t[1])          # "901 21 rpp ..." -> tr21
    return usadas


def juntar(base, extra, saida):
    A = secoes(open(base, encoding="utf-8").read())
    B = secoes(open(extra, encoding="utf-8").read())
    da, db = dict(A), dict(B)
    for s in FISICA:
        if normaliza(da.get(s, [])) != normaliza(db.get(s, [])):
            raise SystemExit("ERRO: [%s] difere entre os decks -- fisica diferente, "
                             "nao da' para juntar" % s)
    usadas = trs_usadas(da["surface"])
    def trs(sec):
        return {l.split()[0].lower(): l for l in normaliza(sec[1:])}
    ta, tb = trs(da.get("transform", [])), trs(db.get("transform", []))
    for k in set(ta) | set(tb):
        if ta.get(k) != tb.get(k) and k.lstrip("tr*") in usadas:
            raise SystemExit("ERRO: %s difere e e' usada por superficie" % k)
    extras = [(n, l) for n, l in B if n in TALLY]
    novo = []
    for n, l in A:
        if n == "t-gshow":
            for _, le in extras:
                novo.extend(["$ >>> tally trazida de %s por juntar_tallies.py" % extra] + le)
        novo.extend(l)
    open(saida, "w", encoding="utf-8", newline="\n").write("\n".join(novo) + "\n")
    arqs = [re.sub(r"^\s*file\s*=\s*", "", x).split()[0] for _, l in A + extras
            for x in l if re.match(r"^\s*file\s*=", x) and "(" not in x]
    dup = sorted(set(a for a in arqs if arqs.count(a) > 1))
    if dup:
        raise SystemExit("ERRO: arquivos de saida repetidos: %s" % dup)
    print("%s + tallies de %s -> %s" % (base, extra, saida))
    print("  fisica identica: %s" % ", ".join("[%s]" % s for s in FISICA))
    print("  tallies trazidas: %d  (%s)" % (len(extras), ", ".join(n for n, _ in extras)))
    print("  arquivos de saida: %s" % ", ".join(arqs))


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    juntar(*sys.argv[1:])
