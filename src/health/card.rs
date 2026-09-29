use super::*;

pub(super) fn card(r: &mut Renderer, h: &Health, cuts: &Cuts, size: u32) -> Result<image::RgbImage> {
    let g = grid_frame(r, cuts, size);
    let view = View {
        basis: top_down(DVec3::X, DVec3::Y),
        cx: (g.x0 + g.x1) / 2.0,
        cy: (g.y0 + g.y1) / 2.0,
        w: g.x1 - g.x0,
        h: g.y1 - g.y0,
        sky_yaw: None,
        persp: None,
    };
    let rc = Cuts { clip: NO_CLIP, use_mask: false, ..*cuts };
    let thumb = r.render_view(&view, g.wpx, g.hpx, 2, &rc, &Look::plain(true, Some([0x1c, 0x1c, 0x1c])), 0.0)?;
    let title = Text::new(18.0);
    let text = Text::new(13.0);
    let pw = 560u32;
    let row = 18.0f32;
    let facts = card_facts(h);
    let limits: Vec<(&str, usize, usize, bool)> = match &h.engine {
        Engine::Gold(g) => g.limits.iter().map(|l| (l.label, l.count, l.cap(), l.over_hlsdk())).collect(),
        Engine::Cs2(_) => Vec::new(),
        Engine::Source(s) => s.limits.iter().filter_map(|l| Some((l.label, l.count, l.cap?, false))).collect(),
    };
    let panel_h = 44.0 + row * (limits.len() as f32 + 1.5) + 16.0 * (facts.len() as f32 + 1.0) + 12.0;
    let (w, ht) = (g.wpx + pw, g.hpx.max(panel_h.ceil() as u32).max(size / 2));
    let mut pm = Pixmap::new(w, ht).context("pixmap")?;
    pm.fill(tiny_skia::Color::from_rgba8(0x1c, 0x1c, 0x1c, 255));
    {
        let data = pm.data_mut();
        for y in 0..g.hpx as usize {
            let src = &thumb.as_raw()[y * g.wpx as usize * 4..(y + 1) * g.wpx as usize * 4];
            data[y * w as usize * 4..y * w as usize * 4 + src.len()].copy_from_slice(src);
        }
    }
    let sx = |x: f64| ((x - g.x0) / g.upp) as f32;
    let sy = |y: f64| ((g.y1 - y) / g.upp) as f32;
    let mut off: Vec<DVec3> = h.nav.as_ref().map(|n| n.off_ground.iter().map(|(_, p)| *p).collect()).unwrap_or_default();
    if let Some(c) = h.modern() {
        off.extend(c.stuck.iter().flatten().map(|s| s.1));
    }
    for (pts, col) in [(&h.t, T), (&h.ct, CT)] {
        for p in pts {
            if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 3.0) {
                pm.fill_path(&c, &paint(col, 255), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&c, &paint([0, 0, 0], 255), &Stroke::default(), Transform::identity(), None);
            }
            if off.contains(p) {
                if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 8.0) {
                    pm.stroke_path(&c, &paint(RED, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
                }
            }
        }
    }
    if let Ok(n) = &h.nav {
        for (i, s) in n.spots.iter().enumerate() {
            let (cx, cy, rr) = (sx(s.pos.x), sy(s.pos.y), (s.radius / g.upp) as f32);
            if let Some(c) = PathBuilder::from_circle(cx, cy, rr.max(2.0)) {
                pm.fill_path(&c, &paint([255, 230, 0], 40), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&c, &paint([255, 230, 0], 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
            let l = format!("{}", i + 1);
            let (tw, th) = text.size(&l);
            text.draw(&mut pm, cx - tw / 2.0, cy - th / 2.0, &l, [255, 230, 0]);
        }
    }
    if let Some(c) = h.modern()
        && let Ok(k) = &c.collision
    {
        for (p, _) in &k.clusters {
            if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 6.0) {
                pm.stroke_path(&c, &paint(HOLE, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
        }
    }

    let x0 = g.wpx as f32 + 16.0;
    let mut y = 12.0;
    title.draw(&mut pm, x0, y, &format!("{} health", h.name), [255, 255, 255]);
    y += 32.0;
    let (lw, bw) = (110.0, 170.0);
    for &(label, count, cap, over) in &limits {
        let pct = count as f64 * 100.0 / cap as f64;
        let col = level_color(pct);
        text.draw(&mut pm, x0, y, label, [220, 220, 220]);
        let bx = x0 + lw;
        rect(&mut pm, bx, y + 3.0, bx + bw, y + 14.0, [60, 60, 60], 255);
        rect(&mut pm, bx, y + 3.0, bx + bw * (pct / 100.0).min(1.0) as f32, y + 14.0, col, 255);
        let hl = if over { "  >HLSDK" } else { "" };
        let s = format!("{:>3.0}%  {} / {}{hl}", pct, count, cap);
        text.draw(&mut pm, bx + bw + 8.0, y, &s, col);
        y += row;
    }
    y += row * 0.5;
    for (s, col) in &facts {
        text.draw(&mut pm, x0, y, s, *col);
        y += 16.0;
    }
    line(&mut pm, (g.wpx as f32 + 0.5, 0.0), (g.wpx as f32 + 0.5, ht as f32), [70, 70, 70], 255, 1.0);
    let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    image::RgbImage::from_raw(w, ht, rgb).context("image")
}

pub(super) fn card_facts(h: &Health) -> Vec<(String, [u8; 3])> {
    match &h.engine {
        Engine::Gold(g) => gold_facts(h, g),
        Engine::Cs2(c) => cs2_facts(h, c),
        Engine::Source(s) => src_facts(h, s),
    }
}
