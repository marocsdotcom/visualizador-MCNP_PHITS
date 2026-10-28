//! Motor de geometria CSG compartilhado pelos leitores de MCNP e PHITS.
//!
//! Tudo é avaliado de forma exata (sem malha): cada superfície é uma função
//! implícita f(p), o sentido é o sinal de f, e as células são expressões
//! booleanas sobre semi-espaços. Universos, lattices e transformações são
//! resolvidos na hora, descendo a hierarquia ponto a ponto.

use glam::{DMat3, DVec3};
use std::collections::{BTreeMap, HashMap};

/// Transformação no estilo do cartão TR: `principal = rot * auxiliar + off`.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub rot: DMat3,
    pub off: DVec3,
}

impl Transform {
    pub fn to_local(&self, p: DVec3) -> DVec3 {
        self.rot.transpose() * (p - self.off)
    }

    pub fn dir_to_local(&self, d: DVec3) -> DVec3 {
        self.rot.transpose() * d
    }

    /// Equivale a aplicar `inner` e depois `self` (do auxiliar para o principal).
    pub fn compose(&self, inner: &Transform) -> Transform {
        Transform { rot: self.rot * inner.rot, off: self.off + self.rot * inner.off }
    }
}

/// Caixa alinhada aos eixos, possivelmente infinita em alguma direção.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub lo: DVec3,
    pub hi: DVec3,
}

impl Aabb {
    pub const ALL: Aabb = Aabb { lo: DVec3::NEG_INFINITY, hi: DVec3::INFINITY };
    pub const EMPTY: Aabb = Aabb { lo: DVec3::INFINITY, hi: DVec3::NEG_INFINITY };

    pub fn contains(&self, p: DVec3) -> bool {
        p.cmpge(self.lo).all() && p.cmple(self.hi).all()
    }

    fn is_empty(&self) -> bool {
        self.lo.cmpgt(self.hi).any()
    }

    fn intersect(self, o: Aabb) -> Aabb {
        let b = Aabb { lo: self.lo.max(o.lo), hi: self.hi.min(o.hi) };
        if b.is_empty() { Aabb::EMPTY } else { b }
    }

    fn hull(self, o: Aabb) -> Aabb {
        Aabb { lo: self.lo.min(o.lo), hi: self.hi.max(o.hi) }
    }

    /// A caixa, dada no referencial auxiliar de `t`, vista do principal.
    fn transformed(self, t: &Transform) -> Aabb {
        if self.is_empty() {
            return Aabb::EMPTY;
        }
        if (t.rot - DMat3::IDENTITY).to_cols_array().iter().all(|v| v.abs() < 1e-12) {
            return Aabb { lo: self.lo + t.off, hi: self.hi + t.off };
        }
        if !(self.lo.is_finite() && self.hi.is_finite()) {
            return Aabb::ALL;
        }
        (0..8).fold(Aabb::EMPTY, |acc, k| {
            let c = DVec3::new(
                if k & 1 == 0 { self.lo.x } else { self.hi.x },
                if k & 2 == 0 { self.lo.y } else { self.hi.y },
                if k & 4 == 0 { self.lo.z } else { self.hi.z },
            );
            let p = t.rot * c + t.off;
            acc.hull(Aabb { lo: p, hi: p })
        })
    }

    /// Folga contra erro de arredondamento.
    fn padded(self) -> Aabb {
        if self.is_empty() {
            return self;
        }
        let pad = |v: f64| if v.is_finite() { 1e-9 + 1e-9 * v.abs() } else { 0.0 };
        Aabb { lo: self.lo - self.lo.map(pad), hi: self.hi + self.hi.map(pad) }
    }
}

/// Quádrica geral `pᵀ·a·p + b·p + c`, com `a` simétrica.
#[derive(Clone, Debug)]
pub struct Quadric {
    pub a: DMat3,
    pub b: DVec3,
    pub c: f64,
    /// Cone de uma folha: só vale onde `n·p - d > 0`.
    pub sheet: Option<(DVec3, f64)>,
}

impl Quadric {
    pub fn zero() -> Self {
        Quadric { a: DMat3::ZERO, b: DVec3::ZERO, c: 0.0, sheet: None }
    }

    /// Coeficientes do cartão GQ: Ax²+By²+Cz²+Dxy+Eyz+Fzx+Gx+Hy+Jz+K.
    pub fn from_gq(v: &[f64]) -> Self {
        let (a, b, c, d, e, f) = (v[0], v[1], v[2], v[3] / 2.0, v[4] / 2.0, v[5] / 2.0);
        Quadric {
            a: DMat3::from_cols(DVec3::new(a, d, f), DVec3::new(d, b, e), DVec3::new(f, e, c)),
            b: DVec3::new(v[6], v[7], v[8]),
            c: v[9],
            sheet: None,
        }
    }

    /// Soma `Σ diag_i (p_i - center_i)²`.
    pub fn add_diag(mut self, diag: DVec3, center: DVec3) -> Self {
        self.a += DMat3::from_diagonal(diag);
        self.b += -2.0 * diag * center;
        self.c += (diag * center).dot(center);
        self
    }

    /// Soma `w (n·p - d)²`.
    pub fn add_sq(mut self, n: DVec3, d: f64, w: f64) -> Self {
        self.a += w * DMat3::from_cols(n * n.x, n * n.y, n * n.z);
        self.b += -2.0 * w * d * n;
        self.c += w * d * d;
        self
    }

    /// Soma `w (n·p - d)`.
    pub fn add_lin(mut self, n: DVec3, d: f64, w: f64) -> Self {
        self.b += w * n;
        self.c -= w * d;
        self
    }

    pub fn add_const(mut self, k: f64) -> Self {
        self.c += k;
        self
    }

    fn eval(&self, p: DVec3) -> f64 {
        p.dot(self.a * p) + self.b.dot(p) + self.c
    }
}

#[derive(Clone, Debug)]
pub enum Prim {
    /// `n·p - d`
    Plane { n: DVec3, d: f64 },
    Quadric(Quadric),
    /// Toro elíptico com eixo `axis` no referencial local `tr`:
    /// `x²/b² + (ρ - a)²/c² - 1`.
    Torus { tr: Transform, axis: usize, a: f64, b: f64, c: f64 },
}

fn torus_f(l: DVec3, axis: usize, a: f64, b: f64, c: f64) -> f64 {
    let x = l[axis];
    let rho = (l.length_squared() - x * x).max(0.0).sqrt();
    x * x / (b * b) + (rho - a) * (rho - a) / (c * c) - 1.0
}

impl Prim {
    pub fn eval(&self, p: DVec3) -> f64 {
        match self {
            Prim::Plane { n, d } => n.dot(p) - d,
            Prim::Quadric(q) => {
                if let Some((n, d)) = q.sheet {
                    if n.dot(p) - d < 0.0 {
                        return 1.0;
                    }
                }
                q.eval(p)
            }
            Prim::Torus { tr, axis, a, b, c } => torus_f(tr.to_local(p), *axis, *a, *b, *c),
        }
    }

    /// Distância aproximada de `p` à superfície: o valor da função dividido
    /// pelo módulo do gradiente (exata para planos, boa perto das demais).
    pub fn distance(&self, p: DVec3) -> f64 {
        let gradient = match self {
            Prim::Plane { n, .. } => n.length(),
            Prim::Quadric(q) => (2.0 * (q.a * p) + q.b).length(),
            Prim::Torus { .. } => {
                let h = 1e-5;
                let slope = |axis: DVec3| (self.eval(p + axis * h) - self.eval(p - axis * h)) / (2.0 * h);
                DVec3::new(slope(DVec3::X), slope(DVec3::Y), slope(DVec3::Z)).length()
            }
        };
        self.eval(p).abs() / gradient.max(1e-300)
    }

    /// Gradiente normalizado (aponta para o lado de sentido positivo).
    pub fn normal(&self, p: DVec3) -> DVec3 {
        let g = match self {
            Prim::Plane { n, .. } => *n,
            Prim::Quadric(q) => 2.0 * (q.a * p) + q.b,
            Prim::Torus { tr, axis, a, b, c } => {
                let l = tr.to_local(p);
                let x = l[*axis];
                let mut radial = l;
                radial[*axis] = 0.0;
                let rho = radial.length().max(1e-300);
                let mut g = radial / rho * (2.0 * (rho - a) / (c * c));
                g[*axis] = 2.0 * x / (b * b);
                tr.rot * g
            }
        };
        g.normalize_or_zero()
    }

    /// Menor `t > tmin` em que o raio `o + t·d` cruza a superfície.
    pub fn intersect(&self, o: DVec3, d: DVec3, tmin: f64) -> Option<f64> {
        match self {
            Prim::Plane { n, d: dd } => {
                let denom = n.dot(d);
                if denom.abs() < 1e-14 * n.length() {
                    return None;
                }
                let t = (dd - n.dot(o)) / denom;
                (t > tmin).then_some(t)
            }
            Prim::Quadric(q) => {
                let ad = q.a * d;
                let qa = d.dot(ad);
                let qb = 2.0 * o.dot(ad) + q.b.dot(d);
                let qc = q.eval(o);
                let scale = q.a.x_axis.abs().max_element()
                    + q.a.y_axis.abs().max_element()
                    + q.a.z_axis.abs().max_element();
                let ok = |t: f64| {
                    t > tmin
                        && q.sheet.is_none_or(|(n, dd)| n.dot(o + d * t) - dd >= 0.0)
                };
                if qa.abs() < 1e-13 * scale {
                    if qb.abs() < 1e-300 {
                        return None;
                    }
                    let t = -qc / qb;
                    return ok(t).then_some(t);
                }
                let disc = qb * qb - 4.0 * qa * qc;
                if disc < 0.0 {
                    return None;
                }
                // Forma estável: evita cancelamento quando qb² >> 4·qa·qc.
                let s = -0.5 * (qb + qb.signum() * disc.sqrt());
                let (mut t1, mut t2) = (s / qa, if s != 0.0 { qc / s } else { f64::NAN });
                if qb == 0.0 {
                    let r = disc.sqrt() / (2.0 * qa);
                    t1 = -r;
                    t2 = r;
                }
                if t1 > t2 {
                    std::mem::swap(&mut t1, &mut t2);
                }
                if ok(t1) {
                    Some(t1)
                } else if ok(t2) {
                    Some(t2)
                } else {
                    None
                }
            }
            Prim::Torus { tr, axis, a, b, c } => {
                // Quártica: resolvida por amostragem dentro da esfera envolvente
                // seguida de bisseção.
                let (lo, ld) = (tr.to_local(o), tr.dir_to_local(d));
                let r = a.abs() + b.abs().max(c.abs());
                let bq = lo.dot(ld);
                let disc = bq * bq - (lo.length_squared() - r * r * 1.0001);
                if disc <= 0.0 {
                    return None;
                }
                let (t0, t1) = ((-bq - disc.sqrt()).max(tmin), -bq + disc.sqrt());
                if t1 <= t0 {
                    return None;
                }
                let f = |t: f64| torus_f(lo + ld * t, *axis, *a, *b, *c);
                const N: usize = 64;
                let (mut ta, mut fa) = (t0, f(t0));
                for i in 1..=N {
                    let tb = t0 + (t1 - t0) * i as f64 / N as f64;
                    let fb = f(tb);
                    if (fa > 0.0) != (fb > 0.0) {
                        let (mut l, mut h) = (ta, tb);
                        for _ in 0..48 {
                            let m = 0.5 * (l + h);
                            if (f(m) > 0.0) == (fa > 0.0) {
                                l = m;
                            } else {
                                h = m;
                            }
                        }
                        return Some(h);
                    }
                    ta = tb;
                    fa = fb;
                }
                None
            }
        }
    }

    /// Caixa que contém o semi-espaço de sentido `pos` ([`Aabb::ALL`] quando
    /// ele não é limitado em nenhum eixo, ou quando não vale a pena calcular).
    fn half_box(&self, pos: bool) -> Aabb {
        let mut bx = Aabb::ALL;
        match self {
            Prim::Plane { n, d } => {
                let len = n.length();
                for i in 0..3 {
                    if n[i].abs() >= len * (1.0 - 1e-12) {
                        let x = d / n[i];
                        if (n[i] > 0.0) == pos {
                            bx.lo[i] = x;
                        } else {
                            bx.hi[i] = x;
                        }
                    }
                }
            }
            // Só o lado de dentro de esferas, elipsoides e cilindros alinhados.
            Prim::Quadric(q) if !pos && q.sheet.is_none() => {
                let diag = [q.a.x_axis.x, q.a.y_axis.y, q.a.z_axis.z];
                let off = q.a.x_axis.y.abs() + q.a.x_axis.z.abs() + q.a.y_axis.z.abs();
                let mag: f64 = diag.iter().map(|v| v.abs()).sum();
                if mag == 0.0 || off > 1e-12 * mag {
                    return bx;
                }
                let (mut k, mut cen) = (q.c, [None; 3]);
                for i in 0..3 {
                    if diag[i].abs() <= 1e-12 * mag {
                        if q.b[i] != 0.0 {
                            return bx;
                        }
                    } else if diag[i] < 0.0 {
                        return bx;
                    } else {
                        let c = -q.b[i] / (2.0 * diag[i]);
                        cen[i] = Some(c);
                        k -= diag[i] * c * c;
                    }
                }
                if k >= 0.0 {
                    return bx;
                }
                for i in 0..3 {
                    if let Some(c) = cen[i] {
                        let r = (-k / diag[i]).sqrt();
                        bx.lo[i] = c - r;
                        bx.hi[i] = c + r;
                    }
                }
            }
            Prim::Torus { tr, a, b, c, .. } if !pos => {
                let r = DVec3::splat(a.abs() + b.abs().max(c.abs()));
                bx = Aabb { lo: tr.off - r, hi: tr.off + r };
            }
            _ => {}
        }
        bx
    }

    /// Chama `f(t)` em todo cruzamento do raio com a superfície em `0 < t < tmax`
    /// (ao contrário de [`Prim::intersect`], que só devolve o primeiro).
    pub fn crossings(&self, o: DVec3, d: DVec3, tmax: f64, mut f: impl FnMut(f64)) {
        match self {
            Prim::Plane { n, d: dd } => {
                let denom = n.dot(d);
                if denom.abs() >= 1e-14 * n.length() {
                    let t = (dd - n.dot(o)) / denom;
                    if t > 0.0 && t < tmax {
                        f(t);
                    }
                }
            }
            Prim::Quadric(q) => {
                let ad = q.a * d;
                let qa = d.dot(ad);
                let qb = 2.0 * o.dot(ad) + q.b.dot(d);
                let qc = q.eval(o);
                let scale = q.a.x_axis.abs().max_element()
                    + q.a.y_axis.abs().max_element()
                    + q.a.z_axis.abs().max_element();
                let mut emit = |t: f64| {
                    if t > 0.0 && t < tmax && q.sheet.is_none_or(|(n, dd)| n.dot(o + d * t) - dd >= 0.0) {
                        f(t);
                    }
                };
                if qa.abs() < 1e-13 * scale {
                    if qb.abs() >= 1e-300 {
                        emit(-qc / qb);
                    }
                    return;
                }
                let disc = qb * qb - 4.0 * qa * qc;
                if disc <= 0.0 {
                    // Tangência não muda o sentido.
                    return;
                }
                if qb == 0.0 {
                    let r = disc.sqrt() / (2.0 * qa);
                    emit(-r);
                    emit(r);
                } else {
                    let s = -0.5 * (qb + qb.signum() * disc.sqrt());
                    emit(s / qa);
                    emit(qc / s);
                }
            }
            Prim::Torus { tr, axis, a, b, c } => {
                let (lo, ld) = (tr.to_local(o), tr.dir_to_local(d));
                let r = a.abs() + b.abs().max(c.abs());
                let bq = lo.dot(ld);
                let disc = bq * bq - (lo.length_squared() - r * r * 1.0001);
                if disc <= 0.0 {
                    return;
                }
                let (t0, t1) = ((-bq - disc.sqrt()).max(0.0), (-bq + disc.sqrt()).min(tmax));
                if t1 <= t0 {
                    return;
                }
                let g = |t: f64| torus_f(lo + ld * t, *axis, *a, *b, *c);
                const N: usize = 64;
                let (mut ta, mut fa) = (t0, g(t0));
                for i in 1..=N {
                    let tb = t0 + (t1 - t0) * i as f64 / N as f64;
                    let fb = g(tb);
                    if (fa > 0.0) != (fb > 0.0) {
                        let (mut l, mut h) = (ta, tb);
                        for _ in 0..48 {
                            let m = 0.5 * (l + h);
                            if (g(m) > 0.0) == (fa > 0.0) {
                                l = m;
                            } else {
                                h = m;
                            }
                        }
                        if h > 0.0 {
                            f(h);
                        }
                    }
                    ta = tb;
                    fa = fb;
                }
            }
        }
    }

    /// A mesma superfície, definida no referencial auxiliar `t`, vista do principal.
    pub fn transformed(&self, t: &Transform) -> Prim {
        match self {
            Prim::Plane { n, d } => {
                let n2 = t.rot * *n;
                Prim::Plane { n: n2, d: d + n2.dot(t.off) }
            }
            Prim::Quadric(q) => {
                let a = t.rot * q.a * t.rot.transpose();
                let b0 = t.rot * q.b;
                let ao = a * t.off;
                Prim::Quadric(Quadric {
                    a,
                    b: b0 - 2.0 * ao,
                    c: q.c + t.off.dot(ao) - b0.dot(t.off),
                    sheet: q.sheet.map(|(n, d)| {
                        let n2 = t.rot * n;
                        (n2, d + n2.dot(t.off))
                    }),
                })
            }
            Prim::Torus { tr, axis, a, b, c } => {
                Prim::Torus { tr: t.compose(tr), axis: *axis, a: *a, b: *b, c: *c }
            }
        }
    }

    /// Plano por `point` com sentido positivo no lado de `outward`.
    pub fn plane_through(point: DVec3, outward: DVec3) -> Prim {
        let n = outward.normalize();
        Prim::Plane { n, d: n.dot(point) }
    }

    /// Coordenadas que ajudam a estimar a caixa envolvente do modelo.
    fn extent_hints(&self, out: &mut Vec<(usize, f64)>) {
        match self {
            Prim::Plane { n, d } => {
                let len = n.length();
                for i in 0..3 {
                    if n[i].abs() > 0.999 * len {
                        out.push((i, d / n[i]));
                        return;
                    }
                }
                let p = *n * (d / (len * len));
                out.extend((0..3).map(|i| (i, p[i])));
            }
            Prim::Quadric(q) => {
                let diag = [q.a.x_axis.x, q.a.y_axis.y, q.a.z_axis.z];
                let off = q.a.x_axis.y.abs() + q.a.x_axis.z.abs() + q.a.y_axis.z.abs();
                let mag: f64 = diag.iter().map(|v| v.abs()).sum();
                if off > 1e-9 * mag {
                    return;
                }
                let mut k = q.c;
                let mut cen = [None; 3];
                for i in 0..3 {
                    if diag[i].abs() > 1e-12 * mag {
                        let c = -q.b[i] / (2.0 * diag[i]);
                        cen[i] = Some(c);
                        k -= diag[i] * c * c;
                    }
                }
                for i in 0..3 {
                    if let Some(c) = cen[i] {
                        let r2 = -k / diag[i];
                        let r = if r2 > 0.0 { r2.sqrt() } else { 0.0 };
                        out.push((i, c - r));
                        out.push((i, c + r));
                    }
                }
            }
            Prim::Torus { tr, a, b, c, .. } => {
                let r = a.abs() + b.abs().max(c.abs());
                for i in 0..3 {
                    out.push((i, tr.off[i] - r));
                    out.push((i, tr.off[i] + r));
                }
            }
        }
    }
}

/// Expressão booleana de uma célula.
#[derive(Clone, Debug)]
pub enum Node {
    Half { prim: u32, pos: bool },
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Empty,
}

impl Node {
    pub fn contains(&self, prims: &[Prim], p: DVec3) -> bool {
        match self {
            Node::Half { prim, pos } => (prims[*prim as usize].eval(p) > 0.0) == *pos,
            Node::And(v) => v.iter().all(|n| n.contains(prims, p)),
            Node::Or(v) => v.iter().any(|n| n.contains(prims, p)),
            Node::Not(n) => !n.contains(prims, p),
            Node::Empty => false,
        }
    }

    /// Como [`Node::contains`], mas com o sentido de cada superfície já
    /// calculado em `signs` (indexado pelo número global da superfície).
    fn eval_signs(&self, signs: &[bool]) -> bool {
        match self {
            Node::Half { prim, pos } => signs[*prim as usize] == *pos,
            Node::And(v) => v.iter().all(|n| n.eval_signs(signs)),
            Node::Or(v) => v.iter().any(|n| n.eval_signs(signs)),
            Node::Not(n) => !n.eval_signs(signs),
            Node::Empty => false,
        }
    }

    /// Forma normal: as negações descem até os semi-espaços (leis de De
    /// Morgan) e interseções/uniões aninhadas são fundidas. O complemento de
    /// uma célula (`#n`), que seria um bloco único, vira uma lista de termos
    /// pequenos que falham ou valem cedo.
    fn normalized(self, neg: bool) -> Node {
        let (is_and, v) = match self {
            Node::Half { prim, pos } => return Node::Half { prim, pos: pos != neg },
            Node::Not(n) => return n.normalized(!neg),
            // Interseção vazia = todo o espaço.
            Node::Empty => return if neg { Node::And(Vec::new()) } else { Node::Empty },
            Node::And(v) => (!neg, v),
            Node::Or(v) => (neg, v),
        };
        let mut out = Vec::with_capacity(v.len());
        for n in v {
            match (n.normalized(neg), is_and) {
                (Node::And(inner), true) | (Node::Or(inner), false) => out.extend(inner),
                (n, _) => out.push(n),
            }
        }
        if out.len() == 1 {
            return out.pop().unwrap();
        }
        if is_and { Node::And(out) } else { Node::Or(out) }
    }

    /// Os semi-espaços, se o nó for um deles ou uma união simples deles.
    fn as_clause(&self) -> Option<Vec<(u32, bool)>> {
        let half = |n: &Node| match n {
            Node::Half { prim, pos } => Some((*prim, *pos)),
            _ => None,
        };
        match self {
            Node::Or(v) => v.iter().map(half).collect(),
            n => half(n).map(|h| vec![h]),
        }
    }

    /// Caixa que contém a região (ou o complemento dela, se `neg`).
    fn bbox(&self, prims: &[Prim], neg: bool) -> Aabb {
        match self {
            Node::Half { prim, pos } => prims[*prim as usize].half_box(*pos != neg),
            Node::And(v) | Node::Or(v) => {
                let boxes = v.iter().map(|n| n.bbox(prims, neg));
                if matches!(self, Node::And(_)) != neg {
                    boxes.fold(Aabb::ALL, Aabb::intersect)
                } else {
                    boxes.fold(Aabb::EMPTY, Aabb::hull)
                }
            }
            Node::Not(n) => n.bbox(prims, !neg),
            Node::Empty => if neg { Aabb::ALL } else { Aabb::EMPTY },
        }
    }

    fn uses(&self, prim: u32) -> bool {
        match self {
            Node::Half { prim: p, .. } => *p == prim,
            Node::And(v) | Node::Or(v) => v.iter().any(|n| n.uses(prim)),
            Node::Not(n) => n.uses(prim),
            Node::Empty => false,
        }
    }

    fn collect_prims(&self, out: &mut Vec<u32>) {
        match self {
            Node::Half { prim, .. } => {
                if !out.contains(prim) {
                    out.push(*prim)
                }
            }
            Node::And(v) | Node::Or(v) => v.iter().for_each(|n| n.collect_prims(out)),
            Node::Not(n) => n.collect_prims(out),
            Node::Empty => {}
        }
    }

    /// Semi-espaços de uma interseção pura, na ordem em que aparecem no cartão.
    fn flatten_and(&self, out: &mut Vec<(u32, bool)>) -> bool {
        match self {
            Node::Half { prim, pos } => {
                out.push((*prim, *pos));
                true
            }
            Node::And(v) => v.iter().all(|n| n.flatten_and(out)),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum FillDef {
    None,
    Single { u: i32, tr: Option<Transform> },
    /// Ordem Fortran: o índice `i` varia mais rápido.
    Array { lo: [i32; 3], hi: [i32; 3], us: Vec<i32> },
}

#[derive(Clone, Debug)]
pub enum Filled {
    None,
    Single { uni: usize, tr: Option<Transform> },
    Array { lo: [i32; 3], dims: [usize; 3], unis: Vec<u32> },
}

/// Elemento de lattice preenchido com o material da própria célula de lattice.
const OWN: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct Lattice {
    pub hex: bool,
    n: [DVec3; 3],
    low: [f64; 3],
    pitch: [f64; 3],
    active: [bool; 3],
    /// Vetores de translação entre elementos vizinhos.
    vec: [DVec3; 3],
    center: DVec3,
    inv: DMat3,
}

impl Lattice {
    /// Índices do elemento que contém `p` e o deslocamento até o elemento [0,0,0].
    pub fn index(&self, p: DVec3) -> ([i32; 3], DVec3) {
        let mut idx = [0i32; 3];
        let axial = |m: usize| ((self.n[m].dot(p) - self.low[m]) / self.pitch[m]).floor() as i32;
        if self.hex {
            let q = p - self.center;
            let f = self.inv * q;
            let (i0, j0) = (f.x.round() as i32, f.y.round() as i32);
            let mut best = f64::INFINITY;
            for di in -1..=1 {
                for dj in -1..=1 {
                    let (i, j) = (i0 + di, j0 + dj);
                    let r = q - self.vec[0] * i as f64 - self.vec[1] * j as f64;
                    let ax = r.dot(self.n[2]);
                    let dist = r.length_squared() - ax * ax;
                    if dist < best {
                        best = dist;
                        idx[0] = i;
                        idx[1] = j;
                    }
                }
            }
            if self.active[2] {
                idx[2] = axial(2);
            }
        } else {
            for m in 0..3 {
                if self.active[m] {
                    idx[m] = axial(m);
                }
            }
        }
        let off = self.vec[0] * idx[0] as f64 + self.vec[1] * idx[1] as f64 + self.vec[2] * idx[2] as f64;
        (idx, off)
    }
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub id: i32,
    pub mat: i32,
    pub rho: f64,
    pub node: Node,
    pub prims: Vec<u32>,
    /// Para cada termo (ver [`Cell::terms`]): os semi-espaços, quando ele é uma
    /// união simples deles. Vale enquanto ao menos um valer, o que se acompanha
    /// por contagem ao longo de um raio.
    clauses: Vec<Option<Vec<(u32, bool)>>>,
    /// Para cada superfície de `prims`, os termos em que ela aparece e como:
    /// 1 e 2 = semi-espaço positivo e negativo de uma cláusula; 0 = termo
    /// qualquer, que precisa ser reavaliado inteiro.
    prim_terms: Vec<Vec<(u32, u8)>>,
    /// Caixa envolvente no referencial do universo da célula: descarta a
    /// maioria das células sem avaliar as superfícies.
    pub bbox: Aabb,
    pub universe: i32,
    pub uni: usize,
    pub lat: u8,
    pub fill: FillDef,
    pub filled: Filled,
    pub trcl: Option<Transform>,
    pub lattice: Option<Lattice>,
    /// Mundo externo: importância zero no MCNP, material -1 no PHITS.
    pub outside: bool,
    pub line: usize,
    /// Começo do cartão como escrito no input (já sem comentários).
    pub card: String,
    /// Resumo do cartão inteiro: dois inputs com o mesmo valor têm o mesmo cartão.
    pub card_hash: u64,
    /// Importância (a maior, se há uma por partícula); NaN quando não é dada.
    pub imp: f64,
}

impl Cell {
    pub fn new(id: i32, line: usize) -> Self {
        Cell {
            id,
            mat: 0,
            rho: 0.0,
            node: Node::Empty,
            prims: Vec::new(),
            clauses: Vec::new(),
            prim_terms: Vec::new(),
            bbox: Aabb::ALL,
            universe: 0,
            uni: 0,
            lat: 0,
            fill: FillDef::None,
            filled: Filled::None,
            trcl: None,
            lattice: None,
            outside: false,
            line,
            card: String::new(),
            card_hash: 0,
            imp: f64::NAN,
        }
    }

    /// A célula vista como interseção de termos: o ponto está dentro quando
    /// todos valem.
    pub fn terms(&self) -> &[Node] {
        match &self.node {
            Node::And(v) => v,
            n => std::slice::from_ref(n),
        }
    }

    /// Material de densidade muito baixa (ar, por exemplo), que costuma
    /// envolver o modelo e esconder tudo na vista 3D. Densidade negativa é
    /// g/cm³; positiva é átomos/(b·cm).
    pub fn is_gas(&self) -> bool {
        self.mat != 0 && if self.rho < 0.0 { self.rho > -0.01 } else { self.rho < 5e-4 }
    }
}

#[derive(Clone, Debug)]
pub struct SurfaceInfo {
    pub id: i32,
    pub mnemonic: String,
    pub prims: Vec<u32>,
    pub is_macro: bool,
    pub line: usize,
    /// Última linha do cartão (0: veio de um arquivo incluído).
    pub last: usize,
    pub card: String,
}

#[derive(Clone, Debug, Default)]
pub struct Material {
    /// (nuclídeo, fração) como escritos no input.
    pub nuclides: Vec<(String, f64)>,
}

/// Um nível da hierarquia de universos onde o ponto foi localizado.
#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub cell: usize,
    /// Raio no referencial em que as superfícies da célula estão definidas.
    pub o: DVec3,
    pub d: DVec3,
    /// Rotação acumulada desse referencial para o global.
    pub rot: DMat3,
    pub lat_idx: Option<[i32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Found {
    /// Célula folha (a que define o material).
    pub leaf: usize,
    /// Identifica a instância: distingue cópias da mesma célula em lattices.
    pub inst: u64,
    /// Dentro de um lattice de voxels: os elementos não são distinguidos em
    /// `inst`, senão cada voxel ganharia o seu contorno.
    pub voxel: bool,
}

/// Lattices preenchidos com mais elementos que isto são tratados como voxels.
const VOXEL_ELEMENTS: usize = 50_000;

/// Um intervalo de um tally de célula: a célula e, de dentro para fora, as
/// células que a contêm (`702 < 704[3 0 0]`), com o elemento de lattice quando
/// ele é dado. Vazio = total, que não corresponde a lugar nenhum.
pub type TallyBin = Vec<(i32, Option<[i32; 3]>)>;

/// Cartão F de tally de célula, guardado para casar com os resultados do mctal.
#[derive(Clone, Debug)]
pub struct TallyDef {
    pub number: i32,
    /// Texto do cartão FC, se houver.
    pub comment: String,
    /// `None` quando o cartão usa uma forma que o leitor não entende.
    pub bins: Option<Vec<TallyBin>>,
}

/// Duas células que contêm o mesmo ponto.
#[derive(Clone, Debug)]
pub struct Overlap {
    pub cells: (usize, usize),
    /// Um ponto de exemplo, e quantos dos pontos sorteados caíram na sobreposição.
    pub at: DVec3,
    pub count: u64,
}

/// Resultado de [`Model::audit`].
#[derive(Clone, Debug, Default)]
pub struct Audit {
    pub samples: u64,
    /// Volume da caixa sorteada, em cm³.
    pub box_volume: f64,
    /// Pontos que não pertencem a nenhuma célula, com alguns exemplos.
    pub undefined: u64,
    pub undefined_at: Vec<DVec3>,
    pub overlaps: Vec<Overlap>,
    /// Pontos que caíram em cada célula folha (somando todas as cópias dela).
    pub hits: Vec<u64>,
}

impl Audit {
    /// Volume estimado da célula em cm³ e o erro relativo (1σ).
    pub fn volume(&self, cell: usize) -> Option<(f64, f64)> {
        let n = *self.hits.get(cell)?;
        if n == 0 || self.samples == 0 {
            return None;
        }
        let f = n as f64 / self.samples as f64;
        Some((f * self.box_volume, ((1.0 - f) / n as f64).sqrt()))
    }

    fn merge(mut self, o: Audit) -> Audit {
        self.samples += o.samples;
        self.undefined += o.undefined;
        self.undefined_at.extend(o.undefined_at);
        self.undefined_at.truncate(12);
        if self.hits.len() < o.hits.len() {
            self.hits.resize(o.hits.len(), 0);
        }
        for (a, b) in self.hits.iter_mut().zip(&o.hits) {
            *a += b;
        }
        for ov in o.overlaps {
            match self.overlaps.iter_mut().find(|x| x.cells == ov.cells) {
                Some(x) => x.count += ov.count,
                None => self.overlaps.push(ov),
            }
        }
        self
    }
}

/// Posição do elemento `idx` na lista de um FILL em matriz.
fn array_slot(lo: &[i32; 3], dims: &[usize; 3], idx: [i32; 3]) -> Option<usize> {
    let mut k = 0usize;
    for m in (0..3).rev() {
        let r = idx[m] - lo[m];
        if r < 0 || r as usize >= dims[m] {
            return None;
        }
        k = k * dims[m] + r as usize;
    }
    Some(k)
}

/// Memória de trabalho de [`Model::next_boundary`], reaproveitada entre raios.
#[derive(Default)]
pub struct Work {
    signs: Vec<bool>,
    cross: Vec<(f64, u32)>,
    count: Vec<u32>,
}

/// Memória de [`Model::descend`] entre pontos consecutivos: a última célula
/// encontrada em cada profundidade e, para cada célula, as que já foram
/// encontradas logo depois dela (em geral as vizinhas). Evita varrer o
/// universo inteiro toda vez que o ponto muda de célula.
#[derive(Default)]
pub struct Hint {
    last: Vec<usize>,
    near: Vec<Vec<u32>>,
}

fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29)
}

#[derive(Default)]
pub struct Model {
    pub title: String,
    pub prims: Vec<Prim>,
    pub surfaces: Vec<SurfaceInfo>,
    pub surf_index: HashMap<i32, usize>,
    pub cells: Vec<Cell>,
    pub cell_index: HashMap<i32, usize>,
    /// Índices das células de cada universo.
    pub universes: Vec<Vec<usize>>,
    pub uni_ids: Vec<i32>,
    pub root: usize,
    pub materials: BTreeMap<i32, Material>,
    pub warnings: Vec<String>,
    pub tallies: Vec<TallyDef>,
    /// Cartões de dados como escritos (em minúsculas): no MCNP, (nome, resto do
    /// cartão); no PHITS, ("seção/parâmetro", valor). Servem ao diagnóstico.
    pub data: Vec<(String, String)>,
    /// O que a saída conta sobre a rodada, quando o arquivo aberto é uma saída.
    pub run: Vec<crate::diag::Finding>,
    /// Partículas perdidas relatadas num arquivo de saída: (posição, direção).
    pub lost: Vec<(DVec3, DVec3)>,
    /// Transformações TR lidas, e onde está o cartão de cada uma: (número,
    /// primeira linha, última linha).
    pub trs: HashMap<i32, Transform>,
    pub tr_cards: Vec<(i32, usize, usize)>,
    /// O texto que foi lido (num arquivo de saída, o input reconstruído do eco):
    /// é sobre ele que as edições são feitas.
    pub source: String,
    pub phits: bool,
    pub bbox: (DVec3, DVec3),
    /// Para cada universo: a célula, se ele tiver uma só e ela for simples
    /// (sem FILL, lattice ou TRCL). É o caso dos universos de um fantoma
    /// voxelizado, um por material.
    uniform: Vec<Option<u32>>,
}

impl Model {
    pub fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    /// Resolve universos, preenchimentos e lattices depois que o leitor
    /// terminou de montar células e superfícies.
    pub fn finalize(&mut self) {
        let mut uni_map: HashMap<i32, usize> = HashMap::new();
        self.universes.clear();
        self.uni_ids.clear();
        self.cell_index.clear();
        for (ci, c) in self.cells.iter_mut().enumerate() {
            let next = self.uni_ids.len();
            let ui = *uni_map.entry(c.universe).or_insert(next);
            if ui == next {
                self.uni_ids.push(c.universe);
                self.universes.push(Vec::new());
            }
            c.uni = ui;
            self.universes[ui].push(ci);
            self.cell_index.insert(c.id, ci);
            c.node = std::mem::replace(&mut c.node, Node::Empty).normalized(false);
            c.prims.clear();
            c.node.collect_prims(&mut c.prims);
            let clauses: Vec<_> = c.terms().iter().map(Node::as_clause).collect();
            c.prim_terms = c
                .prims
                .iter()
                .map(|&p| {
                    let mut uses = Vec::new();
                    for (k, term) in c.terms().iter().enumerate() {
                        match &clauses[k] {
                            Some(lits) => uses.extend(
                                lits.iter().filter(|l| l.0 == p).map(|l| (k as u32, if l.1 { 1 } else { 2 })),
                            ),
                            None if term.uses(p) => uses.push((k as u32, 0)),
                            None => {}
                        }
                    }
                    uses
                })
                .collect();
            c.clauses = clauses;
            let bx = c.node.bbox(&self.prims, false);
            c.bbox = c.trcl.map_or(bx, |t| bx.transformed(&t)).padded();
        }
        self.root = match uni_map.get(&0) {
            Some(&r) => r,
            None => {
                if !self.cells.is_empty() {
                    self.warnings.push("nenhuma célula no universo 0; usando o primeiro universo como raiz".into());
                }
                0
            }
        };

        let mut warnings = Vec::new();
        for c in self.cells.iter_mut() {
            let (id, own) = (c.id, c.universe);
            let resolve = |u: i32, warnings: &mut Vec<String>| -> Option<usize> {
                let r = uni_map.get(&u).copied();
                if r.is_none() {
                    warnings.push(format!("célula {}: universo {} do FILL não existe", id, u));
                }
                r
            };
            c.filled = match &mut c.fill {
                FillDef::None => Filled::None,
                FillDef::Single { u, tr } => match resolve(*u, &mut warnings) {
                    Some(uni) => Filled::Single { uni, tr: *tr },
                    None => Filled::None,
                },
                FillDef::Array { lo, hi, us } => {
                    let dims = [0, 1, 2].map(|m| (hi[m] - lo[m] + 1).max(0) as usize);
                    // Fantomas voxelizados têm dezenas de milhões de entradas
                    // em longas sequências iguais: lembra a última resolvida e
                    // libera a lista original.
                    let mut last = (own, OWN);
                    let unis = std::mem::take(us)
                        .into_iter()
                        .map(|u| {
                            if u != last.0 {
                                let r = if u == own || u == 0 {
                                    OWN
                                } else {
                                    resolve(u, &mut warnings).map_or(OWN, |x| x as u32)
                                };
                                last = (u, r);
                            }
                            last.1
                        })
                        .collect::<Vec<_>>();
                    if unis.len() != dims[0] * dims[1] * dims[2] {
                        warnings.push(format!(
                            "célula {}: FILL tem {} entradas, esperado {}",
                            c.id,
                            unis.len(),
                            dims[0] * dims[1] * dims[2]
                        ));
                    }
                    Filled::Array { lo: *lo, dims, unis }
                }
            };
            c.lattice = None;
            if c.lat != 0 {
                match build_lattice(c, &self.prims) {
                    Ok(l) => c.lattice = Some(l),
                    Err(e) => warnings.push(format!("célula {}: lattice ignorado ({})", c.id, e)),
                }
            }
        }
        self.warnings.extend(warnings);
        self.uniform = self
            .universes
            .iter()
            .map(|cells| match cells[..] {
                [ci] => {
                    let c = &self.cells[ci];
                    (matches!(c.filled, Filled::None) && c.lattice.is_none() && c.trcl.is_none()).then_some(ci as u32)
                }
                _ => None,
            })
            .collect();
        self.compute_bbox();
    }

    fn compute_bbox(&mut self) {
        let mut hints = Vec::new();
        // Primeiro sem os gases: o ar em volta costuma ser muito maior que o
        // modelo e deixaria a vista inicial distante demais.
        for skip_gas in [true, false] {
            for c in &self.cells {
                if c.outside || (c.mat == 0 && matches!(c.filled, Filled::None)) || (skip_gas && c.is_gas()) {
                    continue;
                }
                // Só o universo raiz está em coordenadas globais.
                if c.uni != self.root || c.trcl.is_some() {
                    continue;
                }
                for &p in &c.prims {
                    self.prims[p as usize].extent_hints(&mut hints);
                }
            }
            if !hints.is_empty() {
                break;
            }
        }
        if hints.is_empty() {
            for p in &self.prims {
                p.extent_hints(&mut hints);
            }
        }
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for (i, v) in hints {
            if v.is_finite() {
                lo[i] = lo[i].min(v);
                hi[i] = hi[i].max(v);
            }
        }
        let size = (0..3).map(|i| hi[i] - lo[i]).filter(|s| s.is_finite()).fold(0.0, f64::max);
        let size = if size > 0.0 { size } else { 20.0 };
        for i in 0..3 {
            if !lo[i].is_finite() || !hi[i].is_finite() {
                lo[i] = -size / 2.0;
                hi[i] = size / 2.0;
            } else if hi[i] - lo[i] < 1e-9 * size {
                lo[i] -= size / 2.0;
                hi[i] += size / 2.0;
            }
        }
        self.bbox = (lo, hi);
    }

    /// Aperta a caixa envolvente estimada em torno do que é sólido de fato
    /// (nem vazio, nem gás), sondando a geometria numa grade. A estimativa
    /// pelas superfícies inclui planos distantes e o ar em volta do modelo.
    pub fn refine_bbox(&mut self) {
        use rayon::prelude::*;
        const N: usize = 64;
        let (lo, hi) = self.bbox;
        let step = (hi - lo) / N as f64;
        let this = &*self;
        let empty = ([N; 3], [0usize; 3]);
        let merge = |a: ([usize; 3], [usize; 3]), b: ([usize; 3], [usize; 3])| {
            ([0, 1, 2].map(|m| a.0[m].min(b.0[m])), [0, 1, 2].map(|m| a.1[m].max(b.1[m])))
        };
        let (imin, imax) = (0..N)
            .into_par_iter()
            .map(|k| {
                let (mut hint, mut levels) = (Hint::default(), Vec::new());
                let mut acc = empty;
                for j in 0..N {
                    for i in 0..N {
                        let p = lo + step * DVec3::new(i as f64 + 0.5, j as f64 + 0.5, k as f64 + 0.5);
                        let solid = this.descend(p, DVec3::X, &mut hint, &mut levels).is_some_and(|f| {
                            let c = &this.cells[f.leaf];
                            !c.outside && c.mat != 0 && !c.is_gas()
                        });
                        if solid {
                            acc = merge(acc, ([i, j, k], [i + 1, j + 1, k + 1]));
                        }
                    }
                }
                acc
            })
            .reduce(|| empty, merge);
        if (0..3).all(|m| imax[m] > imin[m]) {
            // Folga de um passo: a grade pode ter pulado as bordas.
            let cell = |v: [usize; 3], pad: f64| DVec3::new(v[0] as f64 + pad, v[1] as f64 + pad, v[2] as f64 + pad);
            self.bbox = (lo + step * cell(imin, -1.0), lo + step * cell(imax, 1.0));
        }
    }

    /// Localiza `p` descendo pelos universos. `levels` recebe o caminho, com o
    /// raio `(p, d)` expresso no referencial de cada nível; `hint` aproveita a
    /// coerência entre pontos vizinhos.
    pub fn descend(&self, p: DVec3, d: DVec3, hint: &mut Hint, levels: &mut Vec<Level>) -> Option<Found> {
        levels.clear();
        if self.universes.is_empty() {
            return None;
        }
        let (mut p, mut d, mut rot, mut ui) = (p, d, DMat3::IDENTITY, self.root);
        let mut inst = 0u64;
        let mut voxel = false;
        for depth in 0..32 {
            let inside = |ci: usize| {
                let c = &self.cells[ci];
                // A célula de lattice ocupa todo o seu universo.
                c.lattice.is_some()
                    || (c.bbox.contains(p) && c.node.contains(&self.prims, c.trcl.map_or(p, |t| t.to_local(p))))
            };
            let scan = || self.universes[ui].iter().copied().find(|&ci| inside(ci));
            let ci = match hint.last.get(depth).copied().filter(|&ci| self.cells[ci].uni == ui) {
                Some(prev) if inside(prev) => prev,
                Some(prev) => {
                    if hint.near.len() < self.cells.len() {
                        hint.near.resize(self.cells.len(), Vec::new());
                    }
                    let near = &mut hint.near[prev];
                    match near.iter().position(|&n| inside(n as usize)) {
                        Some(k) => {
                            // A mais recente vai para a frente da fila.
                            near[..=k].rotate_right(1);
                            near[0] as usize
                        }
                        None => {
                            let ci = scan()?;
                            near.insert(0, ci as u32);
                            ci
                        }
                    }
                }
                None => scan()?,
            };
            if depth < hint.last.len() {
                hint.last[depth] = ci;
            } else {
                hint.last.push(ci);
            }
            let c = &self.cells[ci];
            inst = mix(inst, ci as u64 + 1);
            if let Some(t) = &c.trcl {
                p = t.to_local(p);
                d = t.dir_to_local(d);
                rot *= t.rot;
            }
            let mut lat_idx = None;
            if let Some(lat) = &c.lattice {
                let (idx, off) = lat.index(p);
                p -= off;
                lat_idx = Some(idx);
                if matches!(&c.filled, Filled::Array { unis, .. } if unis.len() > VOXEL_ELEMENTS) {
                    voxel = true;
                } else {
                    for v in idx {
                        inst = mix(inst, v as u32 as u64);
                    }
                }
            }
            levels.push(Level { cell: ci, o: p, d, rot, lat_idx });
            let found = Found { leaf: ci, inst, voxel };
            match &c.filled {
                Filled::None => return Some(found),
                Filled::Single { uni, tr } => {
                    if let Some(t) = tr {
                        p = t.to_local(p);
                        d = t.dir_to_local(d);
                        rot *= t.rot;
                    }
                    ui = *uni;
                }
                Filled::Array { lo, dims, unis } => {
                    let idx = lat_idx.unwrap_or([0; 3]);
                    let mut k = 0usize;
                    for m in (0..3).rev() {
                        let r = idx[m] - lo[m];
                        if r < 0 || r as usize >= dims[m] {
                            return None;
                        }
                        k = k * dims[m] + r as usize;
                    }
                    match unis.get(k) {
                        None => return None,
                        Some(&OWN) => return Some(found),
                        Some(&u) => ui = u as usize,
                    }
                }
            }
        }
        None
    }

    /// Como [`Model::next_boundary`], para um raio dentro de um voxel que não
    /// é desenhado (`visible` por célula folha): atravessa de uma vez todos os
    /// voxels invisíveis seguintes, andando pela grade do lattice sem
    /// relocalizar o ponto a cada um. Para na entrada do primeiro voxel
    /// visível (ou que não seja de um universo simples), ou na saída da célula
    /// que contém o lattice.
    pub fn voxel_skip(&self, levels: &[Level], visible: &[bool], work: &mut Work) -> Option<(f64, DVec3)> {
        let grid = levels.len().checked_sub(2).and_then(|li| {
            let l = &levels[li];
            let c = &self.cells[l.cell];
            match (&c.lattice, &c.filled, l.lat_idx) {
                (Some(lat), Filled::Array { lo, dims, unis }, Some(idx)) if !lat.hex => Some((li, lat, lo, dims, unis, idx)),
                _ => None,
            }
        });
        let Some((li, lat, lo, dims, unis, mut idx)) = grid else {
            return self.next_boundary(levels, work);
        };
        let l = &levels[li];
        // Saída das células acima do lattice (ele próprio ocupa todo o universo).
        let limit = self.next_boundary(&levels[..li], work);
        let t_limit = limit.map_or(f64::INFINITY, |b| b.0);
        // No referencial do nível o voxel atual é o elemento [0 0 0].
        let (mut t_next, mut t_step, mut dir) = ([f64::INFINITY; 3], [0.0; 3], [0i32; 3]);
        for m in 0..3 {
            let (s, ds) = (lat.n[m].dot(l.o), lat.n[m].dot(l.d));
            if !lat.active[m] || ds.abs() < 1e-14 {
                continue;
            }
            dir[m] = if ds > 0.0 { 1 } else { -1 };
            let edge = if ds > 0.0 { lat.low[m] + lat.pitch[m] } else { lat.low[m] };
            t_next[m] = ((edge - s) / ds).max(0.0);
            t_step[m] = lat.pitch[m] / ds.abs();
        }
        for _ in 0..=dims[0] + dims[1] + dims[2] {
            let m = (0..3).min_by(|&a, &b| t_next[a].total_cmp(&t_next[b])).unwrap();
            let t = t_next[m];
            if t >= t_limit {
                return limit;
            }
            idx[m] += dir[m];
            t_next[m] += t_step[m];
            let mut k = 0usize;
            let mut inside = true;
            for a in (0..3).rev() {
                let r = idx[a] - lo[a];
                inside &= r >= 0 && (r as usize) < dims[a];
                k = k.wrapping_mul(dims[a]).wrapping_add(r as usize);
            }
            let leaf = match unis.get(k).filter(|_| inside) {
                None => None,
                Some(&OWN) => Some(l.cell),
                Some(&u) => self.uniform[u as usize].map(|c| c as usize),
            };
            if !leaf.is_some_and(|c| !visible[c]) {
                return Some((t, l.rot * lat.n[m]));
            }
        }
        limit
    }

    /// Centro, em coordenadas globais, do elemento `idx` do lattice da célula
    /// `lat`. Sobe pelos universos seguindo as células que os usam em FILL, então
    /// só vale para lattices retangulares usados num único lugar.
    pub fn element_center(&self, lat: usize, idx: [i32; 3]) -> Option<DVec3> {
        let mut cell = &self.cells[lat];
        let l = cell.lattice.as_ref().filter(|l| !l.hex)?;
        let mut p = DVec3::ZERO;
        for m in 0..3 {
            if l.active[m] {
                p += l.n[m] * (l.low[m] + (idx[m] as f64 + 0.5) * l.pitch[m]);
            }
        }
        for _ in 0..32 {
            if let Some(t) = &cell.trcl {
                p = t.rot * p + t.off;
            }
            if cell.uni == self.root {
                return Some(p);
            }
            let parent = self.cells.iter().find(|c| matches!(&c.filled, Filled::Single { uni, .. } if *uni == cell.uni))?;
            if let Filled::Single { tr: Some(t), .. } = &parent.filled {
                p = t.rot * p + t.off;
            }
            cell = parent;
        }
        None
    }

    /// Duas células do mesmo universo que contêm `p`, se houver: é o erro de
    /// geometria que o MCNP só acusa quando uma partícula se perde ali. Varre
    /// o universo inteiro em cada nível, então é bem mais caro que
    /// [`Model::descend`].
    pub fn overlap_at(&self, p: DVec3) -> Option<(usize, usize)> {
        let (mut p, mut ui) = (p, self.root);
        for _ in 0..32 {
            let (first, second) = {
                let inside = |ci: &usize| {
                    let c = &self.cells[*ci];
                    c.lattice.is_some()
                        || (c.bbox.contains(p) && c.node.contains(&self.prims, c.trcl.map_or(p, |t| t.to_local(p))))
                };
                let mut hits = self.universes.get(ui)?.iter().copied().filter(inside);
                (hits.next(), hits.next())
            };
            let ci = first?;
            if let Some(cj) = second {
                return Some((ci, cj));
            }
            let c = &self.cells[ci];
            if let Some(t) = &c.trcl {
                p = t.to_local(p);
            }
            let mut idx = [0; 3];
            if let Some(lat) = &c.lattice {
                let (i, off) = lat.index(p);
                p -= off;
                idx = i;
            }
            match &c.filled {
                Filled::None => return None,
                Filled::Single { uni, tr } => {
                    if let Some(t) = tr {
                        p = t.to_local(p);
                    }
                    ui = *uni;
                }
                Filled::Array { lo, dims, unis } => match unis.get(array_slot(lo, dims, idx)?) {
                    None | Some(&OWN) => return None,
                    Some(&u) => ui = u as usize,
                },
            }
        }
        None
    }

    /// Sorteia `samples` pontos na caixa envolvente e relata sobreposições,
    /// regiões sem célula e quantos pontos caíram em cada célula (de onde sai
    /// o volume).
    pub fn audit(&self, samples: u64) -> Audit {
        use rayon::prelude::*;
        const CHUNK: u64 = 20_000;
        let (lo, hi) = self.bbox;
        let size = hi - lo;
        let mut out = (0..samples.div_ceil(CHUNK))
            .into_par_iter()
            .map(|chunk| {
                let mut seed = (chunk + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                let mut rnd = move || {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 11) as f64 / (1u64 << 53) as f64
                };
                let (mut hint, mut levels) = (Hint::default(), Vec::new());
                let mut a = Audit { samples: CHUNK, hits: vec![0; self.cells.len()], ..Audit::default() };
                for _ in 0..CHUNK {
                    let p = lo + size * DVec3::new(rnd(), rnd(), rnd());
                    match self.descend(p, DVec3::X, &mut hint, &mut levels) {
                        Some(f) => a.hits[f.leaf] += 1,
                        None => {
                            a.undefined += 1;
                            if a.undefined_at.len() < 4 {
                                a.undefined_at.push(p);
                            }
                        }
                    }
                    if let Some(cells) = self.overlap_at(p) {
                        match a.overlaps.iter_mut().find(|x| x.cells == cells) {
                            Some(x) => x.count += 1,
                            None => a.overlaps.push(Overlap { cells, at: p, count: 1 }),
                        }
                    }
                }
                a
            })
            .reduce(Audit::default, Audit::merge);
        out.box_volume = size.x * size.y * size.z;
        out.overlaps.sort_by(|a, b| b.count.cmp(&a.count));
        out
    }

    /// Um ponto dentro da célula folha `leaf` (perto do meio dela), para
    /// levar a vista até lá. Procura numa grade, então pode não achar células
    /// muito pequenas.
    pub fn point_in(&self, leaf: usize) -> Option<DVec3> {
        const N: usize = 40;
        let c = &self.cells[leaf];
        let own = c.uni == self.root && c.bbox.lo.is_finite() && c.bbox.hi.is_finite();
        let (lo, hi) = if own { (c.bbox.lo, c.bbox.hi) } else { self.bbox };
        let step = (hi - lo) / N as f64;
        let (mut hint, mut levels) = (Hint::default(), Vec::new());
        let mut found = Vec::new();
        for k in 0..N {
            for j in 0..N {
                for i in 0..N {
                    let p = lo + step * DVec3::new(i as f64 + 0.5, j as f64 + 0.5, k as f64 + 0.5);
                    if self.descend(p, DVec3::X, &mut hint, &mut levels).is_some_and(|f| f.leaf == leaf) {
                        found.push(p);
                    }
                }
            }
        }
        let mean = found.iter().sum::<DVec3>() / found.len().max(1) as f64;
        found.into_iter().min_by(|a, b| a.distance_squared(mean).total_cmp(&b.distance_squared(mean)))
    }

    /// Superfície do input mais próxima do ponto localizado em `levels`, entre
    /// as que delimitam as células onde ele está, e a distância aproximada.
    pub fn nearest_surface(&self, levels: &[Level]) -> Option<(i32, f64)> {
        let mut near: Vec<(u32, f64)> = levels
            .iter()
            .flat_map(|l| self.cells[l.cell].prims.iter().map(|&pi| (pi, self.prims[pi as usize].distance(l.o))))
            .filter(|c| c.1.is_finite())
            .collect();
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        // Cópias transformadas (do complemento de células com TRCL) não têm número.
        near.iter().find_map(|&(pi, d)| Some((self.surfaces.iter().find(|s| s.prims.contains(&pi))?.id, d)))
    }

    pub fn locate(&self, p: DVec3) -> Option<(Found, Vec<Level>)> {
        let mut levels = Vec::new();
        self.descend(p, DVec3::X, &mut Hint::default(), &mut levels).map(|f| (f, levels))
    }

    /// Distância até a próxima fronteira ao longo do raio já localizado em
    /// `levels`, com a normal global da superfície atingida.
    ///
    /// Uma célula grande (o ar em volta de tudo, por exemplo) cita centenas de
    /// superfícies, e o raio cruza quase todas sem sair dela. Em vez de parar em
    /// cada uma, acompanha o sentido das superfícies ao longo do raio e só
    /// reavalia os termos da célula que dependem da superfície cruzada.
    pub fn next_boundary(&self, levels: &[Level], work: &mut Work) -> Option<(f64, DVec3)> {
        let mut best: Option<(f64, u32, usize)> = None;
        work.signs.resize(self.prims.len(), false);
        for (li, l) in levels.iter().enumerate() {
            let c = &self.cells[l.cell];
            let tmax = best.map_or(f64::INFINITY, |b| b.0);
            work.cross.clear();
            for (j, &pi) in c.prims.iter().enumerate() {
                let prim = &self.prims[pi as usize];
                work.signs[pi as usize] = prim.eval(l.o) > 0.0;
                prim.crossings(l.o, l.d, tmax, |t| work.cross.push((t, j as u32)));
            }
            if work.cross.is_empty() {
                continue;
            }
            work.cross.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            let terms = c.terms();
            // Semi-espaços que valem em cada cláusula (ou 0/1 nos demais termos).
            work.count.clear();
            work.count.extend(terms.iter().zip(&c.clauses).map(|(term, clause)| match clause {
                Some(lits) => lits.iter().filter(|l| work.signs[l.0 as usize] == l.1).count() as u32,
                None => term.eval_signs(&work.signs) as u32,
            }));
            // Ponto de partida na beirada (erro numérico): a primeira
            // superfície cruzada já conta como saída.
            let inside = work.count.iter().all(|&n| n > 0);
            for &(t, j) in &work.cross {
                let pi = c.prims[j as usize];
                let sign = !work.signs[pi as usize];
                work.signs[pi as usize] = sign;
                let mut out = !inside;
                for &(k, kind) in &c.prim_terms[j as usize] {
                    let n = &mut work.count[k as usize];
                    if kind == 0 {
                        out |= !terms[k as usize].eval_signs(&work.signs);
                    } else if sign == (kind == 1) {
                        *n += 1;
                    } else {
                        *n = n.saturating_sub(1);
                        out |= *n == 0;
                    }
                }
                if out {
                    best = Some((t, pi, li));
                    break;
                }
            }
        }
        best.map(|(t, pi, li)| {
            let l = &levels[li];
            (t, l.rot * self.prims[pi as usize].normal(l.o + l.d * t))
        })
    }

    /// Descrição legível do caminho de universos de um ponto.
    pub fn describe(&self, levels: &[Level]) -> String {
        levels
            .iter()
            .map(|l| {
                let c = &self.cells[l.cell];
                match l.lat_idx {
                    Some([i, j, k]) => format!("{}[{} {} {}]", c.id, i, j, k),
                    None => c.id.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(" > ")
    }
}

/// Deduz os vetores do lattice a partir dos planos da célula, na ordem em que
/// foram escritos (essa ordem define os índices no MCNP e no PHITS).
fn build_lattice(c: &Cell, prims: &[Prim]) -> Result<Lattice, String> {
    let mut halves = Vec::new();
    if !c.node.flatten_and(&mut halves) {
        return Err("a célula não é uma interseção simples de superfícies".into());
    }
    let mut planes = Vec::new();
    for (pi, _) in &halves {
        match &prims[*pi as usize] {
            Prim::Plane { n, d } => planes.push((*n / n.length(), d / n.length())),
            _ => return Err("só planos são aceitos em lattices".into()),
        }
    }
    let hex = c.lat == 2;
    let expected: &[usize] = if hex { &[6, 8] } else { &[2, 4, 6] };
    if !expected.contains(&planes.len()) {
        return Err(format!("{} planos; esperado {:?}", planes.len(), expected));
    }
    // (normal apontando da 2ª para a 1ª superfície, cota inferior, passo)
    let pair = |a: usize, b: usize| -> Result<(DVec3, f64, f64), String> {
        let ((na, da), (nb, db)) = (planes[a], planes[b]);
        let cos = na.dot(nb);
        if cos.abs() < 0.999_999 {
            return Err("par de planos não paralelos".into());
        }
        let db = db * cos.signum();
        if (da - db).abs() < 1e-12 {
            return Err("planos coincidentes".into());
        }
        Ok(if da > db { (na, db, da - db) } else { (-na, -db, db - da) })
    };

    let mut n = [DVec3::ZERO; 3];
    let (mut low, mut pitch, mut active) = ([0.0; 3], [1.0; 3], [false; 3]);
    let pairs: Vec<(usize, usize, usize)> = if hex {
        let mut v = vec![(0, 0, 1), (1, 2, 3)];
        if planes.len() == 8 {
            v.push((2, 6, 7));
        }
        v
    } else {
        (0..planes.len() / 2).map(|m| (m, 2 * m, 2 * m + 1)).collect()
    };
    for (m, a, b) in pairs {
        (n[m], low[m], pitch[m]) = pair(a, b)?;
        active[m] = true;
    }
    // Completa a base nas direções em que o lattice é infinito.
    if !active[1] {
        n[1] = n[0].any_orthonormal_vector();
    }
    if !active[2] {
        n[2] = n[0].cross(n[1]).normalize();
    }
    let rows = DMat3::from_cols(n[0], n[1], n[2]).transpose();
    if rows.determinant().abs() < 1e-9 {
        return Err("planos do lattice são degenerados".into());
    }
    let rinv = rows.inverse();
    let mut vec = [DVec3::ZERO; 3];
    let (mut center, mut inv) = (DVec3::ZERO, DMat3::IDENTITY);
    if hex {
        vec[0] = n[0] * pitch[0];
        vec[1] = n[1] * pitch[1];
        if active[2] {
            vec[2] = n[2] * pitch[2];
        }
        center = rinv * DVec3::new(low[0] + pitch[0] / 2.0, low[1] + pitch[1] / 2.0, 0.0);
        inv = DMat3::from_cols(vec[0], vec[1], n[2]).inverse();
    } else {
        for m in 0..3 {
            if active[m] {
                vec[m] = rinv.col(m) * pitch[m];
            }
        }
    }
    Ok(Lattice { hex, n, low, pitch, active, vec, center, inv })
}
