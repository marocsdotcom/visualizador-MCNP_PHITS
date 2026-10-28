//! Leitor de inputs PHITS: percorre as seções `[ ... ]` e aproveita as que
//! descrevem geometria ([Surface], [Cell], [Transform], [Material]).

use crate::cards::{Builder, Card};
use crate::geom::Model;
use crate::mcnp::push_line;
use std::path::Path;

fn section_name(line: &str) -> Option<(String, bool)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    if indent > 4 || !t.starts_with('[') {
        return None;
    }
    let end = t.find(']')?;
    let name: String = t[1..end].chars().filter(|c| !c.is_whitespace()).collect();
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((name, t[end + 1..].trim().starts_with("off")))
}

pub fn looks_like_phits(src: &str) -> bool {
    src.lines().filter_map(|l| section_name(&l.to_lowercase())).any(|(n, _)| matches!(n.as_str(), "cell" | "surface" | "parameters"))
}

#[derive(Default)]
struct Reader {
    b: Builder,
    section: String,
    off: bool,
    done: bool,
    cells: Vec<Card>,
    surfaces: Vec<Card>,
    transforms: Vec<Card>,
    materials: Vec<Card>,
    amp: bool,
}

impl Reader {
    /// `set: c1[10] c2[c1*2]`
    fn set_vars(&mut self, text: &str) {
        let mut rest = text;
        while let Some(open) = rest.find('[') {
            let Some(close) = rest[open..].find(']').map(|c| open + c) else { break };
            let name = rest[..open].trim().to_string();
            match crate::expr::eval(rest[open + 1..close].trim(), &self.b.vars) {
                Some(v) => {
                    self.b.vars.insert(name, v);
                }
                None => self.b.model.warn(format!("set: não foi possível avaliar {}[{}]", name, &rest[open + 1..close])),
            }
            rest = &rest[close + 1..];
        }
    }

    fn feed(&mut self, src: &str, dir: Option<&Path>, depth: usize) {
        for (i, raw) in src.lines().enumerate() {
            if self.done {
                return;
            }
            let lower = raw.to_lowercase();
            if let Some((name, off)) = section_name(&lower) {
                self.done = name == "end";
                (self.section, self.off, self.amp) = (name, off, false);
                continue;
            }
            let geometry = matches!(self.section.as_str(), "cell" | "surface");
            let first = lower.chars().next().unwrap_or(' ');
            if self.off || first == '%' || (first == '#' && !geometry) {
                continue;
            }
            let text = lower.split(['$', '!']).next().unwrap_or("").trim_end();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("set:") {
                self.set_vars(rest);
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("infl:") {
                let name = rest.trim().trim_start_matches('{').split('}').next().unwrap_or("").trim();
                let path = dir.map_or_else(|| name.into(), |d| d.join(name));
                match std::fs::read(&path) {
                    Ok(bytes) if depth < 8 => self.feed(&String::from_utf8_lossy(&bytes), dir, depth + 1),
                    _ => self.b.model.warn(format!("infl: não foi possível ler '{}'", path.display())),
                }
                continue;
            }
            // `\` no fim da linha equivale ao `&` do MCNP.
            let line = match text.strip_suffix('\\') {
                Some(t) => format!("{} &", t),
                None => text.to_string(),
            };
            match self.section.as_str() {
                "title" if self.b.model.title.is_empty() => self.b.model.title = raw.trim().to_string(),
                "cell" => push_line(&mut self.cells, &line, i + 1, &mut self.amp),
                "surface" => push_line(&mut self.surfaces, &line, i + 1, &mut self.amp),
                "transform" => {
                    let starts = trimmed.trim_start_matches('*').starts_with("tr");
                    join(&mut self.transforms, trimmed, i + 1, starts);
                }
                "material" => {
                    let head = trimmed.split_whitespace().next().unwrap_or("");
                    let starts = head.starts_with("mat[")
                        || head.strip_prefix('m').is_some_and(|r| !r.is_empty() && r.bytes().all(|c| c.is_ascii_digit()));
                    // Linhas como MT1 ou MX1 encerram o material sem abrir outro.
                    let other = head.starts_with(|c: char| c.is_ascii_alphabetic()) && !starts && head.len() > 2 && head.ends_with(|c: char| c.is_ascii_digit());
                    if other {
                        self.materials.push(Card { text: String::new(), line: i + 1, last: i + 1 });
                    } else {
                        join(&mut self.materials, trimmed, i + 1, starts);
                    }
                }
                // Demais seções ([Source], [Parameters], tallies): só os
                // pares `parâmetro = valor`, para o diagnóstico.
                section => {
                    if let Some((key, value)) = trimmed.split_once('=') {
                        self.b.model.data.push((format!("{}/{}", section, key.trim()), value.trim().to_string()));
                    }
                }
            }
            // As linhas de um arquivo incluído não são do arquivo principal:
            // o cartão não pode ser reescrito nele.
            if depth > 0 {
                let list = match self.section.as_str() {
                    "cell" => &mut self.cells,
                    "surface" => &mut self.surfaces,
                    "transform" => &mut self.transforms,
                    _ => continue,
                };
                if let Some(c) = list.last_mut() {
                    c.last = 0;
                }
            }
        }
    }
}

/// Em [Transform] e [Material] a continuação é livre: tudo até o próximo
/// cartão pertence ao anterior.
fn join(cards: &mut Vec<Card>, text: &str, line: usize, starts: bool) {
    let text = text.trim_end_matches(['\\', '&']).trim();
    match cards.last_mut() {
        Some(last) if !starts => {
            if !last.text.is_empty() {
                last.text.push(' ');
                last.text.push_str(text);
                last.last = line;
            }
        }
        _ if starts => cards.push(Card { text: text.to_string(), line, last: line }),
        _ => {}
    }
}

pub fn parse(src: &str, dir: Option<&Path>) -> Model {
    let mut r = Reader::default();
    r.b.phits = true;
    r.feed(src, dir, 0);
    let Reader { mut b, cells, surfaces, transforms, materials, .. } = r;
    transforms.iter().for_each(|c| b.add_transform(c));
    materials.iter().filter(|c| !c.text.is_empty()).for_each(|c| b.add_material(c));
    surfaces.iter().for_each(|c| b.add_surface(c));
    cells.iter().for_each(|c| b.add_cell(c));
    b.finish()
}
