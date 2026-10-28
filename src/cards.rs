//! Interpretação dos cartões de célula, superfície, transformação e material.
//!
//! MCNP e PHITS usam praticamente a mesma sintaxe para esses cartões; os
//! leitores de cada formato só separam o arquivo em cartões e os entregam ao
//! [`Builder`], que monta o [`Model`].

use crate::geom::{Cell, FillDef, Material, Model, Node, Prim, Quadric, SurfaceInfo, Transform};
use glam::{DMat3, DVec3};
use std::collections::HashMap;

/// Um cartão lógico: continuações já unidas, comentários removidos, minúsculas.
#[derive(Clone, Debug)]
pub struct Card {
    pub text: String,
    pub line: usize,
    /// Última linha do cartão no arquivo; 0 quando ele veio de um arquivo
    /// incluído (e por isso não pode ser reescrito no principal).
    pub last: usize,
}

/// Texto do cartão para mostrar na interface. O cartão do lattice de um
/// fantoma voxelizado tem megabytes; só o começo interessa.
fn card_excerpt(text: &str) -> String {
    match text.char_indices().nth(600) {
        Some((i, _)) => format!("{} …", &text[..i]),
        None => text.to_string(),
    }
}

#[derive(Clone, Debug)]
enum Ast {
    Surf { id: i32, facet: u32, pos: bool },
    CellRef(i32),
    And(Vec<Ast>),
    Or(Vec<Ast>),
    Not(Box<Ast>),
}

struct Param {
    key: String,
    star: bool,
    values: Vec<String>,
}

fn card_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.split_whitespace().for_each(|tok| tok.hash(&mut h));
    h.finish()
}

struct RawCell {
    card: String,
    hash: u64,
    id: i32,
    line: usize,
    like: Option<i32>,
    mat: i32,
    rho: f64,
    ast: Option<Ast>,
    params: Vec<Param>,
}

#[derive(Default)]
pub struct Builder {
    pub model: Model,
    /// Variáveis disponíveis nas expressões numéricas (PHITS: c1..c99).
    pub vars: HashMap<String, f64>,
    /// No PHITS o material -1 marca o vazio externo.
    pub phits: bool,
    trs: HashMap<i32, Transform>,
    raw_cells: Vec<RawCell>,
    data_params: Vec<(String, Vec<String>, usize)>,
}

pub fn plain_number(tok: &str) -> Option<f64> {
    if let Ok(v) = tok.parse::<f64>() {
        return Some(v);
    }
    // Notação Fortran: 1.5d3 e 1.5+3.
    let t = tok.replace('d', "e");
    if let Ok(v) = t.parse::<f64>() {
        return Some(v);
    }
    let pos = t.char_indices().skip(1).find(|&(i, ch)| (ch == '+' || ch == '-') && !t[..i].ends_with('e'))?.0;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '+' || ch == '-');
    if !digits(&t[..pos]) || !digits(&t[pos..]) {
        return None;
    }
    format!("{}e{}", &t[..pos], &t[pos..]).parse().ok()
}

impl Builder {
    fn warn(&mut self, line: usize, msg: impl AsRef<str>) {
        self.model.warn(format!("linha {}: {}", line, msg.as_ref()));
    }

    pub fn num(&self, tok: &str) -> Option<f64> {
        plain_number(tok).or_else(|| crate::expr::eval(tok, &self.vars))
    }

    /// Lista numérica com os atalhos nR, nI, nM e nJ.
    fn numbers<S: AsRef<str>>(&self, toks: &[S]) -> Result<Vec<f64>, String> {
        let mut out: Vec<f64> = Vec::new();
        for (i, tok) in toks.iter().enumerate() {
            let tok = tok.as_ref();
            if let Some(v) = plain_number(tok) {
                out.push(v);
                continue;
            }
            if !tok.is_ascii() {
                return Err(format!("valor numérico inválido: '{}'", tok));
            }
            let (head, kind) = tok.split_at(tok.len() - 1);
            let count = || if head.is_empty() { Some(1usize) } else { head.parse::<usize>().ok() };
            let last = out.last().copied();
            match (kind, last) {
                ("r", Some(l)) if count().is_some() => out.extend(std::iter::repeat_n(l, count().unwrap())),
                ("j", _) if count().is_some() => out.extend(std::iter::repeat_n(0.0, count().unwrap())),
                ("m", Some(l)) if plain_number(head).is_some() => out.push(l * plain_number(head).unwrap()),
                ("i", Some(l)) if count().is_some() => {
                    let n = count().unwrap();
                    let next = toks.get(i + 1).and_then(|t| self.num(t.as_ref())).ok_or("atalho I sem valor seguinte")?;
                    out.extend((1..=n).map(|k| l + (next - l) * k as f64 / (n + 1) as f64));
                }
                _ => out.push(self.num(tok).ok_or_else(|| format!("valor numérico inválido: '{}'", tok))?),
            }
        }
        Ok(out)
    }

    fn make_tr(&self, v: &[f64], degrees: bool) -> Result<Transform, String> {
        let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
        let off = DVec3::new(at(0), at(1), at(2));
        let nb = v.len().saturating_sub(3).min(9);
        let b: Vec<f64> = (0..nb).map(|i| if degrees { at(3 + i).to_radians().cos() } else { at(3 + i) }).collect();
        let col = |k: usize| DVec3::new(b[3 * k], b[3 * k + 1], b[3 * k + 2]);
        // Colunas = eixos x', y', z' do sistema auxiliar escritos no principal.
        let rot = match nb {
            0 => DMat3::IDENTITY,
            3 => {
                let x = col(0).normalize();
                let y = x.any_orthonormal_vector();
                DMat3::from_cols(x, y, x.cross(y))
            }
            6 => DMat3::from_cols(col(0), col(1), col(0).cross(col(1))),
            9 => DMat3::from_cols(col(0), col(1), col(2)),
            n => return Err(format!("matriz de rotação com {} elementos não é suportada", n)),
        };
        // M = -1: o deslocamento é a origem do sistema principal vista do auxiliar.
        let off = if v.len() > 12 && v[12] < 0.0 { -(rot * off) } else { off };
        Ok(Transform { rot, off })
    }

    /// Conteúdo de `(...)` em FILL/TRCL: número de um TR ou transformação explícita.
    fn inline_tr(&self, text: &str, star: bool) -> Result<Transform, String> {
        let inner = text.trim_start_matches('(').trim_end_matches(')');
        let toks: Vec<&str> = inner.split_whitespace().collect();
        let v = self.numbers(&toks)?;
        if v.len() == 1 {
            let n = v[0] as i32;
            return self.trs.get(&n).copied().ok_or_else(|| format!("transformação TR{} não definida", n));
        }
        self.make_tr(&v, star)
    }

    /// Cartão `TRn` / `*TRn`.
    pub fn add_transform(&mut self, card: &Card) {
        let toks: Vec<&str> = card.text.split_whitespace().collect();
        let star = toks[0].starts_with('*');
        let name = toks[0].trim_start_matches('*');
        let parsed = name[2..].parse::<i32>().map_err(|_| "número de TR inválido".to_string()).and_then(|n| {
            let v = self.numbers(&toks[1..])?;
            Ok((n, self.make_tr(&v, star)?))
        });
        match parsed {
            Ok((n, tr)) => {
                self.trs.insert(n, tr);
                self.model.tr_cards.push((n, card.line, card.last));
            }
            Err(e) => self.warn(card.line, format!("{}: {}", toks[0], e)),
        }
    }

    pub fn add_material(&mut self, card: &Card) {
        let toks: Vec<&str> = card.text.split_whitespace().collect();
        let Some(id) = toks[0].trim_start_matches("mat[").trim_start_matches('m').trim_end_matches(']').parse::<i32>().ok() else {
            return;
        };
        let mut mat = Material::default();
        for pair in toks[1..].chunks(2) {
            if pair.len() < 2 || pair[0].contains('=') {
                break;
            }
            let Some(frac) = self.num(pair[1]) else { break };
            mat.nuclides.push((pair[0].to_string(), frac));
        }
        self.model.materials.insert(id, mat);
    }

    /// Cartão de dados que vale para as células em ordem (`imp:n 1 1 0`, `u`, `lat`, `fill`).
    pub fn add_cell_data(&mut self, card: &Card) {
        let mut toks = card.text.replace('=', " ").split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let key = toks.remove(0);
        let key = key.split(':').next().unwrap_or("").to_string();
        self.data_params.push((key, toks, card.line));
    }

    pub fn add_surface(&mut self, card: &Card) {
        if let Err(e) = self.try_surface(card) {
            self.warn(card.line, format!("superfície '{}': {}", card.text.split_whitespace().next().unwrap_or(""), e));
        }
    }

    fn try_surface(&mut self, card: &Card) -> Result<(), String> {
        let toks: Vec<&str> = card.text.split_whitespace().collect();
        let id: i32 = toks[0].trim_start_matches(['*', '+']).parse().map_err(|_| "número inválido")?;
        let mut k = 1;
        let mut tr = None;
        if let Some(n) = toks.get(1).and_then(|t| t.parse::<i32>().ok()) {
            k = 2;
            // n negativo indica superfície periódica, sem efeito no desenho.
            if n > 0 {
                tr = Some(*self.trs.get(&n).ok_or_else(|| format!("transformação TR{} não definida", n))?);
            }
        }
        let mn = *toks.get(k).ok_or("falta o tipo da superfície")?;
        let v = self.numbers(&toks[k + 1..])?;
        let (mut prims, is_macro) = build_surface(mn, &v)?;
        if let Some(t) = tr {
            prims = prims.iter().map(|p| p.transformed(&t)).collect();
        }
        if self.model.surf_index.contains_key(&id) {
            return Err("número repetido".into());
        }
        let base = self.model.prims.len() as u32;
        let n = prims.len() as u32;
        self.model.prims.extend(prims);
        self.model.surf_index.insert(id, self.model.surfaces.len());
        self.model.surfaces.push(SurfaceInfo {
            id,
            mnemonic: mn.to_string(),
            prims: (base..base + n).collect(),
            is_macro,
            line: card.line,
            last: card.last,
            card: card_excerpt(card.text.trim()),
        });
        Ok(())
    }

    pub fn add_cell(&mut self, card: &Card) {
        if let Err(e) = self.try_cell(card) {
            self.warn(card.line, format!("célula '{}': {}", card.text.split_whitespace().next().unwrap_or(""), e));
        }
    }

    fn try_cell(&mut self, card: &Card) -> Result<(), String> {
        let text = card.text.trim();
        let (tok, rest) = next_token(text);
        let id: i32 = tok.parse().map_err(|_| "número de célula inválido")?;
        let (tok, mut rest) = next_token(rest);
        let mut raw = RawCell {
            id,
            line: card.line,
            card: card_excerpt(text),
            hash: card_hash(text),
            like: None,
            mat: 0,
            rho: 0.0,
            ast: None,
            params: Vec::new(),
        };
        if tok == "like" {
            let (n, r) = next_token(rest);
            let (but, r) = next_token(r);
            if but != "but" {
                return Err("esperado LIKE n BUT".into());
            }
            raw.like = Some(n.parse().map_err(|_| "célula de referência inválida")?);
            raw.params = parse_params(r);
        } else {
            raw.mat = tok.parse().map_err(|_| "número de material inválido")?;
            if raw.mat > 0 {
                let (d, r) = next_token(rest);
                raw.rho = self.num(d).ok_or("densidade inválida")?;
                rest = r;
            }
            // A geometria nunca contém letras; a primeira marca o início dos parâmetros.
            let split = rest
                .char_indices()
                .find(|&(_, ch)| ch.is_ascii_alphabetic() || ch == '*')
                .map_or(rest.len(), |(i, _)| i);
            raw.ast = Some(GeomParser { s: rest[..split].as_bytes(), i: 0 }.parse()?);
            raw.params = parse_params(&rest[split..]);
        }
        self.raw_cells.push(raw);
        Ok(())
    }

    pub fn finish(mut self) -> Model {
        let index: HashMap<i32, usize> = self.raw_cells.iter().enumerate().map(|(i, r)| (r.id, i)).collect();

        // LIKE n BUT: herda da célula de referência (que pode ser outro LIKE).
        for _ in 0..8 {
            let mut pending = false;
            for i in 0..self.raw_cells.len() {
                let Some(n) = self.raw_cells[i].like else { continue };
                let Some(&bi) = index.get(&n) else {
                    let (line, id) = (self.raw_cells[i].line, self.raw_cells[i].id);
                    self.warn(line, format!("célula {}: LIKE {} não existe", id, n));
                    self.raw_cells[i].like = None;
                    continue;
                };
                if self.raw_cells[bi].like.is_some() {
                    pending = true;
                    continue;
                }
                let base = &self.raw_cells[bi];
                let (mat, rho, ast) = (base.mat, base.rho, base.ast.clone());
                let mut params: Vec<Param> = base
                    .params
                    .iter()
                    .map(|p| Param { key: p.key.clone(), star: p.star, values: p.values.clone() })
                    .collect();
                let own = std::mem::take(&mut self.raw_cells[i].params);
                for p in own {
                    params.retain(|q| q.key != p.key);
                    params.push(p);
                }
                let r = &mut self.raw_cells[i];
                (r.mat, r.rho, r.ast, r.params, r.like) = (mat, rho, ast, params, None);
            }
            if !pending {
                break;
            }
        }

        // Cartões de dados posicionais viram parâmetros das células.
        for (key, toks, line) in std::mem::take(&mut self.data_params) {
            let values: Vec<String> = match self.numbers(&toks) {
                Ok(v) => v.iter().map(|x| x.to_string()).collect(),
                Err(e) => {
                    self.warn(line, format!("cartão {}: {}", key, e));
                    continue;
                }
            };
            for (r, v) in self.raw_cells.iter_mut().zip(values) {
                // No cartão de dados, 0 em U/LAT/FILL significa "sem valor".
                if key != "imp" && v == "0" {
                    continue;
                }
                if !r.params.iter().any(|p| p.key == key) {
                    r.params.push(Param { key: key.clone(), star: false, values: vec![v] });
                }
            }
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut cells = Vec::with_capacity(self.raw_cells.len());
        for raw in &self.raw_cells {
            let mut c = Cell::new(raw.id, raw.line);
            c.card = raw.card.clone();
            c.card_hash = raw.hash;
            (c.mat, c.rho) = (raw.mat, raw.rho);
            if self.phits && c.mat < 0 {
                c.mat = 0;
                c.outside = true;
            }
            let mut imps: Vec<f64> = Vec::new();
            for p in &raw.params {
                let first = p.values.first().map(String::as_str).unwrap_or("");
                let int = |s: &str| self.num(s).map(|v| v as i32);
                let res: Result<(), String> = (|| {
                    match p.key.as_str() {
                        "u" => c.universe = int(first).ok_or("U inválido")?.abs(),
                        "lat" => c.lat = int(first).ok_or("LAT inválido")? as u8,
                        "mat" => c.mat = int(first).ok_or("MAT inválido")?,
                        "rho" => c.rho = self.num(first).ok_or("RHO inválido")?,
                        "imp" => imps.extend(p.values.iter().filter_map(|v| self.num(v))),
                        "trcl" => {
                            c.trcl = Some(self.inline_tr(&p.values.join(" "), p.star)?);
                        }
                        "fill" => c.fill = self.parse_fill(p)?,
                        _ => {}
                    }
                    Ok(())
                })();
                if let Err(e) = res {
                    warnings.push(format!("linha {}: célula {}: {}", raw.line, raw.id, e));
                }
            }
            c.imp = imps.iter().copied().fold(f64::NAN, f64::max);
            if !imps.is_empty() && imps.iter().all(|&v| v == 0.0) {
                c.outside = true;
            }
            if c.lat != 0 && c.lat != 1 && c.lat != 2 {
                warnings.push(format!("linha {}: célula {}: LAT={} desconhecido", raw.line, raw.id, c.lat));
                c.lat = 0;
            }
            cells.push(c);
        }
        // As geometrias vêm depois porque `#n` precisa do TRCL da célula n.
        let mut prims = std::mem::take(&mut self.model.prims);
        for (i, raw) in self.raw_cells.iter().enumerate() {
            let Some(ast) = &raw.ast else { continue };
            let mut warn = |m: String| warnings.push(format!("linha {}: célula {}: {}", raw.line, raw.id, m));
            let mut cx = Lower { index: &index, cells: &cells, prims: &mut prims, warn: &mut warn };
            let node = self.lower(ast, &mut cx, None, 0);
            cells[i].node = node;
        }
        self.model.prims = prims;
        self.model.cells = cells;
        self.model.warnings.extend(warnings);
        self.model.trs = self.trs.clone();
        self.model.phits = self.phits;
        self.model.finalize();
        self.model
    }

    fn parse_fill(&self, p: &Param) -> Result<FillDef, String> {
        let first = p.values.first().ok_or("FILL vazio")?;
        if !first.contains(':') {
            let u = self.num(first).ok_or("FILL inválido")? as i32;
            let tr = match p.values.get(1) {
                Some(t) if t.starts_with('(') => Some(self.inline_tr(t, p.star)?),
                _ => None,
            };
            return Ok(FillDef::Single { u: u.abs(), tr });
        }
        if p.values.len() < 3 {
            return Err("FILL em matriz precisa de três faixas i:j".into());
        }
        let (mut lo, mut hi) = ([0; 3], [0; 3]);
        for m in 0..3 {
            let (a, b) = p.values[m].split_once(':').ok_or("faixa de FILL inválida")?;
            lo[m] = self.num(a).ok_or("faixa de FILL inválida")? as i32;
            hi[m] = self.num(b).ok_or("faixa de FILL inválida")? as i32;
        }
        // Transformações por elemento, `u(tr)`, não são aplicadas.
        let entries: Vec<&String> = p.values[3..].iter().filter(|v| !v.starts_with('(')).collect();
        let us = self.numbers(&entries)?.iter().map(|&v| (v as i32).abs()).collect();
        Ok(FillDef::Array { lo, hi, us })
    }

    fn lower(&self, ast: &Ast, cx: &mut Lower, tr: Option<Transform>, depth: usize) -> Node {
        match ast {
            Ast::And(v) => Node::And(v.iter().map(|a| self.lower(a, cx, tr, depth)).collect()),
            Ast::Or(v) => Node::Or(v.iter().map(|a| self.lower(a, cx, tr, depth)).collect()),
            Ast::Not(a) => Node::Not(Box::new(self.lower(a, cx, tr, depth))),
            Ast::CellRef(n) => {
                let target = cx.index.get(n).and_then(|&i| Some((i, self.raw_cells[i].ast.as_ref()?)));
                match target {
                    Some((i, a)) if depth < 16 => {
                        // As superfícies da célula n valem no referencial do TRCL dela.
                        let inner = match (tr, cx.cells[i].trcl) {
                            (Some(outer), Some(own)) => Some(outer.compose(&own)),
                            (outer, own) => outer.or(own),
                        };
                        Node::Not(Box::new(self.lower(a, cx, inner, depth + 1)))
                    }
                    Some(_) => {
                        (cx.warn)(format!("complemento #{} é recursivo", n));
                        Node::Empty
                    }
                    None => {
                        (cx.warn)(format!("complemento #{}: célula não existe", n));
                        Node::Empty
                    }
                }
            }
            Ast::Surf { id, facet, pos } => {
                let Some(s) = self.model.surf_index.get(id).map(|&i| &self.model.surfaces[i]) else {
                    (cx.warn)(format!("superfície {} não definida", id));
                    return Node::Empty;
                };
                let mut half = |prim: u32| {
                    let prim = match &tr {
                        Some(t) => {
                            let moved = cx.prims[prim as usize].transformed(t);
                            cx.prims.push(moved);
                            cx.prims.len() as u32 - 1
                        }
                        None => prim,
                    };
                    Node::Half { prim, pos: *pos }
                };
                if *facet > 0 {
                    match s.prims.get(*facet as usize - 1) {
                        Some(&p) => half(p),
                        None => {
                            (cx.warn)(format!("superfície {} não tem a faceta {}", id, facet));
                            Node::Empty
                        }
                    }
                } else if !s.is_macro {
                    half(s.prims[0])
                } else if *pos {
                    Node::Or(s.prims.iter().map(|&p| half(p)).collect())
                } else {
                    Node::And(s.prims.iter().map(|&p| half(p)).collect())
                }
            }
        }
    }
}

/// Estado compartilhado ao converter a geometria das células em [`Node`].
struct Lower<'a> {
    index: &'a HashMap<i32, usize>,
    cells: &'a [Cell],
    prims: &'a mut Vec<Prim>,
    warn: &'a mut dyn FnMut(String),
}

fn next_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Separa `chave=valores` da cauda de um cartão de célula. Grupos entre
/// parênteses ficam inteiros num único valor.
fn parse_params(s: &str) -> Vec<Param> {
    let s = s.replace('=', " ");
    let b = s.as_bytes();
    let mut toks: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if b[i] == b'(' {
            let end = s[i..].find(')').map_or(b.len(), |e| i + e + 1);
            toks.push(s[i..end].to_string());
            i = end;
        } else {
            let end = s[i..].find(|ch: char| ch.is_whitespace() || ch == '(').map_or(b.len(), |e| i + e);
            toks.push(s[i..end].to_string());
            i = end;
        }
    }
    let mut out: Vec<Param> = Vec::new();
    for t in toks {
        let name = t.trim_start_matches('*');
        let is_key = name.starts_with(|ch: char| ch.is_ascii_alphabetic()) && !matches!(name, "r" | "i" | "m" | "j");
        if is_key {
            out.push(Param {
                key: name.split(':').next().unwrap_or(name).to_string(),
                star: t.starts_with('*'),
                values: Vec::new(),
            });
        } else if let Some(p) = out.last_mut() {
            p.values.push(t);
        }
    }
    out
}

/// Geometria de célula: espaço = interseção, `:` = união, `#` = complemento.
struct GeomParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl GeomParser<'_> {
    fn parse(mut self) -> Result<Ast, String> {
        let ast = self.union()?;
        self.ws();
        if self.i < self.s.len() {
            return Err(format!("caractere inesperado '{}' na geometria", self.s[self.i] as char));
        }
        Ok(ast)
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn union(&mut self) -> Result<Ast, String> {
        let mut parts = vec![self.inter()?];
        while self.peek() == Some(b':') {
            self.i += 1;
            parts.push(self.inter()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Ast::Or(parts) })
    }

    fn inter(&mut self) -> Result<Ast, String> {
        let mut parts = Vec::new();
        while !matches!(self.peek(), None | Some(b')') | Some(b':')) {
            parts.push(self.unary()?);
        }
        match parts.len() {
            0 => Err("geometria vazia".into()),
            1 => Ok(parts.pop().unwrap()),
            _ => Ok(Ast::And(parts)),
        }
    }

    fn group(&mut self) -> Result<Ast, String> {
        self.i += 1;
        let inner = self.union()?;
        if self.peek() != Some(b')') {
            return Err("falta fechar parêntese".into());
        }
        self.i += 1;
        Ok(inner)
    }

    fn unary(&mut self) -> Result<Ast, String> {
        match self.peek() {
            Some(b'#') => {
                self.i += 1;
                if self.peek() == Some(b'(') {
                    Ok(Ast::Not(Box::new(self.group()?)))
                } else {
                    let (id, _, _) = self.number()?;
                    Ok(Ast::CellRef(id))
                }
            }
            Some(b'(') => self.group(),
            _ => {
                let (id, facet, pos) = self.number()?;
                Ok(Ast::Surf { id, facet, pos })
            }
        }
    }

    /// `[+-]n[.faceta]`
    fn number(&mut self) -> Result<(i32, u32, bool), String> {
        self.ws();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'+') | Some(b'-')) {
            self.i += 1;
        }
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
            self.i += 1;
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        let pos = !tok.starts_with('-');
        let body = tok.trim_start_matches(['+', '-']);
        let (a, b) = body.split_once('.').unwrap_or((body, ""));
        let id = a.parse::<i32>().map_err(|_| {
            let bad = self.s.get(start).map_or(String::new(), |&c| (c as char).to_string());
            format!("esperado número de superfície, encontrado '{}{}'", tok, if tok.is_empty() { bad } else { String::new() })
        })?;
        let facet = if b.is_empty() { 0 } else { b.parse::<u32>().map_err(|_| "faceta inválida")? };
        Ok((id, facet, pos))
    }
}

fn axis(mn: &str) -> usize {
    match mn.as_bytes().last() {
        Some(b'x') => 0,
        Some(b'y') => 1,
        _ => 2,
    }
}

fn sphere(c: DVec3, r: f64) -> Prim {
    Prim::Quadric(Quadric::zero().add_diag(DVec3::ONE, c).add_const(-r * r))
}

/// Cilindro de eixo `u` (unitário) passando por `v`.
fn cylinder(v: DVec3, u: DVec3, r: f64) -> Prim {
    Prim::Quadric(Quadric::zero().add_diag(DVec3::ONE, v).add_sq(u, u.dot(v), -1.0).add_const(-r * r))
}

fn cone(apex: DVec3, ax: usize, t2: f64, sheet: f64) -> Prim {
    let e = DVec3::AXES[ax];
    let mut q = Quadric::zero().add_diag(DVec3::ONE - e * (1.0 + t2), apex);
    if sheet != 0.0 {
        let n = e * sheet.signum();
        q.sheet = Some((n, n.dot(apex)));
    }
    Prim::Quadric(q)
}

/// Converte um cartão de superfície em primitivas. Macrobodies viram uma lista
/// de facetas, cada uma com sentido positivo para fora.
fn build_surface(mn: &str, v: &[f64]) -> Result<(Vec<Prim>, bool), String> {
    let need = |n: usize| -> Result<(), String> {
        if v.len() < n {
            Err(format!("{} precisa de {} parâmetros, recebeu {}", mn.to_uppercase(), n, v.len()))
        } else {
            Ok(())
        }
    };
    let p3 = |i: usize| DVec3::new(v[i], v[i + 1], v[i + 2]);
    let ax = axis(mn);
    let e = DVec3::AXES[ax];
    let simple = |p: Prim| -> Result<(Vec<Prim>, bool), String> { Ok((vec![p], false)) };
    let plane = Prim::plane_through;

    match mn {
        "p" if v.len() >= 9 => {
            // Plano por três pontos; a origem fica no lado negativo.
            let (a, b, c) = (p3(0), p3(3), p3(6));
            let mut n = (b - a).cross(c - a);
            if n.length() < 1e-300 {
                return Err("pontos colineares".into());
            }
            n = n.normalize();
            let mut d = n.dot(a);
            let flip = if d.abs() > 1e-12 {
                d < 0.0
            } else {
                [n.z, n.y, n.x].iter().find(|c| c.abs() > 1e-12).is_some_and(|&c| c < 0.0)
            };
            if flip {
                n = -n;
                d = -d;
            }
            simple(Prim::Plane { n, d })
        }
        "p" => {
            need(4)?;
            simple(Prim::Plane { n: p3(0), d: v[3] })
        }
        "px" | "py" | "pz" => {
            need(1)?;
            simple(Prim::Plane { n: e, d: v[0] })
        }
        "so" => {
            need(1)?;
            simple(sphere(DVec3::ZERO, v[0]))
        }
        "s" => {
            need(4)?;
            simple(sphere(p3(0), v[3]))
        }
        "sx" | "sy" | "sz" => {
            need(2)?;
            simple(sphere(e * v[0], v[1]))
        }
        "c/x" | "c/y" | "c/z" => {
            need(3)?;
            let (i, j) = ((ax + 1) % 3, (ax + 2) % 3);
            let (i, j) = (i.min(j), i.max(j));
            let mut c = DVec3::ZERO;
            c[i] = v[0];
            c[j] = v[1];
            simple(cylinder(c, e, v[2]))
        }
        "cx" | "cy" | "cz" => {
            need(1)?;
            simple(cylinder(DVec3::ZERO, e, v[0]))
        }
        "k/x" | "k/y" | "k/z" => {
            need(4)?;
            simple(cone(p3(0), ax, v[3], v.get(4).copied().unwrap_or(0.0)))
        }
        "kx" | "ky" | "kz" => {
            need(2)?;
            simple(cone(e * v[0], ax, v[1], v.get(2).copied().unwrap_or(0.0)))
        }
        "sq" => {
            need(10)?;
            let c = p3(7);
            let lin = 2.0 * p3(3);
            let mut q = Quadric::zero().add_diag(p3(0), c).add_const(v[6]);
            q.b += lin;
            q.c -= lin.dot(c);
            simple(Prim::Quadric(q))
        }
        "gq" => {
            need(10)?;
            simple(Prim::Quadric(Quadric::from_gq(v)))
        }
        "tx" | "ty" | "tz" => {
            need(6)?;
            let tr = Transform { rot: DMat3::IDENTITY, off: p3(0) };
            simple(Prim::Torus { tr, axis: ax, a: v[3], b: v[4], c: v[5] })
        }
        "x" | "y" | "z" => {
            need(2)?;
            if v.len() < 4 || (v[0] - v[2]).abs() < 1e-12 {
                return simple(Prim::Plane { n: e, d: v[0] });
            }
            if v.len() >= 6 {
                return Err("superfície por três pontos não é suportada".into());
            }
            let (x1, r1, x2, r2) = (v[0], v[1], v[2], v[3]);
            if (r1 - r2).abs() < 1e-12 {
                return simple(cylinder(DVec3::ZERO, e, r1));
            }
            let t = (r2 - r1) / (x2 - x1);
            let apex = x1 - r1 / t;
            let side = if (x1 - apex).abs() > (x2 - apex).abs() { x1 - apex } else { x2 - apex };
            simple(cone(e * apex, ax, t * t, side))
        }
        "box" => {
            need(9)?;
            let o = p3(0);
            let mut f = Vec::new();
            for k in 0..(v.len() - 3) / 3 {
                let a = p3(3 + 3 * k);
                f.push(plane(o + a, a));
                f.push(plane(o, -a));
            }
            Ok((f, true))
        }
        "rpp" => {
            need(6)?;
            let mut f = Vec::new();
            for k in 0..3 {
                f.push(Prim::Plane { n: DVec3::AXES[k], d: v[2 * k + 1] });
                f.push(Prim::Plane { n: -DVec3::AXES[k], d: -v[2 * k] });
            }
            Ok((f, true))
        }
        "sph" => {
            need(4)?;
            Ok((vec![sphere(p3(0), v[3])], true))
        }
        "rcc" => {
            need(7)?;
            let (o, h) = (p3(0), p3(3));
            Ok((vec![cylinder(o, h.normalize(), v[6]), plane(o + h, h), plane(o, -h)], true))
        }
        "rhp" | "hex" => {
            need(9)?;
            let (o, h, r) = (p3(0), p3(3), p3(6));
            let u = h.normalize();
            let rot = |deg: f64| r * deg.to_radians().cos() + u.cross(r) * deg.to_radians().sin();
            let (s, t) = if v.len() >= 15 { (p3(9), p3(12)) } else { (rot(60.0), rot(120.0)) };
            let mut f = Vec::new();
            for a in [r, s, t] {
                f.push(plane(o + a, a));
                f.push(plane(o - a, -a));
            }
            f.push(plane(o + h, h));
            f.push(plane(o, -h));
            Ok((f, true))
        }
        "rec" => {
            need(10)?;
            let (o, h, v1) = (p3(0), p3(3), p3(6));
            let v2 = if v.len() >= 12 { p3(9) } else { h.cross(v1).normalize() * v[9] };
            let (u1, u2) = (v1.normalize(), v2.normalize());
            let q = Quadric::zero()
                .add_sq(u1, u1.dot(o), 1.0 / v1.length_squared())
                .add_sq(u2, u2.dot(o), 1.0 / v2.length_squared())
                .add_const(-1.0);
            Ok((vec![Prim::Quadric(q), plane(o + h, h), plane(o, -h)], true))
        }
        "trc" => {
            need(8)?;
            let (o, h, r1, r2) = (p3(0), p3(3), v[6], v[7]);
            let u = h.normalize();
            let k = (r2 - r1) / h.length();
            // |q|² - (u·q)² - (r1 + k·u·q)², com q = p - o.
            let q = Quadric::zero()
                .add_diag(DVec3::ONE, o)
                .add_sq(u, u.dot(o), -(1.0 + k * k))
                .add_lin(u, u.dot(o), -2.0 * r1 * k)
                .add_const(-r1 * r1);
            Ok((vec![Prim::Quadric(q), plane(o + h, h), plane(o, -h)], true))
        }
        "ell" => {
            need(7)?;
            let (v1, v2, rm) = (p3(0), p3(3), v[6]);
            // rm > 0: focos e semi-eixo maior; rm < 0: centro, semi-eixo maior e |rm| = menor.
            let (c, u, a, b) = if rm > 0.0 {
                let f = (v2 - v1).length() / 2.0;
                ((v1 + v2) / 2.0, (v2 - v1).normalize_or(DVec3::Z), rm, (rm * rm - f * f).max(0.0).sqrt())
            } else {
                (v1, v2.normalize(), v2.length(), -rm)
            };
            if a <= 0.0 || b <= 0.0 {
                return Err("elipsoide degenerado".into());
            }
            let q = Quadric::zero()
                .add_diag(DVec3::splat(1.0 / (b * b)), c)
                .add_sq(u, u.dot(c), 1.0 / (a * a) - 1.0 / (b * b))
                .add_const(-1.0);
            Ok((vec![Prim::Quadric(q)], true))
        }
        "wed" => {
            need(12)?;
            let (o, a, b, c) = (p3(0), p3(3), p3(6), p3(9));
            let toward = |n: DVec3, dir: DVec3| if n.dot(dir) < 0.0 { -n } else { n };
            Ok((
                vec![
                    plane(o + a, toward((b - a).cross(c), a)),
                    plane(o, toward(b.cross(c), -a)),
                    plane(o, toward(a.cross(c), -b)),
                    plane(o + c, toward(a.cross(b), c)),
                    plane(o, toward(a.cross(b), -c)),
                ],
                true,
            ))
        }
        "arb" => {
            need(30)?;
            let pts: Vec<DVec3> = (0..8).map(|k| p3(3 * k)).collect();
            let mut used = [false; 8];
            let mut faces = Vec::new();
            for &code in &v[24..30] {
                let ids: Vec<usize> =
                    (code as i64).to_string().bytes().map(|d| (d - b'0') as usize).filter(|&d| (1..=8).contains(&d)).collect();
                if ids.len() >= 3 {
                    ids.iter().for_each(|&i| used[i - 1] = true);
                    faces.push(ids);
                }
            }
            let n_used = used.iter().filter(|&&u| u).count().max(1);
            let centroid = (0..8).filter(|&i| used[i]).map(|i| pts[i]).sum::<DVec3>() / n_used as f64;
            let mut f = Vec::new();
            for ids in faces {
                let (a, b, c) = (pts[ids[0] - 1], pts[ids[1] - 1], pts[ids[2] - 1]);
                let n = (b - a).cross(c - a);
                if n.length() < 1e-300 {
                    return Err("face degenerada no ARB".into());
                }
                f.push(plane(a, if n.dot(centroid - a) > 0.0 { -n } else { n }));
            }
            Ok((f, true))
        }
        _ => Err(format!("tipo '{}' não suportado", mn.to_uppercase())),
    }
}
