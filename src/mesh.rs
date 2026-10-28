//! Resultados para sobrepor ao corte: malhas retangulares (`meshtal` do MCNP
//! em colunas, saídas de tally do PHITS com `mesh = xyz`) e tallies de célula
//! do MCNP (`mctal` casado com os cartões F do input).

use crate::geom::{Level, Model};
use glam::DVec3;
use std::collections::HashMap;

pub struct Mesh {
    pub name: String,
    /// Fronteiras dos intervalos em x, y e z.
    pub edges: [Vec<f64>; 3],
    /// Valor e erro relativo de cada elemento; o índice x varia mais rápido.
    pub values: Vec<f64>,
    pub errors: Vec<f64>,
}

fn bin(edges: &[f64], x: f64) -> Option<usize> {
    let (first, last) = (*edges.first()?, *edges.last()?);
    if edges.len() < 2 || x < first || x > last {
        return None;
    }
    Some(edges.partition_point(|&e| e <= x).clamp(1, edges.len() - 1) - 1)
}

impl Mesh {
    fn slot(&self, p: DVec3) -> Option<usize> {
        let [i, j, k] = [bin(&self.edges[0], p.x)?, bin(&self.edges[1], p.y)?, bin(&self.edges[2], p.z)?];
        Some((k * (self.edges[1].len() - 1) + j) * (self.edges[0].len() - 1) + i)
    }

    /// (valor, erro relativo) no ponto, se ele estiver dentro da malha.
    pub fn at(&self, p: DVec3) -> Option<(f64, f64)> {
        let s = self.slot(p)?;
        Some((*self.values.get(s)?, *self.errors.get(s)?))
    }

}

/// Malha em construção durante a leitura.
struct Draft {
    mesh: Mesh,
    /// Posição das colunas x, y, z, resultado, erro e (se houver) energia.
    cols: Option<([usize; 5], Option<usize>)>,
    has_total: bool,
    /// Soma dos quadrados dos erros absolutos, quando é preciso somar os
    /// intervalos de energia.
    var: Vec<f64>,
    unsupported: bool,
}

impl Draft {
    fn finish(mut self) -> Option<Mesh> {
        if self.unsupported || self.mesh.values.is_empty() {
            return None;
        }
        if !self.has_total {
            for (e, (v, var)) in self.mesh.errors.iter_mut().zip(self.mesh.values.iter().zip(&self.var)) {
                *e = if *v != 0.0 { var.sqrt() / v.abs() } else { 0.0 };
            }
        }
        Some(self.mesh)
    }

    fn row(&mut self, line: &str) {
        let Some(([cx, cy, cz, cv, ce], cen)) = self.cols else { return };
        let toks: Vec<&str> = line.split_whitespace().collect();
        let num = |c: usize| toks.get(c).and_then(|t| t.parse::<f64>().ok());
        let (Some(x), Some(y), Some(z), Some(v), Some(e)) = (num(cx), num(cy), num(cz), num(cv), num(ce)) else { return };
        let Some(s) = self.mesh.slot(DVec3::new(x, y, z)) else { return };
        let total = cen.and_then(|c| toks.get(c)).is_some_and(|t| t.eq_ignore_ascii_case("total"));
        if total {
            // A linha "Total" já traz a soma em energia: vale mais que somar.
            if !self.has_total {
                self.has_total = true;
                self.mesh.values.fill(0.0);
            }
            (self.mesh.values[s], self.mesh.errors[s]) = (v, e);
        } else if !self.has_total {
            self.mesh.values[s] += v;
            self.var[s] += (v * e) * (v * e);
        }
    }
}

/// Lê todas as malhas retangulares de um arquivo `meshtal`.
pub fn parse_meshtal(src: &str) -> Result<Vec<Mesh>, String> {
    let numbers = |s: &str| s.split_whitespace().map(|t| t.parse::<f64>()).collect::<Result<Vec<f64>, _>>();
    let mut out = Vec::new();
    let mut cur: Option<Draft> = None;
    // Eixo cujas fronteiras continuam na linha seguinte.
    let mut axis: Option<usize> = None;
    let mut seen = 0;
    for line in src.lines() {
        let t = line.trim();
        let low = t.to_lowercase();
        if let Some(rest) = low.strip_prefix("mesh tally number") {
            seen += 1;
            out.extend(cur.take().and_then(Draft::finish));
            let mesh = Mesh { name: format!("FMESH {}", rest.trim()), edges: Default::default(), values: Vec::new(), errors: Vec::new() };
            cur = Some(Draft { mesh, cols: None, has_total: false, var: Vec::new(), unsupported: false });
            axis = None;
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        if d.cols.is_some() {
            d.row(t);
            continue;
        }
        let direction = ["x direction:", "y direction:", "z direction:"].iter().position(|p| low.starts_with(p));
        if let Some(m) = direction {
            axis = Some(m);
            d.mesh.edges[m] = numbers(&t[12..]).unwrap_or_default();
        } else if low.starts_with("r direction") || low.starts_with("theta direction") {
            d.unsupported = true;
            axis = None;
        } else if low.contains("result") && low.contains("rel") {
            let header = low.replace("rel error", "relerr").replace("rslt * vol", "rsltvol");
            let names: Vec<&str> = header.split_whitespace().collect();
            let col = |n: &str| names.iter().position(|x| *x == n);
            if let (Some(x), Some(y), Some(z), Some(v), Some(e)) = (col("x"), col("y"), col("z"), col("result"), col("relerr")) {
                let n: usize = d.mesh.edges.iter().map(|e| e.len().saturating_sub(1)).product();
                d.mesh.values = vec![0.0; n];
                d.mesh.errors = vec![0.0; n];
                d.var = vec![0.0; n];
                d.cols = Some(([x, y, z, v, e], col("energy")));
            }
            axis = None;
        } else if let (Some(m), Ok(more)) = (axis, numbers(t)) {
            d.mesh.edges[m].extend(more);
        } else {
            axis = None;
        }
    }
    out.extend(cur.and_then(Draft::finish));
    if out.is_empty() {
        return Err(if seen == 0 {
            "nenhuma malha encontrada: o arquivo é um meshtal do MCNP?".into()
        } else {
            "as malhas do arquivo não são retangulares em colunas (malhas cilíndricas e os formatos em matriz, out=ij/ik/jk, ainda não são lidos)".into()
        });
    }
    Ok(out)
}

/// Resultado de um tally de célula do MCNP: um valor por célula ou por
/// elemento de lattice.
pub struct CellTally {
    pub name: String,
    /// Número do tally; 0 no conjunto de vários tallies e -1 na soma entre arquivos.
    number: i32,
    /// Peso do arquivo de origem (`MUf=` no título) e o nome dele.
    weight: Option<f64>,
    source: String,
    /// (célula, célula do lattice que a contém, elemento) -> intervalo.
    by_element: HashMap<(usize, usize, [i32; 3]), usize>,
    /// Demais intervalos: a célula e as que a contêm, de dentro para fora.
    chains: Vec<(Vec<(usize, Option<[i32; 3]>)>, usize)>,
    values: Vec<(f64, f64)>,
    /// Caixa em volta dos elementos de lattice do tally, quando dá para situá-los.
    bounds: Option<(DVec3, DVec3)>,
    tfc: Vec<[f64; 4]>,
}

impl CellTally {
    fn at(&self, levels: &[Level]) -> Option<(f64, f64)> {
        for (li, l) in levels.iter().enumerate().rev() {
            for outer in levels[..li].iter().rev() {
                let Some(idx) = outer.lat_idx else { continue };
                if let Some(&bin) = self.by_element.get(&(l.cell, outer.cell, idx)) {
                    return self.values.get(bin).copied();
                }
            }
        }
        let matches = |chain: &[(usize, Option<[i32; 3]>)]| {
            let mut up = levels.iter().rev();
            chain.iter().all(|&(cell, idx)| up.any(|l| l.cell == cell && (idx.is_none() || idx == l.lat_idx)))
        };
        let bin = self.chains.iter().find(|c| matches(&c.0))?.1;
        self.values.get(bin).copied()
    }
}

/// Um resultado que pode ser pintado sobre a geometria.
pub enum Field {
    Mesh(Mesh),
    Cells(CellTally),
}

impl Field {
    pub fn name(&self) -> &str {
        match self {
            Field::Mesh(m) => &m.name,
            Field::Cells(c) => &c.name,
        }
    }

    /// (valor, erro relativo) no ponto `p`, já localizado em `levels`.
    pub fn at(&self, p: DVec3, levels: &[Level]) -> Option<(f64, f64)> {
        match self {
            Field::Mesh(m) => m.at(p),
            Field::Cells(c) => c.at(levels),
        }
    }

    /// Histórico do tally ao longo da rodada, quando o arquivo traz:
    /// (nps, média, erro relativo, figura de mérito).
    pub fn history(&self) -> &[[f64; 4]] {
        match self {
            Field::Mesh(_) => &[],
            Field::Cells(c) => &c.tfc,
        }
    }

    /// (valor, erro relativo) quando o resultado é um número só, como a dose
    /// num órgão.
    pub fn single(&self) -> Option<(f64, f64)> {
        match self {
            Field::Cells(c) if c.values.len() == 1 => Some(c.values[0]),
            _ => None,
        }
    }

    /// É a soma entre arquivos feita por [`weighted_sum`].
    pub fn is_sum(&self) -> bool {
        matches!(self, Field::Cells(c) if c.number == -1)
    }

    /// Arquivo de onde veio um tally de célula.
    pub fn source(&self) -> &str {
        match self {
            Field::Cells(c) => &c.source,
            Field::Mesh(_) => "",
        }
    }

    /// A célula de um tally de célula sem lattice, para ir até ela.
    pub fn cell(&self) -> Option<usize> {
        match self {
            Field::Cells(c) => Some(c.chains.first()?.0.first()?.0),
            Field::Mesh(_) => None,
        }
    }

    /// Menor e maior valor positivo, para a escala de cores.
    pub fn positive_range(&self) -> Option<(f64, f64)> {
        let values: Box<dyn Iterator<Item = f64> + '_> = match self {
            Field::Mesh(m) => Box::new(m.values.iter().copied()),
            Field::Cells(c) => Box::new(c.values.iter().map(|v| v.0)),
        };
        let (lo, hi) = values.filter(|v| *v > 0.0 && v.is_finite()).fold((f64::INFINITY, 0.0f64), |(a, b), v| (a.min(v), b.max(v)));
        (hi > 0.0).then_some((lo, hi))
    }

    /// Cantos da malha, ou da caixa em volta dos detectores de um tally em lattice.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        let m = match self {
            Field::Mesh(m) => m,
            Field::Cells(c) => return c.bounds,
        };
        let end = |last: bool| -> Option<DVec3> {
            let mut p = DVec3::ZERO;
            for a in 0..3 {
                p[a] = *(if last { m.edges[a].last() } else { m.edges[a].first() })?;
            }
            Some(p)
        };
        Some((end(false)?, end(true)?))
    }
}

/// Um tally lido do mctal: os valores na ordem do arquivo.
struct MctalTally {
    number: i32,
    comment: String,
    /// Quantos intervalos em F, e quantos valores há para cada um (produto
    /// dos demais intervalos: energia, tempo, segmento...).
    nf: usize,
    block: usize,
    /// Tem segmentos (cartão FS): os valores não correspondem a células inteiras.
    segmented: bool,
    vals: Vec<(f64, f64)>,
    /// Histórico do tally: (nps, média, erro relativo, figura de mérito).
    tfc: Vec<[f64; 4]>,
}

fn parse_mctal(src: &str) -> Vec<MctalTally> {
    let mut out: Vec<MctalTally> = Vec::new();
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        let mut toks = line.split_whitespace();
        if toks.next() != Some("tally") {
            continue;
        }
        let Some(number) = toks.next().and_then(|t| t.parse().ok()) else { continue };
        let mut t = MctalTally { number, comment: String::new(), nf: 0, block: 1, segmented: false, vals: Vec::new(), tfc: Vec::new() };
        // Lista de partículas, depois as linhas do comentário FC (recuadas).
        lines.next();
        while let Some(l) = lines.peek().filter(|l| l.starts_with("     ")) {
            t.comment = format!("{} {}", t.comment, l.trim()).trim().to_string();
            lines.next();
        }
        for l in lines.by_ref() {
            let mut toks = l.split_whitespace();
            let key = toks.next().unwrap_or("");
            let count = toks.next().and_then(|n| n.parse::<usize>().ok());
            match (key, count) {
                ("f", Some(n)) => t.nf = n,
                ("s" | "st" | "sc", Some(n)) if n > 1 => t.segmented = true,
                ("d" | "u" | "ut" | "uc" | "s" | "st" | "sc" | "m" | "mt" | "mc" | "c" | "ct" | "cc" | "e" | "et" | "ec" | "t" | "tt" | "tc", Some(n)) => {
                    t.block *= n.max(1)
                }
                ("vals", _) => break,
                _ => {}
            }
        }
        let mut numbers = Vec::new();
        while let Some(l) = lines.peek().filter(|l| !l.starts_with("tfc") && !l.starts_with("tally")) {
            numbers.extend(l.split_whitespace().filter_map(|x| x.parse::<f64>().ok()));
            lines.next();
        }
        t.vals = numbers.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        if lines.peek().is_some_and(|l| l.starts_with("tfc")) {
            lines.next();
            while let Some(l) = lines.peek().filter(|l| !l.starts_with("tally")) {
                let row: Vec<f64> = l.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if let [nps, mean, err, fom] = row[..] {
                    t.tfc.push([nps, mean, err, fom]);
                }
                lines.next();
            }
        }
        out.push(t);
    }
    out
}

/// Casa os tallies do mctal com os cartões F do input aberto.
fn cell_tallies(src: &str, file_name: &str, model: &Model) -> Result<Vec<Field>, String> {
    let weight = title_weight(src);
    let source = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name).to_string();
    let tallies = parse_mctal(src);
    if tallies.is_empty() {
        return Err("o mctal não tem nenhum tally".into());
    }
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    // Tallies de uma célula só (dose por órgão, por exemplo) juntos por tipo
    // (o último algarismo do número), para pintar todos de uma vez.
    let mut together: std::collections::BTreeMap<i32, CellTally> = std::collections::BTreeMap::new();
    for t in &tallies {
        let def = model.tallies.iter().find(|d| d.number == t.number);
        let Some(bins) = def.and_then(|d| d.bins.as_ref()).filter(|b| b.len() == t.nf && !t.segmented && t.vals.len() >= t.nf * t.block) else {
            skipped.push(t.number.to_string());
            continue;
        };
        let mut tally = CellTally {
            name: format!("F{} {}", t.number, t.comment).trim().to_string(),
            number: t.number,
            weight,
            source: source.clone(),
            by_element: HashMap::new(),
            chains: Vec::new(),
            // Com vários valores por intervalo (energia, tempo), o último é o total quando há um.
            values: (0..t.nf).map(|b| t.vals[(b + 1) * t.block - 1]).collect(),
            bounds: None,
            tfc: t.tfc.clone(),
        };
        for (b, bin) in bins.iter().enumerate() {
            let resolved: Option<Vec<(usize, Option<[i32; 3]>)>> =
                bin.iter().map(|&(id, idx)| Some((*model.cell_index.get(&id)?, idx))).collect();
            match resolved.as_deref() {
                Some([(cell, None), (lat, Some(idx))]) => {
                    tally.by_element.insert((*cell, *lat, *idx), b);
                }
                Some(chain) if !chain.is_empty() => tally.chains.push((chain.to_vec(), b)),
                _ => {}
            }
        }
        let centers: Vec<DVec3> = tally.by_element.keys().filter_map(|&(_, lat, idx)| model.element_center(lat, idx)).collect();
        if let Some(&first) = centers.first() {
            let (lo, hi) = centers.iter().fold((first, first), |(lo, hi), &c| (lo.min(c), hi.max(c)));
            // Folga para pegar o elemento inteiro, e não só o centro dos extremos.
            let pad = DVec3::splat(0.02 * (hi - lo).max_element() + 0.25);
            tally.bounds = Some((lo - pad, hi + pad));
        }
        if let ([(chain, _)], [value]) = (&tally.chains[..], &tally.values[..]) {
            let all = together.entry(t.number % 10).or_insert_with(|| CellTally {
                name: String::new(),
                number: 0,
                weight: None,
                source: source.clone(),
                by_element: HashMap::new(),
                chains: Vec::new(),
                values: Vec::new(),
                bounds: None,
                tfc: Vec::new(),
            });
            all.chains.push((chain.clone(), all.values.len()));
            all.values.push(*value);
        }
        out.push(Field::Cells(tally));
    }
    for (kind, mut all) in together.into_iter().rev().filter(|(_, all)| all.values.len() > 1) {
        all.name = format!("Todos os F{} de uma célula ({} tallies)", kind, all.values.len());
        out.insert(0, Field::Cells(all));
    }
    if out.is_empty() {
        return Err(format!(
            "nenhum tally do mctal ({}) casa com os cartões F do input aberto: abra antes o input que gerou este mctal",
            skipped.join(", ")
        ));
    }
    Ok(out)
}

/// Saída de tally do PHITS com `mesh = xyz` e um eixo de saída (x, y ou z).
fn parse_phits_tally(src: &str, name: &str) -> Result<Mesh, String> {
    let value = |key: &str| {
        src.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim().to_string())
        })
    };
    if value("mesh").as_deref() != Some("xyz") {
        return Err("só saídas com mesh = xyz são lidas".into());
    }
    let mut edges: [Vec<f64>; 3] = Default::default();
    for (a, letter) in ["x", "y", "z"].iter().enumerate() {
        let n: usize = value(&format!("n{}", letter)).and_then(|v| v.parse().ok()).ok_or(format!("falta n{}", letter))?;
        let marker = format!("data = ( {}(i)", letter);
        let after = src.lines().skip_while(|l| !l.contains(&marker)).skip(1);
        for l in after {
            let row: Option<Vec<f64>> = l.trim_start_matches('#').split_whitespace().map(|t| t.parse().ok()).collect();
            match row {
                Some(row) if !row.is_empty() && edges[a].len() <= n => edges[a].extend(row),
                _ => break,
            }
        }
        if edges[a].len() != n + 1 {
            return Err(format!("fronteiras da malha em {} incompletas", letter));
        }
    }
    let axis = value("axis").and_then(|v| ["x", "y", "z"].iter().position(|l| *l == v)).ok_or("só saídas com axis = x, y ou z são lidas")?;
    let cells = edges.iter().map(|e| e.len() - 1).product();
    let mut mesh = Mesh { name: name.to_string(), edges, values: vec![0.0; cells], errors: vec![0.0; cells] };
    // Cada página traz os índices dos outros dois eixos e as linhas ao longo do eixo de saída.
    let mut fixed: Option<[usize; 3]> = None;
    for l in src.lines() {
        if l.starts_with("#   no. =") {
            let toks: Vec<&str> = l.split(|c: char| c.is_whitespace() || c == '=').filter(|t| !t.is_empty()).collect();
            let index = |key: &str| toks.iter().position(|t| *t == key).and_then(|i| toks.get(i + 1)?.parse::<usize>().ok());
            // Só o primeiro intervalo de energia.
            fixed = (index("ie").unwrap_or(1) == 1)
                .then(|| [index("ix").unwrap_or(1) - 1, index("iy").unwrap_or(1) - 1, index("iz").unwrap_or(1) - 1]);
            continue;
        }
        let Some(mut at) = fixed else { continue };
        let row: Vec<f64> = l.split_whitespace().map_while(|t| t.parse().ok()).collect();
        if l.starts_with('#') || row.len() < 4 {
            continue;
        }
        let Some(i) = bin(&mesh.edges[axis], (row[0] + row[1]) / 2.0) else { continue };
        at[axis] = i;
        let n = [0, 1, 2].map(|a| mesh.edges[a].len() - 1);
        if at[0] < n[0] && at[1] < n[1] && at[2] < n[2] {
            let slot = (at[2] * n[1] + at[1]) * n[0] + at[0];
            (mesh.values[slot], mesh.errors[slot]) = (row[2], row[3]);
        }
    }
    Ok(mesh)
}

/// Peso do arquivo na soma entre pontos de controle: `MUf=` no título do mctal.
fn title_weight(src: &str) -> Option<f64> {
    let title = src.lines().nth(1)?.to_lowercase();
    let rest = &title[title.find("muf=")? + 4..];
    rest.split_whitespace().next()?.parse().ok()
}

/// Um tally somado entre os arquivos carregados.
pub struct SumRow {
    pub name: String,
    pub cell: Option<usize>,
    pub value: f64,
    pub error: f64,
    /// Em quantos arquivos o tally apareceu.
    pub files: usize,
}

/// Soma dos tallies de um valor só entre vários mctal (os pontos de controle
/// de um arco), cada arquivo pesado pelo `MUf` do seu título.
pub struct Sum {
    /// (arquivo, peso)
    pub files: Vec<(String, f64)>,
    /// Arquivos sem `MUf=` no título; havendo algum, a soma não é feita.
    pub missing: Vec<String>,
    pub rows: Vec<SumRow>,
    /// Todos os tallies somados, para pintar sobre a geometria.
    pub field: Option<Field>,
}

/// Total = Σ MUf·valor; as incertezas (independentes entre arquivos) somam em
/// quadratura. `None` quando há tallies de menos de dois arquivos.
pub fn weighted_sum(fields: &[std::sync::Arc<Field>]) -> Option<Sum> {
    let singles: Vec<&CellTally> = fields
        .iter()
        .filter_map(|f| match &**f {
            Field::Cells(c) if c.number > 0 && c.values.len() == 1 => Some(c),
            _ => None,
        })
        .collect();
    let mut sources: Vec<(&str, Option<f64>)> = Vec::new();
    for c in &singles {
        if !sources.iter().any(|s| s.0 == c.source) {
            sources.push((&c.source, c.weight));
        }
    }
    if sources.len() < 2 {
        return None;
    }
    let missing: Vec<String> = sources.iter().filter(|s| s.1.is_none()).map(|s| s.0.to_string()).collect();
    let files: Vec<(String, f64)> = sources.iter().filter_map(|s| Some((s.0.to_string(), s.1?))).collect();
    if !missing.is_empty() {
        return Some(Sum { files, missing, rows: Vec::new(), field: None });
    }
    struct Acc<'a> {
        tally: &'a CellTally,
        total: f64,
        variance: f64,
        seen: Vec<&'a str>,
    }
    let mut groups: std::collections::BTreeMap<i32, Acc> = std::collections::BTreeMap::new();
    for c in &singles {
        let acc = groups.entry(c.number).or_insert(Acc { tally: c, total: 0.0, variance: 0.0, seen: Vec::new() });
        // O mesmo arquivo carregado duas vezes não conta em dobro.
        if acc.seen.contains(&c.source.as_str()) {
            continue;
        }
        acc.seen.push(&c.source);
        let (value, error) = c.values[0];
        let part = c.weight.unwrap_or(0.0) * value;
        acc.total += part;
        acc.variance += (part * error).powi(2);
    }
    let mut all = CellTally {
        name: format!("Soma ponderada por MUf ({} arquivos, {} tallies)", files.len(), groups.len()),
        number: -1,
        weight: None,
        source: String::new(),
        by_element: HashMap::new(),
        chains: Vec::new(),
        values: Vec::new(),
        bounds: None,
        tfc: Vec::new(),
    };
    let mut rows = Vec::new();
    for acc in groups.values() {
        let error = if acc.total > 0.0 { acc.variance.sqrt() / acc.total } else { 0.0 };
        if let Some((chain, _)) = acc.tally.chains.first() {
            all.chains.push((chain.clone(), all.values.len()));
            all.values.push((acc.total, error));
        }
        let cell = acc.tally.chains.first().and_then(|c| c.0.first()).map(|c| c.0);
        rows.push(SumRow { name: acc.tally.name.clone(), cell, value: acc.total, error, files: acc.seen.len() });
    }
    Some(Sum { files, missing, rows, field: Some(Field::Cells(all)) })
}

/// Lê um arquivo de resultados, reconhecendo o formato pelo conteúdo. `model`
/// é o input aberto, necessário para situar os tallies de célula do mctal.
pub fn load_results(src: &str, file_name: &str, model: &Model) -> Result<Vec<Field>, String> {
    if src.contains("Mesh Tally Number") {
        Ok(parse_meshtal(src)?.into_iter().map(Field::Mesh).collect())
    } else if src.lines().take(40).any(|l| l.trim_start().starts_with("[ T-") || l.trim_start().starts_with("[T-")) {
        Ok(vec![Field::Mesh(parse_phits_tally(src, file_name)?)])
    } else if src.lines().take(5).any(|l| l.starts_with("ntal")) {
        cell_tallies(src, file_name, model)
    } else {
        Err("formato não reconhecido: esperado meshtal ou mctal do MCNP, ou saída de tally do PHITS (mesh = xyz)".into())
    }
}
