//! Edição da geometria pelo texto do input: desloca superfícies e
//! transformações reescrevendo só os cartões envolvidos. O resto do arquivo
//! (comentários, espaçamento, cartões de dados) fica como estava, e cada
//! cartão reescrito leva acima um comentário com a versão anterior.

use crate::cards::plain_number;
use crate::geom::Model;
use glam::{DMat3, DVec3};

/// Largura máxima das linhas escritas: o MCNPX ignora o que passa da coluna 80.
const WIDTH: usize = 78;

/// Número escrito de volta no cartão: dez algarismos significativos, sem o
/// ruído de arredondamento da soma.
fn fmt(v: f64) -> String {
    let r: f64 = format!("{:.9e}", v).parse().unwrap_or(v);
    if r == 0.0 {
        "0".into()
    } else if (1e-4..1e9).contains(&r.abs()) {
        format!("{}", r)
    } else {
        format!("{:e}", r)
    }
}

/// Texto da distância para os comentários.
fn vector(d: DVec3) -> String {
    format!("({}, {}, {}) cm", fmt(d.x), fmt(d.y), fmt(d.z))
}

/// O arquivo em linhas, guardando a quebra de linha usada e se termina com uma.
struct Text {
    lines: Vec<String>,
    newline: &'static str,
    trailing: bool,
}

impl Text {
    fn new(src: &str) -> Text {
        Text {
            lines: src.lines().map(str::to_string).collect(),
            newline: if src.contains("\r\n") { "\r\n" } else { "\n" },
            trailing: src.ends_with('\n'),
        }
    }

    fn join(&self) -> String {
        let mut out = self.lines.join(self.newline);
        if self.trailing {
            out.push_str(self.newline);
        }
        out
    }
}

/// Fichas de um cartão (sem comentários e marcas de continuação) e o
/// comentário `$` da primeira linha que tiver um.
fn tokens(lines: &[String], phits: bool) -> (Vec<String>, String) {
    let (mut toks, mut comment) = (Vec::new(), String::new());
    for l in lines {
        let cut = l.find(|c: char| c == '$' || (phits && c == '!')).unwrap_or(l.len());
        if comment.is_empty() && cut < l.len() {
            comment = l[cut + 1..].trim().to_string();
        }
        let code = l[..cut].trim_end().trim_end_matches(['&', '\\']);
        toks.extend(code.split_whitespace().map(str::to_string));
    }
    (toks, comment)
}

/// Fichas em linhas de no máximo [`WIDTH`] colunas; as continuações começam
/// com cinco brancos, que valem no MCNP e no PHITS.
fn wrap(toks: &[String], comment: &str, mark: &str) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new()];
    for t in toks {
        let cur = lines.last_mut().unwrap();
        if !cur.trim().is_empty() && cur.len() + 1 + t.len() > WIDTH {
            lines.push(format!("     {}", t));
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(t);
        }
    }
    if !comment.is_empty() {
        if lines[0].len() + 4 + comment.len() <= WIDTH {
            lines[0] = format!("{}  $ {}", lines[0], comment);
        } else {
            lines.insert(0, note(mark, comment));
        }
    }
    lines
}

/// Linha de comentário, cortada na largura máxima.
fn note(mark: &str, text: &str) -> String {
    format!("{} {}", mark, text).chars().take(80).collect()
}

/// Comentário inserido acima de um cartão reescrito: o que mudou e as linhas
/// como eram.
fn history(mark: &str, what: &str, old: &[String]) -> Vec<String> {
    let mut out = vec![note(mark, &format!("visualizador: {}. Antes:", what))];
    out.extend(old.iter().map(|l| note(mark, l.trim_end())));
    out
}

fn numbers(toks: &[String], mn: &str) -> Result<Vec<f64>, String> {
    toks.iter()
        .map(|t| {
            plain_number(t).ok_or_else(|| format!("o parâmetro '{}' da superfície {} não é um número simples (expressão, variável ou atalho): mova esta à mão", t, mn))
        })
        .collect()
}

/// Mnemônico e parâmetros de uma superfície deslocada de `d`, no referencial
/// em que os parâmetros estão escritos. Parâmetros que não mudam ficam como
/// estavam escritos.
fn shifted(mn: &str, toks: &[String], d: DVec3) -> Result<(String, Vec<String>), String> {
    let v = numbers(toks, mn)?;
    let mn_low = mn.to_lowercase();
    let need = |n: usize| -> Result<(), String> {
        if v.len() < n {
            Err(format!("superfície {} com {} parâmetros, esperados ao menos {}", mn, v.len(), n))
        } else {
            Ok(())
        }
    };
    let mut out: Vec<String> = toks.to_vec();
    let mut bump = |i: usize, delta: f64| {
        if delta != 0.0 {
            out[i] = fmt(v[i] + delta);
        }
    };
    // Eixo dos mnemônicos terminados em x, y ou z, e os outros dois.
    let axis = match mn_low.chars().last() {
        Some('x') => 0,
        Some('y') => 1,
        _ => 2,
    };
    let (i, j) = ((axis + 1) % 3, (axis + 2) % 3);
    let (i, j) = (i.min(j), i.max(j));
    let along = d[i] == 0.0 && d[j] == 0.0;
    let name = |k: usize| ["x", "y", "z"][k];
    match mn_low.as_str() {
        "px" | "py" | "pz" => {
            need(1)?;
            bump(0, d[axis]);
        }
        "p" if v.len() >= 9 => (0..9).for_each(|k| bump(k, d[k % 3])),
        "p" => {
            need(4)?;
            bump(3, v[0] * d.x + v[1] * d.y + v[2] * d.z);
        }
        "so" => {
            need(1)?;
            if d != DVec3::ZERO {
                return Ok(("s".into(), vec![fmt(d.x), fmt(d.y), fmt(d.z), toks[0].clone()]));
            }
        }
        "s" | "sph" => {
            need(4)?;
            (0..3).for_each(|k| bump(k, d[k]));
        }
        "sx" | "sy" | "sz" => {
            need(2)?;
            if along {
                bump(0, d[axis]);
            } else {
                let c = DVec3::AXES[axis] * v[0] + d;
                return Ok(("s".into(), vec![fmt(c.x), fmt(c.y), fmt(c.z), toks[1].clone()]));
            }
        }
        "c/x" | "c/y" | "c/z" => {
            need(3)?;
            bump(0, d[i]);
            bump(1, d[j]);
        }
        "cx" | "cy" | "cz" => {
            need(1)?;
            if !along {
                return Ok((format!("c/{}", name(axis)), vec![fmt(d[i]), fmt(d[j]), toks[0].clone()]));
            }
        }
        "k/x" | "k/y" | "k/z" | "tx" | "ty" | "tz" => {
            need(if mn_low.starts_with('t') { 6 } else { 4 })?;
            (0..3).for_each(|k| bump(k, d[k]));
        }
        "kx" | "ky" | "kz" => {
            need(2)?;
            if along {
                bump(0, d[axis]);
            } else {
                let c = DVec3::AXES[axis] * v[0] + d;
                let mut params = vec![fmt(c.x), fmt(c.y), fmt(c.z)];
                params.extend(toks[1..].iter().cloned());
                return Ok((format!("k/{}", name(axis)), params));
            }
        }
        "sq" => {
            need(10)?;
            (0..3).for_each(|k| bump(7 + k, d[k]));
        }
        "gq" => {
            need(10)?;
            // f(p) = pᵀQp + L·p + K; deslocar é trocar p por p − d.
            let q = DMat3::from_cols(
                DVec3::new(v[0], v[3] / 2.0, v[5] / 2.0),
                DVec3::new(v[3] / 2.0, v[1], v[4] / 2.0),
                DVec3::new(v[5] / 2.0, v[4] / 2.0, v[2]),
            );
            let l = DVec3::new(v[6], v[7], v[8]);
            let new_l = l - 2.0 * (q * d);
            let new_k = v[9] + d.dot(q * d) - l.dot(d);
            for (k, value) in [(6, new_l.x), (7, new_l.y), (8, new_l.z), (9, new_k)] {
                if value != v[k] {
                    out[k] = fmt(value);
                }
            }
        }
        "x" | "y" | "z" => {
            if !along {
                return Err(format!("a superfície {} é simétrica em torno do eixo {} e só pode ser movida ao longo dele", mn, name(axis)));
            }
            (0..v.len()).step_by(2).for_each(|k| bump(k, d[axis]));
        }
        "rpp" => {
            need(6)?;
            (0..6).for_each(|k| bump(k, d[k / 2]));
        }
        "box" | "rcc" | "rhp" | "hex" | "rec" | "trc" | "wed" => {
            need(3)?;
            (0..3).for_each(|k| bump(k, d[k]));
        }
        "ell" => {
            need(7)?;
            // Raio positivo: os dois primeiros vetores são focos, e os dois se movem.
            let n = if v[6] > 0.0 { 6 } else { 3 };
            (0..n).for_each(|k| bump(k, d[k % 3]));
        }
        "arb" => {
            need(24)?;
            (0..24).for_each(|k| bump(k, d[k % 3]));
        }
        _ => return Err(format!("não sei mover superfícies do tipo '{}'", mn)),
    }
    Ok((mn.to_string(), out))
}

/// Desloca as superfícies `ids` de `d` (cm, no sistema principal) e devolve
/// o texto novo do input.
pub fn move_surfaces(src: &str, model: &Model, ids: &[i32], d: DVec3) -> Result<String, String> {
    if ids.is_empty() {
        return Err("nenhuma superfície escolhida".into());
    }
    let mark = if model.phits { "$" } else { "c" };
    let mut text = Text::new(src);
    let mut spans = Vec::new();
    for &id in ids {
        let s = model.surf_index.get(&id).map(|&i| &model.surfaces[i]).ok_or_else(|| format!("a superfície {} não existe", id))?;
        if s.line == 0 || s.last < s.line || s.last > text.lines.len() {
            return Err(format!("a superfície {} está num arquivo incluído (infl:) e não pode ser editada daqui", id));
        }
        if !spans.iter().any(|x: &(usize, usize, i32)| x.2 == id) {
            spans.push((s.line, s.last, id));
        }
    }
    // De baixo para cima, para as linhas de cima continuarem onde estavam.
    spans.sort_by(|a, b| b.0.cmp(&a.0));
    for (first, last, id) in spans {
        let old: Vec<String> = text.lines[first - 1..last].to_vec();
        let (toks, comment) = tokens(&old, model.phits);
        let written = toks.first().map(|t| t.trim_start_matches(['*', '+']).parse::<i32>().ok()).flatten();
        if written != Some(id) {
            return Err(format!("não achei o cartão da superfície {} na linha {}", id, first));
        }
        // Número de transformação (ou de superfície periódica, se negativo) antes do mnemônico.
        let tr = toks.get(1).and_then(|t| t.parse::<i32>().ok());
        let k = if tr.is_some() { 2 } else { 1 };
        let mn = toks.get(k).ok_or_else(|| format!("superfície {} sem tipo", id))?;
        let local = match tr.filter(|&n| n > 0) {
            Some(n) => model.trs.get(&n).ok_or_else(|| format!("a superfície {} usa a TR{}, que não foi lida", id, n))?.dir_to_local(d),
            None => d,
        };
        let (mn, params) = shifted(mn, &toks[k + 1..], local)?;
        let mut new_toks: Vec<String> = toks[..k].to_vec();
        new_toks.push(mn);
        new_toks.extend(params);
        let mut lines = history(mark, &format!("superfície {} deslocada de {}", id, vector(d)), &old);
        lines.extend(wrap(&new_toks, &comment, mark));
        text.lines.splice(first - 1..last, lines);
    }
    Ok(text.join())
}

/// Desloca a transformação TR`n` de `d`: tudo o que a usa (superfícies e
/// células com TRCL=`n`) vai junto.
pub fn move_transform(src: &str, model: &Model, n: i32, d: DVec3) -> Result<String, String> {
    let mark = if model.phits { "$" } else { "c" };
    let &(_, first, last) = model.tr_cards.iter().find(|t| t.0 == n).ok_or_else(|| format!("a TR{} não existe", n))?;
    let mut text = Text::new(src);
    if first == 0 || last < first || last > text.lines.len() {
        return Err(format!("a TR{} está num arquivo incluído (infl:) e não pode ser editada daqui", n));
    }
    let old: Vec<String> = text.lines[first - 1..last].to_vec();
    let (mut toks, comment) = tokens(&old, model.phits);
    if toks.len() < 4 {
        return Err(format!("a TR{} não tem os três números do deslocamento", n));
    }
    let o = numbers(&toks[1..4], &format!("TR{}", n))?;
    let o = DVec3::new(o[0], o[1], o[2]);
    // Com M = -1 o deslocamento escrito é a origem principal vista do auxiliar.
    let reversed = toks.get(13).and_then(|t| plain_number(t)).is_some_and(|m| m < 0.0);
    let new_o = if reversed {
        let rot = model.trs.get(&n).ok_or_else(|| format!("a TR{} não foi lida", n))?.rot;
        o - rot.transpose() * d
    } else {
        o + d
    };
    for k in 0..3 {
        if new_o[k] != o[k] {
            toks[1 + k] = fmt(new_o[k]);
        }
    }
    let mut lines = history(mark, &format!("TR{} deslocada de {}", n, vector(d)), &old);
    lines.extend(wrap(&toks, &comment, mark));
    text.lines.splice(first - 1..last, lines);
    Ok(text.join())
}

/// As linhas de um cartão como estão no texto, para mostrar.
pub fn card_lines(src: &str, first: usize, last: usize) -> String {
    if first == 0 || last < first {
        return String::new();
    }
    src.lines().skip(first - 1).take(last - first + 1).collect::<Vec<_>>().join("\n")
}
