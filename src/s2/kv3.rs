use std::io::Read;

use anyhow::{Context, Result, bail, ensure};

use super::buf::{slice, u16_, u32_};

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Blob(Vec<u8>),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

pub static NULL: Value = Value::Null;

impl Value {
    pub fn get(&self, k: &str) -> &Value {
        match self {
            Value::Object(v) => v.iter().find(|(n, _)| n == k).map(|(_, v)| v).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn at(&self, i: usize) -> &Value {
        match self {
            Value::Array(v) => v.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn arr(&self) -> &[Value] {
        match self {
            Value::Array(v) => v,
            _ => &[],
        }
    }
    pub fn obj(&self) -> &[(String, Value)] {
        match self {
            Value::Object(v) => v,
            _ => &[],
        }
    }
    pub fn i(&self) -> i64 {
        match self {
            Value::Int(v) => *v,
            Value::UInt(v) => *v as i64,
            Value::Float(v) => *v as i64,
            Value::Bool(b) => *b as i64,
            _ => 0,
        }
    }
    pub fn f(&self) -> f64 {
        match self {
            Value::Int(v) => *v as f64,
            Value::UInt(v) => *v as f64,
            Value::Float(v) => *v,
            Value::Bool(b) => *b as i64 as f64,
            _ => 0.0,
        }
    }
    pub fn b(&self) -> bool {
        self.i() != 0
    }
    pub fn s(&self) -> &str {
        match self {
            Value::Str(s) => s,
            _ => "",
        }
    }
    pub fn blob(&self) -> &[u8] {
        match self {
            Value::Blob(b) => b,
            _ => &[],
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn fv(&self) -> Vec<f64> {
        self.arr().iter().map(|v| v.f()).collect()
    }
}

#[derive(Default, Clone, Copy)]
struct Cur {
    b1: usize,
    b2: usize,
    b4: usize,
    b8: usize,
}

struct Ctx<'a> {
    ver: u32,
    buf: &'a [u8],
    cur: Cur,
    aux_buf: &'a [u8],
    aux: Cur,
    types: &'a [u8],
    tp: usize,
    objlen: &'a [u8],
    op: usize,
    strings: Vec<String>,
    blob_lens: Vec<usize>,
    blobs: Vec<u8>,
    bp: usize,
    bi: usize,
    depth: usize,
}

fn layout(off: usize, n1: usize, n2: usize, n4: usize, n8: usize, force8: bool) -> (Cur, usize) {
    let b1 = off;
    let mut o = b1 + n1;
    if n2 > 0 {
        o = o.next_multiple_of(2);
    }
    let b2 = o;
    o += n2 * 2;
    if n4 > 0 {
        o = o.next_multiple_of(4);
    }
    let b4 = o;
    o += n4 * 4;
    if n8 > 0 || force8 {
        o = o.next_multiple_of(8);
    }
    let b8 = o;
    o += n8 * 8;
    (Cur { b1, b2, b4, b8 }, o)
}

pub fn zstd(src: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(cap);
    let mut s = src;
    while s.len() >= 4 {
        let mut d = ruzstd::decoding::StreamingDecoder::new(&mut s).map_err(|e| anyhow::anyhow!("zstd: {e}"))?;
        d.read_to_end(&mut out)?;
    }
    Ok(out)
}

fn lz4(src: &[u8], n: usize) -> Result<Vec<u8>> {
    lz4_flex::block::decompress(src, n).map_err(|e| anyhow::anyhow!("lz4: {e}"))
}

pub fn parse(b: &[u8]) -> Result<Value> {
    parse_len(b).map(|(v, _)| v)
}

pub fn is_kv3(b: &[u8]) -> bool {
    u32_(b, 0).is_ok_and(|m| m & 0xffffff00 == 0x4B563300)
}

pub fn parse_len(b: &[u8]) -> Result<(Value, usize)> {
    let magic = u32_(b, 0)?;
    ensure!(magic & 0xffffff00 == 0x4B563300, "not KV3 (magic {magic:08x})");
    let ver = magic & 0xff;
    ensure!((2..=5).contains(&ver), "KV3 version {ver} not supported");
    let comp = u32_(b, 20)?;
    let q = 24;
    let h = |i: usize| -> Result<usize> { Ok(u32_(b, q + i * 4)? as usize) };
    let frame = u16_(b, q + 2)? as usize;
    let (c1, c4, c8, ctypes) = (h(1)?, h(2)?, h(3)?, h(4)?);
    let (usz, csz, nblocks, blobsz) = (h(6)?, h(7)?, h(8)?, h(9)?);
    let c2 = if ver >= 4 { h(10)? } else { 0 };
    let mut p = q + 40 + if ver >= 4 { 8 } else { 0 };
    let v = |i: usize| -> Result<usize> { Ok(u32_(b, p + i * 4)? as usize) };
    let (u1, cs1, u2, cs2, b2c1, b2c2, b2c4, b2c8, b2obj) = if ver >= 5 {
        (v(0)?, v(1)?, v(2)?, v(3)?, v(4)?, v(5)?, v(6)?, v(7)?, v(9)?)
    } else {
        (usz, csz, 0, 0, 0, 0, 0, 0, 0)
    };
    if ver >= 5 {
        p += 48;
    }
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = slice(b, p, n)?;
        p += n;
        Ok(s)
    };
    let dec = |src: &[u8], n: usize| -> Result<Vec<u8>> {
        Ok(match comp {
            0 => src.to_vec(),
            1 => lz4(src, n)?,
            2 => zstd(src, n)?,
            _ => bail!("KV3 compression {comp}"),
        })
    };
    let buf1 = if comp == 0 {
        take(u1)?.to_vec()
    } else if comp == 2 && ver < 5 {
        dec(take(cs1)?, u1 + blobsz)?
    } else {
        dec(take(cs1)?, u1)?
    };
    let buf2 = if ver < 5 {
        Vec::new()
    } else if comp == 0 {
        take(u2)?.to_vec()
    } else {
        dec(take(cs2)?, u2)?
    };
    let (mut c, off) = layout(0, c1, c2, c4, c8, ver < 5);
    let nstr = u32_(&buf1, c.b4)? as usize;
    c.b4 += 4;
    let mut strings = Vec::with_capacity(nstr.min(1 << 20));
    let mut sp = if ver >= 5 { c.b1 } else { off };
    for _ in 0..nstr {
        let (s, e) = super::buf::cstr(&buf1, sp)?;
        strings.push(s);
        sp = e;
    }
    let (main, cur, aux, types, tail, objlen): (&[u8], Cur, Cur, &[u8], usize, &[u8]) = if ver >= 5 {
        c.b1 = sp;
        let o = b2obj * 4;
        let (cur, o) = layout(o, b2c1, b2c2, b2c4, b2c8, false);
        (&buf2, cur, c, slice(&buf2, o, ctypes)?, o + ctypes, slice(&buf2, 0, b2obj * 4)?)
    } else {
        let tl = (ctypes + off).checked_sub(sp).context("KV3 type count")?;
        (slice(&buf1, 0, u1)?, c, Cur::default(), slice(&buf1, sp, tl)?, sp + tl, &[])
    };
    let mut blob_lens = Vec::new();
    let mut blobs = Vec::new();
    if nblocks > 0 {
        let mut t = tail;
        for _ in 0..nblocks {
            blob_lens.push(u32_(main, t)? as usize);
            t += 4;
        }
        ensure!(u32_(main, t)? == 0xFFEEDD00, "KV3 blob table trailer");
        t += 4;
        match comp {
            0 => blobs = take(blobsz)?.to_vec(),
            1 => {
                blobs.reserve(blobsz);
                while blobs.len() < blobsz {
                    let n = u16_(main, t)? as usize;
                    t += 2;
                    let want = frame.max(1).min(blobsz - blobs.len());
                    let start = blobs.len().saturating_sub(65536);
                    let chunk = take(n)?;
                    let d = lz4_flex::block::decompress_with_dict(chunk, want, &blobs[start..])
                        .map_err(|e| anyhow::anyhow!("lz4 blob: {e}"))?;
                    blobs.extend_from_slice(&d);
                }
            }
            _ if ver >= 5 => {
                let n = csz.checked_sub(cs1 + cs2).context("KV3 blob size")?;
                blobs = zstd(take(n)?, blobsz)?;
            }
            _ => blobs = slice(&buf1, u1, blobsz)?.to_vec(),
        }
        if u32_(b, p).is_ok_and(|m| m == 0xFFEEDD00) {
            p += 4;
        }
    }
    let mut ctx = Ctx {
        ver,
        buf: main,
        cur,
        aux_buf: &buf1,
        aux,
        types,
        tp: 0,
        objlen,
        op: 0,
        strings,
        blob_lens,
        blobs,
        bp: 0,
        bi: 0,
        depth: 0,
    };
    let t = ctx.ty()?;
    Ok((ctx.value(t)?, p))
}

impl<'a> Ctx<'a> {
    fn ty(&mut self) -> Result<u8> {
        let Some(&t) = self.types.get(self.tp) else { bail!("KV3 type stream ended") };
        self.tp += 1;
        if t & 0x80 != 0 {
            self.tp += 1;
            return Ok(t & if self.ver >= 3 { 0x3f } else { 0x7f });
        }
        Ok(t)
    }

    fn n(&mut self, w: usize) -> Result<&'a [u8]> {
        let c = match w {
            1 => &mut self.cur.b1,
            2 => &mut self.cur.b2,
            4 => &mut self.cur.b4,
            _ => &mut self.cur.b8,
        };
        let s = slice(self.buf, *c, w)?;
        *c += w;
        Ok(s)
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.n(4)?.try_into()?))
    }

    fn len(&mut self) -> Result<usize> {
        let n = self.i32()?;
        ensure!(n >= 0, "KV3 negative length");
        Ok(n as usize)
    }

    fn string(&mut self) -> Result<String> {
        let i = self.i32()?;
        if i < 0 {
            return Ok(String::new());
        }
        self.strings.get(i as usize).cloned().context("KV3 string index")
    }

    fn swap(&mut self) {
        std::mem::swap(&mut self.buf, &mut self.aux_buf);
        std::mem::swap(&mut self.cur, &mut self.aux);
    }

    fn list(&mut self, n: usize, t: Option<u8>) -> Result<Value> {
        let mut v = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            let st = match t {
                Some(t) => t,
                None => self.ty()?,
            };
            v.push(self.value(st)?);
        }
        Ok(Value::Array(v))
    }

    fn value(&mut self, t: u8) -> Result<Value> {
        self.depth += 1;
        ensure!(self.depth < 256, "KV3 nesting too deep");
        let v = self.value_inner(t);
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self, t: u8) -> Result<Value> {
        Ok(match t {
            1 => Value::Null,
            13 => Value::Bool(true),
            14 => Value::Bool(false),
            15 => Value::Int(0),
            16 => Value::Int(1),
            17 => Value::Float(0.0),
            18 => Value::Float(1.0),
            2 => Value::Bool(self.n(1)?[0] == 1),
            23 => Value::Int(self.n(1)?[0] as i64),
            20 => Value::Int(i16::from_le_bytes(self.n(2)?.try_into()?) as i64),
            21 => Value::Int(u16::from_le_bytes(self.n(2)?.try_into()?) as i64),
            11 => Value::Int(self.i32()? as i64),
            12 => Value::Int(u32::from_le_bytes(self.n(4)?.try_into()?) as i64),
            19 => Value::Float(f32::from_le_bytes(self.n(4)?.try_into()?) as f64),
            3 => Value::Int(i64::from_le_bytes(self.n(8)?.try_into()?)),
            4 => Value::UInt(u64::from_le_bytes(self.n(8)?.try_into()?)),
            5 => Value::Float(f64::from_le_bytes(self.n(8)?.try_into()?)),
            6 => Value::Str(self.string()?),
            7 => {
                let n = *self.blob_lens.get(self.bi).context("KV3 blob index")?;
                self.bi += 1;
                let v = slice(&self.blobs, self.bp, n)?.to_vec();
                self.bp += n;
                Value::Blob(v)
            }
            8 => {
                let n = self.len()?;
                self.list(n, None)?
            }
            10 => {
                let n = self.len()?;
                let st = self.ty()?;
                self.list(n, Some(st))?
            }
            24 => {
                let n = self.n(1)?[0] as usize;
                let st = self.ty()?;
                self.list(n, Some(st))?
            }
            25 => {
                let n = self.n(1)?[0] as usize;
                let st = self.ty()?;
                self.swap();
                let v = self.list(n, Some(st));
                self.swap();
                v?
            }
            9 => {
                let n = if self.ver >= 5 {
                    let n = u32_(self.objlen, self.op)? as usize;
                    self.op += 4;
                    n
                } else {
                    self.len()?
                };
                let mut v = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    let st = self.ty()?;
                    let k = self.string()?;
                    v.push((k, self.value(st)?));
                }
                Value::Object(v)
            }
            _ => bail!("KV3 type {t}"),
        })
    }
}
