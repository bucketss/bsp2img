pub mod bits;
pub mod container;
pub mod cs2;
pub mod delta;
pub mod messages;
pub mod state;

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub use state::{Death, Event, Team};

use container::{Frame, Reader};
use messages::Parser;
use state::Game;

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub map: String,
    pub frames: u64,
    pub seconds: f32,
    pub rounds: u32,
    pub truncated: bool,
    pub desyncs: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    GoldSrc,
    Cs2,
}

pub fn kind(path: &Path) -> Result<Kind> {
    let mut b = [0u8; 8];
    File::open(path).with_context(|| format!("open {}", path.display()))?.read_exact(&mut b).context("demo header")?;
    match &b {
        b if b.starts_with(b"HLDEMO") => Ok(Kind::GoldSrc),
        cs2::MAGIC => Ok(Kind::Cs2),
        b"HL2DEMO\0" => bail!("Source demos are not supported"),
        _ => bail!("not a demo"),
    }
}

pub fn map_name(path: &Path) -> Result<String> {
    match kind(path)? {
        Kind::GoldSrc => Ok(container::header(path)?.map),
        Kind::Cs2 => cs2::header(path),
    }
}

pub fn scan(path: &Path, presence_every: f32, cancel: &dyn Fn() -> bool, on_event: &mut dyn FnMut(Event)) -> Result<Summary> {
    let mut r = Reader::open(path)?;
    let mut p = Parser::new();
    let mut g = Game::new(presence_every);
    let mut sum = Summary { map: r.header.map.clone(), ..Default::default() };
    if r.header.demo_protocol != 5 || r.header.net_protocol != 48 {
        sum.desyncs.push(format!(
            "demo protocol {}, network protocol {}; only 5 and 48 are known",
            r.header.demo_protocol, r.header.net_protocol
        ));
    }
    while let Some(f) = r.next()? {
        let Frame::Net { time, seq, msgs } = f else { continue };
        sum.frames += 1;
        if sum.frames % 4096 == 0 && cancel() {
            anyhow::bail!("cancelled");
        }
        if let Err(e) = p.frame(seq, msgs)
            && sum.desyncs.len() < 20
        {
            sum.desyncs.push(format!(
                "frame {} at {:.2}s: message {} at byte {} of {}, after {:?}: {}",
                sum.frames,
                time,
                e.kind,
                e.offset,
                msgs.len(),
                e.recent,
                e.why
            ));
        }
        g.frame(time, &mut p, on_event);
        sum.seconds = time;
    }
    sum.truncated = r.truncated;
    sum.rounds = g.round;
    if !g.map.is_empty() {
        sum.map = g.map;
    }
    Ok(sum)
}
