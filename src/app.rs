//! Interface: painel lateral com células e materiais, corte 2D e vista 3D
//! lado a lado.

use crate::compare;
use crate::diag::{self, Finding, Level as Severity, Source};
use crate::dump::{self, Particles};
use crate::geom::{Audit, Level, Model};
use crate::mesh::Field;
use crate::render::{self, Camera, ColorBy, Frame, Overlay, Palette, Scene, SliceView, NONE};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions};
use glam::DVec3;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

const SELECTED_RGB: [u8; 3] = [255, 226, 40];
/// Divisor da resolução da prévia que aparece antes da imagem final.
const PREVIEW_DIV: usize = 4;
const MAX_PIXELS: usize = 1800;
/// Pontos sorteados por "Verificar modelo".
const AUDIT_SAMPLES: u64 = 3_000_000;

/// Quantos arquivos a lista de recentes guarda.
const RECENT_MAX: usize = 12;

/// Arquivo com a lista de inputs abertos recentemente, um caminho por linha.
fn recent_file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("visualizador-mcnp").join("recentes.txt"))
}

fn load_recent() -> Vec<PathBuf> {
    let text = recent_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).take(RECENT_MAX).collect()
}

/// Registro de eventos para depuração: com a variável de ambiente `VIS_LOG`
/// apontando para um arquivo, cada leitura, verificação e renderização
/// iniciada ganha uma linha com o instante. Sem ela não faz nada.
fn trace(what: impl FnOnce() -> String) {
    use std::io::Write;
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let Some(path) = std::env::var_os("VIS_LOG") else { return };
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{:9.3} {}", t, what());
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plane {
    Xy,
    Xz,
    Yz,
}

impl Plane {
    /// (u, v, eixo normal)
    fn axes(self) -> (usize, usize, usize) {
        match self {
            Plane::Xy => (0, 1, 2),
            Plane::Xz => (0, 2, 1),
            Plane::Yz => (1, 2, 0),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Plane::Xy => "XY",
            Plane::Xz => "XZ",
            Plane::Yz => "YZ",
        }
    }
}

const AXIS_NAMES: [&str; 3] = ["X", "Y", "Z"];

/// Tudo o que determina a imagem de um painel; se mudar, renderiza de novo.
#[derive(Clone, PartialEq)]
struct RenderKey {
    generation: u64,
    size: [usize; 2],
    slice: SliceView,
    cam: Camera,
    clip: Option<(DVec3, f64)>,
    edges: bool,
    check: bool,
}

/// Imagem pedida a uma thread de renderização.
struct Pending {
    key: RenderKey,
    cancel: Arc<AtomicBool>,
    /// (imagem, é a final?, tempo em ms)
    rx: mpsc::Receiver<(Frame, bool, f32)>,
    delivered: bool,
}

#[derive(Default)]
struct Pane {
    tex: Option<TextureHandle>,
    frame: Frame,
    /// Vista da imagem exibida, e se ela já está na resolução final.
    shown: Option<RenderKey>,
    complete: bool,
    pending: Option<Pending>,
    millis: f32,
}

impl Pane {
    /// Mantém a imagem em dia com `key` sem travar a interface: `job(w, h,
    /// final, cancel)` roda em outra thread, primeiro numa prévia grosseira e
    /// depois na resolução pedida. Se a vista muda no meio do caminho, a
    /// imagem final é abandonada, mas a prévia sempre chega, para que arrastar
    /// a vista dê retorno contínuo.
    fn sync<F>(&mut self, ctx: &egui::Context, name: &str, key: RenderKey, job: F)
    where
        F: Fn(usize, usize, bool, &AtomicBool) -> Option<Frame> + Send + 'static,
    {
        let mut finished = false;
        if let Some(p) = &mut self.pending {
            loop {
                match p.rx.try_recv() {
                    Ok((frame, full, millis)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied([frame.w, frame.h], &frame.rgba);
                        match &mut self.tex {
                            Some(t) => t.set(img, TextureOptions::LINEAR),
                            None => self.tex = Some(ctx.load_texture(name, img, TextureOptions::LINEAR)),
                        }
                        self.frame = frame;
                        self.shown = Some(p.key.clone());
                        self.complete = full;
                        self.millis = millis;
                        p.delivered = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.pending = None;
        }
        let start = match &self.pending {
            Some(p) if p.key == key => false,
            Some(p) => {
                p.cancel.store(true, Ordering::Relaxed);
                p.delivered
            }
            None => !(self.complete && self.shown.as_ref() == Some(&key)),
        };
        if !start {
            return;
        }
        trace(|| format!("renderiza {} {:?}", name, key.size));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (ctx2, flag, [w, h]) = (ctx.clone(), cancel.clone(), key.size);
        std::thread::spawn(move || {
            let millis = |t: Instant| t.elapsed().as_secs_f32() * 1000.0;
            if w * h > 200 * 200 {
                let (started, never) = (Instant::now(), AtomicBool::new(false));
                if let Some(f) = job((w / PREVIEW_DIV).max(8), (h / PREVIEW_DIV).max(8), false, &never) {
                    let _ = tx.send((f, false, millis(started)));
                    ctx2.request_repaint();
                }
            }
            let started = Instant::now();
            if let Some(f) = job(w, h, true, &flag) {
                let _ = tx.send((f, true, millis(started)));
            }
            drop(tx);
            ctx2.request_repaint();
        });
        self.pending = Some(Pending { key, cancel, rx, delivered: false });
    }

    /// Escreve o andamento no canto da imagem. Fica fora da barra do painel de
    /// propósito: lá, a troca de texto mudava a largura do painel em um pixel
    /// e cada imagem pronta pedia outra, sem fim.
    fn draw_status(&self, painter: &egui::Painter, rect: Rect, color: Color32) {
        let text = if self.complete && self.pending.is_none() {
            format!("{:.0} ms", self.millis)
        } else {
            "renderizando…".into()
        };
        let pos = rect.right_top() + egui::vec2(-10.0, 8.0);
        painter.text(pos, egui::Align2::RIGHT_TOP, text, egui::FontId::proportional(12.0), color);
    }
}

pub struct App {
    ctx: egui::Context,
    model: Option<Arc<Model>>,
    path: Option<PathBuf>,
    error: Option<String>,
    /// Arquivo sendo lido em outra thread, e se a vista deve ser reenquadrada.
    /// O terceiro campo diz se é a releitura de um texto editado.
    loading: Option<(PathBuf, bool, bool, mpsc::Receiver<Result<Model, String>>)>,
    /// Incrementado sempre que modelo, cores ou visibilidade mudam.
    generation: u64,
    color_by: ColorBy,
    palette: Arc<Palette>,
    visible: Arc<Vec<bool>>,
    selected: Option<usize>,
    hover: String,
    /// Texto do filtro da lista de células.
    filter: String,
    audit: Option<Audit>,
    auditing: Option<mpsc::Receiver<Audit>>,
    /// Recarrega o arquivo quando ele muda no disco.
    auto_reload: bool,
    mtime: Option<SystemTime>,
    last_poll: Instant,

    /// Resultados carregados (malhas e tallies de célula) e como são pintados
    /// sobre o corte.
    meshes: Vec<Arc<Field>>,
    mesh_sel: usize,
    mesh_show: bool,
    mesh_log: bool,
    mesh_error: bool,
    mesh_range: [f64; 2],
    mesh_alpha: f32,
    profile_open: bool,
    /// No perfil, divide cada curva pelo seu máximo (para comparar formas).
    profile_norm: bool,
    /// Espaço de fase carregado e como é desenhado no corte.
    particles: Option<Arc<Particles>>,
    part_show: bool,
    /// Só desenha as partículas a até esta distância do plano do corte (0 = todas).
    part_slab: f64,
    part_max: usize,

    /// Diagnóstico: achados do input, da verificação da geometria e da saída.
    diag_open: bool,
    findings: Vec<Finding>,
    audit_notes: Vec<Finding>,
    run: Vec<Finding>,
    source: Option<Source>,
    /// Verifica a geometria sozinho a cada arquivo aberto.
    auto_audit: bool,
    conv_open: bool,
    /// Erro relativo desejado na estimativa de histórias que faltam.
    conv_target: f64,

    /// Destaca células sobrepostas no corte.
    check_2d: bool,
    /// Pontos da régua (no máximo dois).
    measure: Vec<DVec3>,
    plane: Plane,
    slice_pos: f64,
    center: [f64; 2],
    half_width: f64,
    /// Inclinação do plano do corte, em graus: em torno do eixo vertical da
    /// imagem e depois em torno do horizontal.
    tilt: [f64; 2],
    edges_2d: bool,
    /// Proporção largura/altura dos painéis no último quadro, para enquadrar.
    aspect_2d: f64,
    aspect_3d: f64,
    fit_pending: [bool; 2],
    pane_2d: Pane,
    /// Mostra, abaixo do corte, os outros dois planos pelo mesmo ponto.
    three_views: bool,
    minis: [Pane; 2],
    /// Área de cada painel na tela (corte e 3D), para recortar a captura.
    pane_rects: [Rect; 2],
    /// Captura de tela pedida: a área a recortar e o nome sugerido.
    shot: Option<(Rect, &'static str)>,
    recent: Vec<PathBuf>,
    mats_open: bool,
    tally_open: bool,
    /// Edição da geometria: texto antes da primeira edição, e as edições
    /// (descrição, texto resultante).
    edit_open: bool,
    edit_base: String,
    edits: Vec<(String, String)>,
    /// 0 = mover superfícies, 1 = mover uma TR.
    edit_mode: u8,
    edit_targets: Vec<i32>,
    edit_tr: Option<i32>,
    edit_shift: [f64; 3],
    edit_input: String,
    /// Soma dos tallies entre os mctal carregados, e se a tabela mostra ela.
    tally_sum: Option<crate::mesh::Sum>,
    tally_total: bool,
    /// Resultados pedidos na linha de comando, à espera do modelo.
    pending_results: Vec<PathBuf>,
    /// Ordem da tabela de tallies: 0 = do arquivo, 1 = valor, 2 = erro.
    tally_sort: u8,
    /// Outro input com que o aberto é comparado, e as diferenças.
    diff_base: Option<(PathBuf, Arc<Model>)>,
    diff_loading: Option<(PathBuf, mpsc::Receiver<Result<Model, String>>)>,
    diff: Option<compare::Diff>,
    diff_open: bool,
    /// Critério do índice gama no perfil: dose (%) e distância (mm).
    gamma_tol: [f64; 2],

    cam: Camera,
    hide_void: bool,
    hide_gas: bool,
    hidden_mats: BTreeSet<i32>,
    translucent_mats: BTreeSet<i32>,
    translucent: Arc<Vec<bool>>,
    clip_at_slice: bool,
    clip_flip: bool,
    edges_3d: bool,
    pane_3d: Pane,
}

impl App {
    pub fn new(path: Option<PathBuf>, ctx: egui::Context) -> Self {
        let mut app = App {
            ctx,
            model: None,
            path: None,
            error: None,
            loading: None,
            generation: 0,
            color_by: ColorBy::Material,
            palette: Arc::new(Palette { rgb: Vec::new() }),
            visible: Arc::new(Vec::new()),
            selected: None,
            hover: String::new(),
            filter: String::new(),
            audit: None,
            auditing: None,
            auto_reload: true,
            mtime: None,
            last_poll: Instant::now(),
            meshes: Vec::new(),
            mesh_sel: 0,
            mesh_show: true,
            mesh_log: true,
            mesh_error: false,
            mesh_range: [0.0, 1.0],
            mesh_alpha: 0.7,
            profile_open: false,
            profile_norm: true,
            particles: None,
            part_show: true,
            part_slab: 0.0,
            part_max: 8000,
            diag_open: false,
            findings: Vec::new(),
            audit_notes: Vec::new(),
            run: Vec::new(),
            source: None,
            auto_audit: true,
            conv_open: false,
            conv_target: 0.01,
            check_2d: false,
            measure: Vec::new(),
            plane: Plane::Xy,
            slice_pos: 0.0,
            center: [0.0; 2],
            half_width: 10.0,
            tilt: [0.0; 2],
            edges_2d: true,
            aspect_2d: 1.0,
            aspect_3d: 1.0,
            fit_pending: [false; 2],
            pane_2d: Pane::default(),
            three_views: false,
            minis: Default::default(),
            pane_rects: [Rect::NOTHING; 2],
            shot: None,
            recent: load_recent(),
            mats_open: false,
            tally_open: false,
            edit_open: false,
            edit_base: String::new(),
            edits: Vec::new(),
            edit_mode: 0,
            edit_targets: Vec::new(),
            edit_tr: None,
            edit_shift: [0.0; 3],
            edit_input: String::new(),
            tally_sum: None,
            tally_total: true,
            pending_results: Vec::new(),
            tally_sort: 0,
            diff_base: None,
            diff_loading: None,
            diff: None,
            diff_open: false,
            gamma_tol: [2.0, 2.0],
            cam: Camera { target: DVec3::ZERO, yaw: -1.0, pitch: 0.5, dist: 50.0, fov_deg: 32.0 },
            hide_void: true,
            hide_gas: true,
            hidden_mats: BTreeSet::new(),
            translucent_mats: BTreeSet::new(),
            translucent: Arc::new(Vec::new()),
            clip_at_slice: false,
            clip_flip: false,
            edges_3d: true,
            pane_3d: Pane::default(),
        };
        if let Some(p) = path {
            app.open(p, true);
        }
        app
    }

    /// Resultados a carregar assim que o input terminar de ser lido.
    pub fn with_results(mut self, results: Vec<PathBuf>) -> Self {
        self.pending_results = results;
        self
    }

    /// Lê o arquivo em outra thread: um fantoma voxelizado leva segundos.
    fn open(&mut self, path: PathBuf, reset_view: bool) {
        trace(|| format!("abre {} (reenquadra: {})", path.display(), reset_view));
        let (tx, rx) = mpsc::channel();
        let (ctx, p) = (self.ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::load_model(&p));
            ctx.request_repaint();
        });
        self.loading = Some((path, reset_view, false, rx));
    }

    fn poll_loading(&mut self) {
        let Some((_, _, _, rx)) = &self.loading else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("falha inesperada ao ler o arquivo".into()),
        };
        let (path, reset_view, from_text, _) = self.loading.take().unwrap();
        match result {
            Ok(mut model) => {
                if from_text {
                    // O texto editado não traz o que só a saída do MCNP tinha.
                    if let Some(old) = &self.model {
                        (model.lost, model.run) = (old.lost.clone(), old.run.clone());
                    }
                } else {
                    // Arquivo lido do disco: as edições anteriores não valem mais.
                    self.edits.clear();
                }
                self.error = None;
                // Ao recarregar, a seleção acompanha o número da célula.
                let old_id = self.selected.and_then(|s| Some(self.model.as_ref()?.cells.get(s)?.id));
                self.selected = old_id.filter(|_| !reset_view).and_then(|id| model.cell_index.get(&id).copied());
                if reset_view {
                    self.hidden_mats.clear();
                    self.translucent_mats.clear();
                    self.measure.clear();
                    self.tilt = [0.0; 2];
                }
                (self.audit, self.auditing) = (None, None);
                self.mtime = modified(&path);
                self.findings = diag::input_findings(&model);
                self.source = diag::source(&model);
                self.run = model.run.clone();
                self.audit_notes.clear();
                self.model = Some(Arc::new(model));
                self.update_diff();
                self.remember(&path);
                self.path = Some(path);
                // O enquadramento espera o primeiro quadro, quando a
                // proporção dos painéis já é conhecida.
                self.fit_pending = [reset_view; 2];
                self.refresh();
                if self.auto_audit {
                    self.start_audit(true);
                }
                let pending = std::mem::take(&mut self.pending_results);
                if !pending.is_empty() {
                    self.add_results(pending);
                }
            }
            Err(e) => {
                if from_text {
                    self.edits.pop();
                }
                self.error = Some(format!("{}: {}", path.display(), e));
            }
        }
    }

    /// Recarrega sozinho quando o arquivo é salvo no editor.
    fn poll_file(&mut self) {
        // Com edições não salvas, recarregar do disco as perderia.
        if !self.auto_reload || self.loading.is_some() || !self.edits.is_empty() || self.last_poll.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_poll = Instant::now();
        let Some(path) = self.path.clone() else { return };
        let now = modified(&path);
        if now.is_some() && now != self.mtime {
            // Já anota a data nova: se a leitura falhar, não fica tentando.
            self.mtime = now;
            self.open(path, false);
        }
    }

    /// Abre um ou mais arquivos de resultado e os acrescenta à lista.
    fn open_results(&mut self) {
        if self.model.is_none() {
            self.error = Some("abra primeiro o input: os resultados são situados sobre a geometria dele".into());
            return;
        }
        let dialog = rfd::FileDialog::new().set_title("Abrir resultados (mctal ou meshtal do MCNP; saída de tally ou dump do PHITS)");
        let Some(paths) = dialog.pick_files() else { return };
        self.add_results(paths);
    }

    fn add_results(&mut self, paths: Vec<PathBuf>) {
        let Some(model) = self.model.clone() else { return };
        // A soma entre arquivos é refeita no fim, com os que entrarem agora.
        self.meshes.retain(|f| !f.is_sum());
        self.mesh_sel = self.mesh_sel.min(self.meshes.len().saturating_sub(1));
        let (first_new, mut errors) = (self.meshes.len(), Vec::new());
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{}: {}", name, e));
                    continue;
                }
            };
            // Um dump binário tem zeros logo no começo; texto nunca tem.
            let binary = bytes.iter().take(256).any(|b| *b == 0);
            if !binary {
                // Uma saída do MCNP não tem resultado para pintar, mas conta como foi a rodada.
                if let Some(run) = diag::run_findings(&String::from_utf8_lossy(&bytes)) {
                    (self.run, self.diag_open) = (run, true);
                    continue;
                }
            }
            let fields = if binary { Err("arquivo binário não reconhecido".to_string()) } else { crate::mesh::load_results(&String::from_utf8_lossy(&bytes), &name, &model) };
            match fields {
                Ok(fields) => self.meshes.extend(fields.into_iter().map(Arc::new)),
                Err(e) => match dump::parse(&bytes, &name) {
                    Some(parts) => (self.particles, self.part_show) = (Some(Arc::new(parts)), true),
                    None => errors.push(format!("{}: {}", name, e)),
                },
            }
        }
        self.error = (!errors.is_empty()).then(|| errors.join("\n"));
        if self.meshes.len() > first_new {
            (self.mesh_sel, self.mesh_show) = (first_new, true);
            // Vários tallies de um valor só (dose por órgão): a tabela é o jeito de lê-los.
            self.tally_open |= self.meshes[first_new..].iter().filter(|f| f.single().is_some()).count() > 1;
            self.mesh_auto_range();
            self.generation += 1;
        }
        self.tally_sum = crate::mesh::weighted_sum(&self.meshes);
        if let Some(field) = self.tally_sum.as_mut().and_then(|s| s.field.take()) {
            self.meshes.push(Arc::new(field));
            (self.mesh_sel, self.mesh_show, self.tally_open) = (self.meshes.len() - 1, true, true);
            self.mesh_auto_range();
            self.generation += 1;
        }
    }

    /// Leva o corte até a malha selecionada e estende a régua ao longo do
    /// maior lado dela, que é a direção de um PDD ou de um perfil.
    fn frame_mesh(&mut self) {
        let Some((lo, hi)) = self.meshes.get(self.mesh_sel).and_then(|m| m.bounds()) else { return };
        let (mid, size) = ((lo + hi) / 2.0, hi - lo);
        let long = (0..3).max_by(|&a, &b| size[a].total_cmp(&size[b])).unwrap();
        // O corte tem que conter a direção longa.
        if self.plane.axes().2 == long {
            self.plane = if long == 2 { Plane::Xz } else { Plane::Xy };
        }
        self.tilt = [0.0; 2];
        self.go_to(mid);
        self.half_width = 0.6 * size[long].max(1e-6);
        let (mut a, mut b) = (mid, mid);
        (a[long], b[long]) = (lo[long], hi[long]);
        self.measure = vec![a, b];
    }

    /// Escala de cores cobrindo os valores da malha (quatro décadas, se logarítmica).
    fn mesh_auto_range(&mut self) {
        let Some((lo, hi)) = self.meshes.get(self.mesh_sel).and_then(|m| m.positive_range()) else { return };
        self.mesh_range = if self.mesh_log { [lo.max(hi * 1e-4), hi] } else { [0.0, hi] };
    }

    fn overlay(&self) -> Option<Overlay> {
        let field = self.meshes.get(self.mesh_sel).filter(|_| self.mesh_show)?.clone();
        let [lo, hi] = self.mesh_range;
        Some(Overlay { field, lo, hi, log: self.mesh_log, alpha: self.mesh_alpha, error: self.mesh_error })
    }

    fn export_vtk(&mut self) {
        let Some(model) = self.model.clone() else { return };
        let dialog = rfd::FileDialog::new().set_title("Exportar geometria amostrada").add_filter("VTK", &["vtk"]).set_file_name("geometria.vtk");
        let Some(path) = dialog.save_file() else { return };
        match crate::export_vtk(&model, &path) {
            Ok(()) => self.hover = format!("gravado {}", path.display()),
            Err(e) => self.error = Some(format!("{}: {}", path.display(), e)),
        }
    }

    /// `quick` sorteia um terço dos pontos: é a verificação automática, que
    /// roda a cada arquivo aberto ou salvo e não pode ocupar a máquina.
    fn start_audit(&mut self, quick: bool) {
        let samples = if quick { AUDIT_SAMPLES / 3 } else { AUDIT_SAMPLES };
        trace(|| format!("verifica a geometria ({} pontos)", samples));
        let Some(model) = self.model.clone() else { return };
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(model.audit(samples));
            ctx.request_repaint();
        });
        self.auditing = Some(rx);
    }

    fn poll_audit(&mut self) {
        let Some(rx) = &self.auditing else { return };
        match rx.try_recv() {
            Ok(a) => {
                self.audit_notes = self.model.as_ref().map_or(Vec::new(), |m| diag::audit_findings(m, &a));
                (self.audit, self.auditing) = (Some(a), None);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.auditing = None,
        }
    }

    /// Põe o centro do corte (no plano atual) em `p`.
    fn set_center(&mut self, p: DVec3) {
        let (u, v, n) = self.plane.axes();
        self.center = [p[u], p[v]];
        self.slice_pos = p[n];
    }

    /// Leva o corte e o alvo da câmera até `p`.
    fn go_to(&mut self, p: DVec3) {
        self.set_center(p);
        self.cam.target = p;
    }

    /// Troca o plano do corte mantendo o ponto do espaço que está no centro.
    fn set_plane(&mut self, plane: Plane) {
        let center = self.slice_center();
        self.plane = plane;
        self.tilt = [0.0; 2];
        self.set_center(center);
    }

    /// Põe o arquivo no topo da lista de recentes e grava a lista.
    fn remember(&mut self, path: &std::path::Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| *p != path);
        self.recent.insert(0, path);
        self.recent.truncate(RECENT_MAX);
        if let Some(file) = recent_file() {
            let text: String = self.recent.iter().map(|p| format!("{}\n", p.display())).collect();
            let _ = file.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(file, text);
        }
    }

    /// Pede uma captura da janela para salvar o painel como aparece na tela,
    /// com escala, régua, legenda de cores e o que mais estiver desenhado. A
    /// imagem chega num quadro seguinte (ver [`App::poll_screenshot`]).
    fn save_image(&mut self, pane_3d: bool) {
        let (rect, name) = if pane_3d { (self.pane_rects[1], "vista3d.png") } else { (self.pane_rects[0], "corte.png") };
        if rect.is_positive() {
            self.shot = Some((rect, name));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    fn poll_screenshot(&mut self, ctx: &egui::Context) {
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let (Some(image), Some((rect, name))) = (image, self.shot) else { return };
        self.shot = None;
        let crop = image.region(&rect, Some(ctx.pixels_per_point()));
        let frame = Frame { w: crop.width(), h: crop.height(), rgba: crop.as_raw().to_vec(), ..Frame::default() };
        let dialog = rfd::FileDialog::new().set_title("Salvar imagem").add_filter("PNG", &["png"]).set_file_name(name);
        let Some(path) = dialog.save_file() else { return };
        if let Err(e) = crate::save_png(&path.to_string_lossy(), &frame) {
            self.error = Some(format!("{}: {}", path.display(), e));
        }
    }

    fn fit_2d(&mut self) {
        let Some(model) = &self.model else { return };
        let (lo, hi) = model.bbox;
        let mid = (lo + hi) / 2.0;
        let (u, v, n) = self.plane.axes();
        self.center = [mid[u], mid[v]];
        self.slice_pos = mid[n];
        self.half_width = 0.55 * (hi[u] - lo[u]).max((hi[v] - lo[v]) * self.aspect_2d);
    }

    fn fit_3d(&mut self) {
        let Some(model) = &self.model else { return };
        let (lo, hi) = model.bbox;
        self.cam.target = (lo + hi) / 2.0;
        self.cam.dist = 1.9 * (hi - lo).length().max(1e-6) / self.aspect_3d.min(1.0);
    }

    /// Recalcula cores e visibilidade e invalida as imagens.
    fn refresh(&mut self) {
        self.generation += 1;
        let Some(model) = &self.model else { return };
        let mut palette = Palette::new(model, self.color_by);
        if let Some(sel) = self.selected {
            palette.rgb[sel] = SELECTED_RGB;
        }
        self.palette = Arc::new(palette);
        self.translucent = Arc::new(model.cells.iter().map(|c| self.translucent_mats.contains(&c.mat)).collect());
        self.visible = Arc::new(
            model
                .cells
                .iter()
                .map(|c| {
                    !c.outside
                        && !(self.hide_void && c.mat == 0)
                        && !(self.hide_gas && c.is_gas())
                        && !self.hidden_mats.contains(&c.mat)
                })
                .collect(),
        );
    }

    /// Direções horizontal, vertical e normal do corte, já com a inclinação.
    fn plane_basis(&self) -> (DVec3, DVec3, DVec3) {
        let (u, v, n) = self.plane.axes();
        let (u, v, n) = (DVec3::AXES[u], DVec3::AXES[v], DVec3::AXES[n]);
        let (sa, ca) = self.tilt[0].to_radians().sin_cos();
        let (u, n) = (u * ca + n * sa, n * ca - u * sa);
        let (sb, cb) = self.tilt[1].to_radians().sin_cos();
        (u, v * cb + n * sb, n * cb - v * sb)
    }

    /// Ponto do espaço no centro do corte.
    fn slice_center(&self) -> DVec3 {
        let (u, v, n) = self.plane.axes();
        DVec3::AXES[u] * self.center[0] + DVec3::AXES[v] * self.center[1] + DVec3::AXES[n] * self.slice_pos
    }

    fn shift_center(&mut self, d: DVec3) {
        let (u, v, n) = self.plane.axes();
        self.center[0] += d[u];
        self.center[1] += d[v];
        self.slice_pos += d[n];
    }

    fn slice_view(&self) -> SliceView {
        let (u, v, _) = self.plane_basis();
        SliceView { center: self.slice_center(), u, v, half_width: self.half_width }
    }

    fn clip(&self) -> Option<(DVec3, f64)> {
        if !self.clip_at_slice {
            return None;
        }
        let n = self.plane_basis().2;
        let d = n.dot(self.slice_center());
        Some(if self.clip_flip { (-n, -d) } else { (n, d) })
    }

    fn cell_summary(&self, leaf: u32) -> String {
        let Some(model) = &self.model else { return String::new() };
        if leaf == NONE {
            return "sem célula".into();
        }
        let c = &model.cells[leaf as usize];
        match (c.outside, c.mat) {
            (true, _) => format!("célula {} (mundo externo)", c.id),
            (_, 0) => format!("célula {} (vazio)", c.id),
            _ => format!("célula {}  mat {}  ρ {}", c.id, c.mat, c.rho),
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut select = None;
        let (mut run_audit, mut goto, mut goto_cell) = (false, None, None);
        let (mut mesh_changed, mut mesh_auto) = (false, false);
        let (mut mesh_frame, mut mesh_clear) = (false, false);
        let mut part_clear = false;
        let mut tally_goto: Option<usize> = None;
        {
            let Some(model) = &self.model else {
                ui.label("Abra um input MCNP ou PHITS, ou arraste o arquivo para a janela.");
                return;
            };
            ui.strong(if model.title.is_empty() { "(sem título)" } else { &model.title });
            ui.label(format!(
                "{} células · {} superfícies · {} universos",
                model.cells.len(),
                model.surfaces.len(),
                model.universes.len()
            ));
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Colorir por");
                egui::ComboBox::from_id_salt("color_by")
                    .selected_text(color_label(self.color_by))
                    .show_ui(ui, |ui| {
                        for by in [ColorBy::Material, ColorBy::Cell, ColorBy::Universe, ColorBy::Density, ColorBy::Importance] {
                            changed |= ui.selectable_value(&mut self.color_by, by, color_label(by)).changed();
                        }
                    });
            });

            let mut mats: Vec<i32> = model.cells.iter().map(|c| c.mat).filter(|&m| m != 0).collect();
            mats.sort_unstable();
            mats.dedup();
            egui::CollapsingHeader::new(format!("Materiais ({})", mats.len())).default_open(true).show(ui, |ui| {
                if ui.button("Tabela de materiais…").on_hover_text("Densidade, número de células, volume, massa e composição de cada material").clicked() {
                    self.mats_open = true;
                }
                egui::ScrollArea::vertical().id_salt("mats").max_height(180.0).show(ui, |ui| {
                    for (rank, &m) in mats.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let mut shown = !self.hidden_mats.contains(&m);
                            if ui.checkbox(&mut shown, "").on_hover_text("Mostrar no 3D").changed() {
                                if shown {
                                    self.hidden_mats.remove(&m);
                                } else {
                                    self.hidden_mats.insert(m);
                                }
                                changed = true;
                            }
                            let [r, g, b] = render::material_color(rank, m);
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
                            let mut glass = self.translucent_mats.contains(&m);
                            if ui.checkbox(&mut glass, "").on_hover_text("Translúcido no 3D: deixa ver o que há dentro e atrás").changed() {
                                if glass {
                                    self.translucent_mats.insert(m);
                                } else {
                                    self.translucent_mats.remove(&m);
                                }
                                changed = true;
                            }
                            let label = ui.label(format!("m{}", m));
                            if let Some(mat) = model.materials.get(&m) {
                                let text = mat.nuclides.iter().map(|(n, f)| format!("{}  {}", n, f)).collect::<Vec<_>>().join("\n");
                                label.on_hover_text(text);
                            }
                        });
                    }
                });
            });

            if !model.warnings.is_empty() {
                egui::CollapsingHeader::new(format!("⚠ Avisos ({})", model.warnings.len())).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("warns").max_height(160.0).show(ui, |ui| {
                        for w in &model.warnings {
                            ui.colored_label(Color32::from_rgb(230, 170, 60), w);
                        }
                    });
                });
            }

            egui::CollapsingHeader::new("Verificação da geometria").default_open(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let button = ui
                        .add_enabled(self.auditing.is_none(), egui::Button::new("Verificar modelo"))
                        .on_hover_text("Sorteia 3 milhões de pontos no modelo e procura células sobrepostas e regiões sem célula. Também estima o volume de cada célula.");
                    run_audit = button.clicked();
                    if self.auditing.is_some() {
                        ui.spinner();
                    }
                });
                let Some(a) = &self.audit else { return };
                if a.overlaps.is_empty() && a.undefined == 0 {
                    ui.colored_label(Color32::from_rgb(90, 190, 110), "Nenhum problema nos pontos sorteados.");
                    return;
                }
                let warn = Color32::from_rgb(240, 110, 90);
                egui::ScrollArea::vertical().id_salt("audit").max_height(150.0).show(ui, |ui| {
                    if !a.overlaps.is_empty() {
                        ui.colored_label(warn, format!("{} pares de células sobrepostas:", a.overlaps.len()));
                    }
                    for ov in &a.overlaps {
                        let (c1, c2) = (model.cells[ov.cells.0].id, model.cells[ov.cells.1].id);
                        let vol = ov.count as f64 / a.samples as f64 * a.box_volume;
                        let text = format!("{} e {}  (~{:.3e} cm³)", c1, c2, vol);
                        if ui.link(text).on_hover_text("Clique para levar o corte até um ponto da sobreposição").clicked() {
                            goto = Some(ov.at);
                        }
                    }
                    if a.undefined > 0 {
                        let vol = a.undefined as f64 / a.samples as f64 * a.box_volume;
                        ui.colored_label(warn, format!("Região sem célula (~{:.3e} cm³), por exemplo em:", vol));
                        for p in &a.undefined_at {
                            if ui.link(format!("{:.3}  {:.3}  {:.3}", p.x, p.y, p.z)).clicked() {
                                goto = Some(*p);
                            }
                        }
                    }
                });
            });

            if !model.tallies.is_empty() {
                egui::CollapsingHeader::new(format!("Tallies do input ({})", model.tallies.len())).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("tallies").max_height(130.0).show(ui, |ui| {
                        for (i, t) in model.tallies.iter().enumerate() {
                            let bins = t.bins.as_ref().map_or("forma não interpretada".to_string(), |b| format!("{} intervalos", b.len()));
                            let text = format!("F{} {} ({})", t.number, t.comment, bins);
                            if ui.link(text).on_hover_text("Clique para levar o corte até as células deste tally").clicked() {
                                tally_goto = Some(i);
                            }
                        }
                    });
                });
            }

            if !model.lost.is_empty() {
                egui::CollapsingHeader::new(format!("Partículas perdidas na saída ({})", model.lost.len())).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("perdidas").max_height(120.0).show(ui, |ui| {
                        for (p, _) in model.lost.iter().take(300) {
                            if ui.link(format!("{:.3}  {:.3}  {:.3}", p.x, p.y, p.z)).clicked() {
                                goto = Some(*p);
                            }
                        }
                    });
                });
            }

            if let Some(parts) = &self.particles {
                egui::CollapsingHeader::new("Partículas (espaço de fase)").default_open(true).show(ui, |ui| {
                    ui.label(format!("{} · {} partículas", parts.name, parts.list.len()));
                    for (kf, (n, weight, weighted_energy)) in &parts.by_kind {
                        ui.label(format!("{}: {} · energia média {:.3} MeV", dump::kind_name(*kf), n, weighted_energy / weight));
                    }
                    ui.checkbox(&mut self.part_show, "Mostrar no corte")
                        .on_hover_text("Cada partícula é um ponto com um traço na direção em que anda: azul fóton, vermelho elétron, verde pósitron");
                    ui.horizontal(|ui| {
                        ui.label("fatia ±");
                        ui.add(egui::DragValue::new(&mut self.part_slab).speed(0.05).range(0.0..=1e4).suffix(" cm"))
                            .on_hover_text("Só desenha as partículas a até esta distância do plano do corte; 0 desenha todas");
                    });
                    ui.add(egui::Slider::new(&mut self.part_max, 500..=30000).logarithmic(true).text("máx. desenhadas"));
                    ui.horizontal(|ui| {
                        if ui.button("Ir até elas").clicked() {
                            let some = parts.list.iter().step_by((parts.list.len() / 5000).max(1));
                            let (sum, n) = some.fold((DVec3::ZERO, 0.0), |(s, n), p| (s + p.pos(), n + 1.0));
                            goto = Some(sum / n);
                        }
                        part_clear = ui.button("Remover").clicked();
                    });
                });
            }

            if !self.meshes.is_empty() {
                egui::CollapsingHeader::new(format!("Resultados ({})", self.meshes.len())).id_salt("resultados").default_open(true).show(ui, |ui| {
                    let before = self.mesh_sel;
                    egui::ComboBox::from_id_salt("malha").width(200.0).selected_text(self.meshes[self.mesh_sel].name().to_string()).show_ui(ui, |ui| {
                        for (i, m) in self.meshes.iter().enumerate() {
                            ui.selectable_value(&mut self.mesh_sel, i, m.name().to_string());
                        }
                    });
                    ui.horizontal(|ui| {
                        let has_box = self.meshes[self.mesh_sel].bounds().is_some();
                        mesh_frame = ui
                            .add_enabled(has_box, egui::Button::new("Ir até a malha"))
                            .on_hover_text("Leva o corte até a malha e estende a régua ao longo dela, pronta para o perfil")
                            .on_disabled_hover_text("Não foi possível situar este tally: navegue até as células e use a régua")
                            .clicked();
                        mesh_clear = ui.button("Remover todos").clicked();
                    });
                    mesh_auto |= before != self.mesh_sel;
                    ui.horizontal(|ui| {
                        mesh_changed |= ui.checkbox(&mut self.mesh_show, "Mostrar").changed();
                        mesh_auto |= ui.checkbox(&mut self.mesh_log, "Log").changed();
                        mesh_changed |= ui
                            .checkbox(&mut self.mesh_error, "Erro relativo")
                            .on_hover_text("Pinta o erro relativo de cada elemento, de 0 a 20%, em vez do valor")
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("de");
                        let step = (self.mesh_range[1].abs() * 0.02).max(1e-12);
                        let mut lo = egui::DragValue::new(&mut self.mesh_range[0]).speed(step).custom_formatter(|v, _| format!("{:.3e}", v));
                        if self.mesh_log {
                            lo = lo.range(1e-300..=f64::MAX);
                        }
                        mesh_changed |= ui.add(lo).changed();
                        ui.label("a");
                        let hi = egui::DragValue::new(&mut self.mesh_range[1]).speed(step).custom_formatter(|v, _| format!("{:.3e}", v));
                        mesh_changed |= ui.add(hi).changed();
                    });
                    ui.horizontal(|ui| {
                        mesh_auto |= ui.button("Escala automática").clicked();
                        mesh_changed |= ui.add(egui::Slider::new(&mut self.mesh_alpha, 0.1..=1.0).text("opacidade")).changed();
                    });
                    ui.checkbox(&mut self.tally_open, "Tabela de tallies")
                        .on_hover_text("Todos os tallies de um valor só (dose por órgão, por exemplo) numa tabela: valor, erro e quais ainda não convergiram");
                    ui.checkbox(&mut self.conv_open, "Convergência do tally")
                        .on_hover_text("Histórico do tally ao longo da rodada (só o mctal traz) e quantas histórias faltam para o erro desejado");
                    ui.checkbox(&mut self.profile_open, "Perfil ao longo da régua")
                        .on_hover_text("Marque dois pontos no corte com Shift+clique: o gráfico mostra o resultado ao longo da linha (PDD, perfil lateral…)");
                });
            }

            if let Some(ci) = self.selected {
                let c = &model.cells[ci];
                egui::CollapsingHeader::new(format!("Célula {}", c.id)).id_salt("selecionada").default_open(true).show(ui, |ui| {
                    let what = match (c.outside, c.mat) {
                        (true, _) => "mundo externo".to_string(),
                        (_, 0) => "vazio".to_string(),
                        _ => format!("material {}, densidade {}", c.mat, c.rho),
                    };
                    ui.label(format!("{} · universo {} · linha {}", what, c.universe, c.line));
                    match self.audit.as_ref().map(|a| a.volume(ci)) {
                        Some(Some((vol, err))) => {
                            let mut text = format!("volume ≈ {:.4e} cm³ (±{:.1}%)", vol, err * 100.0);
                            // Densidade negativa é g/cm³.
                            if c.mat != 0 && c.rho < 0.0 {
                                text += &format!("\nmassa ≈ {:.4e} g", vol * -c.rho);
                            }
                            ui.label(text).on_hover_text(
                                "Estimado por amostragem dentro da caixa do modelo, somando todas as cópias da célula. Células que passam da caixa (o ar em volta, por exemplo) ficam subestimadas.",
                            );
                        }
                        Some(None) => {
                            ui.weak("volume: nenhum ponto sorteado caiu nesta célula");
                        }
                        None => {
                            ui.weak("volume: use \"Verificar modelo\"");
                        }
                    }
                    if ui.button("Centralizar o corte nela").clicked() {
                        goto_cell = Some(ci);
                    }
                    egui::ScrollArea::vertical().id_salt("cartao").max_height(110.0).show(ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&c.card).monospace()).wrap());
                    });
                });
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Células");
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("nº, m5, u2…").desired_width(110.0))
                    .on_hover_text("Filtra pelo começo do número da célula, por material (m5) ou por universo (u2)");
            });
            let filter = self.filter.trim().to_lowercase();
            let number = |prefix: char| filter.strip_prefix(prefix).and_then(|t| t.parse::<i32>().ok());
            let rows: Vec<usize> = (0..model.cells.len())
                .filter(|&ci| {
                    let c = &model.cells[ci];
                    match (number('m'), number('u')) {
                        (Some(m), _) => c.mat == m,
                        (_, Some(u)) => c.universe == u,
                        _ => c.id.to_string().starts_with(&filter),
                    }
                })
                .collect();
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
            egui::ScrollArea::vertical().id_salt("cells").auto_shrink([false, false]).show_rows(
                ui,
                row_h,
                rows.len(),
                |ui, range| {
                    for ci in rows[range].iter().copied() {
                        let c = &model.cells[ci];
                        let mat = if c.outside { "ext".to_string() } else { c.mat.to_string() };
                        let text = format!("{:>6}  m{:<4} u{:<4} {}", c.id, mat, c.universe, if c.lat != 0 { "lat" } else { "" });
                        let label = egui::RichText::new(text).monospace();
                        if ui.selectable_label(self.selected == Some(ci), label).clicked() {
                            select = Some(ci);
                        }
                    }
                },
            );
        }
        if let Some(ci) = select {
            self.selected = if self.selected == Some(ci) { None } else { Some(ci) };
            changed = true;
        }
        if run_audit {
            self.start_audit(false);
        }
        if part_clear {
            self.particles = None;
        }
        if let Some(i) = tally_goto {
            self.go_to_tally(i);
        }
        if mesh_clear {
            self.meshes.clear();
            self.tally_sum = None;
            self.mesh_sel = 0;
            mesh_changed = true;
        }
        if mesh_frame {
            self.frame_mesh();
            self.profile_open = true;
        }
        if mesh_auto {
            self.mesh_auto_range();
        }
        if mesh_auto || mesh_changed {
            self.generation += 1;
        }
        if let Some(ci) = goto_cell {
            match self.model.as_ref().and_then(|m| m.point_in(ci)) {
                Some(p) => goto = Some(p),
                None => self.hover = "célula pequena demais para ser achada na grade de busca".into(),
            }
        }
        if let Some(p) = goto {
            self.go_to(p);
        }
        if changed {
            self.refresh();
        }
    }

    /// Reserva a área da imagem: a largura toda e a fração dada da altura que resta.
    fn canvas(ui: &mut egui::Ui, fraction: f32) -> (Rect, egui::Response) {
        let size = ui.available_size().max(egui::vec2(32.0, 32.0));
        ui.allocate_exact_size(egui::vec2(size.x, (size.y * fraction).round()), Sense::click_and_drag())
    }

    /// Tamanho de renderização em pixels.
    fn render_size(ui: &egui::Ui, rect: Rect) -> [usize; 2] {
        let mut scale = ui.ctx().pixels_per_point();
        let longest = rect.width().max(rect.height()) * scale;
        if longest > MAX_PIXELS as f32 {
            scale *= MAX_PIXELS as f32 / longest;
        }
        [(rect.width() * scale).round().max(8.0) as usize, (rect.height() * scale).round().max(8.0) as usize]
    }

    /// Cada painel só guarda na chave o que muda a sua imagem: girar a câmera
    /// não refaz o corte, nem mover o corte refaz o 3D (a não ser pelo plano
    /// de corte, que vai em `clip`).
    fn key_2d(&self, size: [usize; 2]) -> RenderKey {
        let cam = Camera { target: DVec3::ZERO, yaw: 0.0, pitch: 0.0, dist: 1.0, fov_deg: 1.0 };
        RenderKey { generation: self.generation, size, slice: self.slice_view(), cam, clip: None, edges: self.edges_2d, check: self.check_2d }
    }

    fn key_3d(&self, size: [usize; 2]) -> RenderKey {
        let slice = SliceView { center: DVec3::ZERO, u: DVec3::X, v: DVec3::Y, half_width: 1.0 };
        RenderKey { generation: self.generation, size, slice, cam: self.cam, clip: self.clip(), edges: self.edges_3d, check: false }
    }

    fn pane_2d(&mut self, ui: &mut egui::Ui) {
        let (mut refit, mut save) = (false, false);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Corte 2D");
            for p in [Plane::Xy, Plane::Xz, Plane::Yz] {
                if ui.selectable_label(self.plane == p, p.label()).clicked() && self.plane != p {
                    self.set_plane(p);
                }
            }
            ui.checkbox(&mut self.three_views, "3 cortes")
                .on_hover_text("Mostra embaixo os outros dois planos pelo mesmo ponto. Neles: clique leva o centro até ali, arrastar desloca, duplo clique troca o plano principal.");
            let n = self.plane.axes().2;
            ui.label(format!("{} =", AXIS_NAMES[n]));
            let step = (self.half_width / 100.0).max(1e-6);
            ui.add(egui::DragValue::new(&mut self.slice_pos).speed(step).suffix(" cm"));
            ui.label("inclinar");
            for (tilt, tip) in self.tilt.iter_mut().zip(["Gira o plano do corte em torno do eixo vertical da imagem", "Gira o plano do corte em torno do eixo horizontal da imagem"]) {
                ui.add(egui::DragValue::new(tilt).speed(0.5).range(-90.0..=90.0).suffix("°")).on_hover_text(tip);
            }
            ui.checkbox(&mut self.edges_2d, "Contornos");
            ui.checkbox(&mut self.check_2d, "Sobreposições")
                .on_hover_text("Destaca com hachura vermelha e amarela onde duas células ocupam o mesmo lugar (deixa o corte mais lento)");
            if ui.button("Enquadrar").clicked() {
                refit = true;
            }
            // Os botões ficam sempre presentes: se aparecessem e sumissem, a
            // barra mudaria de tamanho e o corte seria refeito à toa.
            let clear = ui
                .add_enabled(!self.measure.is_empty(), egui::Button::new("Limpar régua"))
                .on_hover_text("Shift+clique em dois pontos do corte mede a distância entre eles")
                .on_disabled_hover_text("Shift+clique em dois pontos do corte mede a distância entre eles");
            if clear.clicked() {
                self.measure.clear();
            }
            save = ui.button("Salvar PNG").clicked();
        });
        if refit {
            self.fit_2d();
        }
        if save {
            self.save_image(false);
        }

        let (rect, resp) = Self::canvas(ui, if self.three_views { 0.62 } else { 1.0 });
        self.pane_rects[0] = rect;
        let aspect = (rect.width() / rect.height()) as f64;
        self.aspect_2d = aspect;
        if std::mem::take(&mut self.fit_pending[0]) {
            self.fit_2d();
        }
        let cm_per_pt = 2.0 * self.half_width / rect.width() as f64;

        if resp.dragged() {
            let d = resp.drag_delta();
            let (u, v, _) = self.plane_basis();
            self.shift_center((v * d.y as f64 - u * d.x as f64) * cm_per_pt);
        }
        if let Some(pos) = resp.hover_pos() {
            let (fx, fy) = (((pos.x - rect.left()) / rect.width()) as f64, ((pos.y - rect.top()) / rect.height()) as f64);
            let scroll = ui.input(|i| i.smooth_scroll_delta.y) as f64;
            if scroll != 0.0 {
                // Zoom mantendo fixo o ponto sob o cursor.
                let before = self.slice_view().point(fx, fy, aspect);
                self.half_width = (self.half_width * (-scroll * 0.0025).exp()).clamp(1e-6, 1e9);
                let after = self.slice_view().point(fx, fy, aspect);
                self.shift_center(before - after);
            }
            let p = self.slice_view().point(fx, fy, aspect);
            if let Some(model) = &self.model {
                let located = model.locate(p);
                let levels = located.as_ref().map_or(&[][..], |l| &l.1[..]);
                let (leaf, path) = located.as_ref().map_or((NONE, String::new()), |(f, l)| (f.leaf as u32, model.describe(l)));
                self.hover = format!("x {:.4}  y {:.4}  z {:.4}   |   {}", p.x, p.y, p.z, self.cell_summary(leaf));
                if path.contains('>') {
                    self.hover += &format!("   |   {}", path);
                }
                if let Some((surface, distance)) = model.nearest_surface(levels) {
                    self.hover += &format!("   |   superfície mais próxima: {} (a {:.3} cm)", surface, distance);
                }
                if let Some(mesh) = self.meshes.get(self.mesh_sel).filter(|_| self.mesh_show) {
                    if let Some((value, err)) = mesh.at(p, levels) {
                        self.hover += &format!("   |   {}: {:.4e} ± {:.1}%", mesh.name(), value, err * 100.0);
                    }
                }
                if let Some((a, b)) = model.overlap_at(p).filter(|_| self.check_2d) {
                    self.hover += &format!("   |   SOBREPOSIÇÃO: células {} e {}", model.cells[a].id, model.cells[b].id);
                }
                if resp.clicked() && ui.input(|i| i.modifiers.ctrl) && self.edit_open {
                    // Ctrl+clique escolhe a superfície mais próxima para a edição.
                    if let Some((surface, _)) = model.nearest_surface(levels) {
                        match self.edit_targets.iter().position(|&s| s == surface) {
                            Some(k) => {
                                self.edit_targets.remove(k);
                            }
                            None => self.edit_targets.push(surface),
                        }
                        self.edit_mode = 0;
                    }
                } else if resp.clicked() && ui.input(|i| i.modifiers.shift) {
                    if self.measure.len() >= 2 {
                        self.measure.clear();
                    }
                    self.measure.push(p);
                } else if resp.clicked() {
                    self.selected = (leaf != NONE && self.selected != Some(leaf as usize)).then_some(leaf as usize);
                    self.refresh();
                }
            }
        }

        if let Some(model) = &self.model {
            let key = self.key_2d(Self::render_size(ui, rect));
            let (model, pal, view, edges, check) = (model.clone(), self.palette.clone(), key.slice, self.edges_2d, self.check_2d);
            let overlay = self.overlay();
            self.pane_2d.sync(ui.ctx(), "corte", key, move |w, h, full, cancel| {
                render::render_slice(&model, &pal, &view, w, h, edges && full, check, overlay.as_ref(), cancel)
            });
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(245));
        if let (Some(tex), true) = (&self.pane_2d.tex, self.model.is_some()) {
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            self.scale_bar(&painter, rect);
            self.draw_ticks(&painter, rect, aspect);
            self.pane_2d.draw_status(&painter, rect, Color32::from_gray(60));
            self.draw_problems(&painter, rect);
            self.draw_lost(&painter, rect, aspect);
            self.draw_measure(&painter, rect, aspect);
            self.draw_particles(&painter, rect, aspect);
            self.draw_source(&painter, rect, aspect);
            self.draw_color_bar(&painter, rect);
            if self.three_views {
                Self::crosshair(&painter, rect);
            }
        }
        if self.three_views {
            let others: Vec<Plane> = [Plane::Xy, Plane::Xz, Plane::Yz].into_iter().filter(|p| *p != self.plane).collect();
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let size = egui::vec2(((ui.available_width() - gap) / 2.0).floor().max(16.0), ui.available_height().max(16.0));
                for (i, plane) in others.into_iter().enumerate() {
                    self.mini_slice(ui, plane, i, size);
                }
            });
        }
    }

    /// Linhas finas pelo centro: onde os outros planos cortam este.
    fn crosshair(painter: &egui::Painter, rect: Rect) {
        let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 110));
        let c = rect.center();
        painter.line_segment([Pos2::new(rect.left(), c.y), Pos2::new(rect.right(), c.y)], stroke);
        painter.line_segment([Pos2::new(c.x, rect.top()), Pos2::new(c.x, rect.bottom())], stroke);
    }

    /// Corte auxiliar num dos outros dois planos, pelo mesmo ponto e com o
    /// mesmo zoom do principal.
    fn mini_slice(&mut self, ui: &mut egui::Ui, plane: Plane, index: usize, size: egui::Vec2) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let aspect = (rect.width() / rect.height()) as f64;
        let (u, v, _) = plane.axes();
        let view = SliceView { center: self.slice_center(), u: DVec3::AXES[u], v: DVec3::AXES[v], half_width: self.half_width };
        if resp.double_clicked() {
            self.set_plane(plane);
        } else if resp.dragged() {
            let d = resp.drag_delta();
            let cm_per_pt = 2.0 * self.half_width / rect.width() as f64;
            self.shift_center((view.v * d.y as f64 - view.u * d.x as f64) * cm_per_pt);
        } else if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
            let (fx, fy) = (((pos.x - rect.left()) / rect.width()) as f64, ((pos.y - rect.top()) / rect.height()) as f64);
            self.set_center(view.point(fx, fy, aspect));
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y) as f64;
            if scroll != 0.0 {
                self.half_width = (self.half_width * (-scroll * 0.0025).exp()).clamp(1e-6, 1e9);
            }
        }
        if let Some(model) = &self.model {
            let cam = Camera { target: DVec3::ZERO, yaw: 0.0, pitch: 0.0, dist: 1.0, fov_deg: 1.0 };
            let key = RenderKey { generation: self.generation, size: Self::render_size(ui, rect), slice: view, cam, clip: None, edges: self.edges_2d, check: false };
            let (model, pal, edges, overlay) = (model.clone(), self.palette.clone(), self.edges_2d, self.overlay());
            self.minis[index].sync(ui.ctx(), ["corte_aux_0", "corte_aux_1"][index], key, move |w, h, full, cancel| {
                render::render_slice(&model, &pal, &view, w, h, edges && full, false, overlay.as_ref(), cancel)
            });
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(245));
        if let (Some(tex), true) = (&self.minis[index].tex, self.model.is_some()) {
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            Self::crosshair(&painter, rect);
            let text = format!("{}   (horizontal {}, vertical {})", plane.label(), AXIS_NAMES[u], AXIS_NAMES[v]);
            let (pos, font) = (rect.left_top() + egui::vec2(8.0, 6.0), egui::FontId::proportional(12.0));
            painter.text(pos + egui::vec2(1.0, 1.0), egui::Align2::LEFT_TOP, &text, font.clone(), Color32::WHITE);
            painter.text(pos, egui::Align2::LEFT_TOP, &text, font, Color32::BLACK);
        }
    }

    /// Posição na tela de um ponto do espaço projetado no plano do corte.
    fn to_screen(&self, rect: Rect, aspect: f64, p: DVec3) -> Pos2 {
        let view = self.slice_view();
        let r = p - view.center;
        let fx = (r.dot(view.u) / view.half_width + 1.0) / 2.0;
        let fy = (1.0 - r.dot(view.v) * aspect / view.half_width) / 2.0;
        Pos2::new(rect.left() + fx as f32 * rect.width(), rect.top() + fy as f32 * rect.height())
    }

    /// Partículas perdidas: um X vermelho, mais forte (e com a direção) nas
    /// que estão praticamente no plano do corte.
    fn draw_lost(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        let Some(model) = &self.model else { return };
        let view = self.slice_view();
        let normal = view.u.cross(view.v);
        for &(p, d) in &model.lost {
            let at = self.to_screen(rect, aspect, p);
            let near = (p - view.center).dot(normal).abs() < 0.02 * view.half_width;
            let (size, color) = if near { (5.0, Color32::from_rgb(230, 0, 0)) } else { (3.0, Color32::from_rgba_unmultiplied(230, 0, 0, 90)) };
            for dir in [egui::vec2(size, size), egui::vec2(size, -size)] {
                painter.line_segment([at - dir, at + dir], Stroke::new(2.0, color));
            }
            if near {
                let tip = at + egui::vec2(d.dot(view.u) as f32, -d.dot(view.v) as f32) * 22.0;
                painter.line_segment([at, tip], Stroke::new(1.5, color));
            }
        }
    }

    /// Escala de cores do resultado em malha, na borda direita do corte.
    fn draw_color_bar(&self, painter: &egui::Painter, rect: Rect) {
        let Some(o) = self.overlay() else { return };
        const STEPS: usize = 48;
        let (w, h) = (14.0, (rect.height() * 0.45).clamp(60.0, 260.0));
        let top = rect.right_center() + egui::vec2(-w - 12.0, -h / 2.0);
        for i in 0..STEPS {
            let [r, g, b] = Overlay::color_at(1.0 - (i as f64 + 0.5) / STEPS as f64);
            let y = top.y + h * i as f32 / STEPS as f32;
            let cell = Rect::from_min_size(Pos2::new(top.x, y), egui::vec2(w, h / STEPS as f32 + 1.0));
            painter.rect_filled(cell, 0.0, Color32::from_rgb(r, g, b));
        }
        let (hi, lo) = if o.error { ("20%".to_string(), "0%".to_string()) } else { (format!("{:.2e}", o.hi), format!("{:.2e}", o.lo)) };
        let font = egui::FontId::proportional(12.0);
        for (text, pos, align) in [
            (hi, top + egui::vec2(w, -3.0), egui::Align2::RIGHT_BOTTOM),
            (lo, top + egui::vec2(w, h + 3.0), egui::Align2::RIGHT_TOP),
        ] {
            painter.text(pos + egui::vec2(1.0, 1.0), align, &text, font.clone(), Color32::WHITE);
            painter.text(pos, align, &text, font.clone(), Color32::BLACK);
        }
    }

    /// Leva o corte até as células de um tally do input: enquadra os elementos
    /// de lattice, quando é o caso, ou vai a um ponto da primeira célula.
    fn go_to_tally(&mut self, index: usize) {
        let Some(model) = self.model.clone() else { return };
        let Some(bins) = model.tallies.get(index).and_then(|t| t.bins.as_ref()) else { return };
        let cell = |id: i32| model.cell_index.get(&id).copied();
        let centers: Vec<DVec3> = bins
            .iter()
            .filter_map(|b| match b[..] {
                [_, (lat, Some(idx))] => model.element_center(cell(lat)?, idx),
                _ => None,
            })
            .collect();
        if let Some(&first) = centers.first() {
            let (lo, hi) = centers.iter().fold((first, first), |(lo, hi), &c| (lo.min(c), hi.max(c)));
            let size = hi - lo;
            let long = (0..3).max_by(|&a, &b| size[a].total_cmp(&size[b])).unwrap();
            if self.plane.axes().2 == long {
                self.set_plane(if long == 2 { Plane::Xz } else { Plane::Xy });
            }
            self.go_to((lo + hi) / 2.0);
            self.half_width = (0.6 * size[long]).max(1.0);
        } else if let Some(p) = bins.iter().flatten().next().and_then(|b| model.point_in(cell(b.0)?)) {
            self.go_to(p);
        } else {
            self.hover = "não consegui situar as células deste tally".into();
        }
    }

    /// Graduação dos eixos nas bordas do corte (só com o plano sem inclinação).
    fn draw_ticks(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        if self.tilt != [0.0; 2] {
            return;
        }
        let cm_per_pt = 2.0 * self.half_width / rect.width() as f64;
        let target = 90.0 * cm_per_pt;
        let pow = 10f64.powf(target.log10().floor());
        let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * pow).rfind(|&v| v <= target).unwrap_or(pow);
        let decimals = (-step.log10().floor()).max(0.0) as usize;
        let font = egui::FontId::proportional(11.0);
        let label = |pos: Pos2, align: egui::Align2, value: f64| {
            // -0 vira 0.
            let text = format!("{:.*}", decimals, if value.abs() < step * 1e-6 { 0.0 } else { value });
            painter.text(pos + egui::vec2(1.0, 1.0), align, &text, font.clone(), Color32::WHITE);
            painter.text(pos, align, &text, font.clone(), Color32::BLACK);
        };
        let stroke = Stroke::new(1.5, Color32::BLACK);
        let (x0, y0) = (self.center[0] - self.half_width, self.center[1] - self.half_width / aspect);
        let mut x = (x0 / step).ceil() * step;
        while x <= x0 + 2.0 * self.half_width {
            let px = rect.left() + ((x - x0) / cm_per_pt) as f32;
            // A barra de escala ocupa o canto inferior esquerdo.
            if px > rect.left() + 190.0 && px < rect.right() - 20.0 {
                painter.line_segment([Pos2::new(px, rect.bottom()), Pos2::new(px, rect.bottom() - 6.0)], stroke);
                label(Pos2::new(px, rect.bottom() - 8.0), egui::Align2::CENTER_BOTTOM, x);
            }
            x += step;
        }
        let mut y = (y0 / step).ceil() * step;
        while y <= y0 + 2.0 * self.half_width / aspect {
            let py = rect.bottom() - ((y - y0) / cm_per_pt) as f32;
            if py > rect.top() + 60.0 && py < rect.bottom() - 50.0 {
                painter.line_segment([Pos2::new(rect.left(), py), Pos2::new(rect.left() + 6.0, py)], stroke);
                label(Pos2::new(rect.left() + 9.0, py), egui::Align2::LEFT_CENTER, y);
            }
            y += step;
        }
    }

    /// Marca da fonte: um alvo laranja e, se a direção for uma só, uma seta.
    /// Fica esmaecida quando a fonte está longe do plano do corte.
    fn draw_source(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        let Some(src) = &self.source else { return };
        let view = self.slice_view();
        let at = self.to_screen(rect, aspect, src.pos);
        if !rect.contains(at) {
            return;
        }
        let near = (src.pos - view.center).dot(view.u.cross(view.v)).abs() < 0.02 * view.half_width;
        let color = Color32::from_rgba_unmultiplied(235, 110, 0, if near { 255 } else { 110 });
        painter.circle_stroke(at, 7.0, Stroke::new(2.0, color));
        painter.circle_filled(at, 2.5, color);
        if let Some(d) = src.dir {
            let tip = at + egui::vec2(d.dot(view.u) as f32, -d.dot(view.v) as f32) * 34.0;
            painter.arrow(at, tip - at, Stroke::new(2.0, color));
        }
        painter.text(at + egui::vec2(10.0, -10.0), egui::Align2::LEFT_BOTTOM, "fonte", egui::FontId::proportional(12.0), color);
    }

    /// (erros, avisos) somando input, geometria e saída.
    fn diag_counts(&self) -> (usize, usize) {
        let all = self.findings.iter().chain(&self.audit_notes).chain(&self.run);
        all.fold((0, 0), |(e, w), f| (e + (f.level == Severity::Error) as usize, w + (f.level == Severity::Warn) as usize))
    }

    fn diag_window(&mut self, ctx: &egui::Context) {
        if !self.diag_open {
            return;
        }
        let mut open = true;
        let (mut goto, mut run_audit) = (None, false);
        egui::Window::new("Diagnóstico").open(&mut open).default_width(620.0).default_height(520.0).show(ctx, |ui| {
            if self.model.is_none() {
                ui.label("Abra um input para ver o diagnóstico.");
                return;
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.auto_audit, "Verificar a geometria sempre que abrir ou recarregar");
                run_audit = ui.add_enabled(self.auditing.is_none(), egui::Button::new("Verificar agora")).clicked();
                if self.auditing.is_some() {
                    ui.spinner();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                show_findings(ui, "Input", &self.findings, &mut goto);
                if self.audit.is_some() {
                    show_findings(ui, "Geometria (por amostragem)", &self.audit_notes, &mut goto);
                } else {
                    ui.add_space(6.0);
                    ui.strong("Geometria (por amostragem)");
                    ui.weak(if self.auditing.is_some() { "verificando…" } else { "ainda não verificada" });
                }
                if self.run.is_empty() {
                    ui.add_space(6.0);
                    ui.strong("Saída da simulação");
                    ui.weak("Abra a saída do MCNP em \"Resultados…\" para ver se a rodada terminou, os avisos e quais tallies passaram nos 10 testes estatísticos.");
                } else {
                    show_findings(ui, "Saída da simulação", &self.run, &mut goto);
                }
            });
        });
        self.diag_open = open;
        if run_audit {
            self.start_audit(false);
        }
        if let Some(p) = goto {
            self.go_to(p);
        }
    }

    /// Histórico do tally selecionado: o erro tem que cair com 1/√N e a média
    /// tem que parar de se mexer.
    fn convergence_window(&mut self, ctx: &egui::Context) {
        if !self.conv_open {
            return;
        }
        let mut open = true;
        let mut percent = self.conv_target * 100.0;
        egui::Window::new("Convergência do tally").open(&mut open).default_width(540.0).show(ctx, |ui| {
            let Some(field) = self.meshes.get(self.mesh_sel) else {
                ui.label("Carregue um mctal do MCNP em \"Resultados…\".");
                return;
            };
            let h = field.history();
            if h.len() < 2 {
                ui.label(format!("{}: este resultado não traz histórico. Só o mctal do MCNP traz.", field.name()));
                return;
            }
            let last = h[h.len() - 1];
            ui.strong(field.name());
            ui.label(format!("Até {:e} histórias: média {:.4e}, erro relativo {:.2}% (no intervalo que o MCNP acompanha).", last[0], last[1], last[2] * 100.0));
            let blue = Color32::from_rgb(30, 90, 200);
            let gray = Color32::from_gray(150);
            ui.add_space(4.0);
            ui.label("Erro relativo × histórias, em escala log-log. A linha cinza é a queda ideal, 1/√N.");
            let errors: Vec<[f64; 2]> = h.iter().map(|r| [r[0], r[2]]).collect();
            let ideal: Vec<[f64; 2]> = h.iter().map(|r| [r[0], last[2] * (last[0] / r[0].max(1.0)).sqrt()]).collect();
            line_plot(ui, 170.0, &[(ideal, gray), (errors, blue)], [true, true]);
            ui.label("Média ÷ média final × histórias. Tem que estabilizar em 1, sem saltos.");
            let means: Vec<[f64; 2]> = h.iter().map(|r| [r[0], r[1] / last[1]]).collect();
            line_plot(ui, 150.0, &[(vec![[h[0][0], 1.0], [last[0], 1.0]], gray), (means, Color32::from_rgb(215, 55, 40))], [true, false]);

            let warn = Color32::from_rgb(235, 140, 30);
            let second_half = &h[h.len() / 2..];
            let drift = second_half.iter().map(|r| (r[1] / last[1] - 1.0).abs()).fold(0.0, f64::max);
            if drift > 2.0 * last[2] {
                ui.colored_label(warn, format!("A média ainda se moveu {:.1}% na segunda metade da rodada, mais que duas vezes o erro final: não está estável.", drift * 100.0));
            }
            let fom = second_half.iter().map(|r| r[3]).filter(|f| *f > 0.0);
            let (lo, hi) = fom.fold((f64::INFINITY, 0.0f64), |(a, b), f| (a.min(f), b.max(f)));
            if hi > 2.0 * lo {
                ui.colored_label(warn, format!("A figura de mérito variou {:.1}× na segunda metade: sinal de eventos raros de peso alto ainda mal amostrados.", hi / lo));
            }
            ui.horizontal(|ui| {
                ui.label("Erro desejado");
                ui.add(egui::DragValue::new(&mut percent).speed(0.05).range(0.01..=50.0).suffix(" %"));
                let target = percent / 100.0;
                if last[2] <= target {
                    ui.label("já foi atingido.");
                } else {
                    let total = diag::histories_for(last[0], last[2], target);
                    ui.label(format!("pede ~{:.2e} histórias no total ({:.1}× as atuais), se o erro seguir caindo com 1/√N.", total, total / last[0]));
                }
            });
        });
        self.conv_open = open;
        self.conv_target = percent / 100.0;
    }

    /// Imagem para o relatório, já em PNG codificado em base64.
    fn report_image(&self, model: &Arc<Model>, slice: Option<SliceView>) -> String {
        const SIZE: usize = 760;
        let go = AtomicBool::new(false);
        let frame = match slice {
            Some(view) => render::render_slice(model, &self.palette, &view, SIZE, SIZE, true, false, self.overlay().as_ref(), &go),
            None => {
                let scene = Scene { model, pal: &self.palette, visible: &self.visible, translucent: &self.translucent, clip: self.clip(), edges: true };
                render::render_3d(&scene, &self.cam, SIZE, SIZE, &go)
            }
        };
        frame.map_or(String::new(), |f| base64(&crate::png_bytes(&f)))
    }

    /// Monta o relatório em HTML: o que foi simulado, como está a geometria e
    /// o que o diagnóstico achou.
    fn build_report(&self) -> Option<String> {
        let model = self.model.as_ref()?;
        let esc = |t: &str| t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut h = String::from(
            "<!doctype html><html lang=\"pt-br\"><head><meta charset=\"utf-8\"><title>Relatório da simulação</title><style>\
body{font-family:Segoe UI,Arial,sans-serif;max-width:1000px;margin:24px auto;padding:0 16px;color:#222}\
h1{font-size:22px}h2{font-size:17px;margin-top:28px;border-bottom:1px solid #ccc;padding-bottom:4px}\
table{border-collapse:collapse;font-size:13px}td,th{border:1px solid #ccc;padding:3px 8px;text-align:left;vertical-align:top}\
th{background:#f0f0f0}.ok{color:#1a7f37}.info{color:#666}.warn{color:#b35900}.error{color:#c00;font-weight:600}\
figure{display:inline-block;margin:6px 8px 6px 0}img{width:470px;border:1px solid #ccc}figcaption{font-size:12px;color:#555}\
li{margin:2px 0}</style></head><body>",
        );
        let file = self.path.as_ref().map_or(String::new(), |p| p.display().to_string());
        h += &format!("<h1>{}</h1><p>{}<br>{} células · {} superfícies · {} universos · {} materiais</p>", esc(&model.title), esc(&file), model.cells.len(), model.surfaces.len(), model.universes.len(), model.materials.len());

        h += "<h2>Diagnóstico</h2>";
        let sections = [("Input", &self.findings), ("Geometria (por amostragem)", &self.audit_notes), ("Saída da simulação", &self.run)];
        for (title, list) in sections {
            if list.is_empty() {
                continue;
            }
            h += &format!("<h3>{}</h3><ul>", title);
            let mut sorted: Vec<&Finding> = list.iter().collect();
            sorted.sort_by(|a, b| b.level.cmp(&a.level));
            for f in sorted {
                let (class, tag) = severity_tag(f.level);
                h += &format!("<li class=\"{}\">[{}] {}</li>", class, tag, esc(&f.text));
            }
            h += "</ul>";
        }
        if self.audit.is_none() {
            h += "<p class=\"warn\">A geometria não foi verificada por amostragem antes de gerar este relatório.</p>";
        }

        h += "<h2>Geometria</h2>";
        let (lo, hi) = model.bbox;
        let mid = (lo + hi) / 2.0;
        let mut figures = vec![("Corte como está na tela".to_string(), Some(self.slice_view())), ("Vista 3D como está na tela".to_string(), None)];
        for plane in [Plane::Xy, Plane::Xz, Plane::Yz] {
            let (u, v, n) = plane.axes();
            let view = SliceView { center: mid, u: DVec3::AXES[u], v: DVec3::AXES[v], half_width: 0.55 * (hi[u] - lo[u]).max(hi[v] - lo[v]) };
            figures.push((format!("Corte {} em {} = {:.3} cm (modelo inteiro)", plane.label(), AXIS_NAMES[n], mid[n]), Some(view)));
        }
        for (caption, view) in figures {
            h += &format!("<figure><img src=\"data:image/png;base64,{}\"><figcaption>{}</figcaption></figure>", self.report_image(model, view), esc(&caption));
        }

        h += "<h2>Materiais</h2><table><tr><th>material</th><th>densidade</th><th>células</th><th>volume (cm³)</th><th>massa (g)</th><th>composição</th></tr>";
        for r in self.material_rows() {
            let composition = r.nuclides.as_ref().map_or("sem cartão de material".to_string(), |n| n.iter().map(|(n, f)| format!("{} {}", n, f)).collect::<Vec<_>>().join(", "));
            let columns = match r.volume {
                // Pequeno demais para a amostragem: zero aqui enganaria.
                Some(None) => "<td colspan=\"2\">nenhum ponto sorteado caiu neste material</td>".to_string(),
                Some(Some(v)) => format!("<td>{:.4e}</td><td>{}</td>", v, r.mass.map_or("—".to_string(), |g| format!("{:.4e}", g))),
                None => "<td>—</td><td>—</td>".to_string(),
            };
            h += &format!("<tr><td>{}</td><td>{}</td><td>{}</td>{}<td>{}</td></tr>", r.id, esc(&r.rho), r.cells, columns, esc(&composition));
        }
        h += "</table><p class=\"info\">Volume e massa são estimativas por amostragem dentro da caixa do modelo; células que passam dela (o ar em volta) ficam subestimadas.</p>";

        if !model.tallies.is_empty() {
            h += "<h2>Tallies do input</h2><table><tr><th>tally</th><th>descrição</th><th>intervalos</th></tr>";
            for t in &model.tallies {
                let bins = t.bins.as_ref().map_or("forma não interpretada".to_string(), |b| b.len().to_string());
                h += &format!("<tr><td>F{}</td><td>{}</td><td>{}</td></tr>", t.number, esc(&t.comment), bins);
            }
            h += "</table>";
        }
        if !self.meshes.is_empty() {
            h += "<h2>Resultados carregados</h2><table><tr><th>resultado</th><th>menor valor positivo</th><th>maior valor</th><th>histórico</th></tr>";
            for f in &self.meshes {
                let (lo, hi) = f.positive_range().map_or(("—".to_string(), "—".to_string()), |r| (format!("{:.4e}", r.0), format!("{:.4e}", r.1)));
                let history = f.history().last().map_or("—".to_string(), |r| format!("{:e} histórias, erro {:.2}%", r[0], r[2] * 100.0));
                h += &format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(f.name()), lo, hi, history);
            }
            h += "</table>";
        }
        h += "<p class=\"info\">Gerado pelo visualizador de geometria MCNP / PHITS.</p></body></html>";
        Some(h)
    }

    fn save_report(&mut self) {
        let dialog = rfd::FileDialog::new().set_title("Salvar relatório").add_filter("HTML", &["html"]).set_file_name("relatorio.html");
        let Some(path) = dialog.save_file() else { return };
        let Some(html) = self.build_report() else { return };
        match std::fs::write(&path, html) {
            Ok(()) => self.hover = format!("relatório gravado em {}", path.display()),
            Err(e) => self.error = Some(format!("{}: {}", path.display(), e)),
        }
    }

    /// Partículas do espaço de fase projetadas no corte.
    fn draw_particles(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        let Some(parts) = self.particles.as_ref().filter(|_| self.part_show) else { return };
        let view = self.slice_view();
        let normal = view.u.cross(view.v);
        // Duas reduções: uma antes do filtro, para não varrer milhões de
        // partículas a cada quadro, e outra depois, até o máximo pedido.
        let coarse = (parts.list.len() / 400_000).max(1);
        let near: Vec<&dump::Particle> = parts
            .list
            .iter()
            .step_by(coarse)
            .filter(|p| self.part_slab <= 0.0 || (p.pos() - view.center).dot(normal).abs() <= self.part_slab)
            .collect();
        for p in near.iter().step_by((near.len() / self.part_max.max(1)).max(1)) {
            let at = self.to_screen(rect, aspect, p.pos());
            if !rect.contains(at) {
                continue;
            }
            let color = match p.kf {
                22 => Color32::from_rgb(30, 80, 220),
                11 => Color32::from_rgb(220, 40, 40),
                -11 => Color32::from_rgb(20, 150, 60),
                _ => Color32::from_gray(70),
            };
            let d = p.dir();
            let tip = at + egui::vec2(d.dot(view.u) as f32, -d.dot(view.v) as f32) * 10.0;
            painter.line_segment([at, tip], Stroke::new(1.0, color));
            painter.circle_filled(at, 1.3, color);
        }
    }

    /// Janela com os resultados carregados ao longo da linha da régua: uma
    /// curva para cada resultado que a linha atravessa.
    fn profile_window(&mut self, ctx: &egui::Context) {
        if !self.profile_open {
            return;
        }
        const COLORS: [[u8; 3]; 6] = [[30, 90, 200], [215, 55, 40], [30, 150, 70], [150, 70, 180], [225, 135, 20], [0, 150, 160]];
        let mut open = true;
        let mut norm = self.profile_norm;
        let mut gamma = self.gamma_tol;
        let mut csv: Option<String> = None;
        egui::Window::new("Perfil ao longo da régua").open(&mut open).default_width(540.0).show(ctx, |ui| {
            let (Some(model), [a, b]) = (self.model.clone(), &self.measure[..]) else {
                ui.label("Marque dois pontos no corte com Shift+clique (ou use \"Ir até a malha\").");
                return;
            };
            const N: usize = 400;
            let length = (*b - *a).length();
            let points: Vec<(f64, DVec3, Vec<Level>)> = (0..=N)
                .map(|i| {
                    let f = i as f64 / N as f64;
                    let p = *a + (*b - *a) * f;
                    (f * length, p, model.locate(p).map(|l| l.1).unwrap_or_default())
                })
                .collect();
            // (resultado, valores ao longo da linha, máximo)
            let curves: Vec<(usize, Vec<Option<(f64, f64)>>, f64)> = self
                .meshes
                .iter()
                .enumerate()
                .filter_map(|(i, field)| {
                    let values: Vec<_> = points.iter().map(|(_, p, levels)| field.at(*p, levels)).collect();
                    let max = values.iter().flatten().map(|v| v.0).fold(0.0, f64::max);
                    (max > 0.0).then_some((i, values, max))
                })
                .collect();
            if curves.is_empty() {
                ui.label("A linha da régua não passa por valores positivos de nenhum resultado carregado.");
                return;
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut norm, "Normalizar cada curva pelo seu máximo")
                    .on_hover_text("Para comparar a forma de resultados em unidades diferentes (MCNP × PHITS, kerma × fluência)");
                if ui.button("Salvar CSV").clicked() {
                    let mut text = String::from("distancia_cm,x,y,z");
                    for (i, _, _) in &curves {
                        let name = self.meshes[*i].name().replace(',', ";");
                        text += &format!(",{} valor,{} erro_relativo", name, name);
                    }
                    for (k, (d, p, _)) in points.iter().enumerate() {
                        text += &format!("\n{},{},{},{}", d, p.x, p.y, p.z);
                        for (_, values, _) in &curves {
                            let (value, err) = values[k].unwrap_or((f64::NAN, f64::NAN));
                            text += &format!(",{},{}", value, err);
                        }
                    }
                    csv = Some(text + "\n");
                }
            });
            let log = self.mesh_log;
            let scale = |max: f64| if norm { 1.0 / max } else { 1.0 };
            let top = if norm { 1.0 } else { curves.iter().map(|c| c.2).fold(0.0, f64::max) };
            let smallest = curves
                .iter()
                .flat_map(|(_, values, max)| values.iter().flatten().map(move |v| v.0 * scale(*max)))
                .filter(|v| *v > 0.0)
                .fold(top, f64::min);
            let bottom = if log { smallest.max(top * 1e-6) } else { 0.0 };
            let height = |v: f64| {
                if !log {
                    return Some((v / top).clamp(0.0, 1.0));
                }
                (v > 0.0).then(|| ((v / bottom).ln() / (top / bottom).ln().max(1e-12)).clamp(0.0, 1.0))
            };
            let size = egui::vec2(ui.available_width().max(360.0), 260.0);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 3.0, Color32::WHITE);
            let plot = Rect::from_min_max(rect.min + egui::vec2(70.0, 10.0), rect.max - egui::vec2(12.0, 24.0));
            painter.rect_stroke(plot, 0.0, Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Inside);
            for (rank, (_, values, max)) in curves.iter().enumerate() {
                let [r, g, bl] = COLORS[rank % COLORS.len()];
                let mut last: Option<Pos2> = None;
                for ((d, _, _), v) in points.iter().zip(values) {
                    let here = v.and_then(|v| height(v.0 * scale(*max))).map(|t| {
                        Pos2::new(plot.left() + (d / length.max(1e-12)) as f32 * plot.width(), plot.bottom() - t as f32 * plot.height())
                    });
                    if let (Some(p0), Some(p1)) = (last, here) {
                        painter.line_segment([p0, p1], Stroke::new(1.8, Color32::from_rgb(r, g, bl)));
                    }
                    last = here;
                }
            }
            let font = egui::FontId::proportional(12.0);
            let ink = Color32::from_gray(40);
            let tick = |v: f64| if norm { format!("{:.3}", v) } else { format!("{:.2e}", v) };
            painter.text(plot.left_top() + egui::vec2(-4.0, 0.0), egui::Align2::RIGHT_TOP, tick(top), font.clone(), ink);
            painter.text(plot.left_bottom() + egui::vec2(-4.0, 0.0), egui::Align2::RIGHT_BOTTOM, tick(bottom), font.clone(), ink);
            painter.text(plot.left_bottom() + egui::vec2(0.0, 4.0), egui::Align2::LEFT_TOP, "0", font.clone(), ink);
            painter.text(plot.right_bottom() + egui::vec2(0.0, 4.0), egui::Align2::RIGHT_TOP, format!("{:.3} cm", length), font.clone(), ink);
            for (rank, (i, values, max)) in curves.iter().enumerate() {
                let [r, g, bl] = COLORS[rank % COLORS.len()];
                let at = values.iter().position(|v| v.is_some_and(|v| v.0 == *max)).map_or(0.0, |k| points[k].0);
                let text = format!("— {}: máximo {:.4e} a {:.3} cm", self.meshes[*i].name(), max, at);
                ui.colored_label(Color32::from_rgb(r, g, bl), text);
            }
            // Comparação ponto a ponto das duas primeiras curvas (MCNP × PHITS, por exemplo).
            if let [(ia, va, _), (ib, vb, _), ..] = &curves[..] {
                ui.separator();
                let mut pairs: Vec<compare::Pair> = Vec::new();
                for (k, (d, _, _)) in points.iter().enumerate() {
                    if let (Some(a), Some(b)) = (va[k], vb[k]) {
                        if a.0 > 0.0 && b.0 > 0.0 && pairs.last().is_none_or(|l| (l.1, l.3) != (a.0, b.0)) {
                            pairs.push((*d, a.0, a.1, b.0, b.1));
                        }
                    }
                }
                match compare::ratio(&pairs, norm) {
                    None => {
                        ui.weak("As duas primeiras curvas não têm pontos em comum ao longo da régua para comparar.");
                    }
                    Some(r) => {
                        let (name_a, name_b) = (self.meshes[*ia].name(), self.meshes[*ib].name());
                        let fitted = if norm { format!(", com A multiplicado por {:.4e} para ajustar a escala", r.k) } else { String::new() };
                        ui.label(format!("Comparação B ÷ A, onde A = {} e B = {}{}:", name_a, name_b, fitted));
                        let curve: Vec<[f64; 2]> = r.points.iter().map(|p| [p.0, p.1]).collect();
                        let ends = [r.points[0].0, r.points[r.points.len() - 1].0];
                        let unity = vec![[ends[0], 1.0], [ends[1], 1.0]];
                        line_plot(ui, 130.0, &[(unity.clone(), Color32::from_gray(150)), (curve, Color32::from_rgb(20, 20, 20))], [false, false]);
                        ui.label(format!(
                            "{} pontos · desvio médio {:.2}% · maior desvio {:.2}% (a {:.3} cm) · {} de {} dentro de 2σ · χ²/N = {:.2}",
                            r.points.len(),
                            r.mean_dev * 100.0,
                            r.worst.1 * 100.0,
                            r.worst.0,
                            r.within,
                            r.with_sigma,
                            r.chi2
                        ));
                        ui.horizontal(|ui| {
                            ui.label("Índice gama:");
                            ui.add(egui::DragValue::new(&mut gamma[0]).speed(0.1).range(0.1..=20.0).suffix(" %"));
                            ui.label("/");
                            ui.add(egui::DragValue::new(&mut gamma[1]).speed(0.1).range(0.1..=50.0).suffix(" mm"));
                        })
                        .response
                        .on_hover_text("Gama global em uma dimensão: A (já com o ajuste de escala, se ligado) é a referência, a tolerância de dose é uma fração do máximo dela, e ficam de fora os pontos abaixo de 10% do máximo");
                        if let Some(g) = compare::gamma(&pairs, r.k, gamma[0] / 100.0, gamma[1] / 10.0, 0.1) {
                            let curve: Vec<[f64; 2]> = g.points.iter().map(|p| [p.0, p.1]).collect();
                            line_plot(ui, 110.0, &[(unity, Color32::from_gray(150)), (curve, Color32::from_rgb(150, 70, 180))], [false, false]);
                            ui.label(format!(
                                "γ ≤ 1 em {} de {} pontos ({:.1}%) · maior γ = {:.2}",
                                g.pass,
                                g.points.len(),
                                100.0 * g.pass as f64 / g.points.len() as f64,
                                g.max
                            ));
                        }
                    }
                }
            }
        });
        self.profile_open = open;
        self.profile_norm = norm;
        self.gamma_tol = gamma;
        if let Some(text) = csv {
            let dialog = rfd::FileDialog::new().set_title("Salvar perfil").add_filter("CSV", &["csv"]).set_file_name("perfil.csv");
            if let Some(path) = dialog.save_file() {
                if let Err(e) = std::fs::write(&path, text) {
                    self.error = Some(format!("{}: {}", path.display(), e));
                }
            }
        }
    }

    /// Relê a geometria a partir de um texto editado, sem tocar no arquivo.
    fn load_text(&mut self, text: String) {
        let Some(path) = self.path.clone() else { return };
        trace(|| format!("relê o texto editado ({} edições)", self.edits.len()));
        let (tx, rx) = mpsc::channel();
        let (ctx, p) = (self.ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::parse_text(&text, &p));
            ctx.request_repaint();
        });
        self.loading = Some((path, false, true, rx));
    }

    /// Superfícies que aparecem na definição da célula.
    fn cell_surfaces(model: &Model, ci: usize) -> Vec<i32> {
        let prims = &model.cells[ci].prims;
        model.surfaces.iter().filter(|s| s.prims.iter().any(|p| prims.contains(p))).map(|s| s.id).collect()
    }

    fn apply_edit(&mut self) {
        let Some(model) = self.model.clone() else { return };
        let d = DVec3::from_array(self.edit_shift);
        if d == DVec3::ZERO {
            self.error = Some("o deslocamento é zero".into());
            return;
        }
        let (result, what) = if self.edit_mode == 1 {
            let Some(n) = self.edit_tr else { return };
            (crate::edit::move_transform(&model.source, &model, n, d), format!("TR{}", n))
        } else {
            let ids: Vec<String> = self.edit_targets.iter().map(|i| i.to_string()).collect();
            let what = if ids.len() > 6 { format!("{} superfícies", ids.len()) } else { format!("superfície {}", ids.join(", ")) };
            (crate::edit::move_surfaces(&model.source, &model, &self.edit_targets, d), what)
        };
        match result {
            Ok(text) => {
                if self.edits.is_empty() {
                    self.edit_base = model.source.clone();
                }
                self.edits.push((format!("{} deslocada de ({}, {}, {}) cm", what, d.x, d.y, d.z), text.clone()));
                self.error = None;
                self.load_text(text);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn undo_edit(&mut self, all: bool) {
        if all {
            self.edits.clear();
        } else {
            self.edits.pop();
        }
        let text = self.edits.last().map_or_else(|| self.edit_base.clone(), |e| e.1.clone());
        self.load_text(text);
    }

    /// Grava o texto editado num arquivo novo, que passa a ser o aberto.
    fn save_edited(&mut self) {
        let Some(model) = self.model.clone() else { return };
        let original = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Uma saída do MCNP vira input: o texto salvo é o do eco.
        let name = match original.rsplit_once('.') {
            Some((stem, "o")) => format!("{}_editado.i", stem),
            Some((stem, ext)) if ext.len() <= 4 => format!("{}_editado.{}", stem, ext),
            _ => format!("{}_editado", original),
        };
        let mut dialog = rfd::FileDialog::new().set_title("Salvar input editado").set_file_name(name);
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else { return };
        if Some(&path) == self.path.as_ref() {
            self.error = Some("escolha outro nome: o arquivo original não é sobrescrito".into());
            return;
        }
        match std::fs::write(&path, &model.source) {
            Ok(()) => {
                self.hover = format!("input editado gravado em {}", path.display());
                self.edits.clear();
                self.mtime = modified(&path);
                self.remember(&path);
                self.path = Some(path);
            }
            Err(e) => self.error = Some(format!("{}: {}", path.display(), e)),
        }
    }

    fn edit_window(&mut self, ctx: &egui::Context) {
        if !self.edit_open {
            return;
        }
        let Some(model) = self.model.clone() else { return };
        let mut open = true;
        let (mut apply, mut undo, mut undo_all, mut save, mut from_cell) = (false, false, false, false, false);
        let busy = self.loading.is_some();
        egui::Window::new("Editar geometria").open(&mut open).default_width(560.0).show(ctx, |ui| {
            if model.source.is_empty() {
                ui.label("Este arquivo não guarda o texto do input: não dá para editar.");
                return;
            }
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.edit_mode, 0, "Mover superfícies");
                ui.selectable_value(&mut self.edit_mode, 1, "Mover uma transformação (TR)");
            });
            ui.separator();
            if self.edit_mode == 0 {
                ui.label("Superfícies a mover (clique numa para tirar):");
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    if self.edit_targets.is_empty() {
                        ui.weak("nenhuma ainda");
                    }
                    for (k, id) in self.edit_targets.iter().enumerate() {
                        if ui.small_button(id.to_string()).on_hover_text("Tirar da lista").clicked() {
                            remove = Some(k);
                        }
                    }
                });
                if let Some(k) = remove {
                    self.edit_targets.remove(k);
                }
                ui.horizontal(|ui| {
                    from_cell = ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Todas da célula selecionada"))
                        .on_hover_text("Acrescenta as superfícies que definem a célula. As vizinhas que usam as mesmas superfícies acompanham o movimento.")
                        .on_disabled_hover_text("Clique numa célula no corte primeiro")
                        .clicked();
                    ui.add(egui::TextEdit::singleline(&mut self.edit_input).hint_text("nº").desired_width(60.0));
                    let typed = self.edit_input.trim().parse::<i32>().ok().filter(|id| model.surf_index.contains_key(id));
                    if ui.add_enabled(typed.is_some(), egui::Button::new("Acrescentar")).clicked() {
                        let id = typed.unwrap();
                        if !self.edit_targets.contains(&id) {
                            self.edit_targets.push(id);
                        }
                        self.edit_input.clear();
                    }
                    if ui.button("Limpar").clicked() {
                        self.edit_targets.clear();
                    }
                });
                ui.weak("Ctrl+clique no corte acrescenta (ou tira) a superfície mais próxima do cursor.");
                egui::ScrollArea::vertical().id_salt("cartoes_edit").max_height(120.0).show(ui, |ui| {
                    for id in &self.edit_targets {
                        if let Some(s) = model.surf_index.get(id).map(|&i| &model.surfaces[i]) {
                            ui.add(egui::Label::new(egui::RichText::new(crate::edit::card_lines(&model.source, s.line, s.last)).monospace().small()).wrap());
                        }
                    }
                });
            } else {
                if model.tr_cards.is_empty() {
                    ui.label("Este input não tem cartões TR.");
                } else {
                    let text = self.edit_tr.map_or("escolha".to_string(), |n| format!("TR{}", n));
                    egui::ComboBox::from_id_salt("tr_edit").selected_text(text).show_ui(ui, |ui| {
                        for (n, _, _) in &model.tr_cards {
                            ui.selectable_value(&mut self.edit_tr, Some(*n), format!("TR{}", n));
                        }
                    });
                    if let Some(&(_, first, last)) = self.edit_tr.and_then(|n| model.tr_cards.iter().find(|t| t.0 == n)) {
                        ui.add(egui::Label::new(egui::RichText::new(crate::edit::card_lines(&model.source, first, last)).monospace().small()).wrap());
                    }
                    ui.weak("Tudo o que usa a transformação vai junto: as superfícies escritas com ela e as células com TRCL apontando para ela.");
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Deslocamento");
                for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                    ui.label(format!("d{}", axis));
                    ui.add(egui::DragValue::new(&mut self.edit_shift[k]).speed(0.05).suffix(" cm"));
                }
            });
            let ready = if self.edit_mode == 0 { !self.edit_targets.is_empty() } else { self.edit_tr.is_some() };
            apply = ui.add_enabled(ready && !busy, egui::Button::new("Aplicar")).clicked();
            ui.separator();
            if self.edits.is_empty() {
                ui.weak("Nenhuma edição. As edições ficam só na memória até você salvar; o arquivo original nunca é alterado.");
            } else {
                ui.strong(format!("Edições ainda não salvas ({}):", self.edits.len()));
                for (k, (what, _)) in self.edits.iter().enumerate() {
                    ui.label(format!("{}. {}", k + 1, what));
                }
                ui.horizontal(|ui| {
                    undo = ui.add_enabled(!busy, egui::Button::new("Desfazer a última")).clicked();
                    undo_all = ui.add_enabled(!busy, egui::Button::new("Desfazer todas")).clicked();
                    save = ui.add_enabled(!busy, egui::Button::new("Salvar como…")).on_hover_text("Grava o input editado num arquivo novo. Cada cartão mexido leva acima um comentário com a versão anterior.").clicked();
                });
                // Resultado da verificação feita depois da última releitura.
                match (&self.audit, busy || self.auditing.is_some()) {
                    (_, true) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("relendo e verificando a geometria…");
                        });
                    }
                    (Some(a), false) if a.overlaps.is_empty() && a.undefined == 0 => {
                        ui.colored_label(Color32::from_rgb(90, 190, 110), "Verificação depois da edição: nenhuma sobreposição nem região sem célula nos pontos sorteados.");
                    }
                    (Some(a), false) => {
                        let text = format!("Verificação depois da edição: {} pares de células sobrepostas{}. Veja onde no Diagnóstico ou na verificação do painel.", a.overlaps.len(), if a.undefined > 0 { " e região sem célula" } else { "" });
                        ui.colored_label(Color32::from_rgb(240, 80, 70), text);
                    }
                    (None, false) => {
                        ui.weak("A geometria editada não foi verificada (a verificação automática está desligada no Diagnóstico).");
                    }
                }
            }
        });
        self.edit_open = open;
        if from_cell {
            if let Some(ci) = self.selected {
                for id in Self::cell_surfaces(&model, ci) {
                    if !self.edit_targets.contains(&id) {
                        self.edit_targets.push(id);
                    }
                }
            }
        }
        if apply {
            self.apply_edit();
        }
        if undo || undo_all {
            self.undo_edit(undo_all);
        }
        if save {
            self.save_edited();
        }
    }

    /// Tabela dos resultados que são um número só (um tally por órgão, por
    /// exemplo): valor, erro e se a média ainda se mexia no fim da rodada. Com
    /// mais de um mctal carregado, mostra também a soma ponderada por MUf.
    fn tally_window(&mut self, ctx: &egui::Context) {
        if !self.tally_open {
            return;
        }
        struct Row {
            cell: Option<usize>,
            name: String,
            value: f64,
            error: f64,
            /// Histórias da rodada, ou em quantos arquivos o tally entrou na soma.
            count: String,
            unstable: bool,
        }
        let total = self.tally_total && self.tally_sum.as_ref().is_some_and(|s| !s.rows.is_empty());
        let mut rows: Vec<Row> = match &self.tally_sum {
            Some(sum) if total => sum
                .rows
                .iter()
                .map(|r| Row { cell: r.cell, name: r.name.clone(), value: r.value, error: r.error, count: r.files.to_string(), unstable: false })
                .collect(),
            _ => {
                let several = self.tally_sum.is_some();
                let singles = self.meshes.iter().filter(|f| !f.is_sum());
                singles
                    .filter_map(|f| {
                        let (value, error) = f.single()?;
                        let h = f.history();
                        let last = h.last().copied();
                        // Mesmo critério da janela de convergência.
                        let unstable = last.is_some_and(|last| h.len() >= 4 && h[h.len() / 2..].iter().map(|r| (r[1] / last[1] - 1.0).abs()).fold(0.0, f64::max) > 2.0 * last[2]);
                        let name = if several { format!("{} · {}", f.source(), f.name()) } else { f.name().to_string() };
                        Some(Row { cell: f.cell(), name, value, error, count: last.map_or("—".to_string(), |l| format!("{:e}", l[0])), unstable })
                    })
                    .collect()
            }
        };
        match self.tally_sort {
            1 => rows.sort_by(|a, b| b.value.total_cmp(&a.value)),
            2 => rows.sort_by(|a, b| b.error.total_cmp(&a.error)),
            _ => {}
        }
        let (bad, orange) = (Color32::from_rgb(240, 80, 70), Color32::from_rgb(235, 150, 40));
        let count_title = if total { "arquivos" } else { "histórias" };
        let mut open = true;
        let (mut pick, mut csv) = (None, false);
        egui::Window::new("Tabela de tallies").open(&mut open).default_width(660.0).default_height(500.0).show(ctx, |ui| {
            if rows.is_empty() {
                ui.label("Nenhum resultado carregado é um valor único por tally. Carregue um mctal com tallies de uma célula (dose por órgão, por exemplo) em \"Resultados…\".");
                return;
            }
            if let Some(sum) = &self.tally_sum {
                if sum.missing.is_empty() {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.tally_total, true, "Soma ponderada por MUf")
                            .on_hover_text("Total = Σ MUf × valor de cada arquivo, com o MUf lido do título do mctal; as incertezas somam em quadratura");
                        ui.selectable_value(&mut self.tally_total, false, "Cada arquivo");
                    });
                    let weights: Vec<String> = sum.files.iter().map(|(file, w)| format!("{} × {}", file, w)).collect();
                    let short = if weights.len() > 3 { format!("{} + … ({} arquivos)", weights[..2].join(" + "), weights.len()) } else { weights.join(" + ") };
                    ui.weak(short).on_hover_text(weights.join("\n"));
                    let partial = sum.rows.iter().filter(|r| r.files < sum.files.len()).count();
                    if total && partial > 0 {
                        ui.colored_label(orange, format!("{} tallies não aparecem em todos os arquivos: a soma deles está incompleta (veja a coluna \"arquivos\").", partial));
                    }
                } else {
                    ui.colored_label(orange, format!("Há mais de um mctal carregado, mas não somei: falta \"MUf=\" no título de {}.", sum.missing.join(", ")));
                }
            }
            let over = |limit: f64| rows.iter().filter(|r| r.error > limit).count();
            ui.label(format!(
                "{} tallies · {} com erro acima de 10% · {} acima de 5% · {} com a média ainda se movendo no fim",
                rows.len(),
                over(0.10),
                over(0.05),
                rows.iter().filter(|r| r.unstable).count()
            ));
            ui.horizontal(|ui| {
                ui.label("Ordenar por");
                ui.selectable_value(&mut self.tally_sort, 0, "arquivo");
                ui.selectable_value(&mut self.tally_sort, 1, "valor");
                ui.selectable_value(&mut self.tally_sort, 2, "erro");
                csv = ui.button("Salvar CSV").clicked();
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("tabela_tallies").striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                    for title in ["tally", "valor", "erro", count_title, ""] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for r in &rows {
                        if ui.link(&r.name).on_hover_text("Clique para selecionar a célula deste tally e levar o corte até ela").clicked() {
                            pick = r.cell;
                        }
                        ui.label(format!("{:.5e}", r.value));
                        let text = format!("{:.2}%", r.error * 100.0);
                        match r.error {
                            e if e > 0.10 => ui.colored_label(bad, text).on_hover_text("Acima de 10%: o MCNP considera o resultado questionável"),
                            e if e > 0.05 => ui.colored_label(orange, text),
                            _ => ui.label(text),
                        };
                        ui.label(&r.count);
                        if r.unstable {
                            ui.colored_label(orange, "média instável").on_hover_text("Na segunda metade da rodada a média ainda se moveu mais que duas vezes o erro final; veja em \"Convergência do tally\"");
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            });
        });
        self.tally_open = open;
        if csv {
            let mut text = format!("tally,valor,erro_relativo,{}\n", if total { "arquivos" } else { "historias" });
            for r in &rows {
                text += &format!("{},{},{},{}\n", r.name.replace(',', ";"), r.value, r.error, r.count);
            }
            let name = if total { "tallies_soma.csv" } else { "tallies.csv" };
            let dialog = rfd::FileDialog::new().set_title("Salvar tabela de tallies").add_filter("CSV", &["csv"]).set_file_name(name);
            if let Some(path) = dialog.save_file() {
                if let Err(e) = std::fs::write(&path, text) {
                    self.error = Some(format!("{}: {}", path.display(), e));
                }
            }
        }
        let found = pick.and_then(|ci: usize| Some((ci, self.model.as_ref()?.point_in(ci))));
        if let Some((ci, point)) = found {
            self.selected = Some(ci);
            match point {
                Some(p) => self.go_to(p),
                None => self.hover = "célula pequena demais para ser achada na grade de busca; ela fica destacada em amarelo onde aparecer".into(),
            }
            self.refresh();
        }
    }

    /// Uma linha por material usado: densidades, células, volume, massa e composição.
    fn material_rows(&self) -> Vec<MatRow> {
        let Some(model) = &self.model else { return Vec::new() };
        // A posição na lista de todos os materiais é o que define a cor.
        let mut all: Vec<i32> = model.cells.iter().map(|c| c.mat).filter(|&m| m != 0).collect();
        all.sort_unstable();
        all.dedup();
        let mut rows = Vec::new();
        for (rank, &id) in all.iter().enumerate() {
            let cells: Vec<(usize, &crate::geom::Cell)> = model.cells.iter().enumerate().filter(|(_, c)| c.mat == id && !c.outside).collect();
            if cells.is_empty() {
                continue;
            }
            let mut rhos: Vec<String> = cells.iter().map(|(_, c)| c.rho.to_string()).collect();
            rhos.sort();
            rhos.dedup();
            let (mut volume, mut mass) = (None, None);
            if let Some(a) = &self.audit {
                let parts: Vec<(f64, f64)> = cells.iter().filter_map(|(i, c)| Some((a.volume(*i)?.0, c.rho))).collect();
                volume = Some((!parts.is_empty()).then(|| parts.iter().map(|p| p.0).sum::<f64>()));
                // Densidade negativa é g/cm³; com a positiva (átomos/b·cm) não dá para somar massa.
                mass = (!parts.is_empty() && parts.iter().all(|p| p.1 < 0.0)).then(|| parts.iter().map(|p| p.0 * -p.1).sum::<f64>());
            }
            rows.push(MatRow { id, rank, rho: rhos.join("; "), cells: cells.len(), volume, mass, nuclides: model.materials.get(&id).map(|m| m.nuclides.clone()) });
        }
        rows
    }

    fn materials_window(&mut self, ctx: &egui::Context) {
        if !self.mats_open {
            return;
        }
        let rows = self.material_rows();
        let mut open = true;
        let (mut filter, mut csv) = (None, false);
        egui::Window::new("Materiais").open(&mut open).default_width(760.0).default_height(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                csv = ui.button("Salvar CSV").clicked();
                if self.audit.is_none() {
                    ui.weak("Volume e massa aparecem quando a verificação da geometria termina.");
                } else if let Some(total) = rows.iter().map(|r| r.mass).sum::<Option<f64>>() {
                    ui.label(format!("Massa total ≈ {:.4e} g", total));
                }
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).max_height(ui.available_height() - 24.0).show(ui, |ui| {
                egui::Grid::new("tabela_materiais").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                    for title in ["", "material", "densidade", "células", "volume (cm³)", "massa (g)", "frações", "composição"] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for r in &rows {
                        let [red, green, blue] = render::material_color(r.rank, r.id);
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(red, green, blue));
                        if ui.link(format!("m{}", r.id)).on_hover_text("Clique para listar só as células deste material").clicked() {
                            filter = Some(format!("m{}", r.id));
                        }
                        ui.label(&r.rho).on_hover_text("Negativa: g/cm³. Positiva: átomos/(b·cm).");
                        ui.label(r.cells.to_string());
                        match r.volume {
                            Some(Some(v)) => ui.label(format!("{:.4e}", v)),
                            Some(None) => ui.weak("nenhum ponto").on_hover_text("Nenhum ponto sorteado caiu neste material: é pequeno demais para a amostragem"),
                            None => ui.weak("—"),
                        };
                        match r.mass {
                            Some(g) => ui.label(format!("{:.4e}", g)),
                            None => ui.weak("—"),
                        };
                        match &r.nuclides {
                            None => {
                                ui.colored_label(Color32::from_rgb(240, 80, 70), "sem cartão");
                                ui.label("");
                            }
                            Some(list) => {
                                let (atoms, weight) = (list.iter().all(|n| n.1 > 0.0), list.iter().all(|n| n.1 < 0.0));
                                match (atoms, weight) {
                                    (true, _) => ui.label("atômicas"),
                                    (_, true) => ui.label("em massa"),
                                    _ => ui.colored_label(Color32::from_rgb(240, 80, 70), "misturadas"),
                                };
                                let short: Vec<String> = list.iter().take(5).map(|(n, f)| format!("{} {}", n, f)).collect();
                                let more = if list.len() > 5 { format!(" … (+{})", list.len() - 5) } else { String::new() };
                                let full: Vec<String> = list.iter().map(|(n, f)| format!("{}  {}", n, f)).collect();
                                ui.label(short.join(", ") + &more).on_hover_text(full.join("\n"));
                            }
                        }
                        ui.end_row();
                    }
                });
            });
            ui.weak("Volume e massa são estimativas por amostragem dentro da caixa do modelo; células que passam dela (o ar em volta) ficam subestimadas.");
        });
        self.mats_open = open;
        if let Some(f) = filter {
            self.filter = f;
        }
        if csv {
            let mut text = String::from("material,densidade,celulas,volume_cm3,massa_g,composicao\n");
            for r in &rows {
                let number = |v: Option<f64>| v.map_or(String::new(), |v| v.to_string());
                let composition = r.nuclides.as_ref().map_or(String::new(), |n| n.iter().map(|(n, f)| format!("{} {}", n, f)).collect::<Vec<_>>().join(" "));
                text += &format!("{},{},{},{},{},{}\n", r.id, r.rho, r.cells, number(r.volume.flatten()), number(r.mass), composition);
            }
            let dialog = rfd::FileDialog::new().set_title("Salvar tabela de materiais").add_filter("CSV", &["csv"]).set_file_name("materiais.csv");
            if let Some(path) = dialog.save_file() {
                if let Err(e) = std::fs::write(&path, text) {
                    self.error = Some(format!("{}: {}", path.display(), e));
                }
            }
        }
    }

    /// Escolhe o input com que o aberto será comparado (a versão anterior,
    /// por exemplo) e o lê em outra thread.
    fn compare_with(&mut self) {
        let dialog = rfd::FileDialog::new().set_title("Comparar com outro input (a versão anterior, por exemplo)");
        let Some(path) = dialog.pick_file() else { return };
        let (tx, rx) = mpsc::channel();
        let (ctx, p) = (self.ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::load_model(&p));
            ctx.request_repaint();
        });
        self.diff_loading = Some((path, rx));
        self.diff_open = true;
    }

    fn poll_diff(&mut self) {
        let Some((_, rx)) = &self.diff_loading else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("falha inesperada ao ler o arquivo".into()),
        };
        let (path, _) = self.diff_loading.take().unwrap();
        match result {
            Ok(base) => {
                self.diff_base = Some((path, Arc::new(base)));
                self.update_diff();
            }
            Err(e) => self.error = Some(format!("{}: {}", path.display(), e)),
        }
    }

    /// Refaz a comparação: o input aberto mudou ou a base foi escolhida.
    fn update_diff(&mut self) {
        self.diff = match (&self.diff_base, &self.model) {
            (Some((_, base)), Some(model)) => Some(compare::inputs(base, model)),
            _ => None,
        };
    }

    fn diff_window(&mut self, ctx: &egui::Context) {
        if !self.diff_open {
            return;
        }
        let mut open = true;
        let (mut goto_cell, mut close) = (None, false);
        egui::Window::new("Comparação de inputs").open(&mut open).default_width(700.0).default_height(520.0).show(ctx, |ui| {
            let (Some((base, _)), Some(diff)) = (&self.diff_base, &self.diff) else {
                ui.label(if self.diff_loading.is_some() { "Lendo o outro input…" } else { "Use \"Comparar com…\" para escolher o outro input." });
                return;
            };
            let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let current = self.path.as_deref().map(name).unwrap_or_default();
            ui.label(format!("De {} (antes) para {} (aberto agora): {} diferenças.", name(base), current, diff.len()))
                .on_hover_text(base.display().to_string());
            ui.horizontal(|ui| {
                close = ui.button("Encerrar comparação").clicked();
                ui.weak("É refeita a cada recarga: deixe aberta enquanto edita o input.");
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if diff.len() == 0 {
                    ui.colored_label(Color32::from_rgb(90, 190, 110), "Nenhuma diferença nos cartões de célula, superfície, material e dados.");
                }
                for (title, list) in [("Células", &diff.cells), ("Superfícies", &diff.surfaces), ("Materiais", &diff.materials), ("Dados", &diff.data)] {
                    if list.is_empty() {
                        continue;
                    }
                    egui::CollapsingHeader::new(format!("{} ({})", title, list.len())).default_open(true).show(ui, |ui| {
                        const SHOWN: usize = 300;
                        for change in list.iter().take(SHOWN) {
                            show_change(ui, change, &mut goto_cell);
                        }
                        if list.len() > SHOWN {
                            ui.weak(format!("… e mais {}", list.len() - SHOWN));
                        }
                    });
                }
            });
        });
        self.diff_open = open;
        if close {
            (self.diff_base, self.diff, self.diff_open) = (None, None, false);
        }
        let found = goto_cell.and_then(|id: i32| {
            let model = self.model.as_ref()?;
            let ci = *model.cell_index.get(&id)?;
            Some((ci, model.point_in(ci)))
        });
        if let Some((ci, point)) = found {
            self.selected = Some(ci);
            match point {
                Some(p) => self.go_to(p),
                None => self.hover = "célula pequena demais para ser achada na grade de busca".into(),
            }
            self.refresh();
        }
    }

    /// Aviso sobre o corte exibido: região sem célula ou células sobrepostas.
    fn draw_problems(&self, painter: &egui::Painter, rect: Rect) {
        let frame = &self.pane_2d.frame;
        let mut lines = Vec::new();
        if frame.undefined > 0 {
            lines.push("⚠ há região sem célula neste corte (hachura rosa)");
        }
        if frame.overlapping > 0 {
            lines.push("⚠ há células sobrepostas neste corte (hachura vermelha e amarela)");
        }
        let font = egui::FontId::proportional(13.0);
        for (i, line) in lines.into_iter().enumerate() {
            let pos = rect.left_top() + egui::vec2(10.0, 28.0 + 18.0 * i as f32);
            painter.text(pos + egui::vec2(1.0, 1.0), egui::Align2::LEFT_TOP, line, font.clone(), Color32::WHITE);
            painter.text(pos, egui::Align2::LEFT_TOP, line, font.clone(), Color32::from_rgb(190, 20, 20));
        }
    }

    /// Régua: os pontos marcados com Shift+clique e a distância entre eles.
    fn draw_measure(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        let ink = Color32::from_rgb(20, 20, 200);
        let pts: Vec<Pos2> = self.measure.iter().map(|&p| self.to_screen(rect, aspect, p)).collect();
        for &pt in &pts {
            painter.circle_filled(pt, 4.0, Color32::WHITE);
            painter.circle_filled(pt, 3.0, ink);
        }
        if let ([a, b], [pa, pb]) = (&pts[..], &self.measure[..]) {
            painter.line_segment([*a, *b], Stroke::new(3.0, Color32::WHITE));
            painter.line_segment([*a, *b], Stroke::new(1.5, ink));
            let d = *pb - *pa;
            let text = format!("{:.4} cm   (Δx {:.4}  Δy {:.4}  Δz {:.4})", d.length(), d.x, d.y, d.z);
            let pos = Pos2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0) + egui::vec2(8.0, -8.0);
            let font = egui::FontId::proportional(13.0);
            for (off, color) in [(1.0, Color32::WHITE), (-1.0, Color32::WHITE), (0.0, ink)] {
                painter.text(pos + egui::vec2(off, off), egui::Align2::LEFT_BOTTOM, &text, font.clone(), color);
            }
        }
    }

    fn scale_bar(&self, painter: &egui::Painter, rect: Rect) {
        let cm_per_pt = 2.0 * self.half_width / rect.width() as f64;
        let target = 120.0 * cm_per_pt;
        let pow = 10f64.powf(target.log10().floor());
        let nice = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * pow).rfind(|&v| v <= target).unwrap_or(pow);
        let len = (nice / cm_per_pt) as f32;
        let a = rect.left_bottom() + egui::vec2(14.0, -16.0);
        let b = a + egui::vec2(len, 0.0);
        let (u, v, _) = self.plane.axes();
        let font = egui::FontId::proportional(12.0);
        for (off, color) in [(1.0, Color32::WHITE), (0.0, Color32::BLACK)] {
            let o = egui::vec2(off, off);
            painter.line_segment([a + o, b + o], Stroke::new(2.0, color));
            painter.text(a + o + egui::vec2(0.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("{} cm", nice), font.clone(), color);
            painter.text(
                rect.left_top() + o + egui::vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                format!("horizontal: {}   vertical: {}{}", AXIS_NAMES[u], AXIS_NAMES[v], if self.tilt == [0.0; 2] { "" } else { "   (plano inclinado)" }),
                font.clone(),
                color,
            );
        }
    }

    fn pane_3d(&mut self, ui: &mut egui::Ui) {
        let (mut changed, mut save) = (false, false);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Vista 3D");
            changed |= ui.checkbox(&mut self.hide_void, "Ocultar vazio").changed();
            changed |= ui
                .checkbox(&mut self.hide_gas, "Ocultar gases")
                .on_hover_text("Oculta materiais com densidade abaixo de 0,01 g/cm³")
                .changed();
            ui.checkbox(&mut self.clip_at_slice, "Cortar no plano 2D");
            ui.add_enabled(self.clip_at_slice, egui::Checkbox::new(&mut self.clip_flip, "Inverter lado"));
            ui.checkbox(&mut self.edges_3d, "Contornos");
            if ui.button("Enquadrar").clicked() {
                self.fit_3d();
            }
            save = ui.button("Salvar PNG").clicked();
        });
        if changed {
            self.refresh();
        }
        if save {
            self.save_image(true);
        }

        let (rect, resp) = Self::canvas(ui, 1.0);
        self.pane_rects[1] = rect;
        let aspect = (rect.width() / rect.height()) as f64;
        self.aspect_3d = aspect;
        if std::mem::take(&mut self.fit_pending[1]) {
            self.fit_3d();
        }
        let (_, _, right, up) = self.cam.basis();
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.cam.yaw -= d.x as f64 * 0.008;
            self.cam.pitch = (self.cam.pitch + d.y as f64 * 0.008).clamp(-1.55, 1.55);
        } else if resp.dragged() {
            let d = resp.drag_delta();
            let k = 2.0 * self.cam.dist * (self.cam.fov_deg.to_radians() / 2.0).tan() / rect.height() as f64;
            self.cam.target += (up * d.y as f64 - right * d.x as f64) * k;
        }
        if let Some(pos) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y) as f64;
            if scroll != 0.0 {
                self.cam.dist = (self.cam.dist * (-scroll * 0.0025).exp()).clamp(1e-6, 1e9);
            }
            let f = &self.pane_3d.frame;
            if f.w > 0 && !f.leaf.is_empty() {
                let fx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 0.9999);
                let fy = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 0.9999);
                let i = (fy * f.h as f32) as usize * f.w + (fx * f.w as f32) as usize;
                let leaf = f.leaf[i];
                if leaf != NONE {
                    // A imagem exibida pode ser de uma vista anterior à atual.
                    let cam = self.pane_3d.shown.as_ref().map_or(self.cam, |k| k.cam);
                    let (o, d) = cam.ray(fx as f64, fy as f64, aspect);
                    let p = o + d * f.depth[i] as f64;
                    self.hover = format!("x {:.3}  y {:.3}  z {:.3}   |   {}", p.x, p.y, p.z, self.cell_summary(leaf));
                }
                if resp.clicked() {
                    self.selected = (leaf != NONE && self.selected != Some(leaf as usize)).then_some(leaf as usize);
                    self.refresh();
                }
            }
        }

        if let Some(model) = &self.model {
            let key = self.key_3d(Self::render_size(ui, rect));
            let (model, pal, visible, translucent) = (model.clone(), self.palette.clone(), self.visible.clone(), self.translucent.clone());
            let (cam, clip, edges) = (self.cam, key.clip, self.edges_3d);
            self.pane_3d.sync(ui.ctx(), "vista3d", key, move |w, h, full, cancel| {
                let scene = Scene { model: &model, pal: &pal, visible: &visible, translucent: &translucent, clip, edges: edges && full };
                render::render_3d(&scene, &cam, w, h, cancel)
            });
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(34, 37, 46));
        if let (Some(tex), true) = (&self.pane_3d.tex, self.model.is_some()) {
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            self.pane_3d.draw_status(&painter, rect, Color32::from_gray(170));
            // Tríade de eixos no canto.
            let origin = rect.left_bottom() + egui::vec2(44.0, -44.0);
            let colors = [Color32::from_rgb(235, 80, 80), Color32::from_rgb(90, 200, 90), Color32::from_rgb(90, 140, 255)];
            for (i, color) in colors.into_iter().enumerate() {
                let a = DVec3::AXES[i];
                let tip = origin + egui::vec2(a.dot(right) as f32, -a.dot(up) as f32) * 30.0;
                painter.line_segment([origin, tip], Stroke::new(2.0, color));
                painter.text(tip, egui::Align2::CENTER_CENTER, AXIS_NAMES[i], egui::FontId::proportional(12.0), color);
            }
        }
    }
}

/// (classe no relatório, rótulo) de cada gravidade.
fn severity_tag(level: Severity) -> (&'static str, &'static str) {
    match level {
        Severity::Ok => ("ok", "ok"),
        Severity::Info => ("info", "info"),
        Severity::Warn => ("warn", "atenção"),
        Severity::Error => ("error", "ERRO"),
    }
}

/// Lista de achados, os mais graves primeiro; os que têm lugar viram um
/// atalho que leva o corte até lá.
fn show_findings(ui: &mut egui::Ui, title: &str, list: &[Finding], goto: &mut Option<DVec3>) {
    ui.add_space(6.0);
    ui.strong(title);
    let mut sorted: Vec<&Finding> = list.iter().collect();
    sorted.sort_by(|a, b| b.level.cmp(&a.level));
    for f in sorted {
        let color = match f.level {
            Severity::Ok => Color32::from_rgb(90, 190, 110),
            Severity::Info => Color32::from_gray(150),
            Severity::Warn => Color32::from_rgb(235, 150, 40),
            Severity::Error => Color32::from_rgb(240, 80, 70),
        };
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(color, format!("[{}]", severity_tag(f.level).1));
            match f.at {
                Some(p) => {
                    if ui.link(&f.text).on_hover_text("Clique para levar o corte até lá").clicked() {
                        *goto = Some(p);
                    }
                }
                None => {
                    ui.label(&f.text);
                }
            }
        });
    }
}

/// Gráfico de linhas simples, com os extremos de cada eixo anotados. `log`
/// diz quais eixos (x, y) são logarítmicos.
fn line_plot(ui: &mut egui::Ui, height: f32, curves: &[(Vec<[f64; 2]>, Color32)], log: [bool; 2]) {
    let scale = |v: f64, axis: usize| if log[axis] { v.log10() } else { v };
    let usable = |p: &&[f64; 2]| (0..2).all(|a| p[a].is_finite() && (!log[a] || p[a] > 0.0));
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in curves.iter().flat_map(|c| c.0.iter()).filter(usable) {
        for a in 0..2 {
            lo[a] = lo[a].min(scale(p[a], a));
            hi[a] = hi[a].max(scale(p[a], a));
        }
    }
    if lo[0] >= hi[0] || !lo[1].is_finite() {
        return;
    }
    // Folga vertical, e altura mínima quando a curva é plana.
    let pad = ((hi[1] - lo[1]) * 0.06).max(1e-3 * hi[1].abs().max(1e-12));
    (lo[1], hi[1]) = (lo[1] - pad, hi[1] + pad);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(320.0), height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::WHITE);
    let plot = Rect::from_min_max(rect.min + egui::vec2(70.0, 8.0), rect.max - egui::vec2(12.0, 22.0));
    painter.rect_stroke(plot, 0.0, Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Inside);
    let place = |p: &[f64; 2]| {
        let f = [0, 1].map(|a| ((scale(p[a], a) - lo[a]) / (hi[a] - lo[a])) as f32);
        Pos2::new(plot.left() + f[0] * plot.width(), plot.bottom() - f[1] * plot.height())
    };
    for (points, color) in curves {
        let line: Vec<Pos2> = points.iter().filter(usable).map(place).collect();
        for pair in line.windows(2) {
            painter.line_segment([pair[0], pair[1]], Stroke::new(1.8, *color));
        }
    }
    let value = |v: f64, axis: usize| format!("{:.3e}", if log[axis] { 10f64.powf(v) } else { v });
    let (font, ink) = (egui::FontId::proportional(12.0), Color32::from_gray(40));
    painter.text(plot.left_top() + egui::vec2(-4.0, 0.0), egui::Align2::RIGHT_TOP, value(hi[1], 1), font.clone(), ink);
    painter.text(plot.left_bottom() + egui::vec2(-4.0, 0.0), egui::Align2::RIGHT_BOTTOM, value(lo[1], 1), font.clone(), ink);
    painter.text(plot.left_bottom() + egui::vec2(0.0, 4.0), egui::Align2::LEFT_TOP, value(lo[0], 0), font.clone(), ink);
    painter.text(plot.right_bottom() + egui::vec2(0.0, 4.0), egui::Align2::RIGHT_TOP, value(hi[0], 0), font, ink);
}

/// Uma diferença entre os inputs: o que mudou e o cartão antes e agora.
fn show_change(ui: &mut egui::Ui, change: &compare::Change, goto: &mut Option<i32>) {
    let (tag, color) = match change.kind {
        compare::Kind::Added => ("novo", Color32::from_rgb(90, 190, 110)),
        compare::Kind::Removed => ("removido", Color32::from_rgb(240, 80, 70)),
        compare::Kind::Changed => ("mudou", Color32::from_rgb(235, 150, 40)),
    };
    ui.horizontal(|ui| {
        ui.colored_label(color, format!("[{}]", tag));
        match change.cell {
            Some(id) => {
                if ui.link(&change.what).on_hover_text("Clique para selecionar a célula e levar o corte até ela").clicked() {
                    *goto = Some(id);
                }
            }
            None => {
                ui.label(&change.what);
            }
        }
    });
    for (label, text) in [("antes", &change.before), ("agora", &change.after)] {
        if !text.is_empty() {
            let mut clipped: String = text.chars().take(320).collect();
            if clipped.len() < text.len() {
                clipped += " …";
            }
            ui.add(egui::Label::new(egui::RichText::new(format!("  {}: {}", label, clipped)).monospace().weak()).wrap());
        }
    }
}

/// Linha da tabela de materiais.
struct MatRow {
    id: i32,
    /// Posição na lista de materiais, que define a cor.
    rank: usize,
    rho: String,
    cells: usize,
    /// `None`: geometria não verificada; `Some(None)`: nenhum ponto sorteado no material.
    volume: Option<Option<f64>>,
    mass: Option<f64>,
    nuclides: Option<Vec<(String, f64)>>,
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { TABLE[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

fn color_label(by: ColorBy) -> &'static str {
    match by {
        ColorBy::Material => "Material",
        ColorBy::Cell => "Célula",
        ColorBy::Universe => "Universo",
        ColorBy::Density => "Densidade",
        ColorBy::Importance => "Importância",
    }
}

/// Gera o relatório sem abrir a janela (`--relatorio`): lê o input, verifica
/// a geometria, junta os arquivos de resultado e grava o HTML.
pub fn headless_report(input: PathBuf, results: Vec<PathBuf>, out: &std::path::Path) -> Result<(), String> {
    let mut app = App::new(Some(input), egui::Context::default());
    let wait = Duration::from_millis(30);
    while app.loading.is_some() {
        std::thread::sleep(wait);
        app.poll_loading();
    }
    if let Some(e) = app.error.take() {
        return Err(e);
    }
    app.fit_2d();
    app.fit_3d();
    while app.auditing.is_some() {
        std::thread::sleep(wait);
        app.poll_audit();
    }
    app.add_results(results);
    if let Some(e) = app.error.take() {
        eprintln!("aviso: {}", e);
    }
    let html = app.build_report().ok_or("nenhum modelo carregado")?;
    std::fs::write(out, html).map_err(|e| e.to_string())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.open(p, true);
        }
        let reload = ctx.input(|i| i.key_pressed(egui::Key::F5));
        self.poll_screenshot(ctx);
        self.poll_loading();
        self.poll_audit();
        self.poll_diff();
        self.poll_file();
        if self.auto_reload && self.path.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Abrir…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_title("Abrir input MCNP ou PHITS").pick_file() {
                        self.open(p, true);
                    }
                }
                let mut again: Option<PathBuf> = None;
                egui::ComboBox::from_id_salt("recentes").selected_text("Recentes").width(90.0).show_ui(ui, |ui| {
                    if self.recent.is_empty() {
                        ui.weak("nenhum arquivo aberto ainda");
                    }
                    for path in &self.recent {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if ui.selectable_label(false, name).on_hover_text(path.display().to_string()).clicked() {
                            again = Some(path.clone());
                        }
                    }
                });
                if let Some(path) = again {
                    self.open(path, true);
                }
                let can_reload = self.path.is_some();
                if (ui.add_enabled(can_reload, egui::Button::new("Recarregar (F5)")).clicked() || reload) && can_reload {
                    let p = self.path.clone().unwrap();
                    self.open(p, false);
                }
                if ui.button("Resultados…").on_hover_text("Abre resultados para pintar sobre o corte: mctal ou meshtal do MCNP, ou saídas de tally do PHITS (mesh = xyz). Dá para escolher vários arquivos de uma vez.").clicked() {
                    self.open_results();
                }
                let (errors, warns) = self.diag_counts();
                let mut label = egui::RichText::new(match (errors, warns) {
                    (0, 0) => "Diagnóstico".to_string(),
                    (0, w) => format!("Diagnóstico · {} avisos", w),
                    (e, w) => format!("Diagnóstico · {} erros, {} avisos", e, w),
                });
                if errors > 0 {
                    label = label.color(Color32::from_rgb(240, 80, 70)).strong();
                } else if warns > 0 {
                    label = label.color(Color32::from_rgb(235, 150, 40));
                }
                if ui.button(label).on_hover_text("O que conferir antes de rodar (input e geometria) e antes de confiar no resultado (saída)").clicked() {
                    self.diag_open = !self.diag_open;
                }
                let edit = ui
                    .add_enabled(self.model.is_some(), egui::Button::new("Editar geometria"))
                    .on_hover_text("Move superfícies ou transformações TR, confere sobreposições e salva como um input novo");
                if edit.clicked() {
                    self.edit_open = !self.edit_open;
                }
                let other = ui
                    .add_enabled(self.model.is_some(), egui::Button::new("Comparar com…"))
                    .on_hover_text("Escolha outro input (a versão anterior, ou o do PDD contra o do perfil) e veja o que mudou: células, superfícies, materiais e cartões de dados");
                if other.clicked() {
                    self.compare_with();
                }
                let report = ui
                    .add_enabled(self.model.is_some(), egui::Button::new("Relatório…"))
                    .on_hover_text("Grava um HTML com as imagens da geometria, a tabela de materiais (com volume e massa, se a geometria foi verificada), os tallies e o diagnóstico");
                if report.clicked() {
                    self.save_report();
                }
                let vtk = ui
                    .add_enabled(self.model.is_some(), egui::Button::new("Exportar VTK…"))
                    .on_hover_text("Grava célula, material e densidade numa grade regular, para abrir no ParaView ou no VisIt");
                if vtk.clicked() {
                    self.export_vtk();
                }
                ui.checkbox(&mut self.auto_reload, "Recarregar ao salvar")
                    .on_hover_text("Relê o arquivo sozinho quando ele é salvo no editor, mantendo a vista");
                if let Some((p, _, _, _)) = &self.loading {
                    ui.spinner();
                    ui.label(format!("Lendo {}…", p.display()));
                } else if let Some(p) = &self.path {
                    ui.label(p.display().to_string());
                }
                if !self.edits.is_empty() {
                    ui.colored_label(Color32::from_rgb(235, 150, 40), format!("({} edições não salvas)", self.edits.len()));
                }
            });
            if let Some(e) = &self.error {
                ui.colored_label(Color32::from_rgb(240, 90, 90), e);
            }
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(egui::RichText::new(if self.hover.is_empty() { " " } else { &self.hover }).monospace());
        });
        egui::SidePanel::left("side").resizable(true).default_width(260.0).show(ctx, |ui| self.side_panel(ui));
        self.profile_window(ctx);
        self.diag_window(ctx);
        self.convergence_window(ctx);
        self.materials_window(ctx);
        self.tally_window(ctx);
        self.edit_window(ctx);
        self.diff_window(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                self.pane_2d(&mut cols[0]);
                self.pane_3d(&mut cols[1]);
            });
        });
    }
}
