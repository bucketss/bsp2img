use super::*;

pub(super) fn stuck_spawns(bsp: &Bsp, st: &LoadStats, phys: &Phys) -> Vec<(String, DVec3)> {
    let mut out = Vec::new();
    let mut iv = Vec::new();
    let [tn, ctn] = teams(bsp.is_tf2());
    for (i, e) in bsp.entities.iter().enumerate() {
        let team = match e.class() {
            "info_player_deathmatch" => tn,
            "info_player_start" => ctn,
            _ => continue,
        };
        let Some(p) = e.origin() else { continue };
        let feet = if st.raised.contains(&i) { p.z - STAND_OFS } else { p.z };
        let mut floor = f64::NAN;
        for (dx, dy) in [(0.0, 0.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            phys.tops(p.x + dx * PLAYER_HALF, p.y + dy * PLAYER_HALF, Mask::PLAYER, &mut iv);
            let top = iv.iter().map(|&t| t as f64).filter(|&t| t >= feet - HOLE_BELOW as f64 && t <= feet + STEP as f64);
            floor = top.fold(floor, f64::max);
        }
        let base = if floor.is_nan() { feet } else { floor } + BOX_LIFT;
        let lo = DVec3::new(p.x - PLAYER_HALF, p.y - PLAYER_HALF, base);
        let hi = DVec3::new(p.x + PLAYER_HALF, p.y + PLAYER_HALF, base + PLAYER_HEIGHT);
        let up = DVec3::Z * STEP as f64;
        if phys.box_solid(lo, hi, Mask::PLAYER) && phys.box_solid(lo + up, hi + up, Mask::PLAYER) {
            out.push((team.to_string(), p));
        }
    }
    out
}

pub(super) fn graph(nav: &Nav, reach: usize) -> Graph {
    let comp = nav.components();
    let mut size = vec![0usize; comp.len()];
    for &c in &comp {
        size[c as usize] += 1;
    }
    Graph {
        cells: nav.len(),
        reach,
        components: size.iter().filter(|&&n| n > 0).count(),
        largest: size.iter().copied().max().unwrap_or(0),
        ladders: nav.ladders,
    }
}

pub(super) fn collision(phys: &Phys, w: Option<&Walk>) -> Collision {
    let stats = phys.stats().clone();
    let secs = stats.secs;
    let Some(w) = w else { return Collision { stats, checked: 0, holes: 0, clusters: Vec::new(), nclusters: 0, secs } };
    let nav = &w.nav;
    let holes: Vec<usize> = w
        .reach
        .par_iter()
        .map_init(Vec::new, |iv, &n| {
            let (x, y) = nav.cell_xy(nav.col[n] as usize);
            let z = nav.nodes[n].z;
            phys.tops(x, y, Mask::PLAYER, iv);
            (!iv.iter().any(|&t| t >= z - HOLE_BELOW && t <= z + HOLE_ABOVE)).then_some(n)
        })
        .flatten()
        .collect();
    let mut id = vec![u32::MAX; nav.len()];
    for (k, &n) in holes.iter().enumerate() {
        id[n] = k as u32;
    }
    let mut up: Vec<u32> = (0..holes.len() as u32).collect();
    for (k, &n) in holes.iter().enumerate() {
        for v in nav.links(n) {
            if v != NONE && id[v as usize] != u32::MAX {
                unite(&mut up, k as u32, id[v as usize]);
            }
        }
    }
    let mut acc: Vec<(DVec3, usize)> = vec![(DVec3::ZERO, 0); holes.len()];
    for (k, &n) in holes.iter().enumerate() {
        let r = root(&mut up, k as u32) as usize;
        let (x, y) = nav.cell_xy(nav.col[n] as usize);
        acc[r].0 += DVec3::new(x, y, nav.nodes[n].z as f64);
        acc[r].1 += 1;
    }
    let mut clusters: Vec<(DVec3, usize)> = acc.into_iter().filter(|c| c.1 > 0).map(|(p, n)| (p / n as f64, n)).collect();
    clusters.sort_by_key(|c| std::cmp::Reverse(c.1));
    let nclusters = clusters.len();
    clusters.truncate(HOLE_CLUSTERS);
    Collision { stats, checked: w.reach.len(), holes: holes.len(), clusters, nclusters, secs }
}

pub(super) fn inside(f: &Radar, p: DVec3) -> bool {
    let s = f.size();
    p.x >= f.pos_x && p.x <= f.pos_x + s && p.y <= f.pos_y && p.y >= f.pos_y - s
}

pub(super) fn image_px(fs: &crate::s2::vpk::Fs, name: &str) -> Option<(usize, usize)> {
    let h = fs.read_limit(name, 1 << 16).ok()?;
    match crate::s2::texture::info(&h) {
        Ok(inf) => Some((inf.w, inf.h)),
        Err(_) => crate::s1::radar::dims(name, &h),
    }
}

pub(super) fn icon_gap(bsp: &Bsp, key: &str, p: (f64, f64)) -> Option<f64> {
    let d = |q: DVec3| (q.x - p.0).hypot(q.y - p.1);
    let marks = crate::grid::entity_marks(bsp);
    let near = |it: &mut dyn Iterator<Item = f64>| it.reduce(f64::min);
    match key.to_lowercase().as_str() {
        "ctspawn" => near(&mut marks.iter().filter(|m| m.spawn && m.label == "CT").map(|m| d(m.pos))),
        "tspawn" => near(&mut marks.iter().filter(|m| m.spawn && m.label == "T").map(|m| d(m.pos))),
        "bomba" | "bombb" => near(&mut marks.iter().filter(|m| m.label == "BOMB").map(|m| match m.bbox {
            Some([x0, y0, x1, y1]) => (p.0.clamp(x0, x1) - p.0).hypot(p.1.clamp(y0, y1) - p.1),
            None => d(m.pos),
        })),
        _ => None,
    }
}

pub(super) fn radar_check(bsp: &Bsp, x: &Extra, map: &str, reach: &[DVec3]) -> RadarCheck {
    let mut rc = RadarCheck { txt: x.radar.is_some() || x.radar_err.is_some(), err: x.radar_err.clone(), ..Default::default() };
    let names: Vec<String> = match &x.radar {
        Some(o) => o.levels().chain(o.volumes()).map(|s| s.name.clone()).collect(),
        None => vec![DEFAULT.to_string()],
    };
    for n in names {
        let img = x.radar_image_name(map, &n);
        let px = img.as_deref().and_then(|i| image_px(&x.fs, i));
        rc.images.push((n, img, px));
    }
    let Some(o) = &x.radar else { return rc };
    let px_of = |n: &str| rc.images.iter().find(|i| i.0.eq_ignore_ascii_case(n)).and_then(|i| i.2);
    let main_px = o.levels().find_map(|s| px_of(&s.name));
    let main = o.frame;
    rc.frame = Some((o.frame, main_px));
    let frames: Vec<Radar> = std::iter::once(main).chain(o.volumes().filter_map(|s| s.frame)).collect();
    let shrink = |f: &Radar| {
        let m = RADAR_MARGIN_PX * f.scale;
        Radar { pos_x: f.pos_x + m, pos_y: f.pos_y - m, scale: f.scale * (RADAR as f64 - 2.0 * RADAR_MARGIN_PX) / RADAR as f64 }
    };
    let inner: Vec<Radar> = frames.iter().map(shrink).collect();
    if !reach.is_empty() {
        let frac = |fs: &[Radar]| reach.iter().filter(|p| fs.iter().any(|f| inside(f, **p))).count() as f64 / reach.len() as f64;
        rc.cover = Some((frac(&frames), frac(&inner)));
    }
    for (k, [u, v]) in &o.icons {
        if ["ctspawn", "tspawn", "bomba", "bombb"].contains(&k.to_lowercase().as_str()) {
            rc.icons.push((k.clone(), icon_gap(bsp, k, main.world(*u, *v))));
        }
    }
    rc
}

pub(super) fn gather_cs2(scene: &Scene, x: &Extra, name: &str, o: &HealthOpts) -> Health {
    let bsp = &scene.bsp;
    let (t, ct, objectives, buyzones) = basics(bsp);
    let has_sky = bsp.entities.iter().any(|e| e.class() == "env_sky");
    let sky = x.sky.clone().unwrap_or_else(|| if has_sky { "env_sky".into() } else { "none".into() });
    let st = &x.stats;
    let (missing_textures, missing_materials): (Vec<String>, Vec<String>) = st.missing.iter().cloned().partition(|m| m.ends_with(".vtex") || m.ends_with(".vtf"));
    let marks = crate::grid::entity_marks(bsp);
    let comp = teams(bsp.is_tf2()).map(|l| marks.iter().filter(|m| m.spawn && m.label == l && m.comp != Some(false)).count());
    let disabled = ["info_player_terrorist", "info_player_counterterrorist"].map(|c| st.disabled.iter().filter(|d| d.0 == c).count());
    let walked = walk(bsp, o, &t, &ct);
    let phys = x.phys();
    let reach: Vec<DVec3> = walked
        .as_ref()
        .map(|w| {
            w.reach
                .iter()
                .map(|&n| {
                    let (px, py) = w.nav.cell_xy(w.nav.col[n] as usize);
                    DVec3::new(px, py, w.nav.nodes[n].z as f64)
                })
                .collect()
        })
        .unwrap_or_default();
    let cs2 = ModernHealth {
        paks: x.fs.srcs.iter().map(|p| p.path().to_path_buf()).collect(),
        legacy: st.legacy_ents,
        bad_models: st.bad_models.clone(),
        missing_materials,
        missing_textures,
        draws: st.draws,
        dropped: st.dropped,
        materials: st.materials,
        models: st.models,
        tris: st.tris,
        lightmap: st.lightmap,
        probes: st.probes,
        comp,
        disabled,
        gaps: [min_gap(&t), min_gap(&ct)],
        stuck: phys.as_ref().map(|p| stuck_spawns(bsp, st, p)).map_err(|e| format!("{e:#}")),
        areas: x.nav.as_ref().map_or(0, |m| m.areas.len()),
        mesh_ladders: x.nav.as_ref().map_or(0, |m| m.ladders.len()),
        graph: walked.as_ref().ok().map(|w| graph(&w.nav, w.reach.len())),
        collision: phys.as_ref().map(|p| collision(p, walked.as_ref().ok())).map_err(|e| format!("{e:#}")),
        radar: radar_check(bsp, x, &name.to_lowercase(), &reach),
    };
    Health {
        name: name.to_string(),
        t,
        ct,
        objectives,
        buyzones,
        sky,
        sky_found: x.sky.is_some() || !has_sky,
        nav: walked.map(|w| w.info).map_err(|e| format!("{e:#}")),
        tf2: bsp.is_tf2(),
        tf2_checks: tf2_checks(bsp),
        engine: Engine::Cs2(Box::new(cs2)),
    }
}

pub(super) fn radar_warnings(r: &RadarCheck, w: &mut Vec<String>) {
    match (&r.err, r.txt) {
        (Some(e), _) => w.push(format!("overview txt: {e}")),
        (None, false) => w.push("no overview txt".to_string()),
        _ => {}
    }
    let miss: Vec<&str> = r.images.iter().filter(|i| i.1.is_none()).map(|i| i.0.as_str()).collect();
    if !miss.is_empty() {
        w.push(format!("no radar image for {}", miss.join(", ")));
    }
    if let Some((_, m)) = r.cover.filter(|c| c.1 < COVER_MIN) {
        w.push(format!("radar frame holds {:.1}% of reachable nav inside a {RADAR_MARGIN_PX:.0} px margin", m * 100.0));
    }
    for (k, d) in &r.icons {
        if let Some(d) = d.filter(|d| *d > ICON_FAR) {
            w.push(format!("radar icon {k} is {d:.0} units from the nearest match"));
        }
    }
}

pub(super) fn cs2_facts(h: &Health, c: &ModernHealth) -> Vec<(String, [u8; 3])> {
    let white = [220, 220, 220];
    let ok = |b: bool| if b { GREEN } else { RED };
    let mut v = Vec::new();
    let (nm, nt) = (c.missing_materials.len(), c.missing_textures.len());
    v.push((
        format!("load: {} failed models, {nm} missing materials, {nt} textures", c.bad_models.len()),
        ok(c.bad_models.is_empty() && nm == 0 && nt == 0),
    ));
    let lm = c.lightmap.map_or("none".to_string(), |(w, h)| format!("{w}x{h}"));
    v.push((format!("geometry: {:.1}M triangles, {} draws, lightmap {lm}", c.tris as f64 / 1e6, c.draws), white));
    let comp_ok = c.comp.iter().all(|&n| n >= COMP_MIN);
    v.push((
        format!("spawns: T {} (comp {}), CT {} (comp {})", h.t.len(), c.comp[0], h.ct.len(), c.comp[1]),
        if comp_ok { GREEN } else { AMBER },
    ));
    match &h.nav {
        Ok(n) => v.push((format!("spawns off nav: {}", n.off_ground.len()), ok(n.off_ground.is_empty()))),
        Err(_) => v.push(("spawns off nav: unknown".into(), AMBER)),
    }
    match &c.stuck {
        Ok(x) => v.push((format!("spawns in collision: {}", x.len()), ok(x.is_empty()))),
        Err(_) => v.push(("spawns in collision: unknown".into(), AMBER)),
    }
    if let Some(g) = c.gaps.iter().flatten().map(|g| g.0).reduce(f64::min) {
        v.push((format!("closest spawns: {g:.0} units"), white));
    }
    let obj: Vec<String> = h.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
    v.push((format!("objectives: {}", list(&obj)), if obj.is_empty() { AMBER } else { white }));
    v.push((format!("buy zones: {}", h.buyzones), white));
    match &c.collision {
        Ok(k) if k.checked > 0 => v.push((
            format!("collision holes: {} cells in {} clusters", k.holes, k.nclusters),
            white,
        )),
        Ok(_) => v.push(("collision holes: unknown".into(), white)),
        Err(_) => v.push(("collision: failed to load".into(), RED)),
    }
    let r = &c.radar;
    let have = r.images.iter().filter(|i| i.1.is_some()).count();
    let txt = match (&r.err, r.txt) {
        (Some(_), _) => "txt broken",
        (None, true) => "txt",
        (None, false) => "no txt",
    };
    v.push((
        format!("overview: {txt}, radar images {have}/{}", r.images.len()),
        if r.txt && r.err.is_none() && r.images_ok() { GREEN } else { AMBER },
    ));
    if let Some((_, m)) = r.cover {
        v.push((format!("radar frame holds {:.1}% of reachable nav", m * 100.0), if m >= COVER_MIN { GREEN } else { AMBER }));
    }
    let far = r.icons.iter().filter(|(_, d)| d.is_some_and(|d| d > ICON_FAR)).count();
    if !r.icons.is_empty() {
        v.push((format!("radar icons: {} of {} far from spawns or sites", far, r.icons.len()), if far == 0 { GREEN } else { AMBER }));
    }
    match &h.nav {
        Ok(n) => {
            v.push((format!("walkable area: {:.0} m2", n.walkable_m2), white));
            for (i, s) in n.spots.iter().enumerate() {
                v.push((format!("open area {}: {:.0} {:.0}, radius {:.0}", i + 1, s.pos.x, s.pos.y, s.radius), [255, 230, 0]));
            }
        }
        Err(e) => v.push((format!("nav: {e}"), AMBER)),
    }
    v
}

impl Health {
    pub(super) fn cs2_warnings(&self, c: &ModernHealth) -> Vec<String> {
        let mut w = Vec::new();
        if !c.bad_models.is_empty() {
            w.push(format!("{} models failed to load", c.bad_models.len()));
        }
        if !c.missing_materials.is_empty() {
            w.push(format!("{} materials missing", c.missing_materials.len()));
        }
        if !c.missing_textures.is_empty() {
            w.push(format!("{} textures missing", c.missing_textures.len()));
        }
        for (team, n) in [("T", c.comp[0]), ("CT", c.comp[1])] {
            if n < COMP_MIN {
                w.push(format!("{n} competitive {team} spawns (fewer than {COMP_MIN})"));
            }
        }
        match &self.nav {
            Ok(n) if !n.off_ground.is_empty() => w.push(format!("{} spawns off the nav mesh", n.off_ground.len())),
            Err(e) => w.push(format!("nav: {e}")),
            _ => {}
        }
        match &c.stuck {
            Ok(s) if !s.is_empty() => w.push(format!("{} spawns inside collision", s.len())),
            Err(e) => w.push(format!("collision: {e}")),
            _ => {}
        }
        if self.objectives.iter().all(|(_, n)| *n == 0) {
            w.push("no objectives".to_string());
        }
        if !self.sky_found {
            w.push(format!("sky {} missing", self.sky));
        }
        radar_warnings(&c.radar, &mut w);
        w
    }

    pub(super) fn write_open(&self, s: &mut String) -> std::fmt::Result {
        match &self.nav {
            Ok(n) => {
                writeln!(s, "Open areas (walkable {:.0} m2 in {} cells)", n.walkable_m2, n.cells)?;
                for (i, sp) in n.spots.iter().enumerate() {
                    writeln!(s, "  {}. x {:.0} y {:.0} z {:.0}, radius {:.0} units", i + 1, sp.pos.x, sp.pos.y, sp.pos.z, sp.radius)?;
                }
            }
            Err(e) => writeln!(s, "Open areas: unknown ({e})")?,
        }
        Ok(())
    }

    pub(super) fn write_cs2(&self, c: &ModernHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "{} health", self.name)?;
        writeln!(s)?;
        writeln!(s, "Load")?;
        for (i, p) in c.paks.iter().enumerate() {
            writeln!(s, "  {:<15}{}", if i == 0 { "sources" } else { "" }, p.display())?;
        }
        writeln!(s, "  legacy ents    {} skipped", c.legacy)?;
        writeln!(s, "  failed models  {}", short(&c.bad_models))?;
        writeln!(s, "  materials      {} missing: {}", c.missing_materials.len(), short(&c.missing_materials))?;
        writeln!(s, "  textures       {} missing: {}", c.missing_textures.len(), short(&c.missing_textures))?;
        writeln!(s)?;
        writeln!(s, "Geometry")?;
        writeln!(s, "  triangles      {}", c.tris)?;
        writeln!(s, "  draws          {} ({} cropped)", c.draws, c.dropped)?;
        writeln!(s, "  models         {}", c.models)?;
        writeln!(s, "  materials      {}", c.materials)?;
        match c.lightmap {
            Some((w, h)) => writeln!(s, "  lightmap       {w}x{h}")?,
            None => writeln!(s, "  lightmap       none")?,
        }
        writeln!(s, "  probe volumes  {}", c.probes)?;
        writeln!(s)?;
        self.write_spawns(c, s)?;
        writeln!(s, "Nav")?;
        writeln!(s, "  areas          {}, ladders {}", c.areas, c.mesh_ladders)?;
        self.write_graph(c, s)?;
        self.write_collision(c, s)?;
        self.write_overview(c, s)?;
        self.write_open(s)?;
        writeln!(s)?;
        let w = self.warnings();
        writeln!(s, "Warnings ({})", w.len())?;
        for x in &w {
            writeln!(s, "  - {x}")?;
        }
        Ok(())
    }

    pub(super) fn write_spawns(&self, c: &ModernHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "Spawns")?;
        let [tn, ctn] = teams(self.tf2);
        if self.tf2 {
            writeln!(s, "  spawns         {tn} {}, {ctn} {}", self.t.len(), self.ct.len())?;
        } else {
            writeln!(
                s,
                "  spawns         T {} (comp {}, disabled {}), CT {} (comp {}, disabled {})",
                self.t.len(),
                c.comp[0],
                c.disabled[0],
                self.ct.len(),
                c.comp[1],
                c.disabled[1]
            )?;
        }
        let gap = |g: Option<(f64, DVec3)>| g.map_or("-".to_string(), |(d, p)| format!("{d:.0} units near {:.0} {:.0} {:.0}", p.x, p.y, p.z));
        writeln!(s, "  closest pair   {tn} {}, {ctn} {}", gap(c.gaps[0]), gap(c.gaps[1]))?;
        match &c.stuck {
            Ok(v) if v.is_empty() => writeln!(s, "  in collision   none")?,
            Ok(v) => {
                for (team, p) in v {
                    writeln!(s, "  in collision   {team} at {:.0} {:.0} {:.0}", p.x, p.y, p.z)?;
                }
            }
            Err(e) => writeln!(s, "  in collision   unknown ({e})")?,
        }
        match &self.nav {
            Ok(n) => {
                if n.off_ground.is_empty() {
                    writeln!(s, "  off nav        none")?;
                }
                for (team, p) in &n.off_ground {
                    writeln!(s, "  off nav        {team} at {:.0} {:.0} {:.0}", p.x, p.y, p.z)?;
                }
            }
            Err(e) => writeln!(s, "  off nav        unknown ({e})")?,
        }
        let obj: Vec<String> = self.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
        writeln!(s, "  objectives     {}", list(&obj))?;
        if !self.tf2 {
            writeln!(s, "  buy zones      {}", self.buyzones)?;
        }
        writeln!(s, "  sky            {}", if self.sky_found { self.sky.clone() } else { format!("{} missing", self.sky) })?;
        writeln!(s)?;
        Ok(())
    }

    pub(super) fn write_graph(&self, c: &ModernHealth, s: &mut String) -> std::fmt::Result {
        match &c.graph {
            Some(g) => {
                let pct = g.reach as f64 * 100.0 / g.cells.max(1) as f64;
                writeln!(s, "  cells          {} reachable of {} ({pct:.1}%), {} ladders linked", g.reach, g.cells, g.ladders)?;
                writeln!(s, "  components     {}, largest {} cells", g.components, g.largest)?;
            }
            None => writeln!(s, "  cells          unknown")?,
        }
        writeln!(s)?;
        Ok(())
    }

    pub(super) fn write_collision(&self, c: &ModernHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "Collision")?;
        match &c.collision {
            Ok(k) => {
                let st = &k.stats;
                writeln!(s, "  hulls          solid {}, clip {}, decor {}, ignored {}", st.hulls[0], st.hulls[1], st.hulls[2], st.skipped_hulls)?;
                writeln!(s, "  triangles      solid {}, clip {}, decor {}, ignored {}", st.tris[0], st.tris[1], st.tris[2], st.skipped_tris)?;
                writeln!(s, "  shapes         {} capsules, {} spheres, {} brush entities, loaded in {:.1}s", st.capsules, st.spheres, st.ents, k.secs)?;
                if k.checked == 0 {
                    writeln!(s, "  holes          unknown (no nav)")?;
                } else {
                    writeln!(
                        s,
                        "  holes          {} of {} reachable nav cells have no floor within {HOLE_BELOW:.0} below to {HOLE_ABOVE:.0} above, {} clusters",
                        k.holes, k.checked, k.nclusters
                    )?;
                }
                for (i, (p, n)) in k.clusters.iter().enumerate() {
                    writeln!(s, "    {}. x {:.0} y {:.0} z {:.0}, {n} cells", i + 1, p.x, p.y, p.z)?;
                }
            }
            Err(e) => writeln!(s, "  collision      unknown ({e})")?,
        }
        writeln!(s)?;
        Ok(())
    }

    pub(super) fn write_overview(&self, c: &ModernHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "Overview")?;
        let r = &c.radar;
        let txt = s2radar::txt_name(&self.name.to_lowercase());
        match (&r.err, r.txt) {
            (Some(e), _) => writeln!(s, "  txt            {txt}: {e}")?,
            (None, true) => writeln!(s, "  txt            {txt}")?,
            (None, false) => writeln!(s, "  txt            missing")?,
        }
        if let Some((f, px)) = &r.frame {
            let per = px.map_or(RADAR as usize, |p| p.0);
            writeln!(s, "  framing        pos_x {} pos_y {} scale {} per pixel of a {per} px image", num(f.pos_x), num(f.pos_y), num(f.scale * RADAR as f64 / per as f64))?;
        }
        for (n, i, px) in &r.images {
            match (i, px) {
                (Some(i), Some((w, h))) => writeln!(s, "  image          {n}: {i} {w}x{h}")?,
                (Some(i), None) => writeln!(s, "  image          {n}: {i}")?,
                _ => writeln!(s, "  image          {n}: missing")?,
            }
        }
        if let Some((a, m)) = r.cover {
            writeln!(
                s,
                "  coverage       {:.1}% of reachable nav inside the frame, {:.1}% inside a {RADAR_MARGIN_PX:.0} px margin",
                a * 100.0,
                m * 100.0
            )?;
        }
        if let Some(a) = &r.align {
            writeln!(s, "  alignment      {a}")?;
        }
        for (k, d) in &r.icons {
            match d {
                Some(d) => writeln!(s, "  icon           {k} {d:.0} units from the nearest match")?,
                None => writeln!(s, "  icon           {k} has no counterpart in the map")?,
            }
        }
        writeln!(s)?;
        Ok(())
    }

    pub(super) fn cs2_row(&self, c: &ModernHealth) -> String {
        let b = |v: bool| if v { "1" } else { "0" }.to_string();
        let (off, m2, rad) = self.nav_cols();
        let e = String::new;
        let mut cols = vec![
            csv_quote(&self.name),
            "cs2".to_string(),
            match &self.nav {
                Err(e) => csv_quote(&format!("nav: {e}")),
                _ => String::new(),
            },
            self.warnings().len().to_string(),
            e(),
            e(),
            e(),
            e(),
            self.t.len().to_string(),
            self.ct.len().to_string(),
            off,
        ];
        cols.extend(self.objectives.iter().map(|(_, n)| n.to_string()));
        let ntex = c.missing_textures.len().to_string();
        cols.extend([self.buyzones.to_string(), e(), ntex, b(!self.sky_found), e(), b(c.radar.txt), e(), e(), e(), e(), e(), e(), e(), m2, rad]);
        cols.extend(std::iter::repeat_n(String::new(), LIMITS.len() * 2));
        let gap = c.gaps.iter().flatten().map(|g| g.0).reduce(f64::min);
        let holes = c.collision.as_ref().ok().filter(|k| k.checked > 0).map(|k| k.holes);
        cols.extend([
            c.bad_models.len().to_string(),
            c.missing_materials.len().to_string(),
            c.draws.to_string(),
            c.tris.to_string(),
            c.comp[0].to_string(),
            c.comp[1].to_string(),
            (c.disabled[0] + c.disabled[1]).to_string(),
            gap.map_or(String::new(), |g| format!("{g:.0}")),
            c.stuck.as_ref().map_or(String::new(), |s| s.len().to_string()),
            holes.map_or(String::new(), |h| h.to_string()),
            b(c.radar.images_ok()),
        ]);
        cols.join(",")
    }
}
