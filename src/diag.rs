//! Diagnóstico: o que dá para conferir num input (e numa saída) antes de
//! gastar dias de processamento, ou antes de confiar num resultado.

use crate::geom::{Audit, Model};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub level: Level,
    pub text: String,
    /// Onde olhar na geometria, quando o achado tem um lugar.
    pub at: Option<DVec3>,
}

impl Finding {
    fn new(level: Level, text: impl Into<String>) -> Self {
        Finding { level, text: text.into(), at: None }
    }
}

/// Posição (e direção, se houver uma só) da fonte descrita no input.
#[derive(Clone, Debug)]
pub struct Source {
    pub pos: DVec3,
    pub dir: Option<DVec3>,
    /// Alguma coordenada vem de uma distribuição: `pos` é só o centro presumido.
    pub approx: bool,
    pub summary: String,
}

fn card<'a>(model: &'a Model, name: &str) -> Option<&'a str> {
    model.data.iter().find(|d| d.0 == name).map(|d| d.1.as_str())
}

fn number(text: &str) -> Option<f64> {
    text.trim().replace(['d', 'D'], "e").parse().ok().or_else(|| text.trim().parse().ok())
}

pub fn source(model: &Model) -> Option<Source> {
    if let Some(body) = card(model, "sdef") {
        // `chave=valor` e `chave valor` são equivalentes no MCNP.
        let spaced = body.replace('=', " ");
        let toks: Vec<&str> = spaced.split_whitespace().collect();
        let after = |key: &str| toks.iter().position(|t| *t == key).map(|i| &toks[i + 1..]);
        let triple = |key: &str| {
            let t = after(key)?;
            Some(DVec3::new(number(t.first()?)?, number(t.get(1)?)?, number(t.get(2)?)?))
        };
        let mut approx = false;
        let mut pos = triple("pos").unwrap_or(DVec3::ZERO);
        for (axis, key) in ["x", "y", "z"].iter().enumerate() {
            if let Some(value) = after(key).and_then(|t| t.first()) {
                match number(value) {
                    Some(v) => pos[axis] = v,
                    None => approx = true,
                }
            }
        }
        approx |= after("pos").is_some_and(|t| t.first().is_some_and(|v| number(v).is_none()));
        let along = after("dir").and_then(|t| number(t.first()?));
        let dir = triple("vec").zip(along).map(|(v, d)| v.normalize_or_zero() * d.signum());
        let pick = |key: &str| after(key).and_then(|t| t.first()).map(|v| format!("{}={}", key, v));
        let summary = ["par", "erg", "cel", "sur", "rad"].iter().filter_map(|k| pick(k)).collect::<Vec<_>>().join(" ");
        return Some(Source { pos, dir, approx, summary });
    }
    let get = |key: &str| card(model, &format!("source/{}", key));
    get("s-type")?;
    let coord = |key: &str| get(key).and_then(number).unwrap_or(0.0);
    let pos = DVec3::new(coord("x0"), coord("y0"), coord("z0"));
    let dir = get("dir").and_then(number).map(|c| DVec3::new((1.0 - c * c).max(0.0).sqrt(), 0.0, c));
    let summary = ["s-type", "proj", "e0", "r0", "r1"].iter().filter_map(|k| Some(format!("{}={}", k, get(k)?))).collect::<Vec<_>>().join(" ");
    Some(Source { pos, dir, approx: false, summary })
}

fn list(ids: impl IntoIterator<Item = i32>) -> String {
    let ids: Vec<i32> = ids.into_iter().collect();
    let shown: Vec<String> = ids.iter().take(12).map(|i| i.to_string()).collect();
    let more = if ids.len() > 12 { format!(" e mais {}", ids.len() - 12) } else { String::new() };
    format!("{}{}", shown.join(", "), more)
}

/// Conferências que só dependem do input.
pub fn input_findings(model: &Model) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut add = |level, text: String| out.push(Finding::new(level, text));

    for w in &model.warnings {
        // A nota sobre a origem da geometria não é um problema.
        let level = if w.starts_with("arquivo de saída") { Level::Info } else { Level::Warn };
        add(level, format!("leitura: {}", w));
    }

    // --- células e superfícies
    let mut seen = BTreeSet::new();
    let dup: BTreeSet<i32> = model.cells.iter().filter(|c| !seen.insert(c.id)).map(|c| c.id).collect();
    if !dup.is_empty() {
        add(Level::Error, format!("células definidas mais de uma vez: {}", list(dup)));
    }
    let used: BTreeSet<u32> = model.cells.iter().flat_map(|c| c.prims.iter().copied()).collect();
    let idle: Vec<i32> = model.surfaces.iter().filter(|s| !s.prims.iter().any(|p| used.contains(p))).map(|s| s.id).collect();
    if !idle.is_empty() {
        add(Level::Info, format!("{} superfícies não são usadas por nenhuma célula: {}", idle.len(), list(idle)));
    }

    // --- materiais
    let in_cells: BTreeSet<i32> = model.cells.iter().filter(|c| !c.outside && c.mat != 0).map(|c| c.mat).collect();
    let missing: Vec<i32> = in_cells.iter().copied().filter(|m| !model.materials.contains_key(m)).collect();
    if !missing.is_empty() {
        add(Level::Error, format!("materiais usados em células mas sem cartão de material: {}", list(missing)));
    }
    let spare: Vec<i32> = model.materials.keys().copied().filter(|m| !in_cells.contains(m)).collect();
    if !spare.is_empty() {
        add(Level::Info, format!("materiais definidos e não usados: {}", list(spare)));
    }
    for (id, mat) in &model.materials {
        let (pos, neg) = (mat.nuclides.iter().any(|n| n.1 > 0.0), mat.nuclides.iter().any(|n| n.1 < 0.0));
        if pos && neg {
            add(Level::Error, format!("material {}: mistura frações atômicas (positivas) e em massa (negativas)", id));
        }
        if in_cells.contains(id) && mat.nuclides.is_empty() {
            add(Level::Warn, format!("material {}: não consegui ler a composição", id));
        }
    }
    // O mesmo material com densidades diferentes costuma ser de propósito, mas vale saber.
    let mut densities: BTreeMap<i32, BTreeSet<String>> = BTreeMap::new();
    for c in model.cells.iter().filter(|c| c.mat != 0 && !c.outside) {
        densities.entry(c.mat).or_default().insert(c.rho.to_string());
    }
    for (m, set) in densities.iter().filter(|d| d.1.len() > 1) {
        let text = format!("material {} aparece com {} densidades: {}", m, set.len(), set.iter().cloned().collect::<Vec<_>>().join(", "));
        // Cem vezes de diferença já não é o mesmo material comprimido ou
        // aquecido: é ar com composição de tecido, ou o contrário.
        let cells: Vec<&crate::geom::Cell> = model.cells.iter().filter(|c| c.mat == *m && !c.outside && c.rho != 0.0).collect();
        let same_unit = cells.iter().all(|c| c.rho < 0.0) || cells.iter().all(|c| c.rho > 0.0);
        let lightest = cells.iter().min_by(|a, b| a.rho.abs().total_cmp(&b.rho.abs()));
        let heaviest = cells.iter().map(|c| c.rho.abs()).fold(0.0, f64::max);
        match lightest.filter(|c| same_unit && heaviest > 100.0 * c.rho.abs()) {
            Some(c) => add(Level::Warn, format!("{}. A célula {} usa a composição dele com densidade {}: confira se ela não devia ter um material próprio", text, c.id, c.rho)),
            None => add(Level::Info, text),
        }
    }

    // --- fonte
    match source(model) {
        None => add(Level::Warn, "não achei a definição da fonte (SDEF ou [Source])".into()),
        Some(src) => {
            let place = format!("({:.4}, {:.4}, {:.4})", src.pos.x, src.pos.y, src.pos.z);
            let about = if src.approx { " (centro presumido: há coordenadas dadas por distribuição)" } else { "" };
            let (level, text) = match model.locate(src.pos) {
                None => (Level::Error, format!("fonte em {}{}: fora de qualquer célula", place, about)),
                Some((f, _)) => {
                    let c = &model.cells[f.leaf];
                    if c.outside {
                        (Level::Error, format!("fonte em {}{}: dentro da célula {} do mundo externo, onde as partículas morrem", place, about, c.id))
                    } else {
                        let what = if c.mat == 0 { "vazio".to_string() } else { format!("material {}", c.mat) };
                        (Level::Ok, format!("fonte em {}{}: célula {} ({})  {}", place, about, c.id, what, src.summary))
                    }
                }
            };
            out.push(Finding { level, text, at: Some(src.pos) });
        }
    }
    let mut add = |level, text: String| out.push(Finding::new(level, text));

    // --- tallies do MCNP
    let mode: Vec<&str> = card(model, "mode").map_or(vec!["n"], |m| m.split_whitespace().collect());
    for t in &model.tallies {
        let name = format!("tally F{}", t.number);
        let particles: Vec<&str> = model
            .data
            .iter()
            .find(|d| d.0.trim_start_matches('*').split(':').next() == Some(&format!("f{}", t.number)))
            .and_then(|d| d.0.split_once(':'))
            .map_or(Vec::new(), |p| p.1.split(',').collect());
        let absent: Vec<&str> = particles.iter().copied().filter(|p| !mode.contains(p)).collect();
        if !absent.is_empty() {
            add(Level::Error, format!("{}: pede partícula {} que não está no MODE ({})", name, absent.join(","), mode.join(" ")));
        }
        let Some(bins) = &t.bins else {
            add(Level::Info, format!("{}: forma do cartão não interpretada (o resultado não poderá ser pintado na geometria)", name));
            continue;
        };
        let cells: BTreeSet<i32> = bins.iter().flatten().map(|b| b.0).collect();
        let ghost: Vec<i32> = cells.iter().copied().filter(|c| !model.cell_index.contains_key(c)).collect();
        if !ghost.is_empty() {
            add(Level::Error, format!("{}: células que não existem: {}", name, list(ghost)));
        }
        let heating = matches!(t.number % 10, 6 | 7);
        let empty: Vec<i32> = bins
            .iter()
            .filter_map(|b| b.first())
            .filter_map(|b| model.cell_index.get(&b.0))
            .map(|&ci| &model.cells[ci])
            .filter(|c| c.mat == 0 && matches!(c.filled, crate::geom::Filled::None))
            .map(|c| c.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if heating && !empty.is_empty() {
            add(Level::Warn, format!("{}: energia depositada em célula de vazio dá sempre zero: {}", name, list(empty)));
        }
    }

    // --- critério de parada e modo
    if let Some(nps) = card(model, "nps") {
        add(Level::Info, format!("nps = {}", nps));
    } else if let (Some(cas), Some(bch)) = (card(model, "parameters/maxcas").and_then(number), card(model, "parameters/maxbch").and_then(number)) {
        add(Level::Info, format!("histórias = maxcas × maxbch = {:e}", cas * bch));
        if let Some(icntl) = card(model, "parameters/icntl").and_then(number).filter(|v| *v != 0.0) {
            add(Level::Warn, format!("icntl = {}: não é um cálculo de transporte normal (0)", icntl));
        }
    } else if card(model, "sdef").is_some() && card(model, "ctme").is_none() {
        add(Level::Warn, "sem NPS nem CTME: a simulação não tem critério de parada".into());
    }
    out
}

/// Resultado da verificação da geometria por amostragem.
pub fn audit_findings(model: &Model, audit: &Audit) -> Vec<Finding> {
    let mut out = Vec::new();
    if audit.overlaps.is_empty() && audit.undefined == 0 {
        out.push(Finding::new(Level::Ok, format!("nenhuma sobreposição nem região sem célula em {} pontos sorteados", audit.samples)));
    }
    if !audit.overlaps.is_empty() {
        out.push(Finding::new(Level::Error, format!("{} pares de células sobrepostas (lista abaixo)", audit.overlaps.len())));
    }
    for ov in audit.overlaps.iter().take(40) {
        let (a, b) = (model.cells[ov.cells.0].id, model.cells[ov.cells.1].id);
        let vol = ov.count as f64 / audit.samples as f64 * audit.box_volume;
        out.push(Finding { level: Level::Error, text: format!("células {} e {} sobrepostas (~{:.3e} cm³)", a, b, vol), at: Some(ov.at) });
    }
    if audit.undefined > 0 {
        let vol = audit.undefined as f64 / audit.samples as f64 * audit.box_volume;
        let at = audit.undefined_at.first().copied();
        out.push(Finding { level: Level::Error, text: format!("região sem nenhuma célula (~{:.3e} cm³): partículas se perdem ali", vol), at });
    }
    let never: Vec<i32> = model.cells.iter().enumerate().filter(|(i, c)| !c.outside && matches!(c.filled, crate::geom::Filled::None) && audit.hits.get(*i) == Some(&0)).map(|(_, c)| c.id).collect();
    if !never.is_empty() {
        out.push(Finding::new(
            Level::Info,
            format!("{} células não receberam nenhum ponto sorteado (muito pequenas, fora da caixa do modelo, ou vazias por engano): {}", never.len(), list(never)),
        ));
    }
    out
}

fn duration(seconds: f64) -> String {
    if seconds >= 2.0 * 86400.0 {
        format!("{:.1} dias", seconds / 86400.0)
    } else if seconds >= 2.0 * 3600.0 {
        format!("{:.1} horas", seconds / 3600.0)
    } else {
        format!("{:.0} minutos", seconds / 60.0)
    }
}

/// O que o `phits.out` conta sobre a rodada.
fn phits_run(src: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let param = |key: &str| {
        lines.iter().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            if k.trim() != key {
                return None;
            }
            v.split('#').next()?.trim().parse::<f64>().ok()
        })
    };
    let planned = param("maxcas").zip(param("maxbch"));
    // "bat[     999] ncas =         999000."
    let last = lines.iter().rev().find(|l| l.starts_with("bat[")).and_then(|l| {
        let batch: f64 = l[4..l.find(']')?].trim().parse().ok()?;
        let histories: f64 = l.split_once("ncas =")?.1.trim().trim_end_matches('.').parse().ok()?;
        Some((batch, histories))
    });
    let finished = src.contains("job termination date");
    let progress = match (last, planned) {
        (Some((_, done)), Some((cas, bch))) => format!("{:e} de {:e} histórias ({:.1}%)", done, cas * bch, 100.0 * done / (cas * bch)),
        (Some((_, done)), None) => format!("{:e} histórias", done),
        _ => "nenhum lote concluído".to_string(),
    };
    if finished {
        out.push(Finding::new(Level::Ok, format!("a rodada terminou: {}", progress)));
    } else {
        let cpu: Vec<f64> = lines.iter().rev().filter_map(|l| l.trim().strip_prefix("cpu time =")?.trim().trim_end_matches("s.").trim().parse().ok()).take(200).collect();
        let left = match (last, planned, cpu.is_empty()) {
            (Some((batch, _)), Some((_, bch)), false) => {
                let per_batch = cpu.iter().sum::<f64>() / cpu.len() as f64;
                format!("; no ritmo dos últimos lotes, faltam ~{} de CPU", duration((bch - batch).max(0.0) * per_batch))
            }
            _ => String::new(),
        };
        out.push(Finding::new(Level::Warn, format!("a saída não registra o fim da rodada (interrompida ou ainda rodando): {}{}", progress, left)));
    }
    // "        5              0.   : detection of geometry error/warn"
    let counter = |what: &str| lines.iter().rev().find(|l| l.contains(what)).and_then(|l| l.split_whitespace().nth(1)?.trim_end_matches('.').parse::<f64>().ok());
    let unrecovered = lines.iter().rev().find_map(|l| l.split_once("Number of unrecovered errors =")?.1.trim().parse::<f64>().ok());
    match (counter("detection of geometry error/warn"), counter("termination by geometry error"), unrecovered) {
        (Some(d), Some(t), u) if d + t + u.unwrap_or(0.0) > 0.0 => out.push(Finding::new(
            Level::Error,
            format!("erros de geometria durante o transporte: {} detectados, {} histórias encerradas por eles, {} não recuperados", d, t, u.unwrap_or(0.0)),
        )),
        (Some(_), Some(_), _) => out.push(Finding::new(Level::Ok, "nenhum erro de geometria durante o transporte")),
        _ => {}
    }
    let mut notes: BTreeMap<String, usize> = BTreeMap::new();
    for l in &lines {
        let low = l.to_lowercase();
        let summary = low.contains("geometry error") || low.contains("unrecovered errors") || low.contains("geometry warning");
        if (low.contains("warning") || low.contains("error")) && !summary && !l.trim_start().starts_with('#') && !l.contains('=') {
            *notes.entry(l.split_whitespace().collect::<Vec<_>>().join(" ")).or_default() += 1;
        }
    }
    for (note, n) in notes.iter().take(25) {
        let times = if *n > 1 { format!(" ({}×)", n) } else { String::new() };
        let level = if note.to_lowercase().contains("error") { Level::Error } else { Level::Warn };
        out.push(Finding::new(level, format!("mensagem do PHITS: {}{}", note, times)));
    }
    out
}

/// O que uma saída (do MCNP, ou o `phits.out`) conta sobre a rodada. `None`
/// se o texto não for uma.
pub fn run_findings(src: &str) -> Option<Vec<Finding>> {
    if src.contains("bat[") && src.contains("ncas =") {
        return Some(phits_run(src));
    }
    if !src.contains("mcnp") || !(src.contains("tally fluctuation") || src.contains("dump no.") || src.contains("warning.")) {
        return None;
    }
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let after = |line: &str, key: &str| line.find(key).map(|i| line[i + key.len()..].trim().to_string());

    match lines.iter().find_map(|l| after(l, "run terminated when")) {
        Some(why) => out.push(Finding::new(Level::Ok, format!("a rodada terminou: {}", why.trim_end_matches('.')))),
        None => {
            let last = lines.iter().rev().find_map(|l| after(l, "nps =").filter(|_| l.contains("dump no."))).and_then(|t| t.split_whitespace().next().map(str::to_string));
            let progress = last.map_or(String::new(), |n| format!(" (último ponto gravado: nps = {})", n));
            out.push(Finding::new(Level::Warn, format!("a saída não registra o fim da rodada: ela foi interrompida ou ainda está rodando{}", progress)));
        }
    }
    if let Some(time) = lines.iter().rev().find_map(|l| after(l, "computer time =")) {
        out.push(Finding::new(Level::Info, format!("tempo de processamento: {}", time)));
    }
    let fatal: BTreeSet<String> = lines.iter().filter_map(|l| after(l, "fatal error.")).collect();
    for f in fatal.iter().take(12) {
        out.push(Finding::new(Level::Error, format!("erro fatal: {}", f)));
    }
    let lost = lines.iter().filter(|l| l.contains("lost particle no.")).count();
    if lost > 0 {
        out.push(Finding::new(Level::Error, format!("{} partículas perdidas: há erro de geometria (veja a lista no painel lateral)", lost)));
    }

    // Os 10 testes estatísticos de cada tally.
    let key = "statistical checks for the estimated answer for the tally fluctuation chart (tfc) bin of tally";
    let mut checks: BTreeMap<i64, usize> = BTreeMap::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(tally) = after(l, key).and_then(|t| t.parse::<i64>().ok()) else { continue };
        let failed = lines[i + 1..]
            .iter()
            .take(16)
            .find_map(|x| after(x, "did not pass"))
            .and_then(|t| t.split_whitespace().next()?.parse::<usize>().ok())
            .unwrap_or(0);
        // A saída repete o quadro a cada impressão: vale o último.
        checks.insert(tally, failed);
    }
    for (tally, failed) in &checks {
        out.push(if *failed == 0 {
            Finding::new(Level::Ok, format!("tally {}: passou nos 10 testes estatísticos", tally))
        } else {
            Finding::new(Level::Warn, format!("tally {}: não passou em {} dos 10 testes estatísticos — o resultado ainda não é confiável", tally, failed))
        });
    }

    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();
    for l in &lines {
        if let Some(w) = after(l, "warning.").filter(|w| !w.contains("did not pass")) {
            *warnings.entry(w.split_whitespace().collect::<Vec<_>>().join(" ")).or_default() += 1;
        }
    }
    for (w, n) in warnings.iter().take(40) {
        let times = if *n > 1 { format!(" ({}×)", n) } else { String::new() };
        // Os que podem mudar o resultado sem dar erro nenhum.
        let (level, why) = if w.contains("random number period exceeded") {
            (Level::Warn, " — as histórias passaram a repetir a sequência de números aleatórios: aumente o período (cartão RAND) ou o resultado tem menos informação do que o nps sugere")
        } else if w.contains("may be wrong") {
            (Level::Warn, " — em tally de energia depositada o volume/massa entra na conta: confira com um cartão SD")
        } else if w.contains("unnormalized fractions") {
            (Level::Info, " — o MCNP normaliza sozinho; só confira se as frações eram mesmo as pretendidas")
        } else {
            (Level::Info, "")
        };
        out.push(Finding::new(level, format!("aviso do MCNP: {}{}{}", w, times, why)));
    }
    Some(out)
}

/// Histórias necessárias para chegar ao erro relativo `target`, supondo que o
/// erro continue caindo com 1/√N a partir do último ponto.
pub fn histories_for(nps: f64, error: f64, target: f64) -> f64 {
    nps * (error / target).powi(2)
}
