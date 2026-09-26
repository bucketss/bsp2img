use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::bits::text;

pub const HEADER_LEN: u64 = 544;
const INFO_LEN: usize = 436;
const SEQ_LEN: usize = 28;
const MAX_MSG: u32 = 1 << 20;

#[derive(Clone, Debug)]
pub struct Header {
    pub demo_protocol: i32,
    pub net_protocol: i32,
    pub map: String,
    pub game_dir: String,
    pub dir_offset: u32,
}

impl Header {
    fn parse(b: &[u8; HEADER_LEN as usize]) -> Result<Header> {
        if &b[..6] != b"HLDEMO" {
            bail!("not a GoldSrc demo");
        }
        let i32_at = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Ok(Header {
            demo_protocol: i32_at(8),
            net_protocol: i32_at(12),
            map: text(&b[16..276]),
            game_dir: text(&b[276..536]),
            dir_offset: i32_at(540) as u32,
        })
    }
}

pub fn header(path: &Path) -> Result<Header> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut b = [0u8; HEADER_LEN as usize];
    f.read_exact(&mut b).context("demo header")?;
    Header::parse(&b)
}

pub enum Frame<'a> {
    Net { time: f32, seq: i32, msgs: &'a [u8] },
    Other { kind: u8 },
}

pub struct Reader {
    r: BufReader<File>,
    pub header: Header,
    starts: VecDeque<u64>,
    sequential: bool,
    fresh: bool,
    buf: Vec<u8>,
    pub truncated: bool,
    pub counts: [u64; 10],
}

fn eof<T>(r: std::io::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = f.metadata()?.len();
        let mut r = BufReader::with_capacity(1 << 16, f);
        let mut b = [0u8; HEADER_LEN as usize];
        r.read_exact(&mut b).context("demo header")?;
        let header = Header::parse(&b)?;
        let mut starts = VecDeque::new();
        let off = header.dir_offset as u64;
        if off >= HEADER_LEN && off + 4 <= len {
            r.seek(SeekFrom::Start(off))?;
            let mut n = [0u8; 4];
            r.read_exact(&mut n)?;
            let n = i32::from_le_bytes(n);
            if (1..=1024).contains(&n) && off + 4 + n as u64 * 92 <= len {
                for _ in 0..n {
                    let mut e = [0u8; 92];
                    r.read_exact(&mut e)?;
                    let o = u32::from_le_bytes([e[84], e[85], e[86], e[87]]) as u64;
                    if o >= HEADER_LEN && o < off {
                        starts.push_back(o);
                    }
                }
            }
        }
        let sequential = starts.is_empty();
        if sequential {
            starts.push_back(HEADER_LEN);
        }
        Ok(Reader { r, header, starts, sequential, fresh: true, buf: Vec::new(), truncated: false, counts: [0; 10] })
    }

    pub fn next(&mut self) -> Result<Option<Frame<'_>>> {
        loop {
            if self.fresh {
                let Some(s) = self.starts.pop_front() else { return Ok(None) };
                self.r.seek(SeekFrom::Start(s))?;
                self.fresh = false;
            }
            let mut h = [0u8; 9];
            match eof(self.r.read_exact(&mut h))? {
                Some(()) => {}
                None => {
                    self.truncated = !self.sequential || !self.starts.is_empty();
                    return Ok(None);
                }
            }
            let kind = h[0];
            let time = f32::from_le_bytes([h[1], h[2], h[3], h[4]]);
            if kind > 9 {
                bail!("bad frame type {kind} at offset {}", self.r.stream_position()? - 9);
            }
            self.counts[kind as usize] += 1;
            let body = match kind {
                0 | 1 => self.net(),
                3 => self.r.seek_relative(64).map_err(Into::into),
                4 => self.r.seek_relative(32).map_err(Into::into),
                5 => {
                    if !self.sequential {
                        self.fresh = true;
                    }
                    Ok(())
                }
                6 => self.r.seek_relative(84).map_err(Into::into),
                7 => self.r.seek_relative(8).map_err(Into::into),
                8 => self.int().and_then(|_| self.int()).and_then(|n| self.r.seek_relative(n as i64 + 16).map_err(Into::into)),
                9 => self.int().and_then(|n| self.r.seek_relative(n as i64).map_err(Into::into)),
                _ => Ok(()),
            };
            if let Err(e) = body {
                if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == ErrorKind::UnexpectedEof) {
                    self.truncated = true;
                    return Ok(None);
                }
                return Err(e);
            }
            if kind <= 1 {
                let seq = i32::from_le_bytes([self.buf[INFO_LEN], self.buf[INFO_LEN + 1], self.buf[INFO_LEN + 2], self.buf[INFO_LEN + 3]]);
                return Ok(Some(Frame::Net { time, seq, msgs: &self.buf[INFO_LEN + SEQ_LEN + 4..] }));
            }
            if kind != 5 {
                return Ok(Some(Frame::Other { kind }));
            }
        }
    }

    fn int(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.r.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn net(&mut self) -> Result<()> {
        let head = INFO_LEN + SEQ_LEN + 4;
        self.buf.resize(head, 0);
        self.r.read_exact(&mut self.buf[..head])?;
        let n = u32::from_le_bytes([self.buf[head - 4], self.buf[head - 3], self.buf[head - 2], self.buf[head - 1]]);
        if n > MAX_MSG {
            bail!("message block of {n} bytes");
        }
        self.buf.resize(head + n as usize, 0);
        self.r.read_exact(&mut self.buf[head..])?;
        Ok(())
    }
}
