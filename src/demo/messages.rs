use std::collections::HashMap;

use super::bits::{Bits, Bytes, Short, text};
use super::delta::{NSLOT, Slots, Table};

pub const MAX_PLAYERS: usize = 32;
const MAX_EDICTS: usize = 2048;

pub type Players = [Option<Slots>; MAX_PLAYERS + 1];

const TE_LEN: [i8; 128] = [
    24, 20, 6, 11, 6, 10, 12, 17, 16, 6, 6, 6, 8, -1, 9, 19, //
    -2, 10, 16, 24, 24, 24, 10, 11, 16, 19, -2, 12, 16, -1, 19, 17, //
    -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, //
    -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, //
    -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, //
    -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, -2, //
    -2, -2, -2, 2, 10, 14, 12, 14, 9, 5, 17, 13, 24, 9, 17, 7, //
    10, 19, 19, 12, 7, 7, 9, 16, 18, 6, 10, 13, 7, 1, 18, 15, //
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum UKind {
    Other,
    DeathMsg,
    TeamInfo,
    TeamScore,
    TextMsg,
    Hltv,
    ScoreAttrib,
    RoundTime,
    SendAudio,
}

struct UserMsg {
    size: i16,
    name: String,
    kind: UKind,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Time(f32),
    ServerInfo { maxclients: u8, map: String },
    UserInfo { slot: u8, userid: i32, info: String },
    Death { killer: u8, victim: u8, headshot: bool, weapon: String },
    TeamInfo { ent: u8, team: String },
    TeamScore { team: String, score: i16 },
    TextMsg { dest: u8, text: String },
    Hltv { ent: u8, value: u8 },
    ScoreAttrib { ent: u8, flags: u8 },
    RoundTime(i16),
    SendAudio(String),
    Entities,
}

#[derive(Clone, Debug)]
pub struct Desync {
    pub kind: u8,
    pub offset: usize,
    pub recent: Vec<u8>,
    pub why: String,
}

enum Fail {
    Short,
    Bad(String),
}

impl From<Short> for Fail {
    fn from(_: Short) -> Fail {
        Fail::Short
    }
}

type R<T> = Result<T, Fail>;

fn bad<T>(s: impl Into<String>) -> R<T> {
    Err(Fail::Bad(s.into()))
}

pub struct Parser {
    tables: HashMap<String, Table>,
    t_ent: Option<Table>,
    t_player: Option<Table>,
    t_custom: Option<Table>,
    t_event: Option<Table>,
    t_client: Option<Table>,
    t_weapon: Option<Table>,
    pub maxclients: usize,
    pub hltv: bool,
    umsg: Vec<Option<UserMsg>>,
    baseline: Vec<Slots>,
    instanced: Vec<Slots>,
    pub players: Players,
    pub prev_seq: i32,
    pub seq: i32,
    pub base_misses: u64,
    pub odd_fields: u64,
    pub unknown_umsg: HashMap<u8, u64>,
    recent: [u8; 6],
    pub out: Vec<Msg>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            tables: HashMap::new(),
            t_ent: None,
            t_player: None,
            t_custom: None,
            t_event: None,
            t_client: None,
            t_weapon: None,
            maxclients: MAX_PLAYERS,
            hltv: false,
            umsg: (0..256).map(|_| None).collect(),
            baseline: vec![[0.0; NSLOT]; MAX_EDICTS],
            instanced: Vec::new(),
            players: [None; MAX_PLAYERS + 1],
            prev_seq: -1,
            seq: -1,
            base_misses: 0,
            odd_fields: 0,
            unknown_umsg: HashMap::new(),
            recent: [0; 6],
            out: Vec::new(),
        }
    }

    pub fn frame(&mut self, seq: i32, msgs: &[u8]) -> Result<(), Desync> {
        self.prev_seq = self.seq;
        self.seq = seq;
        let mut b = Bytes::new(msgs);
        while b.left() > 0 {
            let start = b.pos;
            let kind = b.d[b.pos];
            b.pos += 1;
            let res = self.message(kind, &mut b);
            let recent = self.recent.to_vec();
            self.recent.rotate_left(1);
            self.recent[5] = kind;
            if let Err(e) = res {
                let why = match e {
                    Fail::Short => "read past end of message block".to_string(),
                    Fail::Bad(s) => s,
                };
                return Err(Desync { kind, offset: start, recent, why });
            }
        }
        Ok(())
    }

    fn message(&mut self, kind: u8, b: &mut Bytes) -> R<()> {
        match kind {
            1 | 19 | 27 | 28 | 30 | 42 => {}
            2 | 8 | 9 | 26 | 31 | 34 | 49 | 56 | 57 => {
                b.string()?;
            }
            3 => self.bitmsg(b, |p, bb| p.event(bb))?,
            4 | 55 => b.skip(4)?,
            5 | 16 | 35 | 37 | 38 | 47 => b.skip(2)?,
            6 => self.bitmsg(b, |_, bb| {
                let flags = bb.bits(9);
                if flags & 1 != 0 {
                    bb.bits(8);
                }
                if flags & 2 != 0 {
                    bb.bits(8);
                }
                bb.bits(3 + 11);
                bb.bits(if flags & 4 != 0 { 16 } else { 8 });
                let has = [bb.bit(), bb.bit(), bb.bit()];
                for h in has {
                    if h {
                        bb.coord();
                    }
                }
                if flags & 8 != 0 {
                    bb.bits(8);
                }
                Ok(())
            })?,
            7 => self.out.push(Msg::Time(b.f32()?)),
            10 => b.skip(6)?,
            11 => {
                b.skip(12 + 16)?;
                let maxclients = b.u8()?;
                b.skip(2)?;
                b.string()?;
                b.string()?;
                let map = text(b.string()?);
                b.string()?;
                if b.u8()? != 0 {
                    let n = b.u8()? as usize;
                    b.skip(n + 16)?;
                }
                self.maxclients = (maxclients as usize).min(MAX_PLAYERS);
                self.out.push(Msg::ServerInfo { maxclients, map });
            }
            12 => {
                b.u8()?;
                b.string()?;
            }
            13 => {
                let slot = b.u8()?;
                let userid = b.i32()?;
                let info = text(b.string()?);
                b.skip(16)?;
                self.out.push(Msg::UserInfo { slot, userid, info });
            }
            14 => {
                let name = text(b.string()?);
                let (count, t) = {
                    let mut bb = Bits::new(b.rest());
                    let count = bb.bits(16) as usize;
                    let t = Table::parse_description(&mut bb, count);
                    if bb.over {
                        return Err(Fail::Short);
                    }
                    b.skip(bb.bytes_used())?;
                    (count, t)
                };
                if t.fields.len() != count {
                    return bad("delta description");
                }
                self.tables.insert(name, t);
                self.refresh_tables();
            }
            15 => {
                if !self.hltv {
                    self.bitmsg(b, |p, bb| {
                        if bb.bit() {
                            bb.bits(8);
                        }
                        p.odd_fields += p.need(&p.t_client, "clientdata_t")?.read_with(bb, |_, _| {}) as u64;
                        while bb.bit() {
                            bb.bits(6);
                            p.odd_fields += p.need(&p.t_weapon, "weapon_data_t")?.read_with(bb, |_, _| {}) as u64;
                        }
                        Ok(())
                    })?
                }
            }
            17 => self.bitmsg(b, |_, bb| {
                while bb.bit() && !bb.over {
                    bb.bits(24);
                }
                Ok(())
            })?,
            18 => b.skip(11)?,
            20 => {
                b.skip(16)?;
                if b.u8()? != 0 {
                    b.skip(5)?;
                }
            }
            21 => self.bitmsg(b, |p, bb| {
                bb.bits(10);
                p.odd_fields += p.need(&p.t_event, "event_t")?.read_with(bb, |_, _| {}) as u64;
                if bb.bit() {
                    bb.bits(16);
                }
                Ok(())
            })?,
            22 => self.bitmsg(b, |p, bb| p.baseline(bb))?,
            23 => {
                let t = b.u8()?;
                match TE_LEN.get(t as usize).copied().unwrap_or(-2) {
                    -2 => return bad(format!("temp entity {t}")),
                    -1 if t == 13 => {
                        b.skip(8)?;
                        if b.i16()? != 0 {
                            b.skip(2)?;
                        }
                    }
                    -1 => {
                        b.skip(5)?;
                        let effect = b.u8()?;
                        b.skip(14 + if effect == 2 { 2 } else { 0 })?;
                        b.string()?;
                    }
                    n => b.skip(n as usize)?,
                }
            }
            24 | 25 => b.skip(1)?,
            29 => b.skip(14)?,
            32 => b.skip(2)?,
            33 => {
                b.string()?;
                let n = b.u8()?;
                for _ in 0..n {
                    b.string()?;
                }
            }
            36 => {
                b.u8()?;
                b.string()?;
            }
            39 => {
                let id = b.u8()?;
                let size = b.u8()?;
                let name = text(b.take(16)?);
                let kind = match name.as_str() {
                    "DeathMsg" => UKind::DeathMsg,
                    "TeamInfo" => UKind::TeamInfo,
                    "TeamScore" => UKind::TeamScore,
                    "TextMsg" => UKind::TextMsg,
                    "HLTV" => UKind::Hltv,
                    "ScoreAttrib" => UKind::ScoreAttrib,
                    "RoundTime" => UKind::RoundTime,
                    "SendAudio" => UKind::SendAudio,
                    _ => UKind::Other,
                };
                self.umsg[id as usize] = Some(UserMsg { size: if size == 255 { -1 } else { size as i16 }, name, kind });
            }
            40 => self.bitmsg(b, |p, bb| p.packet(bb, false))?,
            41 => self.bitmsg(b, |p, bb| p.packet(bb, true))?,
            43 => self.bitmsg(b, |_, bb| {
                let n = bb.bits(12);
                for _ in 0..n {
                    bb.bits(4);
                    bb.skip_string();
                    bb.bits(12);
                    bb.bits(24);
                    let flags = bb.bits(3);
                    if flags & 4 != 0 {
                        bb.skip(128);
                    }
                    if bb.bit() {
                        bb.skip(256);
                    }
                    if bb.over {
                        break;
                    }
                }
                if bb.bit() {
                    while bb.bit() && !bb.over {
                        let short = bb.bit();
                        bb.bits(if short { 5 } else { 10 });
                    }
                }
                Ok(())
            })?,
            44 => {
                b.skip(97)?;
                b.string()?;
            }
            45 => b.skip(8)?,
            46 => {
                b.skip(2)?;
                b.string()?;
                b.skip(6)?;
                if b.u8()? & 4 != 0 {
                    b.skip(16)?;
                }
            }
            48 => b.skip(4)?,
            50 => {
                let mode = b.u8()?;
                if mode == 0 {
                    self.hltv = true;
                } else if mode == 1 {
                    b.skip(18)?;
                } else if mode == 2 {
                    b.string()?;
                }
            }
            51 => {
                let n = b.u8()? as usize;
                b.skip(n)?;
            }
            52 | 54 => {
                b.string()?;
                b.skip(1)?;
            }
            53 => {
                b.u8()?;
                let n = b.u16()? as usize;
                b.skip(n)?;
            }
            58 => {
                b.skip(4)?;
                b.string()?;
            }
            64.. => self.user(kind, b)?,
            _ => return bad(format!("unknown message {kind}")),
        }
        Ok(())
    }

    fn user(&mut self, id: u8, b: &mut Bytes) -> R<()> {
        let Some(u) = &self.umsg[id as usize] else {
            *self.unknown_umsg.entry(id).or_default() += 1;
            return bad(format!("unregistered user message {id}"));
        };
        let n = if u.size < 0 { b.u8()? as usize } else { u.size as usize };
        let d = b.take(n)?;
        let mut r = Bytes::new(d);
        let msg = (|| -> Result<Option<Msg>, Short> {
            Ok(match u.kind {
                UKind::Other => None,
                UKind::DeathMsg => Some(Msg::Death {
                    killer: r.u8()?,
                    victim: r.u8()?,
                    headshot: r.u8()? != 0,
                    weapon: text(r.rest()),
                }),
                UKind::TeamInfo => Some(Msg::TeamInfo { ent: r.u8()?, team: text(r.rest()) }),
                UKind::TeamScore => {
                    let team = text(r.string()?);
                    Some(Msg::TeamScore { team, score: r.i16()? })
                }
                UKind::TextMsg => {
                    let dest = r.u8()?;
                    Some(Msg::TextMsg { dest, text: text(r.rest()) })
                }
                UKind::Hltv => Some(Msg::Hltv { ent: r.u8()?, value: r.u8()? }),
                UKind::ScoreAttrib => Some(Msg::ScoreAttrib { ent: r.u8()?, flags: r.u8()? }),
                UKind::RoundTime => Some(Msg::RoundTime(r.i16()?)),
                UKind::SendAudio => {
                    r.u8()?;
                    Some(Msg::SendAudio(text(r.rest())))
                }
            })
        })();
        match msg {
            Ok(Some(m)) => self.out.push(m),
            Ok(None) => {}
            Err(_) => return bad(format!("short {} user message", u.name)),
        }
        Ok(())
    }

    fn refresh_tables(&mut self) {
        let get = |n: &str| self.tables.get(n).cloned();
        self.t_ent = get("entity_state_t");
        self.t_player = get("entity_state_player_t");
        self.t_custom = get("custom_entity_state_t");
        self.t_event = get("event_t");
        self.t_client = get("clientdata_t");
        self.t_weapon = get("weapon_data_t");
    }

    fn need<'t>(&self, t: &'t Option<Table>, name: &str) -> R<&'t Table> {
        match t {
            Some(t) => Ok(t),
            None => bad(format!("missing delta table {name}")),
        }
    }

    fn bitmsg(&mut self, b: &mut Bytes, f: impl FnOnce(&mut Parser, &mut Bits) -> R<()>) -> R<()> {
        let mut bb = Bits::new(b.rest());
        f(self, &mut bb)?;
        if bb.over {
            return Err(Fail::Short);
        }
        b.skip(bb.bytes_used())?;
        Ok(())
    }

    fn event(&mut self, bb: &mut Bits) -> R<()> {
        let n = bb.bits(5);
        for _ in 0..n {
            bb.bits(10);
            if bb.bit() {
                bb.bits(11);
                if bb.bit() {
                    self.odd_fields += self.need(&self.t_event, "event_t")?.read_with(bb, |_, _| {}) as u64;
                }
            }
            if bb.bit() {
                bb.bits(16);
            }
        }
        Ok(())
    }

    fn table(&self, num: usize, custom: bool) -> R<&Table> {
        if custom {
            self.need(&self.t_custom, "custom_entity_state_t")
        } else if num >= 1 && num <= self.maxclients {
            self.need(&self.t_player, "entity_state_player_t")
        } else {
            self.need(&self.t_ent, "entity_state_t")
        }
    }

    fn baseline(&mut self, bb: &mut Bits) -> R<()> {
        for s in self.baseline.iter_mut() {
            *s = [0.0; NSLOT];
        }
        while bb.peek(16) != 0xFFFF {
            let num = bb.bits(11) as usize;
            let custom = bb.bits(2) & 2 != 0;
            let mut s = [0.0; NSLOT];
            self.odd_fields += self.table(num, custom)?.read(bb, &mut s) as u64;
            self.baseline[num] = s;
            if bb.over {
                return Err(Fail::Short);
            }
        }
        bb.bits(16);
        let n = bb.bits(6) as usize;
        self.instanced.clear();
        for _ in 0..n {
            let mut s = [0.0; NSLOT];
            self.odd_fields += self.need(&self.t_ent, "entity_state_t")?.read(bb, &mut s) as u64;
            self.instanced.push(s);
        }
        Ok(())
    }

    fn packet(&mut self, bb: &mut Bits, delta: bool) -> R<()> {
        bb.bits(16);
        if delta {
            let from = bb.bits(8) as i32;
            if self.prev_seq < 0 || from != self.prev_seq & 0xFF {
                self.base_misses += 1;
            }
        } else {
            self.players = [None; MAX_PLAYERS + 1];
        }
        let mut list: Vec<Slots> = Vec::new();
        let mut numbase = 0usize;
        while bb.peek(16) != 0 {
            if bb.over {
                return Err(Fail::Short);
            }
            let (remove, inc) = if delta { (bb.bit(), false) } else { (false, bb.bit()) };
            let num = if inc {
                numbase + 1
            } else if bb.bit() {
                bb.bits(11) as usize
            } else {
                numbase + bb.bits(6) as usize
            };
            numbase = num;
            if num >= MAX_EDICTS {
                return bad(format!("entity {num}"));
            }
            let player = num >= 1 && num <= self.maxclients;
            if remove {
                if player {
                    self.players[num] = None;
                }
                continue;
            }
            let custom = bb.bit();
            let mut newbl = None;
            if !self.instanced.is_empty() && bb.bit() {
                newbl = Some(bb.bits(6) as usize);
            }
            let mut offset = 0;
            if !delta && newbl.is_none() && bb.bit() {
                offset = bb.bits(6) as usize;
            }
            let mut s = match (newbl, offset, player.then(|| self.players[num]).flatten()) {
                (_, _, Some(old)) if delta => old,
                (Some(i), _, _) => self.instanced.get(i).copied().unwrap_or([0.0; NSLOT]),
                (None, o, _) if o > 0 => {
                    if o > list.len() {
                        return bad("baseline offset");
                    }
                    list[list.len() - o]
                }
                _ => self.baseline[num],
            };
            let t = self.table(num, custom)?;
            self.odd_fields += t.read(bb, &mut s) as u64;
            if !delta {
                list.push(s);
            }
            if player {
                self.players[num] = Some(s);
            }
        }
        bb.bits(16);
        self.out.push(Msg::Entities);
        Ok(())
    }
}
