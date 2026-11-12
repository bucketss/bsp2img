use std::collections::HashMap;

use glam::{DMat4, DQuat, DVec3, DVec4, EulerRot};

const BONE_SIZE: usize = 216;
const SEQ_SIZE: usize = 212;
const ANIM_SIZE: usize = 100;
const RAWPOS: u8 = 0x01;
const RAWROT: u8 = 0x02;
const ANIMPOS: u8 = 0x04;
const ANIMROT: u8 = 0x08;
const RAWROT2: u8 = 0x20;
const SEQ_DELTA: i32 = 0x4;
const ANIM_DELTA: i32 = 0x4;
const FRAMEANIM: i32 = 0x40;
const MAX_INCLUDES: usize = 8;

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn i32(&self, o: usize) -> Option<i32> {
        Some(i32::from_le_bytes(self.0.get(o..o + 4)?.try_into().ok()?))
    }
    fn u(&self, o: usize) -> Option<usize> {
        usize::try_from(self.i32(o)?).ok()
    }
    fn f(&self, o: usize) -> Option<f64> {
        Some(f32::from_le_bytes(self.0.get(o..o + 4)?.try_into().ok()?) as f64)
    }
    fn v3(&self, o: usize) -> Option<DVec3> {
        Some(DVec3::new(self.f(o)?, self.f(o + 4)?, self.f(o + 8)?))
    }
    fn u16(&self, o: usize) -> Option<u16> {
        Some(u16::from_le_bytes(self.0.get(o..o + 2)?.try_into().ok()?))
    }
    fn i16(&self, o: usize) -> Option<i16> {
        self.u16(o).map(|v| v as i16)
    }
    fn str(&self, o: usize) -> Option<String> {
        let b = self.0.get(o..)?;
        let end = b.iter().position(|&c| c == 0)?;
        Some(String::from_utf8_lossy(&b[..end]).into_owned())
    }
    fn rel_str(&self, base: usize, field: usize) -> Option<String> {
        let off = self.i32(base + field)?;
        self.str((base as i64 + off as i64).try_into().ok()?)
    }
}

#[derive(Clone)]
pub struct Bone {
    pub name: String,
    pub parent: i32,
    pub pos: DVec3,
    pub quat: DQuat,
    pub rot: DVec3,
    pub pos_scale: DVec3,
    pub rot_scale: DVec3,
    pub pose_to_bone: DMat4,
}

pub fn bones(d: &[u8]) -> Option<Vec<Bone>> {
    let r = R(d);
    let (n, at) = (r.u(156)?, r.u(160)?);
    (n <= 512).then_some(())?;
    (0..n)
        .map(|i| {
            let o = at + i * BONE_SIZE;
            let m = |row: usize, col: usize| r.f(o + 96 + (row * 4 + col) * 4);
            let col = |c: usize| -> Option<DVec4> { Some(DVec4::new(m(0, c)?, m(1, c)?, m(2, c)?, if c == 3 { 1.0 } else { 0.0 })) };
            Some(Bone {
                name: r.rel_str(o, 0)?.to_lowercase(),
                parent: r.i32(o + 4)?,
                pos: r.v3(o + 32)?,
                quat: DQuat::from_xyzw(r.f(o + 44)?, r.f(o + 48)?, r.f(o + 52)?, r.f(o + 56)?),
                rot: r.v3(o + 60)?,
                pos_scale: r.v3(o + 72)?,
                rot_scale: r.v3(o + 84)?,
                pose_to_bone: DMat4::from_cols(col(0)?, col(1)?, col(2)?, col(3)?),
            })
        })
        .collect()
}

fn includes(d: &[u8]) -> Vec<String> {
    let r = R(d);
    let (Some(n), Some(at)) = (r.u(336), r.u(340)) else { return Vec::new() };
    (0..n.min(MAX_INCLUDES)).filter_map(|i| r.rel_str(at + i * 8, 4)).collect()
}

fn idle_anim(d: &[u8]) -> Option<usize> {
    let r = R(d);
    let (ns, si, na) = (r.u(188)?, r.u(192)?, r.u(180)?);
    let seqs: Vec<(String, String, usize)> = (0..ns.min(4096))
        .filter_map(|i| {
            let o = si + i * SEQ_SIZE;
            if r.i32(o + 12)? & SEQ_DELTA != 0 {
                return None;
            }
            let ai = r.u(o + 60)?;
            let anim = r.i16(o + ai)?.max(0) as usize;
            (anim < na).then_some(())?;
            Some((r.rel_str(o, 4)?.to_lowercase(), r.rel_str(o, 8).unwrap_or_default().to_uppercase(), anim))
        })
        .collect();
    let pick = |f: &dyn Fn(&(String, String, usize)) -> bool| seqs.iter().find(|s| f(s)).map(|s| s.2);
    pick(&|s| s.1 == "ACT_IDLE").or_else(|| pick(&|s| s.0.contains("idle"))).or_else(|| pick(&|s| s.0.starts_with("stand_")))
}

fn anim_data<'a>(d: &'a [u8], anim: usize, read: &dyn Fn(&str) -> Option<Vec<u8>>, ani: &'a mut Option<Vec<u8>>) -> Option<(&'a [u8], usize)> {
    let r = R(d);
    let o = r.u(184)? + anim * ANIM_SIZE;
    let flags = r.i32(o + 12)?;
    if flags & (ANIM_DELTA | FRAMEANIM) != 0 {
        return None;
    }
    let (block, index) = if r.i32(o + 84)? != 0 {
        let s = (o as i64 + r.i32(o + 80)? as i64) as usize;
        (r.u(s)?, r.i32(s + 4)?)
    } else {
        (r.u(o + 52)?, r.i32(o + 56)?)
    };
    if block == 0 {
        return Some((d, (o as i64 + index as i64).try_into().ok()?));
    }
    let name = r.str(r.u(348)?)?;
    let start = r.u(r.u(356)? + block * 8)?;
    *ani = Some(read(&name.replace('\\', "/").to_lowercase())?);
    Some((ani.as_deref()?, start + usize::try_from(index).ok()?))
}

fn f16(h: u16) -> f64 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f64;
    s * match e {
        0 => m * 2f64.powi(-24),
        31 => 0.0,
        _ => (1.0 + m / 1024.0) * 2f64.powi(e - 15),
    }
}

fn value0(r: &R, ptr: usize) -> Option<DVec3> {
    let mut v = [0.0; 3];
    for (k, x) in v.iter_mut().enumerate() {
        let off = r.i16(ptr + k * 2)?;
        if off <= 0 {
            continue;
        }
        let p = ptr + off as usize;
        *x = if r.0.get(p)? > &0 { r.i16(p + 2)? } else { r.i16(p)? } as f64;
    }
    Some(DVec3::from(v))
}

fn quat48(r: &R, o: usize) -> Option<DQuat> {
    let (a, b, c) = (r.u16(o)?, r.u16(o + 2)?, r.u16(o + 4)?);
    let x = (a as f64 - 32768.0) / 32768.0;
    let y = (b as f64 - 32768.0) / 32768.0;
    let z = ((c & 0x7fff) as f64 - 16384.0) / 16384.0;
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    Some(DQuat::from_xyzw(x, y, z, if c & 0x8000 != 0 { -w } else { w }))
}

fn quat64(r: &R, o: usize) -> Option<DQuat> {
    let v = u64::from_le_bytes(r.0.get(o..o + 8)?.try_into().ok()?);
    let f = |s: u32| ((v >> s) & 0x1f_ffff) as f64;
    let x = (f(0) - 1048576.0) / 1048576.5;
    let y = (f(21) - 1048576.0) / 1048576.5;
    let z = (f(42) - 1048576.0) / 1048576.5;
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    Some(DQuat::from_xyzw(x, y, z, if v >> 63 != 0 { -w } else { w }))
}

fn euler(a: DVec3) -> DQuat {
    DQuat::from_euler(EulerRot::ZYX, a.z, a.y, a.x)
}

type Channels = HashMap<String, (Option<DQuat>, Option<DVec3>)>;

fn decode(data: &[u8], start: usize, src: &[Bone]) -> Option<Channels> {
    let r = R(data);
    let mut out = HashMap::new();
    let mut p = start;
    for _ in 0..1024 {
        let bone = *data.get(p)? as usize;
        let flags = *data.get(p + 1)?;
        let next = r.i16(p + 2)?;
        let b = src.get(bone)?;
        let mut o = p + 4;
        let q = if flags & RAWROT != 0 {
            o += 6;
            quat48(&r, p + 4)
        } else if flags & RAWROT2 != 0 {
            o += 8;
            quat64(&r, p + 4)
        } else if flags & ANIMROT != 0 {
            o += 6;
            value0(&r, p + 4).map(|v| euler(b.rot + v * b.rot_scale))
        } else {
            None
        };
        let t = if flags & RAWPOS != 0 {
            Some(DVec3::new(f16(r.u16(o)?), f16(r.u16(o + 2)?), f16(r.u16(o + 4)?)))
        } else if flags & ANIMPOS != 0 {
            value0(&r, o).map(|v| b.pos + v * b.pos_scale)
        } else {
            None
        };
        out.insert(b.name.clone(), (q, t));
        if next <= 0 {
            break;
        }
        p += next as usize;
    }
    Some(out)
}

pub fn skin(mdl: &[u8], read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<Vec<DMat4>> {
    let target = bones(mdl)?;
    let mut files: Vec<Vec<u8>> = vec![mdl.to_vec()];
    files.extend(includes(mdl).iter().filter_map(|n| read(&n.replace('\\', "/").to_lowercase())));
    let mut anim = None;
    for f in &files {
        let Some(a) = idle_anim(f) else { continue };
        let mut ani = None;
        if let (Some(src), Some((data, start))) = (bones(f), anim_data(f, a, read, &mut ani)) {
            anim = decode(data, start, &src);
        }
        if anim.is_some() {
            break;
        }
    }
    let anim = anim?;
    let mut world: Vec<DMat4> = Vec::with_capacity(target.len());
    for (i, b) in target.iter().enumerate() {
        let (q, t) = anim.get(&b.name).copied().unwrap_or((None, None));
        let local = DMat4::from_rotation_translation(q.unwrap_or(b.quat).normalize(), t.unwrap_or(b.pos));
        let m = match usize::try_from(b.parent).ok().filter(|&p| p < i) {
            Some(p) => world[p] * local,
            None => local,
        };
        world.push(m);
    }
    Some(world.iter().zip(&target).map(|(w, b)| *w * b.pose_to_bone).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(f16(0x3c00), 1.0);
        assert_eq!(f16(0xc000), -2.0);
        assert_eq!(f16(0), 0.0);
    }

    #[test]
    fn q48_identity() {
        let b = [0x00, 0x80, 0x00, 0x80, 0x00, 0x40];
        let q = quat48(&R(&b), 0).unwrap();
        assert!((q.w - 1.0).abs() < 1e-9 && q.x.abs() < 1e-9);
    }
}
