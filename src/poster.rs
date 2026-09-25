use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glam::DVec3;
use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::Bsp;
use crate::camera::{Camera, Framing, ISO_PITCH, camera_basis, extents, top_down};
use crate::grid::{Text, entity_marks, nice_step, paint};
use crate::look::Look;
use crate::paths::{Partial, free_name};
use crate::render::{Crop, Cuts, Renderer, Targets, View, composite_bg, downsample, unpremultiply};
use crate::scene::Report;

pub const PAPERS: [(&str, f64, f64); 7] = [
    ("a0", 841.0, 1189.0),
    ("a1", 594.0, 841.0),
    ("a2", 420.0, 594.0),
    ("a3", 297.0, 420.0),
    ("a4", 210.0, 297.0),
    ("letter", 215.9, 279.4),
    ("tabloid", 279.4, 431.8),
];
const TILE_MAX: u32 = 4096;
const SHADOW_RES: u32 = 8192;
const LANCZOS_PAD: u32 = 4;
const MARGIN_MM: f64 = 12.0;
const STRIP_MM: f64 = 10.0;
const TITLE_MM: f64 = 30.0;
const GAP_MM: f64 = 5.0;
const PAD_MM: f64 = 4.0;
const PAGE: [u8; 3] = [255, 255, 255];
const INK: [u8; 3] = [28, 28, 28];
const MUTED: [u8; 3] = [110, 110, 110];
const INCH_M: f64 = 0.0254;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Orient {
    Auto,
    Portrait,
    Landscape,
}

impl Orient {
    pub const ALL: [Orient; 3] = [Orient::Auto, Orient::Portrait, Orient::Landscape];

    pub fn key(self) -> &'static str {
        match self {
            Orient::Auto => "auto",
            Orient::Portrait => "portrait",
            Orient::Landscape => "landscape",
        }
    }

    pub fn parse(s: &str) -> Option<Orient> {
        Orient::ALL.into_iter().find(|o| o.key() == s.trim())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PosterOpts {
    pub paper: String,
    pub dpi: f64,
    pub orient: Orient,
    pub px: Option<[u32; 2]>,
    pub top: bool,
    pub yaw: f64,
    pub pitch: f64,
    pub ss: u32,
    pub layout: bool,
    pub look: Look,
    pub framing: Framing,
    pub tile: Option<u32>,
}

impl Default for PosterOpts {
    fn default() -> Self {
        PosterOpts {
            paper: "a2".into(),
            dpi: 300.0,
            orient: Orient::Auto,
            px: None,
            top: false,
            yaw: 45.0,
            pitch: ISO_PITCH,
            ss: 3,
            layout: true,
            look: Look::default(),
            framing: Framing::default(),
            tile: None,
        }
    }
}

pub fn paper_mm(name: &str) -> Option<(f64, f64)> {
    PAPERS.iter().find(|p| p.0.eq_ignore_ascii_case(name.trim())).map(|p| (p.1, p.2))
}

impl PosterOpts {
    fn mm(&self, v: f64) -> f64 {
        v * self.dpi / 25.4
    }

    fn pt(&self, v: f64) -> f32 {
        (v * self.dpi / 72.0) as f32
    }

    pub fn page(&self, aspect: f64) -> Result<(u32, u32)> {
        if let Some([w, h]) = self.px {
            return Ok((w.max(64), h.max(64)));
        }
        let (a, b) = paper_mm(&self.paper).with_context(|| format!("unknown paper: {}", self.paper))?;
        let land = match self.orient {
            Orient::Auto => aspect > 1.0,
            o => o == Orient::Landscape,
        };
        let (w, h) = if land { (b, a) } else { (a, b) };
        Ok((self.mm(w).round() as u32, self.mm(h).round() as u32))
    }

    fn tag(&self) -> String {
        let size = match self.px {
            Some([w, h]) => format!("{w}x{h}"),
            None => self.paper.to_lowercase(),
        };
        let kind = if self.framing.camera.is_some() {
            String::new()
        } else if self.top {
            "_top".into()
        } else {
            format!("_{:03}", (self.yaw as i64).rem_euclid(360))
        };
        format!("_poster_{size}{kind}")
    }
}

struct Framed {
    view: View,
    w: u32,
    h: u32,
}

impl Framed {
    fn upp(&self) -> Option<f64> {
        self.view.persp.is_none().then(|| self.view.w / self.w as f64)
    }

    fn project(&self, p: DVec3) -> Option<(f64, f64)> {
        let v = &self.view;
        let b = &v.basis;
        let (w, h) = (self.w as f64, self.h as f64);
        match v.persp {
            None => Some(((p.dot(b.r) - (v.cx - v.w / 2.0)) / v.w * w, ((v.cy + v.h / 2.0) - p.dot(b.u)) / v.h * h)),
            Some(pp) => {
                let d = p - pp.eye;
                let z = d.dot(b.f);
                if z < 1.0 {
                    return None;
                }
                let t = (pp.fov_y.to_radians() / 2.0).tan();
                let nx = d.dot(b.r) / (z * t * w / h);
                let ny = d.dot(b.u) / (z * t);
                Some(((nx + 1.0) / 2.0 * w, (1.0 - ny) / 2.0 * h))
            }
        }
    }
}

fn shape_aspect(pts: &[DVec3], o: &PosterOpts) -> f64 {
    if let Some(c) = &o.framing.camera {
        return c.aspect;
    }
    let b = if o.top { top_down(DVec3::X, DVec3::Y) } else { camera_basis(o.yaw, o.pitch) };
    let e = extents(pts, &b);
    (e[0].1 - e[0].0) / (e[1].1 - e[1].0).max(1.0)
}

fn frame_view(pts: &[DVec3], o: &PosterOpts, w: u32, h: u32, pad: f64) -> View {
    if let Some(c) = &o.framing.camera {
        return c.view(w, h);
    }
    let (yaw, pitch) = if o.top { (90.0, 90.0) } else { (o.yaw, o.pitch) };
    if let Some(fov) = o.framing.persp {
        let mut c = Camera { yaw, pitch, fov, ortho: false, ..Camera::default() };
        c.frame_points(pts, w as f64 / h as f64, 1.0 - 2.0 * pad / w.min(h) as f64);
        return c.view(w, h);
    }
    let b = if o.top { top_down(DVec3::X, DVec3::Y) } else { camera_basis(yaw, pitch) };
    let e = extents(pts, &b);
    let upp = ((e[0].1 - e[0].0) / (w as f64 - 2.0 * pad).max(1.0)).max((e[1].1 - e[1].0) / (h as f64 - 2.0 * pad).max(1.0));
    View {
        basis: b,
        cx: (e[0].0 + e[0].1) / 2.0,
        cy: (e[1].0 + e[1].1) / 2.0,
        w: w as f64 * upp,
        h: h as f64 * upp,
        sky_yaw: (!o.top).then_some(yaw),
        persp: None,
    }
}

fn up4(v: u32) -> u32 {
    v.div_ceil(4) * 4
}

fn min_upp(fr: &Framed, pts: &[DVec3], ss: u32) -> f64 {
    let px = (fr.w * ss) as f64;
    match fr.view.persp {
        None => fr.view.w / px,
        Some(p) => {
            let z = pts.iter().map(|q| (*q - p.eye).dot(fr.view.basis.f)).fold(f64::INFINITY, f64::min).max(4.0);
            z * 2.0 * (p.fov_y.to_radians() / 2.0).tan() / (fr.h * ss) as f64
        }
    }
}

enum Item {
    Line((f32, f32), (f32, f32), [u8; 3], f32),
    Rect([f32; 4], [u8; 3], u8),
    Outline([f32; 4], [u8; 3], f32),
    Circle(f32, f32, f32, [u8; 3], f32),
    Text(usize, f32, f32, String, [u8; 3]),
}

struct Deco {
    fonts: Vec<Text>,
    items: Vec<Item>,
}

impl Deco {
    fn font(&mut self, px: f32) -> usize {
        self.fonts.push(Text::new(px));
        self.fonts.len() - 1
    }

    fn size(&self, f: usize, s: &str) -> (f32, f32) {
        self.fonts[f].size(s)
    }

    fn draw(&self, pm: &mut Pixmap, y0: f32) {
        let hb = pm.height() as f32;
        let vis = |a: f32, b: f32| b >= y0 - 4.0 && a <= y0 + hb + 4.0;
        let sh = Transform::from_translate(0.0, -y0);
        for it in &self.items {
            match it {
                Item::Line(a, b, c, w) => {
                    if !vis(a.1.min(b.1) - w, a.1.max(b.1) + w) {
                        continue;
                    }
                    let mut pb = PathBuilder::new();
                    pb.move_to(a.0, a.1);
                    pb.line_to(b.0, b.1);
                    if let Some(p) = pb.finish() {
                        pm.stroke_path(&p, &paint(*c, 255), &Stroke { width: *w, ..Default::default() }, sh, None);
                    }
                }
                Item::Rect([x0, ya, x1, yb], c, a) => {
                    if !vis(*ya, *yb) {
                        continue;
                    }
                    if let Some(r) = tiny_skia::Rect::from_ltrb(*x0, *ya, *x1, *yb) {
                        pm.fill_rect(r, &paint(*c, *a), sh, None);
                    }
                }
                Item::Outline([x0, ya, x1, yb], c, w) => {
                    if !vis(ya - w, yb + w) {
                        continue;
                    }
                    if let Some(r) = tiny_skia::Rect::from_ltrb(*x0, *ya, *x1, *yb) {
                        let p = PathBuilder::from_rect(r);
                        pm.stroke_path(&p, &paint(*c, 255), &Stroke { width: *w, ..Default::default() }, sh, None);
                    }
                }
                Item::Circle(x, y, r, c, sw) => {
                    if !vis(y - r - sw, y + r + sw) {
                        continue;
                    }
                    if let Some(p) = PathBuilder::from_circle(*x, *y, *r) {
                        pm.fill_path(&p, &paint(*c, 255), FillRule::Winding, sh, None);
                        pm.stroke_path(&p, &paint(INK, 255), &Stroke { width: *sw, ..Default::default() }, sh, None);
                    }
                }
                Item::Text(f, x, y, s, c) => {
                    let (_, th) = self.fonts[*f].size(s);
                    if !vis(*y, y + th * 1.3) {
                        continue;
                    }
                    self.fonts[*f].draw(pm, *x, y - y0, s, *c);
                }
            }
        }
    }
}

fn legend_name(label: &str) -> &'static str {
    match label {
        "CT" => "CT spawn",
        "T" => "T spawn",
        "H" => "Hostage",
        "VIP" => "VIP start",
        "BOMB" => "Bomb site",
        "RESCUE" => "Rescue zone",
        "ESCAPE" => "VIP escape",
        _ => "",
    }
}

fn nice_len(max: f64) -> f64 {
    let mut best = 0.0;
    let mut p = 1e-3;
    while p <= max * 10.0 {
        for m in [1.0, 2.0, 5.0] {
            if m * p <= max {
                best = m * p;
            }
        }
        p *= 10.0;
    }
    best
}

fn fmt_len(m: f64) -> String {
    if m >= 1.0 { format!("{} m", crate::scene::num(m)) } else { format!("{} cm", crate::scene::num((m * 100.0).round())) }
}

pub fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let z = (secs / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

#[allow(clippy::too_many_arguments)]
fn decorate(bsp: &Bsp, name: &str, cuts: &Cuts, o: &PosterOpts, fr: &Framed, fx: u32, fy: u32, pw: u32, ph: u32) -> Deco {
    let mut d = Deco { fonts: Vec::new(), items: Vec::new() };
    let mm = |v: f64| o.mm(v) as f32;
    let (fx, fy) = (fx as f32, fy as f32);
    let (fx1, fy1) = (fx + fr.w as f32, fy + fr.h as f32);
    let top = o.top && o.framing.camera.is_none() && fr.upp().is_some();
    let label = d.font(o.pt(9.0));
    let [cx0, cy0, cx1, cy1] = cuts.clip;
    let marks: Vec<_> = entity_marks(bsp)
        .into_iter()
        .filter(|m| m.pos.x >= cx0 && m.pos.x <= cx1 && m.pos.y >= cy0 && m.pos.y <= cy1)
        .filter(|m| m.bbox.is_some() || (m.pos.z >= cuts.zmin && m.pos.z <= cuts.zmax))
        .collect();
    let mut shown: Vec<(&'static str, [u8; 3])> = Vec::new();
    let inside = |x: f32, y: f32| x >= fx && x <= fx1 && y >= fy && y <= fy1;
    for m in &marks {
        let Some((x, y)) = fr.project(m.pos) else { continue };
        let (x, y) = (fx + x as f32, fy + y as f32);
        if !inside(x, y) {
            continue;
        }
        if !shown.iter().any(|s| s.0 == m.label) {
            shown.push((m.label, m.color));
        }
        match m.bbox {
            Some(b) if top => {
                let (Some(a), Some(c)) = (fr.project(DVec3::new(b[0], b[3], 0.0)), fr.project(DVec3::new(b[2], b[1], 0.0))) else {
                    continue;
                };
                let r = [fx + a.0 as f32, fy + a.1 as f32, fx + c.0 as f32, fy + c.1 as f32];
                d.items.push(Item::Rect(r, m.color, 60));
                d.items.push(Item::Outline(r, m.color, mm(0.5)));
                let (tw, th) = d.size(label, m.label);
                d.items.push(Item::Rect([x - tw / 2.0 - mm(0.8), y - th / 2.0 - mm(0.4), x + tw / 2.0 + mm(0.8), y + th / 2.0 + mm(0.4)], INK, 190));
                d.items.push(Item::Text(label, x - tw / 2.0, y - th / 2.0, m.label.into(), m.color));
            }
            Some(_) => {
                d.items.push(Item::Circle(x, y, mm(1.6), m.color, mm(0.3)));
                let (tw, th) = d.size(label, m.label);
                let lx = x + mm(2.4);
                d.items.push(Item::Rect([lx - mm(0.6), y - th / 2.0 - mm(0.3), lx + tw + mm(0.6), y + th / 2.0 + mm(0.3)], INK, 190));
                d.items.push(Item::Text(label, lx, y - th / 2.0, m.label.into(), m.color));
            }
            None => d.items.push(Item::Circle(x, y, mm(1.0), m.color, mm(0.25))),
        }
    }
    if !o.layout {
        return d;
    }
    d.items.push(Item::Outline([fx, fy, fx1, fy1], INK, mm(0.35)));
    if let (true, Some(upp)) = (top, fr.upp()) {
        let v = &fr.view;
        let (wx0, wy1) = (v.cx - v.w / 2.0, v.cy + v.h / 2.0);
        let (wx1, wy0) = (wx0 + v.w, wy1 - v.h);
        let step = nice_step((wx1 - wx0).max(wy1 - wy0));
        let tick = d.font(o.pt(7.0));
        let minor = step / 4.0;
        let mut x = (wx0 / minor).ceil() * minor;
        while x <= wx1 {
            let px = fx + ((x - wx0) / upp) as f32;
            let major = (x / step).round() * step == x;
            let len = if major { mm(2.0) } else { mm(1.0) };
            d.items.push(Item::Line((px, fy), (px, fy - len), INK, mm(0.2)));
            d.items.push(Item::Line((px, fy1), (px, fy1 + len), INK, mm(0.2)));
            if major {
                let s = format!("{x:.0}");
                let (tw, th) = d.size(tick, &s);
                d.items.push(Item::Text(tick, px - tw / 2.0, fy - len - th - mm(0.5), s.clone(), MUTED));
                d.items.push(Item::Text(tick, px - tw / 2.0, fy1 + len + mm(0.5), s, MUTED));
            }
            x += minor;
        }
        let mut y = (wy0 / minor).ceil() * minor;
        while y <= wy1 {
            let py = fy + ((wy1 - y) / upp) as f32;
            let major = (y / step).round() * step == y;
            let len = if major { mm(2.0) } else { mm(1.0) };
            d.items.push(Item::Line((fx, py), (fx - len, py), INK, mm(0.2)));
            d.items.push(Item::Line((fx1, py), (fx1 + len, py), INK, mm(0.2)));
            if major {
                let s = format!("{y:.0}");
                let (tw, th) = d.size(tick, &s);
                d.items.push(Item::Text(tick, fx - len - mm(0.5) - tw, py - th / 2.0, s.clone(), MUTED));
                d.items.push(Item::Text(tick, fx1 + len + mm(0.5), py - th / 2.0, s, MUTED));
            }
            y += minor;
        }
    }
    let m = mm(MARGIN_MM);
    let (pw, ph) = (pw as f32, ph as f32);
    let ty = ph - m - mm(TITLE_MM);
    d.items.push(Item::Line((m, ty), (pw - m, ty), INK, mm(0.3)));
    let big = d.font(o.pt(30.0));
    let (_, bh) = d.size(big, name);
    d.items.push(Item::Text(big, m, ty + mm(2.5), name.into(), INK));
    let msg = bsp.worldspawn().get("message").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let sub = d.font(o.pt(12.0));
    if let Some(msg) = msg {
        d.items.push(Item::Text(sub, m, ty + mm(3.5) + bh, msg, MUTED));
    }
    let small = d.font(o.pt(8.0));
    let view = match (&o.framing.camera, o.framing.persp) {
        (Some(_), _) => "camera".to_string(),
        (None, Some(f)) if o.top => format!("top-down, perspective {f:.0}\u{b0}"),
        (None, Some(f)) => format!("yaw {:.0}\u{b0}, pitch {:.1}\u{b0}, perspective {f:.0}\u{b0}", o.yaw, o.pitch),
        (None, None) if o.top => "top-down".to_string(),
        (None, None) => format!("isometric, yaw {:.0}\u{b0}, pitch {:.1}\u{b0}", o.yaw, o.pitch),
    };
    let mut info = vec![today(), view];
    if let Some(upp) = fr.upp() {
        info.push(format!("scale 1:{:.0} at {:.0} dpi", upp * o.dpi, o.dpi));
    }
    let info = info.join("   ");
    let (_, ih) = d.size(small, &info);
    d.items.push(Item::Text(small, m, ph - m - ih - mm(1.0), info, MUTED));

    let sw = mm(3.0);
    let rows = 3usize;
    let cols: Vec<&[(&str, [u8; 3])]> = shown.chunks(rows).collect();
    let col_w: Vec<f32> = cols.iter().map(|c| c.iter().map(|e| d.size(label, legend_name(e.0)).0).fold(0.0, f32::max) + sw + mm(6.0)).collect();
    let mut lx = pw - m - col_w.iter().sum::<f32>() + mm(4.0);
    let legend_x = lx;
    for (c, w) in cols.iter().zip(&col_w) {
        for (i, (l, color)) in c.iter().enumerate() {
            let y = ty + mm(4.0) + i as f32 * mm(6.5);
            d.items.push(Item::Rect([lx, y, lx + sw, y + sw], *color, 255));
            d.items.push(Item::Outline([lx, y, lx + sw, y + sw], INK, mm(0.2)));
            let (_, th) = d.size(label, legend_name(l));
            d.items.push(Item::Text(label, lx + sw + mm(1.5), y + sw / 2.0 - th / 2.0, legend_name(l).into(), INK));
        }
        lx += w;
    }
    if let Some(upp) = fr.upp() {
        let px_m = upp * INCH_M;
        let len = nice_len(o.mm(60.0) * px_m);
        if len > 0.0 {
            let bl = (len / px_m) as f32;
            let bx = (pw / 2.0 - bl / 2.0).min(legend_x - mm(10.0) - bl).max(m + mm(70.0));
            let by = ty + mm(8.0);
            let bhh = mm(1.6);
            for k in 0..4 {
                let x0 = bx + bl * k as f32 / 4.0;
                let c = if k % 2 == 0 { INK } else { PAGE };
                d.items.push(Item::Rect([x0, by, x0 + bl / 4.0, by + bhh], c, 255));
            }
            d.items.push(Item::Outline([bx, by, bx + bl, by + bhh], INK, mm(0.2)));
            let s0 = "0".to_string();
            let s1 = fmt_len(len);
            let (w1, _) = d.size(small, &s1);
            d.items.push(Item::Text(small, bx - mm(0.6), by + bhh + mm(1.0), s0, INK));
            d.items.push(Item::Text(small, bx + bl - w1 / 2.0, by + bhh + mm(1.0), s1, INK));
            let note = "1 unit = 1 inch";
            let (nw, _) = d.size(small, note);
            d.items.push(Item::Text(small, bx + bl / 2.0 - nw / 2.0, by - mm(4.5), note.into(), MUTED));
        }
    }
    d
}

struct Plan {
    t: u32,
    m: u32,
    cols: u32,
    rows: u32,
}

fn plan(r: &Renderer, fr: &Framed, pts: &[DVec3], o: &PosterOpts) -> Result<Plan> {
    let ss = o.ss.max(1);
    let cap = TILE_MAX.min(r.gpu.max_dim);
    let reach = if crate::post::needed(&o.look) { crate::post::reach(&o.look, min_upp(fr, pts, ss), ss) } else { 0.0 };
    let reach = reach.min(cap as f64 / 4.0);
    let m = up4((reach / ss as f64).ceil() as u32 + LANCZOS_PAD);
    let single = fr.w * ss <= cap && fr.h * ss <= cap;
    let t = match o.tile {
        Some(t) => up4(t.max(16)),
        None if single => fr.w.max(fr.h),
        None => (cap / ss).saturating_sub(2 * m) / 4 * 4,
    };
    if t < 64 {
        bail!("post effects need a {m} px tile margin at ss {ss}; lower --ss or --ao-radius");
    }
    let cols = fr.w.div_ceil(t);
    let rows = fr.h.div_ceil(t);
    if cols * rows > 1 && (t + 2 * m) * ss > r.gpu.max_dim {
        bail!("tile {}x{} exceeds GPU max {}", (t + 2 * m) * ss, (t + 2 * m) * ss, r.gpu.max_dim);
    }
    Ok(Plan { t, m, cols, rows })
}

struct TileCtx<'a> {
    r: &'a Renderer,
    fr: &'a Framed,
    cuts: &'a Cuts,
    look: &'a Look,
    ss: u32,
    targets: Option<Targets>,
}

impl TileCtx<'_> {
    fn render(&mut self, p: &Plan, col: u32, row: u32) -> Result<(u32, u32, u32, u32, Vec<u8>)> {
        let (fw, fh, ss) = (self.fr.w, self.fr.h, self.ss);
        let (x0, y0) = (col * p.t, row * p.t);
        let (x1, y1) = ((x0 + p.t).min(fw), (y0 + p.t).min(fh));
        let (rx0, ry0) = (x0.saturating_sub(p.m), y0.saturating_sub(p.m));
        let (rx1, ry1) = ((x1 + p.m).min(fw), (y1 + p.m).min(fh));
        let (rw, rh) = (rx1 - rx0, ry1 - ry0);
        let (tw, th) = (rw * ss, rh * ss);
        if self.targets.as_ref().is_none_or(|t| t.w != tw || t.h != th) {
            self.targets = None;
            self.targets = Some(self.r.make_targets(tw, th, crate::post::needed(self.look)));
        }
        let t = self.targets.as_ref().unwrap();
        let crop = Crop { x: rx0 * ss, y: ry0 * ss, w: fw * ss, h: fh * ss };
        let px = self.r.render_crop(t, &self.fr.view, Some(crop), ss, self.cuts, self.look, 0.0)?;
        let mut px = if ss > 1 { downsample(&px, tw, th, rw, rh)? } else { px };
        unpremultiply(&mut px);
        composite_bg(&mut px, self.look.bg.unwrap_or(PAGE));
        let (cw, ch) = (x1 - x0, y1 - y0);
        let (ox, oy) = (x0 - rx0, y0 - ry0);
        let mut out = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            let s = (((y + oy) * rw + ox) * 4) as usize;
            out[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize].copy_from_slice(&px[s..s + (cw * 4) as usize]);
        }
        Ok((x0, y0, cw, ch, out))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn export_poster(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    sky_tag: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &PosterOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<PathBuf> {
    rep.step(0.0)?;
    let pts = r.points_in(cuts);
    let (pw, ph) = o.page(shape_aspect(&pts, o))?;
    let strip = if o.layout && o.top && o.framing.camera.is_none() && o.framing.persp.is_none() { o.mm(STRIP_MM) } else { 0.0 };
    let (fx, fy, fw, fh) = if o.layout {
        let e = (o.mm(MARGIN_MM) + strip).round() as u32;
        let bottom = (o.mm(MARGIN_MM + TITLE_MM + GAP_MM) + strip).round() as u32;
        (e, e, pw.saturating_sub(2 * e), ph.saturating_sub(e + bottom))
    } else {
        (0, 0, pw, ph)
    };
    if fw < 64 || fh < 64 {
        bail!("page {pw}x{ph} leaves no room for the map; raise --dpi or the page size");
    }
    let fr = Framed { view: frame_view(&pts, o, fw, fh, o.mm(PAD_MM)), w: fw, h: fh };
    let p = plan(r, &fr, &pts, o)?;
    let deco = decorate(bsp, name, cuts, o, &fr, fx, fy, pw, ph);
    rep.log(format!(
        "  page {pw}x{ph}, map {fw}x{fh}, {}x{} tiles of {} px (+{} margin)",
        p.cols, p.rows, p.t, p.m
    ));

    let f = free_name(out, &format!("{name}{sky_tag}{cut_tag}{}{}", o.framing.tag(), o.tag()), ".png");
    let mut files = Partial::default();
    files.add(f.clone());
    let file = BufWriter::with_capacity(1 << 20, File::create(&f).with_context(|| format!("creating {}", f.display()))?);
    let mut enc = png::Encoder::new(file, pw, ph);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let ppm = (o.dpi / INCH_M).round() as u32;
    enc.set_pixel_dims(Some(png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter }));
    let mut sw = enc.write_header()?.into_stream_writer_with_size(1 << 20)?;

    let mut look = o.look.clone();
    if look.shadow_res == 0 {
        look.shadow_res = SHADOW_RES;
    }
    let mut ctx = TileCtx { r, fr: &fr, cuts, look: &look, ss: o.ss.max(1), targets: None };
    let total = (p.cols * p.rows) as f32;
    let mut bands = vec![(0, fy, None)];
    for row in 0..p.rows {
        let y0 = fy + row * p.t;
        bands.push((y0, fy + ((row + 1) * p.t).min(fh), Some(row)));
    }
    bands.push((fy + fh, ph, None));
    let mut rgb = vec![0u8; pw as usize * 3];
    let mut done = 0;
    for (b0, b1, row) in bands {
        if b1 <= b0 {
            continue;
        }
        let mut pm = Pixmap::new(pw, b1 - b0).context("band pixmap")?;
        pm.fill(tiny_skia::Color::from_rgba8(PAGE[0], PAGE[1], PAGE[2], 255));
        if let Some(row) = row {
            for col in 0..p.cols {
                let (x0, _, cw, ch, px) = ctx.render(&p, col, row)?;
                let data = pm.data_mut();
                for y in 0..ch {
                    let d = ((y * pw + fx + x0) * 4) as usize;
                    data[d..d + (cw * 4) as usize].copy_from_slice(&px[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize]);
                }
                done += 1;
                rep.step(done as f32 / total * 0.98)?;
            }
        }
        deco.draw(&mut pm, b0 as f32);
        for line in pm.data().chunks_exact(pw as usize * 4) {
            for (o3, i4) in rgb.chunks_exact_mut(3).zip(line.chunks_exact(4)) {
                o3.copy_from_slice(&i4[..3]);
            }
            sw.write_all(&rgb)?;
        }
    }
    sw.finish()?;
    files.keep();
    let _ = rep.step(1.0);
    rep.log(format!("  {} {}x{}", f.display(), pw, ph));
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2_pixels() {
        let o = PosterOpts::default();
        assert_eq!(o.page(1.5).unwrap(), (7016, 4961));
        assert_eq!(o.page(0.5).unwrap(), (4961, 7016));
    }

    #[test]
    fn orientation() {
        let o = PosterOpts { orient: Orient::Landscape, ..PosterOpts::default() };
        assert_eq!(o.page(0.5).unwrap(), (7016, 4961));
        assert_eq!(nice_len(7.3), 5.0);
        assert_eq!(today().len(), 10);
    }
}
