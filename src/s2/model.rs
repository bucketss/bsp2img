use anyhow::{Context, Result, bail, ensure};

use super::buf::{half, slice, u32_};
use super::kv3::{self, Value};
use super::resource::Res;

pub struct Attr {
    pub name: String,
    pub map: bool,
    pub index: u32,
    pub format: u32,
    pub offset: usize,
}

pub struct Buf {
    pub stride: usize,
    pub count: usize,
    pub data: Vec<u8>,
    pub layout: Vec<Attr>,
}

pub struct DrawCall {
    pub material: String,
    pub base_vertex: usize,
    pub start_index: usize,
    pub index_count: usize,
    pub ib: usize,
    pub ib_bind: usize,
    pub vbs: Vec<(usize, usize)>,
    pub tint: [f32; 3],
    pub alpha: f32,
    pub triangles: bool,
    pub lightmapped: bool,
}

pub struct Indices<'a> {
    data: &'a [u8],
    wide: bool,
    base: u32,
}

impl Indices<'_> {
    pub fn len(&self) -> usize {
        self.data.len() >> if self.wide { 2 } else { 1 }
    }

    pub fn get(&self, k: usize) -> u32 {
        self.base
            + if self.wide {
                u32::from_le_bytes(self.data[k * 4..k * 4 + 4].try_into().unwrap())
            } else {
                u16::from_le_bytes([self.data[k * 2], self.data[k * 2 + 1]]) as u32
            }
    }
}

pub struct Model {
    pub vbs: Vec<Buf>,
    pub ibs: Vec<Buf>,
    pub draws: Vec<DrawCall>,
}

#[derive(Clone, Copy)]
pub struct Stream<'a> {
    buf: &'a Buf,
    format: u32,
    offset: usize,
}

pub fn format_size(f: u32) -> Option<usize> {
    Some(match f {
        2 | 3 | 4 => 16,
        6 | 7 | 8 => 12,
        10..=14 | 16 | 17 | 18 => 8,
        24 | 28..=32 | 34..=38 | 41 | 42 | 43 => 4,
        _ => return None,
    })
}

fn fetch(f: u32, d: &[u8]) -> [f32; 4] {
    let f32_ = |k: usize| f32::from_le_bytes(d[k * 4..k * 4 + 4].try_into().unwrap());
    let u16_ = |k: usize| u16::from_le_bytes([d[k * 2], d[k * 2 + 1]]);
    let i16_ = |k: usize| u16_(k) as i16;
    let u32_ = |k: usize| u32::from_le_bytes(d[k * 4..k * 4 + 4].try_into().unwrap());
    match f {
        2 => [f32_(0), f32_(1), f32_(2), f32_(3)],
        6 => [f32_(0), f32_(1), f32_(2), 1.0],
        16 => [f32_(0), f32_(1), 0.0, 1.0],
        41 => [f32_(0), 0.0, 0.0, 1.0],
        3 | 4 => [0, 1, 2, 3].map(|k| u32_(k) as f32),
        7 | 8 => [u32_(0) as f32, u32_(1) as f32, u32_(2) as f32, 1.0],
        17 | 18 => [u32_(0) as f32, u32_(1) as f32, 0.0, 1.0],
        42 | 43 => [u32_(0) as f32, 0.0, 0.0, 1.0],
        10 => [0, 1, 2, 3].map(|k| half(u16_(k))),
        34 => [half(u16_(0)), half(u16_(1)), 0.0, 1.0],
        11 => [0, 1, 2, 3].map(|k| u16_(k) as f32 / 65535.0),
        35 => [u16_(0) as f32 / 65535.0, u16_(1) as f32 / 65535.0, 0.0, 1.0],
        13 => [0, 1, 2, 3].map(|k| (i16_(k) as f32 / 32767.0).max(-1.0)),
        37 => [(i16_(0) as f32 / 32767.0).max(-1.0), (i16_(1) as f32 / 32767.0).max(-1.0), 0.0, 1.0],
        12 | 14 => [0, 1, 2, 3].map(|k| if f == 12 { u16_(k) as f32 } else { i16_(k) as f32 }),
        36 => [u16_(0) as f32, u16_(1) as f32, 0.0, 1.0],
        38 => [i16_(0) as f32, i16_(1) as f32, 0.0, 1.0],
        28 => [0, 1, 2, 3].map(|k| d[k] as f32 / 255.0),
        30 => [0, 1, 2, 3].map(|k| d[k] as f32),
        31 => [0, 1, 2, 3].map(|k| ((d[k] as i8) as f32 / 127.0).max(-1.0)),
        32 => [0, 1, 2, 3].map(|k| (d[k] as i8) as f32),
        24 => {
            let v = u32_(0);
            [(v & 0x3ff) as f32 / 1023.0, ((v >> 10) & 0x3ff) as f32 / 1023.0, ((v >> 20) & 0x3ff) as f32 / 1023.0, (v >> 30) as f32 / 3.0]
        }
        _ => [0.0; 4],
    }
}

fn normal_v2(v: u32) -> [f32; 3] {
    let mut x = ((v >> 12) & 0x3ff) as f32 / 1023.0 * 2.0 - 1.0;
    let mut y = ((v >> 22) & 0x3ff) as f32 / 1023.0 * 2.0 - 1.0;
    let z = 1.0 - x.abs() - y.abs();
    let neg = (-z).clamp(0.0, 1.0);
    x += if x >= 0.0 { -neg } else { neg };
    y += if y >= 0.0 { -neg } else { neg };
    norm([x, y, z])
}

fn normal_v1(bx: u8, by: u8) -> [f32; 3] {
    let (mut x, mut y) = (bx as f32 - 128.0, by as f32 - 128.0);
    let zs = if x < 0.0 { -1.0 } else { 1.0 };
    let ts = if y < 0.0 { -1.0 } else { 1.0 };
    x = x * zs - (x < 0.0) as i32 as f32 - 64.0;
    y = y * ts - (y < 0.0) as i32 as f32 - 64.0;
    let xs = if x < 0.0 { -1.0 } else { 1.0 };
    let ys = if y < 0.0 { -1.0 } else { 1.0 };
    x = (x * xs - (x < 0.0) as i32 as f32) / 63.0;
    y = (y * ys - (y < 0.0) as i32 as f32) / 63.0;
    let z = 1.0 - x - y;
    let n = norm([x, y, z]);
    [n[0] * xs, n[1] * ys, n[2] * zs]
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 0.0, 1.0] }
}

impl Stream<'_> {
    fn bytes(&self, i: usize) -> Option<&[u8]> {
        let n = format_size(self.format)?;
        let p = i.checked_mul(self.buf.stride)? + self.offset;
        self.buf.data.get(p..p + n)
    }

    pub fn get(&self, i: usize) -> [f32; 4] {
        self.bytes(i).map(|d| fetch(self.format, d)).unwrap_or([0.0; 4])
    }

    pub fn normal(&self, i: usize) -> Option<[f32; 3]> {
        let d = self.bytes(i)?;
        Some(match self.format {
            42 => normal_v2(u32::from_le_bytes(d[..4].try_into().ok()?)),
            28 => normal_v1(d[0], d[1]),
            _ => {
                let v = fetch(self.format, d);
                norm([v[0], v[1], v[2]])
            }
        })
    }
}

pub fn unpack(raw: &[u8], n: usize, sz: usize, index: bool, seq: bool, zstd: bool, meshopt: bool) -> Result<Vec<u8>> {
    let want = n.checked_mul(sz).context("buffer size overflow")?;
    let z;
    let raw = if zstd {
        z = kv3::zstd(raw, want)?;
        &z[..]
    } else {
        raw
    };
    if !meshopt {
        ensure!(raw.len() >= want, "buffer holds {} bytes, needs {want}", raw.len());
        return Ok(raw[..want].to_vec());
    }
    let mut out = vec![0u8; want];
    let rc = unsafe {
        if !index {
            meshopt::ffi::meshopt_decodeVertexBuffer(out.as_mut_ptr().cast(), n, sz, raw.as_ptr(), raw.len())
        } else if seq {
            meshopt::ffi::meshopt_decodeIndexSequence(out.as_mut_ptr().cast(), n, sz, raw.as_ptr(), raw.len())
        } else {
            meshopt::ffi::meshopt_decodeIndexBuffer(out.as_mut_ptr().cast(), n, sz, raw.as_ptr(), raw.len())
        }
    };
    if rc != 0 {
        bail!("meshopt decode failed ({rc})");
    }
    Ok(out)
}

fn decode_buf(res: &Res, b: &Value, index: bool) -> Result<Buf> {
    let raw = res.raw(b.get("m_nBlockIndex").i() as usize)?;
    let count = b.get("m_nElementCount").i() as usize;
    let stride = b.get("m_nElementSizeInBytes").i() as usize;
    let data = unpack(
        raw,
        count,
        stride,
        index,
        b.get("m_bMeshoptIndexSequence").b(),
        b.get("m_bCompressedZSTD").b(),
        b.get("m_bMeshoptCompressed").b(),
    )?;
    let layout = b
        .get("m_inputLayoutFields")
        .arr()
        .iter()
        .map(|f| {
            let name = f.get("m_pSemanticName").s();
            Attr {
                name: name.to_uppercase(),
                map: name.chars().any(|c| c.is_ascii_lowercase()),
                index: f.get("m_nSemanticIndex").i() as u32,
                format: f.get("m_Format").i() as u32,
                offset: f.get("m_nOffset").i() as usize,
            }
        })
        .collect();
    Ok(Buf { stride, count, data, layout })
}

fn vbib(blk: &[u8]) -> Result<(Vec<Buf>, Vec<Buf>)> {
    let u = |p: usize| -> Result<usize> { Ok(u32_(blk, p)? as usize) };
    let one = |p: usize, index: bool| -> Result<Buf> {
        let count = u(p)?;
        let size = u(p + 4)?;
        let stride = size & 0x3FFFFFF;
        let ra = p + 8;
        let (aoff, acount) = (u(ra)?, u(ra + 4)?);
        let rb = p + 16;
        let (doff, total) = (u(rb)?, u(rb + 4)?);
        let mut layout = Vec::with_capacity(acount);
        for i in 0..acount {
            let q = ra + aoff + i * 56;
            let nb = slice(blk, q, 32)?;
            let e = nb.iter().position(|&c| c == 0).unwrap_or(32);
            let name = String::from_utf8_lossy(&nb[..e]).into_owned();
            layout.push(Attr {
                map: name.chars().any(|c| c.is_ascii_lowercase()),
                name: name.to_uppercase(),
                index: u(q + 32)? as u32,
                format: u(q + 36)? as u32,
                offset: u(q + 40)?,
            });
        }
        let raw = slice(blk, rb + doff, total)?;
        let packed = count.checked_mul(stride).is_some_and(|w| w > total);
        let data = unpack(raw, count, stride, index, false, size & 0x8000000 != 0, packed && size & 0x4000000 == 0)?;
        Ok(Buf { stride, count, data, layout })
    };
    let (voff, vn, ioff, inn) = (u(0)?, u(4)?, u(8)?, u(12)?);
    let vbs = (0..vn).map(|i| one(voff + i * 24, false)).collect::<Result<Vec<_>>>()?;
    let ibs = (0..inn).map(|i| one(8 + ioff + i * 24, true)).collect::<Result<Vec<_>>>()?;
    Ok((vbs, ibs))
}

fn draw_call(dc: &Value) -> DrawCall {
    let t = dc.get("m_vTintColor").fv();
    let prim = dc.get("m_nPrimitiveType");
    let triangles = match prim {
        Value::Str(s) => s == "RENDER_PRIM_TRIANGLES",
        v => v.i() == 5 || v.is_null(),
    };
    DrawCall {
        material: dc.get("m_material").s().to_string(),
        base_vertex: dc.get("m_nBaseVertex").i().max(0) as usize,
        start_index: dc.get("m_nStartIndex").i().max(0) as usize,
        index_count: dc.get("m_nIndexCount").i().max(0) as usize,
        ib: dc.get("m_indexBuffer").get("m_hBuffer").i().max(0) as usize,
        ib_bind: dc.get("m_indexBuffer").get("m_nBindOffsetBytes").i().max(0) as usize,
        vbs: dc
            .get("m_vertexBuffers")
            .arr()
            .iter()
            .map(|r| (r.get("m_hBuffer").i().max(0) as usize, r.get("m_nBindOffsetBytes").i().max(0) as usize))
            .collect(),
        tint: if t.len() >= 3 { [t[0] as f32, t[1] as f32, t[2] as f32] } else { [1.0; 3] },
        alpha: if dc.get("m_flAlpha").is_null() { 1.0 } else { dc.get("m_flAlpha").f() as f32 },
        triangles,
        lightmapped: dc.get("m_bHasBakedLightingFromLightMap").b(),
    }
}

impl Model {
    pub fn load(bytes: Vec<u8>) -> Result<Model> {
        let res = Res::new(bytes)?;
        let mut m = Model { vbs: Vec::new(), ibs: Vec::new(), draws: Vec::new() };
        if res.find("CTRL").is_none() {
            return Ok(m);
        }
        let ctrl = res.kv("CTRL")?;
        let Some(em) = ctrl.get("embedded_meshes").arr().first() else { return Ok(m) };
        let mdat = if !em.get("vbib_block").is_null() {
            (m.vbs, m.ibs) = vbib(res.raw(em.get("vbib_block").i() as usize)?).context("MBUF")?;
            res.kv_at(em.get("data_block").i() as usize)?
        } else {
            for vb in em.get("m_vertexBuffers").arr() {
                m.vbs.push(decode_buf(&res, vb, false).context("vertex buffer")?);
            }
            for ib in em.get("m_indexBuffers").arr() {
                m.ibs.push(decode_buf(&res, ib, true).context("index buffer")?);
            }
            res.kv_at(em.get("m_nDataBlock").i() as usize)?
        };
        for so in mdat.get("m_sceneObjects").arr() {
            m.draws.extend(so.get("m_drawCalls").arr().iter().map(draw_call));
        }
        Ok(m)
    }

    pub fn stream(&self, dc: &DrawCall, name: &str, index: u32) -> Option<Stream<'_>> {
        self.find(dc, |a| a.name == name && a.index == index)
    }

    pub fn map_stream(&self, dc: &DrawCall, name: &str, index: u32) -> Option<Stream<'_>> {
        self.find(dc, |a| a.map && a.name == name && a.index == index)
    }

    fn find(&self, dc: &DrawCall, f: impl Fn(&Attr) -> bool) -> Option<Stream<'_>> {
        for &(h, bind) in &dc.vbs {
            let b = self.vbs.get(h)?;
            if let Some(a) = b.layout.iter().find(|a| f(a)) {
                return Some(Stream { buf: b, format: a.format, offset: a.offset + bind });
            }
        }
        None
    }

    pub fn index_view(&self, dc: &DrawCall) -> Result<Indices<'_>> {
        ensure!(dc.triangles, "draw call is not a triangle list");
        let ib = self.ibs.get(dc.ib).context("index buffer out of range")?;
        ensure!(ib.stride == 2 || ib.stride == 4, "index size {}", ib.stride);
        let s = dc.start_index + dc.ib_bind / ib.stride;
        let n = dc.index_count / 3 * 3;
        let data = slice(&ib.data, s * ib.stride, n * ib.stride).context("index range")?;
        Ok(Indices { data, wide: ib.stride == 4, base: dc.base_vertex as u32 })
    }
}
