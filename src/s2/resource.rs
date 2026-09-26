use anyhow::{Context, Result, bail, ensure};

use super::buf::{slice, u16_, u32_};
use super::kv3::{self, Value};

#[derive(Clone, Copy)]
pub struct Block {
    pub kind: [u8; 4],
    pub off: usize,
    pub size: usize,
}

pub struct Res {
    pub data: Vec<u8>,
    pub version: u16,
    pub blocks: Vec<Block>,
}

impl Res {
    pub fn new(data: Vec<u8>) -> Result<Res> {
        ensure!(data.len() >= 16, "resource too short");
        let hv = u16_(&data, 4)?;
        ensure!(hv == 12, "resource header version {hv}");
        let version = u16_(&data, 6)?;
        let boff = u32_(&data, 8)? as usize;
        let n = u32_(&data, 12)? as usize;
        let mut blocks = Vec::with_capacity(n);
        let mut p = 8 + boff;
        for _ in 0..n {
            let kind: [u8; 4] = slice(&data, p, 4)?.try_into()?;
            let off = p + 4 + u32_(&data, p + 4)? as usize;
            let size = u32_(&data, p + 8)? as usize;
            slice(&data, off, size).with_context(|| format!("block {}", String::from_utf8_lossy(&kind)))?;
            blocks.push(Block { kind, off, size });
            p += 12;
        }
        Ok(Res { data, version, blocks })
    }

    pub fn raw(&self, i: usize) -> Result<&[u8]> {
        let Some(b) = self.blocks.get(i) else { bail!("block index {i} of {}", self.blocks.len()) };
        Ok(&self.data[b.off..b.off + b.size])
    }

    pub fn find(&self, kind: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| &b.kind == kind.as_bytes())
    }

    pub fn block(&self, kind: &str) -> Option<&[u8]> {
        self.find(kind).map(|b| &self.data[b.off..b.off + b.size])
    }

    pub fn kv(&self, kind: &str) -> Result<Value> {
        match self.block(kind) {
            Some(b) => kv3::parse(b).with_context(|| format!("{kind} block")),
            None => bail!("no {kind} block"),
        }
    }

    pub fn kv_at(&self, i: usize) -> Result<Value> {
        kv3::parse(self.raw(i)?).with_context(|| format!("block {i}"))
    }
}
