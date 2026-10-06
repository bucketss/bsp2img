use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use tiny_skia::Pixmap;

use super::cs2::Cs2;
use super::draw2d::{self, Frame, GREEN, Layer, RED, YELLOW};
use super::shots::{self, Ctx, Kind, LETHAL_HEAD, Shot, ShotOpts, Spot};
use super::texture::Materials;
use super::{DrawBase, EntOpts, Gold, NAV_CELL, NAV_STEP, WallbangOpts, World, cache};
use crate::bsp::{Bsp, Engine};
use crate::nav::{Nav, Stance};
use crate::paths::{Partial, free_name};
use crate::pixelwalk::present::{Pixelwalk, PixelwalkOpts};
use crate::radar::Section;
use crate::render::{Cuts, Renderer};
use crate::scene::{Cancelled, Report};

const SIM_FROM: f32 = 0.1;
const SIM_TO: f32 = 0.7;
const POLL: std::time::Duration = std::time::Duration::from_millis(100);

fn stances(o: &ShotOpts) -> &'static str {
    match (o.stand, o.duck) {
        (true, true) => "standing + crouched",
        (true, false) => "standing",
        _ => "crouched",
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_bases(
    r: &mut Renderer,
    bsp: &Bsp,
    dirs: &[PathBuf],
    cuts: &Cuts,
    size: u32,
    base: crate::grid::Backdrop,
    secs: &[Section],
    rep: &mut Report,
) -> Result<Vec<DrawBase>> {
    if base == crate::grid::Backdrop::Radar {
        let found = if bsp.extra.is_some() { super::radar_bases(bsp, size, secs, rep) } else { super::overview_bases(bsp, dirs, size, secs, rep) };
        match found {
            Ok(v) if !v.is_empty() => return Ok(v),
            Ok(_) => rep.log("  no overview for this map; using the rendered backdrop".into()),
            Err(e) => rep.log(format!("  overview: {e:#}; using the rendered backdrop")),
        }
    }
    let mut out = Vec::new();
    for s in secs {
        let (g, px) = crate::grid::dark_base(r, &s.cuts(cuts), size)?;
        let default = secs.len() == 1 || s.name.eq_ignore_ascii_case(crate::radar::DEFAULT);
        out.push(DrawBase {
            tag: if default { String::new() } else { format!("_{}", s.name) },
            title: if default { String::new() } else { format!(" {} ({})", s.name, s.range()) },
            frame: Frame::from_grid(&g),
            px,
            section: s.clone(),
        });
    }
    Ok(out)
}

fn csv_header() -> String {
    let mut h = String::from("shooter,stance,pitch,yaw,eye_x,eye_y,eye_z,made,walls");
    for k in 1..=2 {
        for f in
            ["model", "face", "entry_x", "entry_y", "entry_z", "exit_x", "exit_y", "exit_z", "made", "death_x", "death_y", "death_z", "damage", "material", "thickness", "path"]
        {
            write!(h, ",w{k}_{f}").unwrap();
        }
    }
    h
}

fn csv_row(w: &mut impl std::io::Write, s: &Shot, spot: &Spot, eye: f64, pitch: f64, yaw: f64) -> std::io::Result<()> {
    let e = spot.feet + glam::DVec3::Z * eye;
    write!(w, "{},{},{},{},{:.2},{:.2},{:.2},{},{}", s.shooter, s.stance.label(), pitch, yaw, e.x, e.y, e.z, u8::from(s.made()), s.crossings().len())?;
    for k in 0..2 {
        match s.crossings().get(k) {
            Some(c) => {
                let (n, x) = (c.entry(), s.exit(c));
                write!(w, ",{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}", c.model, c.face().map_or(String::new(), |f| f.to_string()), n.x, n.y, n.z, x.x, x.y, x.z, u8::from(c.made))?;
                match s.death(c) {
                    Some(d) => write!(w, ",{:.2},{:.2},{:.2}", d.x, d.y, d.z)?,
                    None => write!(w, ",,,")?,
                }
                let m = if c.material == 0 { String::new() } else { (c.material as char).to_string() };
                write!(w, ",{},{m},{:.1},{:.1}", c.damage, c.thickness, c.path)?;
            }
            None => write!(w, ",,,,,,,,,,,,,,,,")?,
        }
    }
    writeln!(w)
}

#[allow(dead_code)]
fn strongest(shots: &[Shot], spots: &[Spot], pitches: &[f64], yaws: &[f64]) -> Result<String> {
    let mut txt = String::new();
    writeln!(txt)?;
    writeln!(txt, "strongest bangs, one per spot (unarmoured chest damage just past the last wall; x4 for a headshot):")?;
    let mut best: BTreeMap<u32, &Shot> = BTreeMap::new();
    for s in shots.iter().filter(|s| s.made()) {
        let e = best.entry(s.shooter).or_insert(s);
        if (s.damage(), s.path()) > (e.damage(), e.path()) {
            *e = s;
        }
    }
    let mut top: Vec<&Shot> = best.into_values().collect();
    top.sort_by(|a, b| b.damage().cmp(&a.damage()).then(b.path().total_cmp(&a.path())).then(a.shooter.cmp(&b.shooter)));
    for s in top.iter().take(25) {
        let sp = &spots[s.shooter as usize];
        let org = sp.origin();
        let th: Vec<String> = s
            .crossings()
            .iter()
            .map(|c| format!("{:.0}u {}", c.path, if c.material == 0 { '-' } else { c.material as char }))
            .collect();
        writeln!(
            txt,
            "{:>4} dmg  {:<5}  walls {}  setpos {:.1} {:.1} {:.1}; setang {} {} 0",
            s.damage(),
            sp.stance.label(),
            th.join(" + "),
            org.x,
            org.y,
            org.z,
            pitches[s.pitch_idx as usize],
            yaws[s.yaw_idx as usize]
        )?;
    }
    Ok(txt)
}

pub struct Base {
    pub world: World,
    pub nav: Arc<Nav>,
    pub reach: Vec<bool>,
    pub reachable: usize,
    pub mat_label: String,
    pub mat_hash: u64,
    pub data_hash: u64,
}

pub struct Sim {
    pub base: Arc<Base>,
    pub spots: Vec<Spot>,
    pub shots: Vec<Shot>,
    pub counts: shots::Counts,
}

impl Sim {
    pub fn ctx<'a>(&'a self, opts: &'a ShotOpts) -> Ctx<'a> {
        Ctx::new(&self.base.world, &self.base.nav, &self.base.reach, opts, None)
    }
}

fn stopped(stop: Option<&AtomicBool>) -> Result<()> {
    if stop.is_some_and(|s| s.load(Ordering::Relaxed)) { Err(Cancelled.into()) } else { Ok(()) }
}

pub fn base(bsp: &Bsp, dirs: &[PathBuf], ents: EntOpts, materials: Option<&Path>, stop: Option<&AtomicBool>, rep: &mut Report) -> Result<Base> {
    let (world, mat_label, mat_hash, data_hash) = match bsp.engine() {
        Engine::GoldSrc => gold_world(bsp, dirs, ents, materials, rep)?,
        Engine::Cs2 => {
            if ents.skip_doors || ents.skip_translucent.is_some() || materials.is_some() {
                rep.log("  --skip-doors, --skip-translucent and --materials are GoldSrc only; ignored".into());
            }
            let w = Cs2::load(bsp)?;
            let st = &w.bullet.stats;
            rep.log(format!(
                "  bullet geometry: {} triangles, {} hulls, {} entities, {} surfaces ({:.1}s)",
                st.tris,
                st.hulls,
                st.ents,
                w.bullet.surfs.len(),
                st.secs
            ));
            let label = format!("surface properties and weapons.vdata from {}", w.root);
            rep.log(format!("  materials: {label}"));
            let (sh, dh) = (w.surf_hash, w.data_hash);
            (World::Cs2(w), label, sh, dh)
        }
        _ => bail!("wallbang supports GoldSrc (CS 1.6 / CZ) and CS2 maps; Source uses a different penetration model"),
    };
    stopped(stop)?;
    let nav = Nav::build(bsp, NAV_CELL, NAV_STEP)?;
    stopped(stop)?;
    let reach = shots::reachable(bsp, &nav);
    let reachable = reach.iter().filter(|&&b| b).count();
    rep.log(format!("  nav: {} floor cells, {reachable} reachable", nav.len()));
    Ok(Base { world, nav, reach, reachable, mat_label, mat_hash, data_hash })
}

fn gold_world(bsp: &Bsp, dirs: &[PathBuf], ents: EntOpts, materials: Option<&Path>, rep: &mut Report) -> Result<(World, String, u64, u64)> {
    let mat_path = materials.map(Path::to_path_buf).or_else(|| Materials::locate(&bsp.path, dirs));
    let mats = match &mat_path {
        Some(p) => Materials::load(p).with_context(|| format!("reading {}", p.display()))?,
        None => Materials::empty(),
    };
    let mat_label = mat_path.as_ref().map_or("none found (every texture counts as concrete)".to_string(), |p| p.display().to_string());
    rep.log(format!("  materials: {mat_label}"));
    let mat_hash = mat_path.as_deref().and_then(cache::file_hash).unwrap_or(0);
    let world = Gold::new(bsp, &mats, Some(ents));
    rep.log(format!("  brush entities traced: {}", world.ents.len()));
    if !world.rotated.is_empty() {
        let list: Vec<String> = world.rotated.iter().map(|(c, n)| format!("{n} {c}")).collect();
        rep.log(format!("  warning: skipped rotated brush entities: {}", list.join(", ")));
    }
    Ok((World::Gold(world), mat_label, mat_hash, 0))
}

pub fn simulate(bsp: &Bsp, dirs: &[PathBuf], o: &WallbangOpts, base: Option<Arc<Base>>, stop: Option<&AtomicBool>, rep: &mut Report) -> Result<Sim> {
    o.shots.check()?;
    let base = match base {
        Some(b) => b,
        None => Arc::new(self::base(bsp, dirs, o.ents, o.materials.as_deref(), stop, rep)?),
    };
    if let World::Cs2(w) = &base.world {
        let name = o.shots.weapon.name;
        let wp = w.weapon(name).with_context(|| format!("{name} is not in weapons.vdata"))?;
        rep.log(format!(
            "  weapon: {} from weapons.vdata (damage {}, penetration {}, range modifier {}, range {})",
            wp.name, wp.damage, wp.pen, wp.range_mod, wp.range
        ));
    }
    let key = o.cache.as_ref().and_then(|d| cache::key(bsp, &base, o.ents, &o.shots).map(|k| (cache::path(d, &k), k)));
    if let Some((p, k)) = &key
        && p.is_file()
    {
        let ts = std::time::Instant::now();
        match cache::load(p, k) {
            Ok((spots, shots, counts)) => {
                rep.log(format!("  {} shooter spots", spots.len()));
                rep.log(format!(
                    "  {} shots fired, {} hit a wall within {} units: {} made it, {} died (cached, {:.1}s: {})",
                    counts.fired,
                    counts.hits - counts.far,
                    o.shots.max_hit,
                    counts.made,
                    counts.died,
                    ts.elapsed().as_secs_f64(),
                    p.display()
                ));
                return Ok(Sim { base, spots, shots, counts });
            }
            Err(e) => rep.log(format!("  sim cache: {e:#} ({}); simulating", p.display())),
        }
    }
    let spots = shots::spots(&base.nav, &base.reach, &o.shots);
    rep.log(format!(
        "  {} shooter spots ({} standing, {} crouched)",
        spots.len(),
        spots.iter().filter(|s| s.stance == Stance::Stand).count(),
        spots.iter().filter(|s| s.stance == Stance::Duck).count()
    ));
    if spots.is_empty() {
        bail!("no reachable shooter positions (no spawns on walkable ground?)");
    }
    rep.step(SIM_FROM)?;
    let ts = std::time::Instant::now();
    let own = AtomicBool::new(false);
    let flag = stop.unwrap_or(&own);
    let done = AtomicU32::new(0);
    let (shots, counts) = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            let ctx = Ctx::new(&base.world, &base.nav, &base.reach, &o.shots, Some(flag));
            shots::simulate(&ctx, &spots, &|f| done.store(f.to_bits(), Ordering::Relaxed))
        });
        while !h.is_finished() {
            std::thread::sleep(POLL);
            if rep.step(SIM_FROM + (SIM_TO - SIM_FROM) * f32::from_bits(done.load(Ordering::Relaxed))).is_err() {
                flag.store(true, Ordering::Relaxed);
            }
        }
        h.join().expect("wallbang simulation panicked")
    });
    stopped(Some(flag))?;
    rep.log(format!(
        "  {} shots fired, {} hit a wall within {} units: {} made it, {} died ({:.1}s)",
        counts.fired,
        counts.hits - counts.far,
        o.shots.max_hit,
        counts.made,
        counts.died,
        ts.elapsed().as_secs_f64()
    ));
    if let Some((p, k)) = &key {
        match cache::save(p, k, &spots, &shots, &counts) {
            Ok(()) => rep.log(format!("  sim cache: {}", p.display())),
            Err(e) => rep.log(format!("  sim cache: could not write {}: {e:#}", p.display())),
        }
    }
    Ok(Sim { base, spots, shots, counts })
}

#[allow(clippy::too_many_arguments)]
pub fn export_wallbang(
    r: &mut Renderer,
    bsp: &Bsp,
    dirs: &[PathBuf],
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &WallbangOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<(Vec<PathBuf>, Sim, Vec<Pixelwalk>)> {
    let t0 = std::time::Instant::now();
    rep.step(0.0)?;
    let sim = simulate(bsp, dirs, o, None, None, rep)?;
    rep.step(SIM_TO)?;
    let (shots, spots, c) = (&sim.shots, &sim.spots, &sim.counts);

    let (secs, why) = o.split.sections(bsp.extra.as_ref().and_then(|x| x.radar.as_ref()), bsp, cuts);
    if let Some(w) = why {
        rep.log(format!("  sections: {w}"));
    }
    let bases = render_bases(r, bsp, dirs, cuts, o.size, o.base, &secs, rep)?;
    let w = o.shots.weapon;
    let (pa, pb, ps) = o.shots.pitch;
    let settings = format!(
        "{}, pitch {pa}..{pb} step {ps}, yaw step {}{}",
        stances(&o.shots),
        o.shots.yaw_step,
        if o.damage { ", damage colours" } else { "" }
    );
    let pws = if o.pixelwalks && bsp.engine() != Engine::GoldSrc {
        rep.log("  pixelwalk layer: GoldSrc only, skipped".into());
        Vec::new()
    } else if o.pixelwalks {
        let po = PixelwalkOpts { size: o.size, ..Default::default() };
        let v = crate::pixelwalk::present::find_pixelwalks(bsp, &po)?;
        rep.log(format!("  pixelwalk layer: {} pixelwalks", v.len()));
        v
    } else {
        Vec::new()
    };
    let zr = crate::pixelwalk::present::world_z(bsp);
    let mut files = Partial::default();
    for (bi, b) in bases.iter().enumerate() {
        rep.step(SIM_TO + 0.25 * bi as f32 / bases.len() as f32)?;
        let mut layer = Layer { main: Vec::new(), faint: Vec::new(), damage: o.damage };
        for s in shots {
            if b.section.contains(spots[s.shooter as usize].feet.z) {
                layer.main.push(s);
            } else if o.others_faint {
                layer.faint.push(s);
            }
        }
        let count = |k: Kind| layer.main.iter().filter(|s| s.kind(o.damage) == k).count();
        let legend = if o.damage {
            vec![
                (GREEN, format!("made it, a headshot kills ({})", count(Kind::Made))),
                (YELLOW, format!("made it, under {LETHAL_HEAD} damage ({})", count(Kind::Weak))),
                (RED, format!("died in the wall ({})", count(Kind::Died))),
            ]
        } else {
            vec![(GREEN, format!("made it through ({})", count(Kind::Made))), (RED, format!("died in the wall ({})", count(Kind::Died)))]
        };
        let mut styles = Vec::new();
        if o.areas {
            styles.push(("", "wallbang areas"));
        }
        if o.lines {
            styles.push(("_lines", "wallbang shots"));
        }
        for (suffix, what) in styles {
            let size = tiny_skia::IntSize::from_wh(b.frame.w, b.frame.h).context("bad size")?;
            let px = if suffix.is_empty() {
                draw2d::areas(&b.px, &b.frame, &layer, o.sigma)
            } else {
                let mut px = b.px.clone();
                draw2d::lines(&mut px, &b.frame, &layer);
                px
            };
            let mut pm = Pixmap::from_vec(px, size).context("pixmap")?;
            draw2d::markers(&mut pm, bsp, &b.frame);
            if !pws.is_empty() {
                let sel: Vec<(usize, &Pixelwalk)> =
                    pws.iter().enumerate().filter(|(_, p)| b.section.contains(p.from.z.min(p.to.z) - p.stance.feet())).collect();
                crate::pixelwalk::present::draw(&mut pm, &b.frame, &sel, zr, false);
            }
            draw2d::title(&mut pm, &format!("{name}{}  {what}  {}  ({settings})", b.title, w.name));
            draw2d::legend(&mut pm, &legend);
            draw2d::scale_bar(&mut pm, &b.frame);
            let f = free_name(out, &format!("{name}{cut_tag}_wallbang_{}{suffix}{}", w.name, b.tag), ".png");
            files.add(f.clone());
            draw2d::save_png(&pm, &f)?;
            rep.log(format!("  {}", f.display()));
        }
    }

    let world = &sim.base.world;
    let mut txt = String::new();
    writeln!(txt, "{name} wallbang, {}", w.name)?;
    writeln!(txt, "materials: {}", sim.base.mat_label)?;
    writeln!(
        txt,
        "settings: {settings}, spot cell {}, max first hit {}, slab {}, min thickness {}, nav cell {NAV_CELL}",
        o.shots.spot_cell, o.shots.max_hit, o.shots.slab, o.shots.min_thickness
    )?;
    match world {
        World::Gold(world) => writeln!(
            txt,
            "brush entities: {} traced{}{}{}",
            world.ents.len(),
            if o.ents.skip_doors { ", doors skipped" } else { "" },
            o.ents.skip_translucent.map_or(String::new(), |a| format!(", translucent below renderamt {a} skipped")),
            if world.rotated.is_empty() {
                String::new()
            } else {
                format!(", rotated skipped: {}", world.rotated.iter().map(|(c, n)| format!("{n} {c}")).collect::<Vec<_>>().join(", "))
            }
        )?,
        World::Cs2(w) => {
            let st = &w.bullet.stats;
            writeln!(txt, "bullet geometry: {} triangles, {} hulls, {} entities, {} surfaces", st.tris, st.hulls, st.ents, w.bullet.surfs.len())?;
            if let Some(wp) = w.weapon(o.shots.weapon.name) {
                writeln!(txt, "weapon: {} (damage {}, penetration {}, range modifier {}, range {})", wp.name, wp.damage, wp.pen, wp.range_mod, wp.range)?;
            }
        }
    }
    writeln!(txt, "nav: {} floor cells, {} reachable", sim.base.nav.len(), sim.base.reachable)?;
    writeln!(txt)?;
    writeln!(txt, "shooter spots            {:>10}", spots.len())?;
    writeln!(txt, "shots fired              {:>10}", c.fired)?;
    writeln!(txt, "hit a wall               {:>10}", c.hits)?;
    writeln!(txt, "dropped:")?;
    for (k, v) in [
        ("first hit too far", c.far),
        ("no exit found", c.no_exit),
        ("nothing playable behind", c.behind),
        ("floor or ceiling slab", c.slab),
        ("path under floor/ceiling", c.column),
        ("died outside play", c.void),
        ("exact graze", c.graze),
        ("corner clip", c.corner),
        ("thinner than minimum", c.thin),
    ] {
        writeln!(txt, "  {k:<23}{v:>10}")?;
    }
    writeln!(txt, "second wall cut by filter{:>10}", c.cut)?;
    writeln!(txt, "made it through          {:>10}", c.made)?;
    writeln!(txt, "died in the wall         {:>10}", c.died)?;
    let f = free_name(out, &format!("{name}{cut_tag}_wallbang_{}", w.name), ".txt");
    files.add(f.clone());
    std::fs::write(&f, txt)?;
    rep.log(format!("  {}", f.display()));
    if o.csv {
        let pitches = o.shots.pitches();
        let yaws = o.shots.yaws();
        let f = free_name(out, &format!("{name}{cut_tag}_wallbang_{}", w.name), ".csv");
        files.add(f.clone());
        let mut wr = std::io::BufWriter::new(std::fs::File::create(&f).with_context(|| format!("creating {}", f.display()))?);
        writeln!(wr, "{}", csv_header())?;
        for s in shots {
            let sp = &spots[s.shooter as usize];
            csv_row(&mut wr, s, sp, world.eye(sp.stance), pitches[s.pitch_idx as usize], yaws[s.yaw_idx as usize])?;
        }
        wr.flush()?;
        rep.log(format!("  {}", f.display()));
    }
    rep.step(1.0)?;
    rep.log(format!("  total {:.1}s", t0.elapsed().as_secs_f64()));
    Ok((files.keep(), sim, pws))
}
