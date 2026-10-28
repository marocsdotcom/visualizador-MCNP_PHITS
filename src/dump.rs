//! Partículas de um arquivo de espaço de fase (o `dump` dos tallies do
//! PHITS), para desenhar sobre a geometria.

use glam::DVec3;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub struct Particle {
    /// Código kf do PHITS (22 fóton, 11 elétron, -11 pósitron, 2112 nêutron...).
    pub kf: i32,
    pub pos: [f32; 3],
    pub dir: [f32; 3],
    pub energy: f32,
    pub weight: f32,
}

impl Particle {
    pub fn pos(&self) -> DVec3 {
        DVec3::new(self.pos[0] as f64, self.pos[1] as f64, self.pos[2] as f64)
    }

    pub fn dir(&self) -> DVec3 {
        DVec3::new(self.dir[0] as f64, self.dir[1] as f64, self.dir[2] as f64)
    }
}

pub struct Particles {
    pub name: String,
    pub list: Vec<Particle>,
    /// Por tipo de partícula: (quantas, soma dos pesos, soma de peso × energia).
    pub by_kind: BTreeMap<i32, (u64, f64, f64)>,
}

pub fn kind_name(kf: i32) -> String {
    match kf {
        22 => "fóton".into(),
        11 => "elétron".into(),
        -11 => "pósitron".into(),
        2112 => "nêutron".into(),
        2212 => "próton".into(),
        other => format!("kf {}", other),
    }
}

/// Uma partícula a partir das grandezas gravadas, na ordem padrão dos
/// identificadores 1 a 9 do PHITS: kf, x, y, z, u, v, w, energia, peso.
fn particle(v: &[f64]) -> Option<Particle> {
    if v.len() < 8 || !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    Some(Particle {
        kf: v[0] as i32,
        pos: [v[1] as f32, v[2] as f32, v[3] as f32],
        dir: [v[4] as f32, v[5] as f32, v[6] as f32],
        energy: v[7] as f32,
        weight: v.get(8).copied().unwrap_or(1.0) as f32,
    })
}

/// Comprimento do registro se `bytes` começar por um registro binário do
/// Fortran com 8 a 20 números de 8 bytes (marcador de 4 bytes antes e depois).
fn record_len(bytes: &[u8]) -> Option<usize> {
    let n = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let tail = bytes.get(4 + n..8 + n)?;
    (n % 8 == 0 && (64..=160).contains(&n) && tail == &bytes[..4]).then_some(n)
}

/// Lê um dump do PHITS, binário (`dump = 9`) ou em texto (`dump = -9`).
/// `None` se o arquivo não tiver essa cara.
pub fn parse(bytes: &[u8], name: &str) -> Option<Particles> {
    let list: Vec<Particle> = match record_len(bytes) {
        Some(n) => bytes
            .chunks_exact(n + 8)
            .filter_map(|rec| {
                let v: Vec<f64> = rec[4..4 + n].chunks_exact(8).map(|b| f64::from_le_bytes(b.try_into().unwrap())).collect();
                particle(&v)
            })
            .collect(),
        None => {
            // Texto: só vale se o começo for feito de linhas de números.
            let text = std::str::from_utf8(bytes).ok()?;
            let row = |l: &str| l.split_whitespace().map(|t| t.replace(['D', 'd'], "E").parse::<f64>().ok()).collect::<Option<Vec<f64>>>();
            if !text.lines().take(5).all(|l| row(l).is_some_and(|v| v.len() >= 8)) {
                return None;
            }
            text.lines().filter_map(|l| particle(&row(l)?)).collect()
        }
    };
    if list.is_empty() {
        return None;
    }
    let mut by_kind: BTreeMap<i32, (u64, f64, f64)> = BTreeMap::new();
    for p in &list {
        let e = by_kind.entry(p.kf).or_default();
        e.0 += 1;
        e.1 += p.weight as f64;
        e.2 += p.weight as f64 * p.energy as f64;
    }
    Some(Particles { name: name.to_string(), list, by_kind })
}
