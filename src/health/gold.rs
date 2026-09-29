use super::*;

pub(super) const HLSDK_MAX_MAP_MODELS: usize = 400;
pub(super) const HLSDK_MAX_MAP_ENTITIES: usize = 1024;
pub(super) const HLSDK_MAX_MAP_ENTSTRING: usize = 128 * 1024;
pub(super) const HLSDK_MAX_MAP_PLANES: usize = 32767;
pub(super) const HLSDK_MAX_MAP_NODES: usize = 32767;
pub(super) const HLSDK_MAX_MAP_CLIPNODES: usize = 32767;
pub(super) const HLSDK_MAX_MAP_LEAFS: usize = 8192;
pub(super) const HLSDK_MAX_MAP_VERTS: usize = 65535;
pub(super) const HLSDK_MAX_MAP_FACES: usize = 65535;
pub(super) const HLSDK_MAX_MAP_MARKSURFACES: usize = 65535;
pub(super) const HLSDK_MAX_MAP_TEXINFO: usize = 8192;
pub(super) const HLSDK_MAX_MAP_EDGES: usize = 256000;
pub(super) const HLSDK_MAX_MAP_SURFEDGES: usize = 512000;
pub(super) const HLSDK_MAX_MAP_TEXTURES: usize = 512;
pub(super) const HLSDK_MAX_MAP_MIPTEX: usize = 0x200000;
pub(super) const HLSDK_MAX_MAP_LIGHTING: usize = 0x200000;
pub(super) const HLSDK_MAX_MAP_VISIBILITY: usize = 0x200000;

pub(super) const VHLT34_MAX_MAP_MODELS: usize = 512;
pub(super) const VHLT34_MAX_MAP_ENTITIES: usize = 16384;
pub(super) const VHLT34_MAX_MAP_ENTSTRING: usize = 2048 * 1024;
pub(super) const VHLT34_MAX_MAP_PLANES: usize = 32768;
pub(super) const VHLT34_MAX_MAP_NODES: usize = 32767;
pub(super) const VHLT34_MAX_MAP_CLIPNODES: usize = 32767;
pub(super) const VHLT34_MAX_MAP_LEAFS: usize = 32760;
pub(super) const VHLT34_MAX_MAP_LEAFS_ENGINE: usize = 8192;
pub(super) const VHLT34_MAX_MAP_VERTS: usize = 65535;
pub(super) const VHLT34_MAX_MAP_FACES: usize = 65535;
pub(super) const VHLT34_MAX_MAP_WORLDFACES: usize = 32768;
pub(super) const VHLT34_MAX_MAP_MARKSURFACES: usize = 65535;
pub(super) const VHLT34_MAX_MAP_TEXINFO: usize = 32767;
pub(super) const VHLT34_MAX_MAP_EDGES: usize = 256000;
pub(super) const VHLT34_MAX_MAP_SURFEDGES: usize = 512000;
pub(super) const VHLT34_MAX_MAP_TEXTURES: usize = 4096;
pub(super) const VHLT34_DEFAULT_MAX_MAP_MIPTEX: usize = 0x2000000;
pub(super) const VHLT34_DEFAULT_MAX_MAP_LIGHTDATA: usize = 0x3000000;
pub(super) const VHLT34_MAX_MAP_VISIBILITY: usize = 0x800000;

pub(super) const REHLDS_QLIMITS_MAX_MODELS: usize = 512;

pub(super) type LimitRow = (&'static str, &'static str, Option<usize>, Option<usize>, usize);

pub(super) const LIMITS: [LimitRow; 19] = [
    ("models", "models", Some(REHLDS_QLIMITS_MAX_MODELS), Some(HLSDK_MAX_MAP_MODELS), VHLT34_MAX_MAP_MODELS),
    ("planes", "planes", None, Some(HLSDK_MAX_MAP_PLANES), VHLT34_MAX_MAP_PLANES),
    ("vertices", "vertices", None, Some(HLSDK_MAX_MAP_VERTS), VHLT34_MAX_MAP_VERTS),
    ("nodes", "nodes", None, Some(HLSDK_MAX_MAP_NODES), VHLT34_MAX_MAP_NODES),
    ("texinfo", "texinfo", None, Some(HLSDK_MAX_MAP_TEXINFO), VHLT34_MAX_MAP_TEXINFO),
    ("faces", "faces", None, Some(HLSDK_MAX_MAP_FACES), VHLT34_MAX_MAP_FACES),
    ("world_faces", "world faces", None, None, VHLT34_MAX_MAP_WORLDFACES),
    ("clipnodes", "clipnodes", None, Some(HLSDK_MAX_MAP_CLIPNODES), VHLT34_MAX_MAP_CLIPNODES),
    ("leaves", "leaves", None, Some(HLSDK_MAX_MAP_LEAFS), VHLT34_MAX_MAP_LEAFS),
    ("world_leaves", "world leaves", Some(VHLT34_MAX_MAP_LEAFS_ENGINE), None, VHLT34_MAX_MAP_LEAFS_ENGINE),
    ("marksurfaces", "marksurfaces", None, Some(HLSDK_MAX_MAP_MARKSURFACES), VHLT34_MAX_MAP_MARKSURFACES),
    ("edges", "edges", None, Some(HLSDK_MAX_MAP_EDGES), VHLT34_MAX_MAP_EDGES),
    ("surfedges", "surfedges", None, Some(HLSDK_MAX_MAP_SURFEDGES), VHLT34_MAX_MAP_SURFEDGES),
    ("textures", "textures", None, Some(HLSDK_MAX_MAP_TEXTURES), VHLT34_MAX_MAP_TEXTURES),
    ("texture_bytes", "texture data", None, Some(HLSDK_MAX_MAP_MIPTEX), VHLT34_DEFAULT_MAX_MAP_MIPTEX),
    ("lighting_bytes", "lighting data", None, Some(HLSDK_MAX_MAP_LIGHTING), VHLT34_DEFAULT_MAX_MAP_LIGHTDATA),
    ("vis_bytes", "vis data", None, Some(HLSDK_MAX_MAP_VISIBILITY), VHLT34_MAX_MAP_VISIBILITY),
    ("entities", "entities", None, Some(HLSDK_MAX_MAP_ENTITIES), VHLT34_MAX_MAP_ENTITIES),
    ("entity_bytes", "entity data", None, Some(HLSDK_MAX_MAP_ENTSTRING), VHLT34_MAX_MAP_ENTSTRING),
];

pub(super) fn limits(bsp: &Bsp) -> Vec<Limit> {
    let counts = [
        bsp.models.len(),
        bsp.planes.len(),
        bsp.vertices.len(),
        bsp.nodes.len(),
        bsp.texinfo.len(),
        bsp.faces.len(),
        bsp.models[0].numfaces.max(0) as usize,
        bsp.clipnodes.len(),
        bsp.leaves.len(),
        bsp.models[0].visleafs.max(0) as usize,
        bsp.marksurfaces.len(),
        bsp.edges.len(),
        bsp.surfedges.len(),
        bsp.miptex.len(),
        bsp.lump_len[LUMP_TEXTURES],
        bsp.lump_len[LUMP_LIGHTING],
        bsp.lump_len[LUMP_VISIBILITY],
        bsp.entities.len(),
        bsp.lump_len[LUMP_ENTITIES],
    ];
    LIMITS.iter().zip(counts).map(|(&(_, label, engine, hlsdk, vhlt), count)| Limit { label, count, engine, hlsdk, vhlt }).collect()
}

pub(super) fn find_file(dirs: &[PathBuf], rel: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| rel.split('/').fold(d.clone(), |p, s| p.join(s))).find(|p| p.is_file())
}

pub(super) fn game_dirs(scene: &Scene) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in &scene.dirs {
        if !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

pub(super) fn norm(s: &str) -> String {
    s.trim().replace('\\', "/").trim_start_matches('/').to_string()
}

pub(super) fn referenced_files(bsp: &Bsp) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in &bsp.entities {
        for (k, v) in &e.0 {
            let v = norm(v);
            let lv = v.to_lowercase();
            if v.starts_with('*') || v.starts_with('!') || v.is_empty() {
                continue;
            }
            let sound = lv.ends_with(".wav") || lv.ends_with(".mp3");
            let is_sound_key = k.starts_with("noise") || (k == "message" && e.class() == "ambient_generic");
            if k == "model" && (lv.ends_with(".mdl") || lv.ends_with(".spr")) {
                out.insert(v);
            } else if is_sound_key && sound {
                out.insert(format!("sound/{v}"));
            }
        }
    }
    out
}

pub(super) fn read_bmp8(p: &Path) -> Result<Bmp8, String> {
    let d = std::fs::read(p).map_err(|e| e.to_string())?;
    if d.len() < 54 || &d[..2] != b"BM" {
        return Err("not a BMP".into());
    }
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let off = u32_at(10) as usize;
    let w = u32_at(18) as i32;
    let h = u32_at(22) as i32;
    let bpp = u16_at(28);
    let comp = u32_at(30);
    if bpp != 8 {
        return Err(format!("{bpp}-bit, not 8-bit"));
    }
    if comp != 0 {
        return Err("compressed".into());
    }
    let row = (w.max(0) as usize).div_ceil(4) * 4;
    let rows = h.unsigned_abs() as usize;
    let mut n255 = 0;
    for y in 0..rows {
        let s = off + y * row;
        let Some(r) = d.get(s..s + w.max(0) as usize) else { return Err("truncated".into()) };
        n255 += r.iter().filter(|&&c| c == 255).count();
    }
    Ok(Bmp8 { w, h: h.abs(), n255 })
}

pub(super) fn res_entries(p: &Path) -> Vec<String> {
    let text: String = std::fs::read(p).unwrap_or_default().iter().map(|&c| c as char).collect();
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .flat_map(|l| l.split_whitespace().map(|t| t.trim_matches('"').to_string()).collect::<Vec<_>>())
        .map(|t| norm(&t).to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

pub(super) fn gold_facts(h: &Health, g: &GoldHealth) -> Vec<(String, [u8; 3])> {
    let white = [220, 220, 220];
    let ok = |b: bool| if b { GREEN } else { RED };
    let mut v = Vec::new();
    v.push((
        if g.vis_bytes == 0 { "VIS: not run".to_string() } else { format!("VIS: {} KiB", g.vis_bytes / 1024) },
        ok(g.vis_bytes > 0),
    ));
    v.push((format!("lighting: {} KiB, {} unlit faces", g.light_bytes / 1024, g.unlit), ok(g.light_bytes > 0 && g.unlit == 0)));
    let spawns_ok = h.t.len() >= SPAWNS_FOR_32_SLOTS && h.ct.len() >= SPAWNS_FOR_32_SLOTS;
    v.push((format!("spawns: T {}, CT {}", h.t.len(), h.ct.len()), if spawns_ok { GREEN } else { AMBER }));
    match &h.nav {
        Ok(n) => v.push((format!("spawns off ground: {}", n.off_ground.len()), ok(n.off_ground.is_empty()))),
        Err(_) => v.push(("spawns off ground: unknown".into(), AMBER)),
    }
    let obj: Vec<String> = h.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
    v.push((format!("objectives: {}", list(&obj)), if obj.is_empty() { AMBER } else { white }));
    v.push((
        if h.buyzones == 0 { "buy zones: default".to_string() } else { format!("buy zones: {}", h.buyzones) },
        white,
    ));
    let miss = g.missing_wads.len() + g.missing_textures.len() + g.missing_files.len() + usize::from(!h.sky_found);
    v.push((
        format!(
            "missing: {} WADs, {} textures, {} files{}",
            g.missing_wads.len(),
            g.missing_textures.len(),
            g.missing_files.len(),
            if h.sky_found { "" } else { ", sky" }
        ),
        ok(miss == 0),
    ));
    let have: Vec<&str> = OV_EXTS.iter().zip(&g.ov).filter(|(_, p)| p.is_some()).map(|(e, _)| *e).collect();
    let bmp_ok = !matches!(&g.bmp, Some(Ok(b)) if b.n255 > 0) && !matches!(&g.bmp, Some(Err(_)));
    v.push((
        format!(
            "overview: {}{}",
            if have.is_empty() { "none".to_string() } else { have.join(" ") },
            match &g.bmp {
                Some(Ok(b)) if b.n255 > 0 => ", BMP uses index 255",
                Some(Err(_)) => ", BMP not 8-bit",
                _ => "",
            }
        ),
        if g.ov[0].is_some() && g.ov[1].is_some() && bmp_ok { GREEN } else { AMBER },
    ));
    let listed: Vec<&str> = OV_EXTS.iter().zip(g.res_lists).filter(|(_, b)| *b).map(|(e, _)| *e).collect();
    v.push((
        match &g.res {
            Some(_) => format!(".res lists overview: {}", if listed.is_empty() { "nothing".into() } else { listed.join(" ") }),
            None => ".res: none".to_string(),
        },
        if g.ov[1].is_none() || g.res_lists[1] { white } else { AMBER },
    ));
    match &h.nav {
        Ok(n) => {
            v.push((format!("walkable area: {:.0} m2", n.walkable_m2), white));
            for (i, s) in n.spots.iter().enumerate() {
                v.push((
                    format!("open area {}: {:.0} {:.0}, radius {:.0}", i + 1, s.pos.x, s.pos.y, s.radius),
                    [255, 230, 0],
                ));
            }
        }
        Err(e) => v.push((format!("walk grid: {e}"), AMBER)),
    }
    v
}

impl Health {
    pub(super) fn gold_warnings(&self, g: &GoldHealth) -> Vec<String> {
        let mut w = Vec::new();
        if g.vis_bytes == 0 {
            w.push("VIS was not run (usually a leak, or -novis)".to_string());
        }
        if g.light_bytes == 0 {
            w.push("no lighting data (RAD not run)".to_string());
        } else if g.unlit > 0 {
            w.push(format!("{} of {} lightable faces have no lightmap (-nolight or partial RAD)", g.unlit, g.lightable));
        }
        for l in &g.limits {
            if l.pct() > BAD_PCT {
                w.push(format!("{} at {:.0}% of the limit", l.label, l.pct()));
            }
        }
        for (team, n) in [("T", self.t.len()), ("CT", self.ct.len())] {
            if n < SPAWNS_FOR_32_SLOTS {
                w.push(format!("{n} {team} spawns (fewer than {SPAWNS_FOR_32_SLOTS} for a 32-slot server)"));
            }
        }
        match &self.nav {
            Ok(n) if !n.off_ground.is_empty() => w.push(format!("{} spawns not on walkable ground", n.off_ground.len())),
            Err(e) => w.push(format!("walk grid failed: {e}")),
            _ => {}
        }
        if self.objectives.iter().all(|(_, n)| *n == 0) {
            w.push("no objectives".to_string());
        }
        if !g.missing_wads.is_empty() {
            w.push(format!("{} WADs missing", g.missing_wads.len()));
        }
        if !g.missing_textures.is_empty() {
            w.push(format!("{} textures missing", g.missing_textures.len()));
        }
        if !self.sky_found {
            w.push(format!("sky {} missing", self.sky));
        }
        if !g.missing_files.is_empty() {
            w.push(format!("{} models/sprites/sounds missing", g.missing_files.len()));
        }
        if g.ov[0].is_none() || g.ov[1].is_none() {
            w.push("no overview".to_string());
        }
        if let Some(Ok(b)) = &g.bmp {
            if b.n255 > 0 {
                w.push(format!("overview BMP uses palette index 255 in {} pixels", b.n255));
            }
        }
        if let Some(Err(e)) = &g.bmp {
            w.push(format!("overview BMP: {e}"));
        }
        if g.ov[1].is_some() && !g.res_lists[1] {
            w.push("overview BMP not listed in the .res".to_string());
        }
        w
    }

    pub(super) fn write_gold(&self, g: &GoldHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "{} health", self.name)?;
        writeln!(s)?;
        writeln!(s, "Compile")?;
        if g.vis_bytes == 0 {
            writeln!(s, "  VIS            not run")?;
        } else {
            writeln!(s, "  VIS            {} bytes", g.vis_bytes)?;
        }
        writeln!(s, "  lighting       {} bytes, {} of {} lightable faces unlit", g.light_bytes, g.unlit, g.lightable)?;
        writeln!(s)?;
        writeln!(s, "Limits (% of the engine limit where known, else VHLT 34)")?;
        writeln!(s, "  {:<15}{:>10}{:>10}{:>10}{:>10}{:>7}", "", "count", "engine", "HLSDK", "VHLT", "%")?;
        for l in &g.limits {
            writeln!(
                s,
                "  {:<15}{:>10}{:>10}{:>10}{:>10}{:>6.0}%{}",
                l.label,
                l.count,
                opt(l.engine),
                opt(l.hlsdk),
                l.vhlt,
                l.pct(),
                if l.over_hlsdk() { "  over HLSDK" } else { "" }
            )?;
        }
        writeln!(s)?;
        writeln!(s, "Gameplay")?;
        writeln!(s, "  spawns         T {}, CT {}", self.t.len(), self.ct.len())?;
        match &self.nav {
            Ok(n) => {
                if n.off_ground.is_empty() {
                    writeln!(s, "  off ground     none")?;
                }
                for (team, p) in &n.off_ground {
                    writeln!(s, "  off ground     {team} at {:.0} {:.0} {:.0}", p.x, p.y, p.z)?;
                }
            }
            Err(e) => writeln!(s, "  off ground     unknown ({e})")?,
        }
        let obj: Vec<String> = self.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
        writeln!(s, "  objectives     {}", list(&obj))?;
        if self.buyzones == 0 {
            writeln!(s, "  buy zones      none (CS adds default zones around spawns)")?;
        } else {
            writeln!(s, "  buy zones      {}", self.buyzones)?;
        }
        writeln!(s)?;
        writeln!(s, "Missing assets")?;
        writeln!(s, "  WADs           {}", list(&g.missing_wads))?;
        writeln!(s, "  textures       {}", list(&g.missing_textures))?;
        writeln!(s, "  sky            {}", if self.sky_found { format!("{} found", self.sky) } else { format!("{} missing", self.sky) })?;
        writeln!(s, "  files          {}", list(&g.missing_files))?;
        writeln!(s)?;
        writeln!(s, "Overview")?;
        for (i, e) in OV_EXTS.iter().enumerate() {
            let where_ = g.ov[i].as_ref().map_or("missing".to_string(), |p| p.display().to_string());
            writeln!(s, "  .{e:<13} {where_}")?;
        }
        match &g.bmp {
            Some(Ok(b)) => writeln!(
                s,
                "  BMP            {}x{}, palette index 255 in {} pixels",
                b.w,
                b.h,
                b.n255
            )?,
            Some(Err(e)) => writeln!(s, "  BMP            {e}")?,
            None => {}
        }
        match &g.res {
            Some(p) => {
                let listed: Vec<String> =
                    OV_EXTS.iter().zip(g.res_lists).filter(|(_, b)| *b).map(|(e, _)| format!(".{e}")).collect();
                writeln!(s, "  .res           {} lists {}", p.display(), if listed.is_empty() { "no overview files".into() } else { listed.join(" ") })?;
            }
            None => writeln!(s, "  .res           missing")?,
        }
        writeln!(s)?;
        self.write_open(s)?;
        writeln!(s)?;
        let w = self.warnings();
        writeln!(s, "Warnings ({})", w.len())?;
        for x in &w {
            writeln!(s, "  - {x}")?;
        }
        Ok(())
    }

    pub(super) fn gold_row(&self, g: &GoldHealth) -> String {
        let b = |v: bool| if v { "1" } else { "0" }.to_string();
        let (off, m2, rad) = self.nav_cols();
        let mut cols = vec![
            csv_quote(&self.name),
            "goldsrc".to_string(),
            match &self.nav {
                Err(e) => csv_quote(&format!("walk grid: {e}")),
                _ => String::new(),
            },
            self.warnings().len().to_string(),
            g.vis_bytes.to_string(),
            g.light_bytes.to_string(),
            g.unlit.to_string(),
            g.lightable.to_string(),
            self.t.len().to_string(),
            self.ct.len().to_string(),
            off,
        ];
        cols.extend(self.objectives.iter().map(|(_, n)| n.to_string()));
        cols.extend([
            self.buyzones.to_string(),
            g.missing_wads.len().to_string(),
            g.missing_textures.len().to_string(),
            b(!self.sky_found),
            g.missing_files.len().to_string(),
            b(g.ov[0].is_some()),
            b(g.ov[1].is_some()),
            b(g.ov[2].is_some()),
            match &g.bmp {
                Some(Ok(x)) => x.n255.to_string(),
                Some(Err(_)) => "?".into(),
                None => String::new(),
            },
            b(g.res.is_some()),
            b(g.res_lists[0]),
            b(g.res_lists[1]),
            b(g.res_lists[2]),
            m2,
            rad,
        ]);
        for l in &g.limits {
            cols.push(l.count.to_string());
            cols.push(format!("{:.1}", l.pct()));
        }
        cols.extend(std::iter::repeat_n(String::new(), CS2_COLS.len()));
        cols.join(",")
    }
}
