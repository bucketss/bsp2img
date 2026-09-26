pub mod bits;
pub mod container;
pub mod delta;
pub mod messages;
pub mod state;

use std::path::Path;

use anyhow::Result;

pub use container::{Header, header};
pub use state::{Death, Event, Presence, Team};

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

pub fn scan(path: &Path, presence_every: f32, cancel: &dyn Fn() -> bool, on_event: &mut dyn FnMut(Event)) -> Result<Summary> {
    let mut r = Reader::open(path)?;
    let mut p = Parser::new();
    let mut g = Game::new(presence_every);
    let mut sum = Summary { map: r.header.map.clone(), ..Default::default() };
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
