//! Renderização por amostragem direta da geometria: cortes 2D (um ponto por
//! pixel) e vista 3D (um raio por pixel caminhando de fronteira em fronteira).

use crate::geom::{Hint, Level, Model, Work};
use std::sync::atomic::{AtomicBool, Ordering};
use glam::DVec3;
use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorBy {
    Material,
    Cell,
    Universe,
    Density,
    Importance,
}

pub const NONE: u32 = u32::MAX;
const VOID_RGB: [u8; 3] = [236, 239, 243];
const OUTSIDE_RGB: [u8; 3] = [150, 156, 166];
/// Região sem célula definida: quase sempre um erro de geometria.
const UNDEFINED_RGB: [u8; 3] = [255, 0, 144];
const EDGE_RGB: [u8; 3] = [18, 18, 22];
/// Célula sem importância declarada, no modo de cor por importância.
const NO_IMP_RGB: [u8; 3] = [205, 205, 210];
/// Hachura das regiões onde duas células se sobrepõem.
const OVERLAP_RGB: [[u8; 3]; 2] = [[225, 30, 30], [255, 232, 0]];

const CATEGORICAL: [[u8; 3]; 12] = [
    [78, 121, 167],
    [242, 142, 43],
    [89, 161, 79],
    [225, 87, 89],
    [118, 183, 178],
    [237, 201, 72],
    [176, 122, 161],
    [255, 157, 167],
    [156, 117, 95],
    [108, 138, 60],
    [70, 160, 220],
    [200, 110, 50],
];

fn hsv(h: f64, s: f64, v: f64) -> [u8; 3] {
    let h6 = h.fract() * 6.0;
    let (c, x) = (v * s, v * s * (1.0 - (h6 % 2.0 - 1.0).abs()));
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - v * s;
    [r, g, b].map(|ch| ((ch + m) * 255.0).round() as u8)
}

fn hashed(id: i32) -> [u8; 3] {
    let k = id as f64 * 0.618_033_988_75;
    hsv(k.rem_euclid(1.0), 0.50 + 0.25 * ((id as f64 * 0.37).rem_euclid(1.0)), 0.92)
}

/// Cor do material pela posição na lista ordenada de materiais usados.
pub fn material_color(rank: usize, id: i32) -> [u8; 3] {
    CATEGORICAL.get(rank).copied().unwrap_or_else(|| hashed(id))
}

fn ramp(t: f64) -> [u8; 3] {
    // Aproximação do mapa "viridis".
    const STOPS: [[f64; 3]; 5] = [
        [68.0, 1.0, 84.0],
        [59.0, 82.0, 139.0],
        [33.0, 145.0, 140.0],
        [94.0, 201.0, 98.0],
        [253.0, 231.0, 37.0],
    ];
    let x = t.clamp(0.0, 1.0) * 4.0;
    let i = (x as usize).min(3);
    let f = x - i as f64;
    [0, 1, 2].map(|k| (STOPS[i][k] + (STOPS[i + 1][k] - STOPS[i][k]) * f).round() as u8)
}

/// Cor de cada célula folha para o modo escolhido.
pub struct Palette {
    pub rgb: Vec<[u8; 3]>,
}

impl Palette {
    pub fn new(model: &Model, by: ColorBy) -> Self {
        let mut mats: Vec<i32> = model.cells.iter().map(|c| c.mat).filter(|&m| m != 0).collect();
        mats.sort_unstable();
        mats.dedup();
        let dens: Vec<f64> = model.cells.iter().filter(|c| c.mat != 0).map(|c| c.rho.abs()).collect();
        let (dmin, dmax) = dens.iter().fold((f64::INFINITY, 0.0f64), |(a, b), &d| (a.min(d), b.max(d)));
        let log = dmin > 0.0 && dmax / dmin > 100.0;
        let imps = model.cells.iter().map(|c| c.imp).filter(|v| *v > 0.0);
        let (imin, imax) = imps.fold((f64::INFINITY, 0.0f64), |(a, b), v| (a.min(v), b.max(v)));
        let rgb = model
            .cells
            .iter()
            .map(|c| {
                if c.outside {
                    return OUTSIDE_RGB;
                }
                match by {
                    ColorBy::Cell => hashed(c.id),
                    // A importância cresce em potências: escala logarítmica.
                    ColorBy::Importance if c.imp.is_nan() => NO_IMP_RGB,
                    ColorBy::Importance if c.imp <= 0.0 => OUTSIDE_RGB,
                    ColorBy::Importance if imax <= imin => ramp(0.5),
                    ColorBy::Importance => ramp((c.imp / imin).ln() / (imax / imin).ln()),
                    ColorBy::Universe if c.mat == 0 => VOID_RGB,
                    ColorBy::Universe => hashed(c.universe * 7 + 3),
                    _ if c.mat == 0 => VOID_RGB,
                    ColorBy::Material => material_color(mats.binary_search(&c.mat).unwrap_or(usize::MAX), c.mat),
                    ColorBy::Density => {
                        let d = c.rho.abs();
                        if dmax <= dmin {
                            ramp(0.5)
                        } else if log {
                            ramp((d / dmin).ln() / (dmax / dmin).ln())
                        } else {
                            ramp((d - dmin) / (dmax - dmin))
                        }
                    }
                }
            })
            .collect();
        Palette { rgb }
    }
}

/// Resultado em malha pintado por cima do corte.
pub struct Overlay {
    pub field: std::sync::Arc<crate::mesh::Field>,
    /// Limites da escala de cores.
    pub lo: f64,
    pub hi: f64,
    pub log: bool,
    pub alpha: f32,
    /// Mostra o erro relativo (escala fixa de 0 a 20%) em vez do valor.
    pub error: bool,
}

impl Overlay {
    /// Posição de `(valor, erro)` na escala de cores, de 0 a 1.
    pub fn fraction(&self, (value, err): (f64, f64)) -> Option<f64> {
        if self.error {
            return (value != 0.0).then_some((err / 0.2).clamp(0.0, 1.0));
        }
        if value <= 0.0 || self.hi <= self.lo {
            return None;
        }
        let t = if self.log && self.lo > 0.0 {
            (value / self.lo).ln() / (self.hi / self.lo).ln()
        } else {
            (value - self.lo) / (self.hi - self.lo)
        };
        Some(t.clamp(0.0, 1.0))
    }

    pub fn color_at(t: f64) -> [u8; 3] {
        ramp(t)
    }

    /// `base` coberta pela cor da posição `t` da escala (NaN = sem resultado).
    fn tint(&self, base: [u8; 3], t: f32) -> [u8; 3] {
        if t.is_nan() {
            return base;
        }
        let over = ramp(t as f64);
        [0, 1, 2].map(|k| (base[k] as f32 * (1.0 - self.alpha) + over[k] as f32 * self.alpha) as u8)
    }
}

/// Imagem renderizada mais os dados por pixel usados na interação.
#[derive(Default)]
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    /// Índice da célula folha em cada pixel, ou [`NONE`].
    pub leaf: Vec<u32>,
    /// Distância ao longo do raio (só no 3D).
    pub depth: Vec<f32>,
    /// Pixels do corte sem célula e com células sobrepostas (este último só
    /// quando a verificação foi pedida).
    pub undefined: usize,
    pub overlapping: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliceView {
    pub center: DVec3,
    pub u: DVec3,
    pub v: DVec3,
    /// Metade da largura da vista, em cm.
    pub half_width: f64,
}

impl SliceView {
    /// `fx`, `fy` em [0,1] a partir do canto superior esquerdo.
    pub fn point(&self, fx: f64, fy: f64, aspect: f64) -> DVec3 {
        self.center
            + self.u * ((2.0 * fx - 1.0) * self.half_width)
            + self.v * ((1.0 - 2.0 * fy) * self.half_width / aspect)
    }
}

#[derive(Clone, Copy)]
struct Sample {
    leaf: u32,
    inst: u64,
    t: f32,
    normal: [f32; 3],
    /// Elemento de um lattice de voxels (ver [`crate::geom::Found::voxel`]).
    voxel: bool,
    overlap: bool,
    /// Cor acumulada das camadas translúcidas na frente do pixel (já
    /// multiplicada pela opacidade) e quanto elas cobrem do que vem atrás.
    tint: [f32; 3],
    cover: f32,
    /// `leaf` é uma superfície opaca (e não só a primeira camada translúcida).
    opaque: bool,
    /// Posição do resultado sobreposto na escala de cores (NaN = nenhum).
    over: f32,
}

const MISS: Sample = Sample {
    leaf: NONE,
    inst: 0,
    t: f32::INFINITY,
    normal: [0.0; 3],
    voxel: false,
    overlap: false,
    tint: [0.0; 3],
    cover: 0.0,
    opaque: false,
    over: f32::NAN,
};

/// Opacidade de cada camada de material translúcido.
const GLASS_ALPHA: f32 = 0.3;

fn shade(normal: glam::Vec3, light: glam::Vec3) -> f32 {
    0.38 + 0.62 * normal.dot(light).max(0.0)
}

/// Estado por thread reaproveitado de um ponto ou raio para o seguinte.
#[derive(Default)]
struct Scratch {
    hint: Hint,
    levels: Vec<Level>,
    work: Work,
}

fn differs(a: &Sample, b: &Sample) -> bool {
    // Entre voxels não há contorno: seriam milhões de quadradinhos pretos.
    if a.voxel && b.voxel {
        return false;
    }
    if a.inst != b.inst || a.leaf != b.leaf {
        return true;
    }
    let dot = a.normal[0] * b.normal[0] + a.normal[1] * b.normal[1] + a.normal[2] * b.normal[2];
    a.leaf != NONE && dot < 0.55
}

/// Contorno onde o pixel difere do vizinho da direita ou de baixo.
fn is_edge(samples: &[Sample], w: usize, h: usize, x: usize, y: usize) -> bool {
    let s = &samples[y * w + x];
    (x + 1 < w && differs(s, &samples[y * w + x + 1])) || (y + 1 < h && differs(s, &samples[(y + 1) * w + x]))
}

/// `cancel` interrompe a renderização (devolve `None`) quando outra imagem
/// mais nova já foi pedida. `check` procura células sobrepostas em cada pixel
/// (mais lento) e as destaca com hachura.
#[allow(clippy::too_many_arguments)]
pub fn render_slice(
    model: &Model,
    pal: &Palette,
    view: &SliceView,
    w: usize,
    h: usize,
    edges: bool,
    check: bool,
    overlay: Option<&Overlay>,
    cancel: &AtomicBool,
) -> Option<Frame> {
    let aspect = w as f64 / h as f64;
    let mut samples = vec![MISS; w * h];
    samples.par_chunks_mut(w).enumerate().for_each_init(Scratch::default, |sc, (y, row)| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        for (x, s) in row.iter_mut().enumerate() {
            let p = view.point((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64, aspect);
            if let Some(f) = model.descend(p, DVec3::X, &mut sc.hint, &mut sc.levels) {
                *s = Sample { leaf: f.leaf as u32, inst: f.inst, t: 0.0, normal: [0.0, 0.0, 1.0], voxel: f.voxel, ..MISS };
                if let Some(t) = overlay.and_then(|o| o.fraction(o.field.at(p, &sc.levels)?)) {
                    s.over = t as f32;
                }
            }
            s.overlap = check && model.overlap_at(p).is_some();
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let mut rgba = vec![255u8; w * h * 4];
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let s = &samples[y * w + x];
            let c = if s.overlap {
                OVERLAP_RGB[(x + y) / 4 % 2]
            } else if edges && is_edge(&samples, w, h, x, y) {
                EDGE_RGB
            } else if s.leaf == NONE {
                // Hachura para destacar a região indefinida.
                if (x + y) / 4 % 2 == 0 { UNDEFINED_RGB } else { [255, 255, 255] }
            } else {
                let base = pal.rgb[s.leaf as usize];
                overlay.map_or(base, |o| o.tint(base, s.over))
            };
            row[x * 4..x * 4 + 3].copy_from_slice(&c);
        }
    });
    Some(Frame {
        w,
        h,
        rgba,
        leaf: samples.iter().map(|s| s.leaf).collect(),
        depth: Vec::new(),
        undefined: samples.iter().filter(|s| s.leaf == NONE).count(),
        overlapping: samples.iter().filter(|s| s.overlap).count(),
    })
}

/// Câmera orbital com Z para cima.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: DVec3,
    pub yaw: f64,
    pub pitch: f64,
    pub dist: f64,
    pub fov_deg: f64,
}

impl Camera {
    /// (olho, frente, direita, cima)
    pub fn basis(&self) -> (DVec3, DVec3, DVec3, DVec3) {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        let back = DVec3::new(cp * cy, cp * sy, sp);
        let right = DVec3::new(-sy, cy, 0.0);
        (self.target + back * self.dist, -back, right, right.cross(-back))
    }

    pub fn ray(&self, fx: f64, fy: f64, aspect: f64) -> (DVec3, DVec3) {
        let (eye, fwd, right, up) = self.basis();
        let k = (self.fov_deg.to_radians() / 2.0).tan();
        let d = fwd + right * ((2.0 * fx - 1.0) * k * aspect) + up * ((1.0 - 2.0 * fy) * k);
        (eye, d.normalize())
    }
}

pub struct Scene<'a> {
    pub model: &'a Model,
    pub pal: &'a Palette,
    /// Células folha desenhadas no 3D; as demais são atravessadas.
    pub visible: &'a [bool],
    /// Células desenhadas como vidro colorido, deixando ver o que há atrás.
    pub translucent: &'a [bool],
    /// Plano de corte: só aparece a região com `n·p - d <= 0`.
    pub clip: Option<(DVec3, f64)>,
    pub edges: bool,
}

/// Um raio na diagonal de um fantoma voxelizado atravessa milhares de voxels.
const MAX_STEPS: usize = 20_000;

fn trace(scene: &Scene, o: DVec3, d: DVec3, light: glam::Vec3, sc: &mut Scratch) -> Sample {
    let (mut t, mut t_end) = (0.0f64, f64::INFINITY);
    let mut normal = -d;
    if let Some((n, k)) = scene.clip {
        let (side, slope) = (n.dot(o) - k, n.dot(d));
        if slope.abs() < 1e-12 {
            if side > 0.0 {
                return MISS;
            }
        } else {
            let tc = -side / slope;
            if slope < 0.0 {
                if tc > 0.0 {
                    t = tc;
                    normal = n;
                }
            } else if tc <= 0.0 {
                return MISS;
            } else {
                t_end = tc;
            }
        }
    }
    let model = scene.model;
    // O que o pixel mostra se o raio só encontrar camadas translúcidas.
    let mut out = MISS;
    // Última camada translúcida somada, para não contar a mesma duas vezes.
    let mut glass = (NONE, 0u64);
    for _ in 0..MAX_STEPS {
        if t >= t_end {
            break;
        }
        let Some(found) = model.descend(o + d * t, d, &mut sc.hint, &mut sc.levels) else { break };
        if scene.visible[found.leaf] {
            let n = (if normal.dot(d) > 0.0 { -normal } else { normal }).as_vec3();
            let hit = Sample { leaf: found.leaf as u32, inst: found.inst, t: t as f32, normal: n.to_array(), voxel: found.voxel, ..out };
            if !scene.translucent[found.leaf] {
                return Sample { opaque: true, ..hit };
            }
            if glass != (hit.leaf, hit.inst) {
                glass = (hit.leaf, hit.inst);
                let a = GLASS_ALPHA * (1.0 - out.cover);
                let rgb = scene.pal.rgb[found.leaf];
                for k in 0..3 {
                    out.tint[k] += a * rgb[k] as f32 * shade(n, light);
                }
                out.cover += a;
                if out.leaf == NONE {
                    out = Sample { tint: out.tint, cover: out.cover, ..hit };
                }
            }
        } else {
            glass = (NONE, 0);
        }
        let step = if found.voxel {
            model.voxel_skip(&sc.levels, scene.visible, &mut sc.work)
        } else {
            model.next_boundary(&sc.levels, &mut sc.work)
        };
        let Some((dt, n)) = step else { break };
        // Passa um pouco da fronteira para cair dentro da próxima célula.
        t += dt + 1e-8 * (1.0 + t);
        normal = n;
    }
    out
}

pub fn render_3d(scene: &Scene, cam: &Camera, w: usize, h: usize, cancel: &AtomicBool) -> Option<Frame> {
    let aspect = w as f64 / h as f64;
    let (_, fwd, right, up) = cam.basis();
    let light = (-fwd * 0.75 + up * 0.5 - right * 0.4).normalize().as_vec3();
    let mut samples = vec![MISS; w * h];
    samples.par_chunks_mut(w).enumerate().for_each_init(Scratch::default, |sc, (y, row)| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        for (x, s) in row.iter_mut().enumerate() {
            let (o, d) = cam.ray((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64, aspect);
            *s = trace(scene, o, d, light, sc);
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let mut rgba = vec![255u8; w * h * 4];
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let fy = y as f32 / h.max(2) as f32;
        let bg = [44.0 - 18.0 * fy, 48.0 - 20.0 * fy, 60.0 - 24.0 * fy].map(|v| v as u8);
        for x in 0..w {
            let s = &samples[y * w + x];
            let edge = scene.edges && is_edge(&samples, w, h, x, y);
            let c = if s.leaf == NONE {
                if edge { EDGE_RGB } else { bg }
            } else {
                // Atrás das camadas translúcidas: a superfície opaca ou o fundo.
                let behind = if s.opaque {
                    let k = shade(glam::Vec3::from_array(s.normal), light) * if edge { 0.25 } else { 1.0 };
                    scene.pal.rgb[s.leaf as usize].map(|ch| ch as f32 * k)
                } else {
                    bg.map(|ch| ch as f32 * if edge { 0.4 } else { 1.0 })
                };
                [0, 1, 2].map(|k| (s.tint[k] + (1.0 - s.cover) * behind[k]) as u8)
            };
            row[x * 4..x * 4 + 3].copy_from_slice(&c);
        }
    });
    Some(Frame {
        w,
        h,
        rgba,
        leaf: samples.iter().map(|s| s.leaf).collect(),
        depth: samples.iter().map(|s| s.t).collect(),
        undefined: 0,
        overlapping: 0,
    })
}
