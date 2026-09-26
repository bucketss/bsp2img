use anyhow::{Result, bail};

pub fn slice(b: &[u8], o: usize, n: usize) -> Result<&[u8]> {
    match o.checked_add(n).and_then(|e| b.get(o..e)) {
        Some(s) => Ok(s),
        None => bail!("read of {n} bytes at {o} past end ({})", b.len()),
    }
}

pub fn u8_(b: &[u8], o: usize) -> Result<u8> {
    Ok(slice(b, o, 1)?[0])
}

pub fn u16_(b: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(slice(b, o, 2)?.try_into()?))
}

pub fn u32_(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(b, o, 4)?.try_into()?))
}

pub fn i32_(b: &[u8], o: usize) -> Result<i32> {
    Ok(u32_(b, o)? as i32)
}

pub fn f32_(b: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_bits(u32_(b, o)?))
}

pub fn cstr(b: &[u8], o: usize) -> Result<(String, usize)> {
    let rest = b.get(o..).unwrap_or(&[]);
    let Some(e) = rest.iter().position(|&c| c == 0) else { bail!("unterminated string at {o}") };
    Ok((String::from_utf8_lossy(&rest[..e]).into_owned(), o + e + 1))
}

pub fn half(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f32;
    s * match e {
        0 => m / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

pub struct Rd<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Rd<'a> {
        Rd { b, p: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = slice(self.b, self.p, n)?;
        self.p += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    pub fn cstr(&mut self) -> Result<String> {
        let (s, e) = cstr(self.b, self.p)?;
        self.p = e;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(drop)
    }
    pub fn align(&mut self, a: usize) {
        self.p = self.p.next_multiple_of(a);
    }
}
