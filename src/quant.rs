use std::collections::HashMap;

struct Bx {
    colors: Vec<([u8; 3], u32)>,
    count: u64,
}

impl Bx {
    fn new(colors: Vec<([u8; 3], u32)>) -> Bx {
        let count = colors.iter().map(|c| c.1 as u64).sum();
        Bx { colors, count }
    }
    fn range(&self) -> (usize, u8) {
        let mut lo = [255u8; 3];
        let mut hi = [0u8; 3];
        for (c, _) in &self.colors {
            for k in 0..3 {
                lo[k] = lo[k].min(c[k]);
                hi[k] = hi[k].max(c[k]);
            }
        }
        (0..3).map(|k| (k, hi[k] - lo[k])).max_by_key(|&(_, r)| r).unwrap()
    }
    fn priority(&self) -> u64 {
        if self.colors.len() < 2 {
            return 0;
        }
        self.count * self.range().1 as u64
    }
    fn mean(&self) -> [u8; 3] {
        let mut s = [0u64; 3];
        for (c, n) in &self.colors {
            for k in 0..3 {
                s[k] += c[k] as u64 * *n as u64;
            }
        }
        let n = self.count.max(1);
        [((s[0] + n / 2) / n) as u8, ((s[1] + n / 2) / n) as u8, ((s[2] + n / 2) / n) as u8]
    }
}

pub fn nearest(pal: &[[i32; 3]], c: [i32; 3]) -> usize {
    let mut best = 0;
    let mut bd = i32::MAX;
    for (i, p) in pal.iter().enumerate() {
        let d = (p[0] - c[0]).pow(2) + (p[1] - c[1]).pow(2) + (p[2] - c[2]).pow(2);
        if d < bd {
            bd = d;
            best = i;
            if d == 0 {
                break;
            }
        }
    }
    best
}

pub fn median_cut(hist: HashMap<[u8; 3], u32>, n: usize) -> Vec<[u8; 3]> {
    let mut boxes = vec![Bx::new(hist.into_iter().collect())];
    while boxes.len() < n {
        let (bi, pr) = boxes.iter().enumerate().map(|(i, b)| (i, b.priority())).max_by_key(|x| x.1).unwrap();
        if pr == 0 {
            break;
        }
        let b = boxes.swap_remove(bi);
        let (axis, _) = b.range();
        let mut cols = b.colors;
        cols.sort_by_key(|(c, _)| c[axis]);
        let half = b.count / 2;
        let mut acc = 0u64;
        let mut cut = 1;
        for (i, (_, k)) in cols.iter().enumerate() {
            acc += *k as u64;
            if acc >= half {
                cut = (i + 1).clamp(1, cols.len() - 1);
                break;
            }
        }
        let right = cols.split_off(cut);
        boxes.push(Bx::new(cols));
        boxes.push(Bx::new(right));
    }
    boxes.iter().map(Bx::mean).collect()
}

pub fn quantize(img: &image::RgbaImage, n: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut hist: HashMap<[u8; 3], u32> = HashMap::new();
    for p in img.pixels() {
        if p[3] >= 128 {
            *hist.entry([p[0], p[1], p[2]]).or_default() += 1;
        }
    }
    let mut idx = vec![0u8; w * h];
    if hist.len() <= n {
        let pal: Vec<[u8; 3]> = hist.keys().copied().collect();
        let lookup: HashMap<[u8; 3], u8> = pal.iter().enumerate().map(|(i, c)| (*c, i as u8)).collect();
        for (i, p) in img.pixels().enumerate() {
            idx[i] = lookup.get(&[p[0], p[1], p[2]]).copied().unwrap_or(0);
        }
        return (pal, idx);
    }

    let pal = median_cut(hist, n);
    let pal_i: Vec<[i32; 3]> = pal.iter().map(|c| [c[0] as i32, c[1] as i32, c[2] as i32]).collect();

    let mut err = vec![[0i32; 3]; (w + 2) * 2];
    for y in 0..h {
        let (cur, next) = if y % 2 == 0 { (0, w + 2) } else { (w + 2, 0) };
        for e in &mut err[next..next + w + 2] {
            *e = [0; 3];
        }
        for x in 0..w {
            let p = img.get_pixel(x as u32, y as u32);
            if p[3] < 128 {
                continue;
            }
            let e = err[cur + x + 1];
            let c = [
                (p[0] as i32 + e[0] / 16).clamp(0, 255),
                (p[1] as i32 + e[1] / 16).clamp(0, 255),
                (p[2] as i32 + e[2] / 16).clamp(0, 255),
            ];
            let k = nearest(&pal_i, c);
            idx[y * w + x] = k as u8;
            let d = [c[0] - pal_i[k][0], c[1] - pal_i[k][1], c[2] - pal_i[k][2]];
            for ch in 0..3 {
                err[cur + x + 2][ch] += d[ch] * 7;
                err[next + x][ch] += d[ch] * 3;
                err[next + x + 1][ch] += d[ch] * 5;
                err[next + x + 2][ch] += d[ch];
            }
        }
    }
    (pal, idx)
}
