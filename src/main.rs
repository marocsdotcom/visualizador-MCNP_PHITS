#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cards;
mod compare;
mod diag;
mod dump;
mod edit;
mod expr;
mod geom;
mod mcnp;
mod mesh;
mod phits;
mod render;
#[cfg(test)]
mod tests;

use eframe::egui;
use glam::DVec3;
use std::path::{Path, PathBuf};

/// Lê um input, detectando o formato (PHITS tem seções `[ ... ]`).
pub fn load_model(path: &Path) -> Result<geom::Model, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // Comentários em Latin-1 são comuns; a geometria é sempre ASCII.
    let src = String::from_utf8_lossy(&bytes);
    if let Some(input) = mcnp::input_from_output(&src) {
        let mut m = parse_text(&input, path)?;
        m.warnings.insert(0, "arquivo de saída do MCNP: geometria lida do eco do input".into());
        m.lost = mcnp::lost_particles(&src);
        m.run = diag::run_findings(&src).unwrap_or_default();
        return Ok(m);
    }
    parse_text(&src, path)
}

/// Lê um input já em texto; `path` só serve para achar os arquivos incluídos.
pub fn parse_text(src: &str, path: &Path) -> Result<geom::Model, String> {
    let mut model = if phits::looks_like_phits(src) { phits::parse(src, path.parent()) } else { mcnp::parse(src) };
    if model.cells.is_empty() {
        return Err("nenhuma célula encontrada (o arquivo é um input MCNP ou PHITS?)".into());
    }
    model.refine_bbox();
    model.source = src.to_string();
    Ok(model)
}

pub fn save_png(path: &str, frame: &render::Frame) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.w as u32, frame.h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(&frame.rgba)).map_err(|e| e.to_string())
}

/// A imagem codificada em PNG, na memória.
pub fn png_bytes(frame: &render::Frame) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, frame.w as u32, frame.h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = enc.write_header() {
        let _ = writer.write_image_data(&frame.rgba);
    }
    out
}

/// Grava o modelo amostrado numa grade regular (célula, material e densidade
/// em cada elemento) no formato VTK legado, que o ParaView e o VisIt abrem.
pub fn export_vtk(model: &geom::Model, path: &Path) -> Result<(), String> {
    use rayon::prelude::*;
    use std::io::Write;
    const LONGEST: f64 = 160.0;
    let (lo, hi) = model.bbox;
    let size = hi - lo;
    let step = size.max_element() / LONGEST;
    let n = [0, 1, 2].map(|m| ((size[m] / step).round() as usize).max(1));
    let cells: Vec<(i32, i32, f32)> = (0..n[2] * n[1])
        .into_par_iter()
        .flat_map_iter(|row| {
            let (j, k) = (row % n[1], row / n[1]);
            let (mut hint, mut levels) = (geom::Hint::default(), Vec::new());
            (0..n[0])
                .map(|i| {
                    let p = lo + step * DVec3::new(i as f64 + 0.5, j as f64 + 0.5, k as f64 + 0.5);
                    match model.descend(p, DVec3::X, &mut hint, &mut levels) {
                        Some(f) => {
                            let c = &model.cells[f.leaf];
                            (c.id, c.mat, c.rho.abs() as f32)
                        }
                        None => (-1, -1, 0.0),
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    let mut write = || -> std::io::Result<()> {
        writeln!(out, "# vtk DataFile Version 3.0")?;
        writeln!(out, "geometria amostrada pelo visualizador MCNP/PHITS")?;
        writeln!(out, "ASCII\nDATASET STRUCTURED_POINTS")?;
        writeln!(out, "DIMENSIONS {} {} {}", n[0] + 1, n[1] + 1, n[2] + 1)?;
        writeln!(out, "ORIGIN {} {} {}", lo.x, lo.y, lo.z)?;
        writeln!(out, "SPACING {0} {0} {0}", step)?;
        writeln!(out, "CELL_DATA {}", cells.len())?;
        writeln!(out, "SCALARS celula int 1\nLOOKUP_TABLE default")?;
        cells.iter().try_for_each(|c| writeln!(out, "{}", c.0))?;
        writeln!(out, "SCALARS material int 1\nLOOKUP_TABLE default")?;
        cells.iter().try_for_each(|c| writeln!(out, "{}", c.1))?;
        writeln!(out, "SCALARS densidade float 1\nLOOKUP_TABLE default")?;
        cells.iter().try_for_each(|c| writeln!(out, "{}", c.2))?;
        out.flush()
    };
    write().map_err(|e| e.to_string())
}

/// Modo sem janela: grava os três cortes centrais e uma vista 3D em PNG.
fn export(path: &Path, prefix: &str, size: usize) -> Result<(), String> {
    let started = std::time::Instant::now();
    let model = load_model(path)?;
    println!("{}", model.title);
    println!("leitura: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    println!("{} células, {} superfícies, {} universos", model.cells.len(), model.surfaces.len(), model.universes.len());
    println!("caixa envolvente: {:?} a {:?}", model.bbox.0.to_array(), model.bbox.1.to_array());
    for w in &model.warnings {
        println!("aviso: {}", w);
    }
    // VIS_COR=importancia pinta pela importância em vez do material.
    let by = if std::env::var("VIS_COR").is_ok_and(|v| v == "importancia") { render::ColorBy::Importance } else { render::ColorBy::Material };
    let mut imps: Vec<String> = model.cells.iter().map(|c| c.imp.to_string()).collect();
    imps.sort();
    imps.dedup();
    println!("importâncias: {}", imps.join(" "));
    let pal = render::Palette::new(&model, by);
    let go = std::sync::atomic::AtomicBool::new(false);
    // VIS_MALHA=<arquivo de resultados> pinta um resultado sobre os cortes
    // (VIS_INDICE escolhe qual, a partir de 0) e centra os cortes nele.
    let mut focus: Option<(DVec3, DVec3)> = None;
    let overlay = match std::env::var("VIS_MALHA") {
        Ok(file) => {
            let text = std::fs::read(&file).map_err(|e| e.to_string())?;
            let mut fields = mesh::load_results(&String::from_utf8_lossy(&text), &file, &model)?;
            for f in &fields {
                println!("resultado {}: {:?}", f.name(), f.positive_range());
            }
            let pick = std::env::var("VIS_INDICE").ok().and_then(|v| v.parse().ok()).unwrap_or(0usize);
            let field = fields.swap_remove(pick.min(fields.len() - 1));
            let (min, max) = field.positive_range().ok_or("resultado sem valores positivos")?;
            focus = field.bounds();
            Some(render::Overlay { field: std::sync::Arc::new(field), lo: min.max(max * 1e-4), hi: max, log: true, alpha: 0.8, error: false })
        }
        Err(_) => None,
    };
    if let Some(at) = std::env::var("VIS_FOCO").ok().and_then(|v| {
        let n: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
        (n.len() == 6).then(|| (DVec3::new(n[0], n[1], n[2]), DVec3::new(n[3], n[4], n[5])))
    }) {
        focus = Some(at);
    }
    let (lo, hi) = focus.unwrap_or(model.bbox);
    let mid = (lo + hi) / 2.0;
    for (name, u, v) in [("xy", 0, 1), ("xz", 0, 2), ("yz", 1, 2)] {
        let view = render::SliceView {
            center: mid,
            u: DVec3::AXES[u],
            v: DVec3::AXES[v],
            half_width: 0.55 * (hi[u] - lo[u]).max(hi[v] - lo[v]),
        };
        let started = std::time::Instant::now();
        let frame = render::render_slice(&model, &pal, &view, size, size, true, true, overlay.as_ref(), &go).unwrap();
        println!(
            "corte {}: {:.0} ms, {} px sem célula, {} px com sobreposição",
            name,
            started.elapsed().as_secs_f64() * 1000.0,
            frame.undefined,
            frame.overlapping
        );
        save_png(&format!("{}_{}.png", prefix, name), &frame)?;
    }
    let started = std::time::Instant::now();
    let audit = model.audit(2_000_000);
    println!(
        "verificação: {:.0} ms, {} pontos, {} sem célula, {} pares sobrepostos",
        started.elapsed().as_secs_f64() * 1000.0,
        audit.samples,
        audit.undefined,
        audit.overlaps.len()
    );
    for ov in audit.overlaps.iter().take(10) {
        let (a, b) = (model.cells[ov.cells.0].id, model.cells[ov.cells.1].id);
        println!("  células {} e {}: {} pontos, por exemplo em {:?}", a, b, ov.count, ov.at.to_array());
    }
    for p in audit.undefined_at.iter().take(4) {
        println!("  sem célula em {:?}", p.to_array());
    }
    println!("partículas perdidas na saída: {}", model.lost.len());
    let tag = |l: diag::Level| ["ok", "info", "ATENÇÃO", "ERRO"][l as usize];
    for f in diag::input_findings(&model).iter().chain(&diag::audit_findings(&model, &audit)).chain(&model.run) {
        println!("diagnóstico [{}] {}", tag(f.level), f.text);
    }
    if let Ok(file) = std::env::var("VIS_DUMP") {
        let started = std::time::Instant::now();
        let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
        let parts = dump::parse(&bytes, &file).ok_or("não é um dump do PHITS")?;
        println!("dump: {} partículas em {:.0} ms", parts.list.len(), started.elapsed().as_secs_f64() * 1000.0);
        for (kf, (n, w, we)) in &parts.by_kind {
            println!("  {}: {} (peso {:.4e}, energia média {:.4} MeV)", dump::kind_name(*kf), n, w, we / w);
        }
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for p in &parts.list {
            (lo, hi) = (lo.min(p.pos()), hi.max(p.pos()));
        }
        println!("  posições de {:?} a {:?}", lo.to_array(), hi.to_array());
    }
    if std::env::var_os("VIS_VTK").is_some() {
        let started = std::time::Instant::now();
        export_vtk(&model, Path::new(&format!("{}.vtk", prefix)))?;
        println!("vtk: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
    let (lo, hi) = model.bbox;
    let mid = (lo + hi) / 2.0;
    let visible: Vec<bool> = model.cells.iter().map(|c| !c.outside && c.mat != 0 && !c.is_gas()).collect();
    // VIS_VIDRO=<material> desenha esse material translúcido.
    let glass: Option<i32> = std::env::var("VIS_VIDRO").ok().and_then(|v| v.parse().ok());
    let translucent: Vec<bool> = model.cells.iter().map(|c| Some(c.mat) == glass).collect();
    let cam = render::Camera { target: mid, yaw: -1.0, pitch: 0.5, dist: 1.9 * (hi - lo).length(), fov_deg: 32.0 };
    for (name, clip) in [("3d", None), ("3d_corte", Some((-DVec3::Y, -mid.y)))] {
        let scene = render::Scene { model: &model, pal: &pal, visible: &visible, translucent: &translucent, clip, edges: true };
        let started = std::time::Instant::now();
        let frame = render::render_3d(&scene, &cam, size, size, &go).unwrap();
        println!("{}: {:.0} ms", name, started.elapsed().as_secs_f64() * 1000.0);
        save_png(&format!("{}_{}.png", prefix, name), &frame)?;
    }
    Ok(())
}

fn print_diff(old: &Path, new: &Path) -> Result<(), String> {
    let (a, b) = (load_model(old)?, load_model(new)?);
    let diff = compare::inputs(&a, &b);
    println!("de {} para {}: {} diferenças", old.display(), new.display(), diff.len());
    for (title, list) in [("células", &diff.cells), ("superfícies", &diff.surfaces), ("materiais", &diff.materials), ("dados", &diff.data)] {
        println!("{}: {}", title, list.len());
        for c in list.iter().take(40) {
            let clip = |t: &str| t.chars().take(110).collect::<String>();
            println!("  [{:?}] {}\n      antes: {}\n      agora: {}", c.kind, c.what, clip(&c.before), clip(&c.after));
        }
    }
    Ok(())
}

/// `--mover "701 702=0,0,-10;tr21=0,0,-10" --saida novo.i`: as mesmas
/// edições da janela, na ordem dada, gravadas num arquivo novo.
fn move_cli(input: &Path, spec: &str, out: &Path) -> Result<(), String> {
    if std::path::absolute(input).ok() == std::path::absolute(out).ok() {
        return Err("a saída tem que ser outro arquivo: o original não é sobrescrito".into());
    }
    let mut model = load_model(input)?;
    for step in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (what, by) = step.split_once('=').ok_or_else(|| format!("'{}': esperado <superfícies ou trN>=dx,dy,dz", step))?;
        let v: Vec<f64> = by.split(',').map(|t| t.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|_| format!("'{}': deslocamento inválido", by))?;
        let [dx, dy, dz] = v[..] else { return Err(format!("'{}': o deslocamento tem três números", by)) };
        let d = DVec3::new(dx, dy, dz);
        let what = what.trim().to_lowercase();
        let text = match what.strip_prefix("tr") {
            Some(n) => edit::move_transform(&model.source, &model, n.parse().map_err(|_| format!("'{}': número de TR inválido", what))?, d)?,
            None => {
                let ids: Vec<i32> = what.split_whitespace().map(|t| t.parse::<i32>()).collect::<Result<_, _>>().map_err(|_| format!("'{}': números de superfície inválidos", what))?;
                edit::move_surfaces(&model.source, &model, &ids, d)?
            }
        };
        model = parse_text(&text, input)?;
        println!("{} deslocado de {:?} cm", what, d.to_array());
    }
    std::fs::write(out, &model.source).map_err(|e| e.to_string())?;
    let audit = model.audit(1_000_000);
    println!("gravado {}; verificação: {} pares sobrepostos, {} pontos sem célula", out.display(), audit.overlaps.len(), audit.undefined);
    for ov in audit.overlaps.iter().take(10) {
        println!("  células {} e {}, por exemplo em {:?}", model.cells[ov.cells.0].id, model.cells[ov.cells.1].id, ov.at.to_array());
    }
    Ok(())
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let values: Vec<String> = ["--png", "--tamanho", "--relatorio", "--resultados", "--comparar", "--mover", "--saida"].iter().filter_map(|f| flag(f)).collect();
    let file = args.iter().find(|a| !a.starts_with("--") && !values.contains(a));
    let file = file.map(PathBuf::from);

    // --relatorio <saida.html> [--resultados "a;b;c"]: gera o relatório sem abrir a janela.
    if let Some(out) = flag("--relatorio") {
        let results = flag("--resultados").map_or(Vec::new(), |r| r.split(';').map(PathBuf::from).collect());
        let result = file.ok_or_else(|| "informe o arquivo de input".to_string()).and_then(|f| app::headless_report(f, results, Path::new(&out)));
        if let Err(e) = result {
            eprintln!("erro: {}", e);
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(spec) = flag("--mover") {
        let out = flag("--saida");
        let result = match (file, out) {
            (Some(f), Some(o)) => move_cli(&f, &spec, Path::new(&o)),
            _ => Err("uso: <input> --mover \"701 702=0,0,-10;tr21=0,0,-10\" --saida <novo arquivo>".into()),
        };
        if let Err(e) = result {
            eprintln!("erro: {}", e);
            std::process::exit(1);
        }
        return Ok(());
    }
    // --comparar <outro input>: lista o que mudou do outro para o informado.
    if let Some(other) = flag("--comparar") {
        let result = file.ok_or_else(|| "informe o arquivo de input".to_string()).and_then(|f| print_diff(Path::new(&other), &f));
        if let Err(e) = result {
            eprintln!("erro: {}", e);
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(prefix) = flag("--png") {
        let size = flag("--tamanho").and_then(|s| s.parse().ok()).unwrap_or(900);
        let result = file.ok_or_else(|| "informe o arquivo de input".to_string()).and_then(|f| export(&f, &prefix, size));
        if let Err(e) = result {
            eprintln!("erro: {}", e);
            std::process::exit(1);
        }
        return Ok(());
    }

    // Com janela, --resultados "a;b;c" já abre os resultados sobre o input.
    let results: Vec<PathBuf> = flag("--resultados").map_or(Vec::new(), |r| r.split(';').map(PathBuf::from).collect());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Visualizador de geometria MCNP / PHITS")
            .with_inner_size([1280.0, 760.0])
            .with_maximized(true)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("visualizador-mcnp", options, Box::new(move |cc| Ok(Box::new(app::App::new(file, cc.egui_ctx.clone()).with_results(results)))))
}
