use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use anyhow::Result;
use eframe::egui;

use super::{App, Msg};
use crate::explode::ExplodeOpts;
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview};
use crate::gltf::{GltfOpts, export_gltf};
use crate::health::{HealthOpts, export_health};
use crate::kills::{KillOpts, export_kills, load_demos};
use crate::paths::run_dir;
use crate::poster::{PosterOpts, export_poster};
use crate::render::{Cuts, Gpu};
use crate::scene::{Report, Scene};
use crate::spin::{AnimOpts, export_anim};
use crate::svg::{SvgOpts, export_svg};
use crate::stl::{StlOpts, export_stl};
use crate::timing::{TimingOpts, export_timing};

pub enum Job {
    Iso(IsoOpts),
    Anim(AnimOpts),
    Overview(OverviewOpts),
    Timing(TimingOpts),
    Kills(KillOpts, Vec<PathBuf>),
    Health(HealthOpts),
    Svg(SvgOpts),
    Stl(StlOpts),
    Gltf(GltfOpts),
    Poster(PosterOpts),
}

impl Job {
    pub fn look(&self) -> Option<&crate::look::Look> {
        match self {
            Job::Iso(o) => Some(&o.look),
            Job::Anim(o) if !matches!(o.kind, crate::spin::Anim::Day(_)) => Some(&o.look),
            Job::Poster(o) => Some(&o.look),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Job::Iso(_) => "isometric",
            Job::Anim(_) => "animation",
            Job::Overview(_) => "overview",
            Job::Timing(_) => "rush timings",
            Job::Kills(..) => "kill heatmap",
            Job::Health(_) => "health report",
            Job::Svg(_) => "SVG callouts",
            Job::Stl(_) => "STL diorama",
            Job::Gltf(o) => if o.obj { "OBJ" } else { "glTF" },
            Job::Poster(_) => "poster",
        }
    }
}

pub struct JobSpec {
    pub job: Job,
    pub scene: Arc<Scene>,
    pub gpu: Gpu,
    pub cuts: Cuts,
    pub cut_tag: String,
    pub nearest: bool,
    pub sky: Option<(String, f64, f64)>,
    pub out: PathBuf,
    pub explode: ExplodeOpts,
}

pub struct Running {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    pub progress: f32,
    pub label: String,
}

impl Running {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn work(spec: &JobSpec, rep: &mut Report, out: &Path, sky_tag: &str, r: &mut crate::render::Renderer) -> Result<()> {
    let s = &spec.scene;
    let name = s.bsp.name();
    match &spec.job {
        Job::Iso(o) => export_iso(r, &s.bsp, &name, sky_tag, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Anim(o) => export_anim(r, &s.levels, &name, sky_tag, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Overview(o) => export_overview(r, &s.bsp, &name, &spec.cuts, o, out, rep).map(drop),
        Job::Timing(o) => export_timing(r, &s.bsp, &name, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Kills(o, demos) => {
            let data = load_demos(demos, o, rep, (0.0, 0.5))?;
            if data.is_empty() {
                anyhow::bail!("none of the demos could be read");
            }
            rep.log(format!("  {} demos parsed", data.len()));
            export_kills(r, &s.bsp, &name, &spec.cut_tag, &spec.cuts, o, &data, out, rep, 0.5).map(drop)
        }
        Job::Health(o) => export_health(r, s, &name, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Svg(o) => export_svg(r, s, &name, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Stl(o) => export_stl(&s.bsp, &name, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Gltf(o) => export_gltf(s, &name, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
        Job::Poster(o) => export_poster(r, &s.bsp, &name, sky_tag, &spec.cut_tag, &spec.cuts, o, out, rep).map(drop),
    }
}

pub fn start(spec: JobSpec, ctx: &egui::Context) -> Running {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let ctx = ctx.clone();
    let label = format!("Exporting {}", spec.job.label());
    let tag = label.clone();
    std::thread::spawn(move || {
        let mut spec = spec;
        let send = |m: Msg| {
            let _ = tx.send(m);
            ctx.request_repaint();
        };
        let name = spec.scene.bsp.name();
        send(Msg::Log(format!("== export {name}")));
        let res = run_dir(&spec.out, &name).and_then(|out| {
            let mut log = |s: String| send(Msg::Log(s));
            let (mut r, sky_tag) = spec.scene.job_renderer(&spec.gpu, spec.nearest, spec.sky.clone(), &mut log);
            if matches!(spec.job, Job::Iso(_) | Job::Anim(_) | Job::Poster(_)) {
                spec.cut_tag += &spec.scene.apply_explode(&mut r, &spec.explode, &spec.cuts, &mut log);
            }
            if let Some(l) = spec.job.look() {
                spec.scene.log_light(l, &mut log);
                spec.cut_tag += &l.light_tag();
            }
            let mut progress = |f: f32| {
                send(Msg::Progress(f, tag.clone()));
                !flag.load(Ordering::Relaxed)
            };
            let mut rep = Report { log: &mut log, progress: &mut progress };
            let res = work(&spec, &mut rep, &out, &sky_tag, &mut r);
            if res.is_err() {
                let _ = std::fs::remove_dir(&out);
            }
            res.map(|_| out)
        });
        send(Msg::JobDone(res.map_err(|e| format!("{e:#}"))));
    });
    Running { rx, cancel, progress: 0.0, label }
}

impl App {
    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn start_job(&mut self, job: Job, ctx: &egui::Context) {
        let Some(scene) = self.scene.clone() else { return };
        if self.busy() {
            return;
        }
        let sky = matches!(job, Job::Iso(_) | Job::Anim(_) | Job::Poster(_)).then(|| self.look.sky_spec()).flatten();
        let spec = JobSpec {
            job,
            cut_tag: self.cut_opts().tag(scene.opts.hull),
            scene,
            gpu: self.gpu.clone(),
            cuts: self.cuts(),
            nearest: self.look.nearest,
            sky,
            out: PathBuf::from(&self.out),
            explode: self.explode.clone(),
        };
        self.status.clear();
        self.job = Some(start(spec, ctx));
        self.save_cfg();
    }

    pub(super) fn poll_job(&mut self) {
        let Some(j) = &mut self.job else { return };
        let mut done = None;
        while let Ok(m) = j.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Progress(f, l) => {
                    j.progress = f;
                    j.label = l;
                }
                Msg::JobDone(r) => done = Some(r),
                Msg::Loaded(_) => {}
            }
        }
        let Some(r) = done else { return };
        let cancelled = j.cancelling();
        self.job = None;
        match r {
            Ok(out) => {
                self.status = format!("wrote {}", out.display());
                self.show_health(&out);
                self.last_out = Some(out);
            }
            Err(_) if cancelled => {
                self.status = "export cancelled".into();
                self.log.push(self.status.clone());
            }
            Err(e) => {
                self.status = format!("export failed: {e}");
                self.log.push(self.status.clone());
            }
        }
    }
}
