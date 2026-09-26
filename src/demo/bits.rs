pub struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    pub over: bool,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Bits<'a> {
        Bits { d, pos: 0, over: false }
    }

    pub fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let end = self.pos + n as usize;
        if end > self.d.len() * 8 {
            self.over = true;
            self.pos = end;
            return 0;
        }
        let byte = self.pos >> 3;
        let avail = (self.d.len() - byte).min(8);
        let mut buf = [0u8; 8];
        buf[..avail].copy_from_slice(&self.d[byte..byte + avail]);
        let v = u64::from_le_bytes(buf) >> (self.pos & 7);
        self.pos = end;
        (v & ((1u64 << n) - 1)) as u32
    }

    pub fn peek(&mut self, n: u32) -> u32 {
        let (pos, over) = (self.pos, self.over);
        let v = self.bits(n);
        (self.pos, self.over) = (pos, over);
        v
    }

    pub fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }

    pub fn sbits(&mut self, n: u32) -> i32 {
        let neg = self.bit();
        let v = self.bits(n.saturating_sub(1)) as i32;
        if neg { -v } else { v }
    }

    pub fn string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let c = self.bits(8) as u8;
            if c == 0 || self.over {
                return out;
            }
            out.push(c);
        }
    }

    pub fn skip_string(&mut self) {
        while self.bits(8) != 0 && !self.over {}
    }

    pub fn skip(&mut self, n: usize) {
        self.pos += n;
        if self.pos > self.d.len() * 8 {
            self.over = true;
        }
    }

    pub fn coord(&mut self) -> f32 {
        let int = self.bit();
        let frac = self.bit();
        if !int && !frac {
            return 0.0;
        }
        let neg = self.bit();
        let i = if int { self.bits(12) } else { 0 };
        let f = if frac { self.bits(3) } else { 0 };
        let v = i as f32 + f as f32 / 8.0;
        if neg { -v } else { v }
    }

    pub fn bytes_used(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

pub struct Bytes<'a> {
    pub d: &'a [u8],
    pub pos: usize,
}

pub struct Short;

impl<'a> Bytes<'a> {
    pub fn new(d: &'a [u8]) -> Bytes<'a> {
        Bytes { d, pos: 0 }
    }

    pub fn left(&self) -> usize {
        self.d.len().saturating_sub(self.pos)
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Short> {
        if self.left() < n {
            return Err(Short);
        }
        let s = &self.d[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Short> {
        self.take(n).map(drop)
    }

    pub fn u8(&mut self) -> Result<u8, Short> {
        Ok(self.take(1)?[0])
    }

    pub fn i16(&mut self) -> Result<i16, Short> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u16(&mut self) -> Result<u16, Short> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn string(&mut self) -> Result<&'a [u8], Short> {
        let rest = &self.d[self.pos.min(self.d.len())..];
        let n = rest.iter().position(|&c| c == 0).ok_or(Short)?;
        self.pos += n + 1;
        Ok(&rest[..n])
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.d[self.pos.min(self.d.len())..]
    }
}

pub fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
