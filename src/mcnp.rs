//! Leitor de inputs MCNP: separa o arquivo nos três blocos (células,
//! superfícies, dados) e entrega os cartões ao [`Builder`].

use crate::cards::{Builder, Card};
use crate::geom::{Model, TallyBin, TallyDef};

/// Tabulações contam como avanço até a próxima coluna múltipla de 8.
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch == '\t' {
            let n = 8 - out.chars().count() % 8;
            out.extend(std::iter::repeat_n(' ', n));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Linha de comentário: `c` nas colunas 1 a 5 seguido de espaço ou fim de linha.
pub fn is_comment(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    indent < 5 && (t == "c" || t.starts_with("c "))
}

/// Junta linhas em cartões. Continuação: cinco brancos iniciais ou `&` no
/// fim da linha anterior.
pub fn push_line(cards: &mut Vec<Card>, raw: &str, line: usize, amp: &mut bool) {
    let text = expand_tabs(raw).to_lowercase();
    let text = text.split('$').next().unwrap_or("").trim_end().to_string();
    if text.trim().is_empty() || is_comment(&text) {
        return;
    }
    let continues = *amp || text.starts_with("     ");
    *amp = text.ends_with('&');
    let body = text.trim_end_matches('&');
    match cards.last_mut() {
        Some(last) if continues => {
            last.text.push(' ');
            last.text.push_str(body.trim());
            last.last = line;
        }
        _ => cards.push(Card { text: body.trim().to_string(), line, last: line }),
    }
}

/// Texto de uma linha do eco do input num arquivo de saída do MCNP: número
/// alinhado à direita (5 colunas no MCNPX, 10 no MCNP6; `*****` depois de
/// 99999), `-`, 7 brancos e a linha.
fn echo_line(line: &str) -> Option<&str> {
    let dash = line.find('-').filter(|&i| i == 5 || i == 10)?;
    let num = line[..dash].trim_start();
    if num.is_empty() || !(num.bytes().all(|c| c == b'*') || num.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let rest = &line[dash + 1..];
    if rest.trim().is_empty() {
        return Some("");
    }
    rest.strip_prefix("       ").map(str::trim_end)
}

/// Se `src` for um arquivo de saída do MCNP, reconstrói o input a partir do
/// eco que o código imprime no começo.
pub fn input_from_output(src: &str) -> Option<String> {
    let first = |l: &&str| l.starts_with("    1-       ") || l.starts_with("         1-       ");
    let mut lines = src.lines().skip_while(|l| !first(l));
    lines.clone().next()?;
    let mut out = String::new();
    // Avisos do MCNP aparecem intercalados no eco; ele só acabou quando as
    // linhas numeradas param de vez.
    let mut gap = 0;
    for l in lines.by_ref() {
        match echo_line(l) {
            Some(text) => {
                gap = 0;
                out.push_str(text);
                out.push('\n');
            }
            None => {
                gap += 1;
                if gap > 60 {
                    break;
                }
            }
        }
    }
    Some(out)
}

/// Intervalos de um grupo entre parênteses: `702<704[-30:30 0:0 0:0]` vira um
/// intervalo por elemento do lattice, com o primeiro índice variando mais rápido.
fn chain_bins(group: &str) -> Option<Vec<TallyBin>> {
    let mut chain: TallyBin = Vec::new();
    let mut range: Option<(usize, [(i32, i32); 3])> = None;
    for part in group.split('<') {
        let part = part.trim();
        let (cells, index) = match part.find('[') {
            Some(i) => (&part[..i], Some(part[i + 1..].trim_end().strip_suffix(']')?)),
            None => (part, None),
        };
        let mut cells = cells.split_whitespace();
        let cell: i32 = cells.next()?.parse().ok()?;
        if cells.next().is_some() {
            // União de células num só intervalo: não é lida.
            return None;
        }
        let Some(index) = index else {
            chain.push((cell, None));
            continue;
        };
        let spans: Vec<(i32, i32)> = index
            .split_whitespace()
            .map(|t| {
                let (a, b) = t.split_once(':').unwrap_or((t, t));
                Some((a.parse().ok()?, b.parse().ok()?))
            })
            .collect::<Option<_>>()?;
        let spans: [(i32, i32); 3] = spans.try_into().ok()?;
        if spans.iter().any(|s| s.0 != s.1) {
            if range.is_some() {
                return None;
            }
            range = Some((chain.len(), spans));
        }
        chain.push((cell, Some(spans.map(|s| s.0))));
    }
    let Some((at, [si, sj, sk])) = range else { return Some(vec![chain]) };
    let mut out = Vec::new();
    for k in sk.0..=sk.1 {
        for j in sj.0..=sj.1 {
            for i in si.0..=si.1 {
                let mut bin = chain.clone();
                bin[at].1 = Some([i, j, k]);
                out.push(bin);
            }
        }
    }
    Some(out)
}

/// Intervalos de um cartão F de célula, na ordem em que o MCNP os grava.
fn tally_bins(body: &str) -> Option<Vec<TallyBin>> {
    let mut bins = Vec::new();
    let mut rest = body.trim();
    while !rest.is_empty() {
        if let Some(group) = rest.strip_prefix('(') {
            let end = group.find(')')?;
            bins.extend(chain_bins(&group[..end])?);
            rest = group[end + 1..].trim_start();
        } else {
            let end = rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len());
            let tok = &rest[..end];
            bins.push(if tok == "t" { Vec::new() } else { vec![(tok.parse().ok()?, None)] });
            rest = rest[end..].trim_start();
        }
    }
    Some(bins)
}

/// Número do tally se `name` for o nome de um cartão com o prefixo dado
/// (`f6:p`, `*f8:p`, `fc16`).
fn tally_number(name: &str, prefix: &str) -> Option<i32> {
    let digits = name.trim_start_matches('*').strip_prefix(prefix)?.split(':').next()?;
    (!digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit())).then(|| digits.parse().ok())?
}

/// Posição e direção das partículas perdidas relatadas numa saída do MCNP.
/// Cada evento começa com "lost particle no." e traz as linhas
/// "x,y,z coordinates:" e "u,v,w direction cosines:".
pub fn lost_particles(src: &str) -> Vec<(glam::DVec3, glam::DVec3)> {
    let triple = |line: &str, label: &str| -> Option<glam::DVec3> {
        let rest = &line[line.find(label)? + label.len()..];
        let v: Vec<f64> = rest.split_whitespace().take(3).filter_map(|t| t.parse().ok()).collect();
        (v.len() == 3).then(|| glam::DVec3::new(v[0], v[1], v[2]))
    };
    let mut out = Vec::new();
    // Linhas que ainda podem pertencer ao evento em leitura.
    let mut window = 0;
    let mut pos = None;
    for line in src.lines() {
        if line.contains("lost particle no.") {
            (window, pos) = (60, None);
        } else if window > 0 {
            window -= 1;
            if let Some(p) = triple(line, "x,y,z coordinates:") {
                pos = Some(p);
            } else if let (Some(p), Some(d)) = (pos, triple(line, "u,v,w direction cosines:")) {
                out.push((p, d));
                window = 0;
            }
        }
    }
    out
}

pub fn parse(src: &str) -> Model {
    let mut lines = src.lines().enumerate().peekable();
    // Bloco opcional "message:" termina na primeira linha em branco.
    if lines.peek().is_some_and(|(_, l)| l.trim_start().to_lowercase().starts_with("message:")) {
        for (_, l) in lines.by_ref() {
            if l.trim().is_empty() {
                break;
            }
        }
    }
    let mut b = Builder::default();
    b.model.title = lines.next().map(|(_, l)| l.trim().to_string()).unwrap_or_default();

    let mut blocks: [Vec<Card>; 3] = Default::default();
    let (mut block, mut amp) = (0, false);
    for (i, raw) in lines {
        if raw.trim().is_empty() {
            block += 1;
            amp = false;
            if block > 2 {
                break;
            }
            continue;
        }
        push_line(&mut blocks[block], raw, i + 1, &mut amp);
    }

    let [cells, surfaces, data] = blocks;
    if surfaces.is_empty() {
        b.model.warn("bloco de superfícies vazio: confira as linhas em branco que separam os blocos");
    }
    b.model.data = data
        .iter()
        .map(|c| {
            let (name, body) = c.text.split_once(char::is_whitespace).unwrap_or((&c.text, ""));
            (name.to_string(), body.trim().to_string())
        })
        .collect();
    let mut comments: Vec<(i32, String)> = Vec::new();
    // As transformações precisam existir antes das superfícies e células.
    for card in &data {
        let name = card.text.split_whitespace().next().unwrap_or("").trim_start_matches('*');
        let digits = |p: &str| name.strip_prefix(p).is_some_and(|r| !r.is_empty() && r.bytes().all(|c| c.is_ascii_digit()));
        let key = name.split([':', '=']).next().unwrap_or("");
        let body = card.text.split_once(char::is_whitespace).map_or("", |x| x.1);
        if let Some(number) = tally_number(name, "fc") {
            comments.push((number, body.trim().to_string()));
        } else if let Some(number) = tally_number(name, "f") {
            b.model.tallies.push(TallyDef { number, comment: String::new(), bins: tally_bins(body) });
        }
        if digits("tr") {
            b.add_transform(card);
        } else if digits("m") {
            b.add_material(card);
        } else if matches!(key, "imp" | "u" | "lat" | "fill") {
            b.add_cell_data(card);
        }
    }
    for t in &mut b.model.tallies {
        if let Some((_, text)) = comments.iter().find(|c| c.0 == t.number) {
            t.comment = text.clone();
        }
    }
    surfaces.iter().for_each(|c| b.add_surface(c));
    cells.iter().for_each(|c| b.add_cell(c));
    b.finish()
}
