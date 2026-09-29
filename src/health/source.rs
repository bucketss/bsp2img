use super::*;

pub(super) type SrcLimitRow = (&'static str, &'static str, usize, usize, usize, usize);

pub(super) const SRC_LIMITS: [SrcLimitRow; 22] = [
    ("models", "models", s1b::LUMP_MODELS, 48, 1024, 1024),
    ("brushes", "brushes", s1b::LUMP_BRUSHES, 12, 8192, 8192),
    ("brush_sides", "brush sides", s1b::LUMP_BRUSHSIDES, 8, 65536, 65536),
    ("planes", "planes", s1b::LUMP_PLANES, 20, 65536, 65536),
    ("vertices", "vertices", s1b::LUMP_VERTEXES, 12, 65536, 65536),
    ("nodes", "nodes", s1b::LUMP_NODES, 32, 65536, 65536),
    ("leaves", "leaves", s1b::LUMP_LEAFS, 0, 65536, 65536),
    ("leaf_faces", "leaf faces", s1b::LUMP_LEAFFACES, 2, 65536, 65536),
    ("leaf_brushes", "leaf brushes", s1b::LUMP_LEAFBRUSHES, 2, 65536, 65536),
    ("faces", "faces", s1b::LUMP_FACES, 56, 65536, 65536),
    ("texinfo", "texinfo", s1b::LUMP_TEXINFO, 72, 12288, 12288),
    ("texdata", "texdata", s1b::LUMP_TEXDATA, 32, 2048, 2048),
    ("texdata_bytes", "texture names", s1b::LUMP_TEXDATA_STRING_DATA, 1, 256000, 256000),
    ("displacements", "displacements", s1b::LUMP_DISPINFO, 176, 2048, 32768),
    ("edges", "edges", s1b::LUMP_EDGES, 4, 256000, 256000),
    ("surfedges", "surfedges", s1b::LUMP_SURFEDGES, 4, 512000, 512000),
    ("areas", "areas", s1b::LUMP_AREAS, 8, 256, 1024),
    ("overlays", "overlays", s1b::LUMP_OVERLAYS, 352, 512, 1024),
    ("cubemaps", "cubemaps", s1b::LUMP_CUBEMAPS, 16, 1024, 1024),
    ("lighting_bytes", "lighting LDR", s1b::LUMP_LIGHTING, 1, 0x1000000, 0),
    ("lighting_hdr_bytes", "lighting HDR", s1b::LUMP_LIGHTING_HDR, 1, 0x1000000, 0),
    ("vis_bytes", "vis data", s1b::LUMP_VISIBILITY, 1, 0x1000000, 0x1000000),
];

pub(super) const SRC_ENTITIES: (usize, usize) = (8192, 20480);

pub(super) fn lump_len(map: &Map, i: usize) -> usize {
    if map.compressed(i) { u32::from_le_bytes(map.lumps[i].fourcc) as usize } else { map.lumps[i].len }
}

pub(super) fn src_limits(map: &Map, entities: usize) -> Vec<SrcLimit> {
    let csgo = map.version >= 21;
    let mut out: Vec<SrcLimit> = SRC_LIMITS
        .iter()
        .map(|&(key, label, lump, size, sdk, go)| {
            let len = lump_len(map, lump);
            let size = match size {
                0 if map.lumps[lump].version == 0 && len.is_multiple_of(56) => 56,
                0 if len.is_multiple_of(32) => 32,
                0 => 56,
                n => n,
            };
            let cap = if csgo { go } else { sdk };
            SrcLimit { key, label, count: len / size, cap: (cap > 0).then_some(cap) }
        })
        .collect();
    out.push(SrcLimit { key: "entities", label: "entities", count: entities, cap: Some(if csgo { SRC_ENTITIES.1 } else { SRC_ENTITIES.0 }) });
    out
}

pub(super) fn lit(map: &Map, faces: usize, light: usize) -> Option<Lit> {
    if !map.has(light) {
        return None;
    }
    let fs = map.faces_lump(faces).ok()?;
    let ti = map.texinfo().ok()?;
    let (mut lightable, mut unlit) = (0, 0);
    for f in &fs {
        let flags = usize::try_from(f.texinfo).ok().and_then(|t| ti.get(t)).map_or(s1b::SURF_NOLIGHT, |t| t.flags);
        if flags & (s1b::SURF_NOLIGHT | s1b::SURF_HIDDEN | s1b::SURF_WARP) != 0 {
            continue;
        }
        lightable += 1;
        if f.lightofs < 0 {
            unlit += 1;
        }
    }
    Some(Lit { bytes: lump_len(map, light), lightable, unlit })
}

pub(super) fn nav_file(x: &Extra, map: &Map, name: &str) -> NavFile {
    let file = crate::s1::nav::name(name);
    if !x.fs.has(&file) {
        return NavFile::Missing { file };
    }
    let parsed = x.fs.read(&file).and_then(|b| crate::s1::nav::parse(&b));
    let why = match (&parsed, &x.nav) {
        (Err(e), _) => format!("{e:#}"),
        (Ok(n), _) if n.mesh.areas.len() < crate::s1::nav::MIN_AREAS => format!("{} areas, fewer than {}", n.mesh.areas.len(), crate::s1::nav::MIN_AREAS),
        (Ok(n), None) if std::env::var("BSP2IMG_SOURCE_NAV").is_ok_and(|v| v == "off") => format!("BSP2IMG_SOURCE_NAV=off, {} areas", n.mesh.areas.len()),
        (Ok(n), None) => format!("{} areas, none under a spawn point", n.mesh.areas.len()),
        (Ok(n), Some(_)) => {
            let named = (0..n.mesh.areas.len()).filter(|&a| n.place(a).is_some()).count();
            let built_for = (n.bsp_size != 0 && n.bsp_size as usize != map.bytes().len()).then_some(n.bsp_size as usize);
            return NavFile::Used { file, version: n.version, sub: n.sub_version, analyzed: n.analyzed, areas: n.mesh.areas.len(), places: n.places.len(), named, built_for };
        }
    };
    NavFile::Ignored { file, why }
}

pub(super) fn gather_src(scene: &Scene, x: &Extra, map: &Arc<Map>, name: &str, o: &HealthOpts) -> Health {
    let Health { name: hname, t, ct, objectives, buyzones, sky, sky_found, nav, engine } = gather_cs2(scene, x, name, o);
    let Engine::Cs2(mut c) = engine else { unreachable!() };
    let (missing_models, missing_materials): (Vec<String>, Vec<String>) = std::mem::take(&mut c.missing_materials).into_iter().partition(|m| m.ends_with(".mdl"));
    c.missing_materials = missing_materials;
    let failed_models: Vec<String> = c.bad_models.iter().filter(|m| !m.ends_with(": not found")).cloned().collect();
    let ents = map.entities().map(|t| crate::s1::entities::parse(&t)).map_or(0, |e| e.list.len() + e.disabled.len());
    let static_props = StaticProps::from_bsp(map.bytes(), map.raw(s1b::LUMP_GAME_LUMP)).ok().flatten().map_or(0, |s| s.props.len());
    let pak = crate::s1::fs::Zip::open(map.clone()).ok().flatten().map(|z| (z.bytes(), z.len()));
    let csgo = map.version >= 21;
    let s = SrcHealth {
        version: map.version,
        revision: map.revision,
        game: if csgo { "CS:GO" } else { "CS:S" },
        sdk: if csgo { "CS:GO" } else { "SDK 2013" },
        limits: src_limits(map, ents),
        ldr: lit(map, s1b::LUMP_FACES, s1b::LUMP_LIGHTING),
        hdr: lit(map, s1b::LUMP_FACES_HDR, s1b::LUMP_LIGHTING_HDR),
        vis_bytes: lump_len(map, s1b::LUMP_VISIBILITY),
        displacements: lump_len(map, s1b::LUMP_DISPINFO) / 176,
        static_props,
        failed_models,
        missing_models,
        nav: nav_file(x, map, &name.to_lowercase()),
        pak,
        base: *c,
    };
    Health { name: hname, t, ct, objectives, buyzones, sky, sky_found, nav, engine: Engine::Source(Box::new(s)) }
}

pub(super) fn src_facts(h: &Health, s: &SrcHealth) -> Vec<(String, [u8; 3])> {
    let c = &s.base;
    let white = [220, 220, 220];
    let ok = |b: bool| if b { GREEN } else { RED };
    let mut v = Vec::new();
    v.push((format!("Source BSP v{} ({}), limits from {} bspfile.h", s.version, s.game, s.sdk), white));
    let light: Vec<String> = [("LDR", &s.ldr), ("HDR", &s.hdr)]
        .iter()
        .filter_map(|(k, l)| l.as_ref().map(|l| format!("{k} {} KiB, {} unlit", l.bytes / 1024, l.unlit)))
        .collect();
    let lit_ok = !light.is_empty() && s.ldr.iter().chain(&s.hdr).all(|l| l.unlit as f64 * 100.0 <= l.lightable as f64 * UNLIT_WARN_PCT);
    v.push((format!("lighting: {}", if light.is_empty() { "none".to_string() } else { light.join(", ") }), ok(lit_ok)));
    v.push((if s.vis_bytes == 0 { "VIS: not run".to_string() } else { format!("VIS: {} KiB", s.vis_bytes / 1024) }, ok(s.vis_bytes > 0)));
    let (nf, nm, nmat, nt) = (s.failed_models.len(), s.missing_models.len(), c.missing_materials.len(), c.missing_textures.len());
    v.push((format!("load: {nf} failed, {nm} missing models, {nmat} materials, {nt} textures"), ok(nf + nm + nmat + nt == 0)));
    v.push((format!("geometry: {:.1}M triangles, {} displacements, {} props", c.tris as f64 / 1e6, s.displacements, s.static_props), white));
    v.push((s.pak.map_or("pakfile: none".to_string(), |(b, n)| format!("pakfile: {:.1} MB, {n} files", b as f64 / 1048576.0)), white));
    let comp_ok = c.comp.iter().all(|&n| n >= COMP_MIN);
    v.push((
        format!("spawns: T {} (comp {}), CT {} (comp {}), {} disabled", h.t.len(), c.comp[0], h.ct.len(), c.comp[1], c.disabled[0] + c.disabled[1]),
        if comp_ok { GREEN } else { AMBER },
    ));
    match &h.nav {
        Ok(n) => v.push((format!("spawns off walkable ground: {}", n.off_ground.len()), ok(n.off_ground.is_empty()))),
        Err(_) => v.push(("spawns off walkable ground: unknown".into(), AMBER)),
    }
    match &c.stuck {
        Ok(x) => v.push((format!("spawns in collision: {}", x.len()), ok(x.is_empty()))),
        Err(_) => v.push(("spawns in collision: unknown".into(), AMBER)),
    }
    let obj: Vec<String> = h.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
    v.push((format!("objectives: {}", list(&obj)), if obj.is_empty() { AMBER } else { white }));
    match &s.nav {
        NavFile::Used { version, sub, areas, places, built_for, .. } => v.push((
            format!("nav: v{version}.{sub}, {areas} areas, {places} places{}", if built_for.is_some() { ", other BSP build" } else { "" }),
            if built_for.is_some() { AMBER } else { GREEN },
        )),
        NavFile::Ignored { .. } => v.push(("nav: ignored, collision walk fallback".into(), AMBER)),
        NavFile::Missing { .. } => v.push(("nav: none, collision walk fallback".into(), AMBER)),
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
        v.push((format!("radar frame holds {:.1}% of reachable walk", m * 100.0), if m >= COVER_MIN { GREEN } else { AMBER }));
    }
    let far = r.icons.iter().filter(|(_, d)| d.is_some_and(|d| d > ICON_FAR)).count();
    if !r.icons.is_empty() {
        v.push((format!("radar icons: {} of {} far from spawns or sites", far, r.icons.len()), if far == 0 { GREEN } else { AMBER }));
    }
    match &h.nav {
        Ok(n) => {
            v.push((format!("walkable area: {:.0} m2", n.walkable_m2), white));
            for (i, sp) in n.spots.iter().enumerate() {
                v.push((format!("open area {}: {:.0} {:.0}, radius {:.0}", i + 1, sp.pos.x, sp.pos.y, sp.radius), [255, 230, 0]));
            }
        }
        Err(e) => v.push((format!("walk: {e}"), AMBER)),
    }
    v
}

impl Health {
    pub(super) fn src_warnings(&self, h: &SrcHealth) -> Vec<String> {
        let c = &h.base;
        let mut w = Vec::new();
        if h.ldr.is_none() && h.hdr.is_none() {
            w.push("no lighting data (VRAD not run)".to_string());
        }
        for l in h.ldr.iter().chain(&h.hdr).filter(|l| l.unlit as f64 * 100.0 > l.lightable as f64 * UNLIT_WARN_PCT).take(1) {
            w.push(format!("{} of {} lightable faces have no lightmap", l.unlit, l.lightable));
        }
        if h.vis_bytes == 0 {
            w.push("VIS was not run (usually a leak)".to_string());
        }
        for l in &h.limits {
            if let Some(p) = l.pct().filter(|&p| p > BAD_PCT) {
                w.push(format!("{} at {p:.0}% of the {} limit", l.label, h.sdk));
            }
        }
        if !h.failed_models.is_empty() {
            w.push(format!("{} models failed to load", h.failed_models.len()));
        }
        if !h.missing_models.is_empty() {
            w.push(format!("{} models missing", h.missing_models.len()));
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
        match &h.nav {
            NavFile::Missing { file } => w.push(format!("no {file}")),
            NavFile::Ignored { file, why } => w.push(format!("{file} ignored: {why}")),
            NavFile::Used { file, built_for: Some(n), .. } => w.push(format!("{file} was generated for a {n} byte BSP")),
            _ => {}
        }
        match &self.nav {
            Ok(n) if !n.off_ground.is_empty() => w.push(format!("{} spawns not on walkable ground", n.off_ground.len())),
            Err(e) => w.push(format!("walk: {e}")),
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

    pub(super) fn write_src(&self, h: &SrcHealth, s: &mut String) -> std::fmt::Result {
        let c = &h.base;
        writeln!(s, "{} health", self.name)?;
        writeln!(s)?;
        writeln!(s, "Load")?;
        writeln!(s, "  engine         Source BSP v{} ({}), map revision {}", h.version, h.game, h.revision)?;
        for (i, p) in c.paks.iter().enumerate() {
            writeln!(s, "  {:<15}{}", if i == 0 { "sources" } else { "" }, p.display())?;
        }
        match h.pak {
            Some((b, n)) => writeln!(s, "  pakfile        {:.1} MB, {n} files", b as f64 / 1048576.0)?,
            None => writeln!(s, "  pakfile        none")?,
        }
        writeln!(s, "  failed models  {}", short(&h.failed_models))?;
        writeln!(s, "  models         {} missing: {}", h.missing_models.len(), short(&h.missing_models))?;
        writeln!(s, "  materials      {} missing: {}", c.missing_materials.len(), short(&c.missing_materials))?;
        writeln!(s, "  textures       {} missing: {}", c.missing_textures.len(), short(&c.missing_textures))?;
        writeln!(s)?;
        writeln!(s, "Compile")?;
        for (k, l) in [("LDR", &h.ldr), ("HDR", &h.hdr)] {
            match l {
                Some(l) => writeln!(s, "  lighting {k}   {} bytes, {} of {} lightable faces unlit", l.bytes, l.unlit, l.lightable)?,
                None => writeln!(s, "  lighting {k}   none")?,
            }
        }
        if h.vis_bytes == 0 {
            writeln!(s, "  VIS            not run")?;
        } else {
            writeln!(s, "  VIS            {} bytes", h.vis_bytes)?;
        }
        writeln!(s)?;
        writeln!(s, "Limits (% of the {} bspfile.h limit)", h.sdk)?;
        writeln!(s, "  {:<15}{:>10}{:>10}{:>7}", "", "count", "limit", "%")?;
        for l in &h.limits {
            match l.pct() {
                Some(p) => writeln!(s, "  {:<15}{:>10}{:>10}{p:>6.0}%", l.label, l.count, opt(l.cap))?,
                None => writeln!(s, "  {:<15}{:>10}{:>10}{:>7}", l.label, l.count, "unknown", "-")?,
            }
        }
        writeln!(s)?;
        writeln!(s, "Geometry")?;
        writeln!(s, "  triangles      {}", c.tris)?;
        writeln!(s, "  faces          {} ({} cropped)", c.draws, c.dropped)?;
        writeln!(s, "  displacements  {}", h.displacements)?;
        writeln!(s, "  static props   {}, {} models", h.static_props, c.models)?;
        writeln!(s, "  materials      {}", c.materials)?;
        match c.lightmap {
            Some((w, h)) => writeln!(s, "  lightmap       {w}x{h}")?,
            None => writeln!(s, "  lightmap       none")?,
        }
        writeln!(s)?;
        self.write_spawns(c, s)?;
        writeln!(s, "Nav")?;
        match &h.nav {
            NavFile::Used { file, version, sub, analyzed, areas, places, named, built_for } => {
                writeln!(s, "  file           {file} v{version}.{sub}{}", if *analyzed { ", analyzed" } else { "" })?;
                writeln!(s, "  areas          {areas}, ladders {}, {places} places on {named} areas", c.mesh_ladders)?;
                if let Some(n) = built_for {
                    writeln!(s, "  built for      a {n} byte BSP")?;
                }
            }
            NavFile::Ignored { file, why } => {
                writeln!(s, "  file           {file} ignored: {why}")?;
                writeln!(s, "  walk           brush collision fallback")?;
            }
            NavFile::Missing { file } => {
                writeln!(s, "  file           {file} missing")?;
                writeln!(s, "  walk           brush collision fallback")?;
            }
        }
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

    pub(super) fn src_row(&self, h: &SrcHealth) -> String {
        let c = &h.base;
        let b = |v: bool| if v { "1" } else { "0" }.to_string();
        let (off, m2, rad) = self.nav_cols();
        let e = String::new;
        let lit = h.hdr.as_ref().or(h.ldr.as_ref());
        let light = h.ldr.as_ref().map_or(0, |l| l.bytes) + h.hdr.as_ref().map_or(0, |l| l.bytes);
        let mut cols = vec![
            csv_quote(&self.name),
            format!("source{}", h.version),
            match &self.nav {
                Err(e) => csv_quote(&format!("walk: {e}")),
                _ => String::new(),
            },
            self.warnings().len().to_string(),
            h.vis_bytes.to_string(),
            light.to_string(),
            lit.map_or(String::new(), |l| l.unlit.to_string()),
            lit.map_or(String::new(), |l| l.lightable.to_string()),
            self.t.len().to_string(),
            self.ct.len().to_string(),
            off,
        ];
        cols.extend(self.objectives.iter().map(|(_, n)| n.to_string()));
        let ntex = c.missing_textures.len().to_string();
        let nfiles = h.missing_models.len().to_string();
        cols.extend([self.buyzones.to_string(), e(), ntex, b(!self.sky_found), nfiles, b(c.radar.txt), e(), e(), e(), e(), e(), e(), e(), m2, rad]);
        for (key, ..) in LIMITS {
            match h.limits.iter().find(|l| l.key == key) {
                Some(l) => cols.extend([l.count.to_string(), l.pct().map_or(String::new(), |p| format!("{p:.1}"))]),
                None => cols.extend([e(), e()]),
            }
        }
        let gap = c.gaps.iter().flatten().map(|g| g.0).reduce(f64::min);
        let holes = c.collision.as_ref().ok().filter(|k| k.checked > 0).map(|k| k.holes);
        cols.extend([
            h.failed_models.len().to_string(),
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
