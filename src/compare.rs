//! Comparações: duas curvas de resultado ao longo da régua (razão e índice
//! gama) e dois inputs (o que mudou de um para o outro).

use crate::geom::Model;
use std::collections::BTreeMap;

/// Um ponto comum às duas curvas: (distância, A, erro relativo de A, B, erro
/// relativo de B).
pub type Pair = (f64, f64, f64, f64, f64);

/// Razão B ÷ (k·A) ponto a ponto.
pub struct Ratio {
    /// Fator aplicado a A (1 quando não se pede o ajuste de escala).
    pub k: f64,
    /// (distância, razão, incerteza da razão)
    pub points: Vec<(f64, f64, f64)>,
    pub mean_dev: f64,
    /// (distância, desvio) do ponto que mais se afasta de 1.
    pub worst: (f64, f64),
    /// Quantos pontos ficam a até 2σ de 1, entre os que têm incerteza.
    pub within: usize,
    pub with_sigma: usize,
    pub chi2: f64,
}

/// Com `fit`, k é o fator que melhor leva A em B por mínimos quadrados
/// ponderados pela incerteza de B: serve para comparar a forma de curvas em
/// unidades diferentes.
pub fn ratio(pairs: &[Pair], fit: bool) -> Option<Ratio> {
    if pairs.len() < 2 {
        return None;
    }
    let k = if fit {
        let sums = pairs.iter().fold((0.0, 0.0), |(ab, aa), p| {
            let w = 1.0 / (p.3 * p.4.max(1e-3)).powi(2);
            (ab + w * p.1 * p.3, aa + w * p.1 * p.1)
        });
        sums.0 / sums.1
    } else {
        1.0
    };
    let points: Vec<(f64, f64, f64)> = pairs.iter().map(|p| (p.0, p.3 / (k * p.1), p.3 / (k * p.1) * (p.2 * p.2 + p.4 * p.4).sqrt())).collect();
    let mean_dev = points.iter().map(|r| (r.1 - 1.0).abs()).sum::<f64>() / points.len() as f64;
    let worst = points.iter().map(|r| (r.0, (r.1 - 1.0).abs())).max_by(|x, y| x.1.total_cmp(&y.1))?;
    let with_sigma: Vec<&(f64, f64, f64)> = points.iter().filter(|r| r.2 > 0.0).collect();
    let within = with_sigma.iter().filter(|r| (r.1 - 1.0).abs() <= 2.0 * r.2).count();
    let chi2 = with_sigma.iter().map(|r| ((r.1 - 1.0) / r.2).powi(2)).sum::<f64>() / with_sigma.len().max(1) as f64;
    Some(Ratio { k, mean_dev, worst, within, with_sigma: with_sigma.len(), chi2, points })
}

/// Índice gama em uma dimensão.
pub struct Gamma {
    /// (distância, gama) dos pontos avaliados.
    pub points: Vec<(f64, f64)>,
    pub pass: usize,
    pub max: f64,
}

/// Gama global de B contra a referência k·A: para cada ponto de B, a menor
/// distância combinada (em dose, como fração `dose_tol` do máximo da
/// referência, e em posição, em unidades de `dta`) até a curva de referência,
/// interpolada linearmente. Passa quem tem gama ≤ 1. Pontos em que a
/// referência está abaixo de `cutoff` × máximo ficam de fora.
pub fn gamma(pairs: &[Pair], k: f64, dose_tol: f64, dta: f64, cutoff: f64) -> Option<Gamma> {
    const SUB: usize = 20;
    let reference: Vec<(f64, f64)> = pairs.iter().map(|p| (p.0, k * p.1)).collect();
    let top = reference.iter().map(|r| r.1).fold(0.0, f64::max);
    if reference.len() < 2 || top <= 0.0 || dose_tol <= 0.0 || dta <= 0.0 {
        return None;
    }
    let dose_unit = dose_tol * top;
    let mut points = Vec::new();
    for (i, p) in pairs.iter().enumerate() {
        if reference[i].1 < cutoff * top {
            continue;
        }
        let mut best = ((p.3 - reference[i].1) / dose_unit).abs();
        for seg in reference.windows(2) {
            // Segmento inteiro mais longe que o melhor já achado: não melhora.
            let nearest = if p.0 < seg[0].0 { seg[0].0 - p.0 } else if p.0 > seg[1].0 { p.0 - seg[1].0 } else { 0.0 };
            if nearest / dta >= best {
                continue;
            }
            for s in 0..=SUB {
                let f = s as f64 / SUB as f64;
                let (d, v) = (seg[0].0 + (seg[1].0 - seg[0].0) * f, seg[0].1 + (seg[1].1 - seg[0].1) * f);
                best = best.min((((p.0 - d) / dta).powi(2) + ((p.3 - v) / dose_unit).powi(2)).sqrt());
            }
        }
        points.push((p.0, best));
    }
    if points.is_empty() {
        return None;
    }
    let pass = points.iter().filter(|g| g.1 <= 1.0).count();
    let max = points.iter().map(|g| g.1).fold(0.0, f64::max);
    Some(Gamma { points, pass, max })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Added,
    Removed,
    Changed,
}

/// Uma diferença entre dois inputs.
#[derive(Clone, Debug)]
pub struct Change {
    pub kind: Kind,
    /// O que mudou: "célula 12", "superfície 3", "m5", "nps"…
    pub what: String,
    pub before: String,
    pub after: String,
    /// Célula do input novo onde dá para ver a mudança.
    pub cell: Option<i32>,
}

#[derive(Default)]
pub struct Diff {
    pub cells: Vec<Change>,
    pub surfaces: Vec<Change>,
    pub materials: Vec<Change>,
    pub data: Vec<Change>,
}

impl Diff {
    pub fn len(&self) -> usize {
        self.cells.len() + self.surfaces.len() + self.materials.len() + self.data.len()
    }
}

fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compara dois dicionários de cartões; `same` decide se um cartão presente
/// nos dois mudou.
fn changes<K: Ord + Copy, V>(old: &BTreeMap<K, V>, new: &BTreeMap<K, V>, name: impl Fn(K) -> String, text: impl Fn(&V) -> String, same: impl Fn(&V, &V) -> bool) -> Vec<Change> {
    let mut out = Vec::new();
    for (k, a) in old {
        match new.get(k) {
            None => out.push(Change { kind: Kind::Removed, what: name(*k), before: text(a), after: String::new(), cell: None }),
            Some(b) if !same(a, b) => out.push(Change { kind: Kind::Changed, what: name(*k), before: text(a), after: text(b), cell: None }),
            _ => {}
        }
    }
    for (k, b) in new {
        if !old.contains_key(k) {
            out.push(Change { kind: Kind::Added, what: name(*k), before: String::new(), after: text(b), cell: None });
        }
    }
    out
}

/// Cartões de dados com o número de ordem entre os de mesmo nome (no PHITS
/// várias seções de tally repetem os mesmos parâmetros).
fn data_cards(model: &Model) -> BTreeMap<(&str, usize), String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for (name, body) in &model.data {
        // Os materiais são comparados à parte.
        let material = name.strip_prefix('m').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if material {
            continue;
        }
        let n = seen.entry(name).or_insert(0);
        *n += 1;
        out.insert((name.as_str(), *n), squeeze(body));
    }
    out
}

/// O que mudou do input `old` para o `new`, cartão a cartão.
pub fn inputs(old: &Model, new: &Model) -> Diff {
    let cells = |m: &Model| -> BTreeMap<i32, (u64, String)> { m.cells.iter().map(|c| (c.id, (c.card_hash, squeeze(&c.card)))).collect() };
    let mut diff = Diff {
        cells: changes(&cells(old), &cells(new), |id| format!("célula {}", id), |v| v.1.clone(), |a, b| a.0 == b.0),
        ..Diff::default()
    };
    for (c, id) in diff.cells.iter_mut().filter(|c| c.kind != Kind::Removed).filter_map(|c| {
        let id = c.what.strip_prefix("célula ")?.parse().ok()?;
        Some((c, id))
    }) {
        c.cell = Some(id);
    }

    let surfaces = |m: &Model| -> BTreeMap<i32, String> { m.surfaces.iter().map(|s| (s.id, squeeze(&s.card))).collect() };
    diff.surfaces = changes(&surfaces(old), &surfaces(new), |id| format!("superfície {}", id), String::clone, |a, b| a == b);
    for c in diff.surfaces.iter_mut().filter(|c| c.kind != Kind::Removed) {
        // Uma célula do input novo que usa a superfície, para ir até ela.
        let id: Option<i32> = c.what.strip_prefix("superfície ").and_then(|t| t.parse().ok());
        let Some(surface) = id.and_then(|id| new.surf_index.get(&id)).map(|&i| &new.surfaces[i]) else { continue };
        c.cell = new.cells.iter().find(|cell| cell.prims.iter().any(|p| surface.prims.contains(p))).map(|cell| cell.id);
    }

    let materials = |m: &Model| -> BTreeMap<i32, String> {
        m.materials.iter().map(|(id, mat)| (*id, mat.nuclides.iter().map(|(n, f)| format!("{} {}", n, f)).collect::<Vec<_>>().join("  "))).collect()
    };
    diff.materials = changes(&materials(old), &materials(new), |id| format!("m{}", id), String::clone, |a, b| a == b);

    let name = |(name, n): (&str, usize)| if n > 1 { format!("{} ({}º)", name, n) } else { name.to_string() };
    diff.data = changes(&data_cards(old), &data_cards(new), name, String::clone, |a, b| a == b);
    diff
}
