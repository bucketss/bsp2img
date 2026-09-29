use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use demoparser::first_pass::parser_settings::{FirstPassParser, ParserInputs, rm_user_friendly_names};
use demoparser::parse_demo::{DemoOutput, Parser, ParsingMode};
use demoparser::second_pass::game_events::GameEvent;
use demoparser::second_pass::parser_settings::create_huffman_lookup_table;
use demoparser::second_pass::variants::{VarVec, Variant};

use super::Summary;
use super::state::{Death, Event, Presence, Team};
use crate::nav::STAND_OFS;

pub const MAGIC: &[u8; 8] = b"PBDEMS2\0";
pub const TICK_RATE: f32 = 64.0;
const HEADER_BYTES: u64 = 1 << 20;
const EVENTS: [&str; 3] = ["player_death", "round_end", "round_officially_ended"];
const DEATH_PROPS: [&str; 5] = ["X", "Y", "Z", "team_num", "entity_id"];
const TICK_PROPS: [&str; 7] = ["X", "Y", "Z", "team_num", "is_alive", "is_warmup_period", "is_freeze_period"];

fn settings(huf: &Vec<(u8, u8)>) -> ParserInputs<'_> {
    ParserInputs {
        real_name_to_og_name: Default::default(),
        wanted_players: Vec::new(),
        wanted_player_props: Vec::new(),
        wanted_other_props: Vec::new(),
        wanted_prop_states: Default::default(),
        wanted_ticks: Vec::new(),
        wanted_events: Vec::new(),
        parse_ents: true,
        parse_projectiles: false,
        parse_grenades: false,
        only_header: false,
        only_convars: false,
        huffman_lookup_table: huf,
        order_by_steamid: false,
        list_props: false,
        fallback_bytes: None,
    }
}

fn want(s: &mut ParserInputs, player: &[&str], other: &[&str]) -> Result<()> {
    let real = |v: &[&str]| -> Result<(Vec<String>, Vec<String>)> {
        let og: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        Ok((rm_user_friendly_names(&og)?, og))
    };
    let (p, po) = real(player)?;
    let (o, oo) = real(other)?;
    s.real_name_to_og_name = p.iter().chain(&o).cloned().zip(po.into_iter().chain(oo)).collect();
    s.wanted_player_props = p;
    s.wanted_other_props = o;
    Ok(())
}

pub fn header(path: &Path) -> Result<String> {
    let mut b = Vec::new();
    File::open(path).with_context(|| format!("open {}", path.display()))?.take(HEADER_BYTES).read_to_end(&mut b)?;
    let huf = Vec::new();
    let mut s = settings(&huf);
    s.parse_ents = false;
    s.only_header = true;
    let h = FirstPassParser::new(&s).parse_header_only(&b)?;
    h.get("map_name").filter(|m| !m.is_empty()).cloned().context("no map name in the demo header")
}

fn field<'a>(e: &'a GameEvent, k: &str) -> Option<&'a Variant> {
    e.fields.iter().find(|f| f.name == k)?.data.as_ref()
}

fn num(v: Option<&Variant>) -> Option<f64> {
    Some(match v? {
        Variant::F32(x) => *x as f64,
        Variant::I32(x) => *x as f64,
        Variant::U32(x) => *x as f64,
        Variant::U64(x) => *x as f64,
        Variant::Bool(x) => *x as u8 as f64,
        _ => return None,
    })
}

fn text(e: &GameEvent, k: &str) -> Option<String> {
    match field(e, k)? {
        Variant::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn team_of(n: Option<f64>) -> Team {
    match n {
        Some(2.0) => Team::T,
        Some(3.0) => Team::Ct,
        Some(1.0) => Team::Spec,
        _ => Team::None,
    }
}

fn winner(v: Option<&Variant>) -> Option<Team> {
    let t = match v? {
        Variant::String(s) => match s.to_ascii_uppercase().as_str() {
            "T" => Team::T,
            "CT" => Team::Ct,
            _ => Team::None,
        },
        v => team_of(num(Some(v))),
    };
    t.playing().then_some(t)
}

fn death(e: &GameEvent, round: u32, ids: &mut HashMap<i32, u16>) -> Death {
    let pos = |p: &str| -> Option<[f32; 3]> {
        let c = |a: &str| num(field(e, &format!("{p}_{a}")));
        Some([c("X")? as f32, c("Y")? as f32, (c("Z")? + STAND_OFS) as f32])
    };
    let mut ent = |p: &str| {
        num(field(e, &format!("{p}_entity_id"))).map_or(0, |v| {
            let n = ids.len().min(65534) as u16 + 1;
            *ids.entry(v as i32).or_insert(n)
        })
    };
    let (killer_ent, victim_ent) = (ent("attacker"), ent("user"));
    Death {
        time: e.tick as f32 / TICK_RATE,
        round,
        killer: (killer_ent != 0).then(|| text(e, "attacker_name").unwrap_or_default()),
        victim: text(e, "user_name").unwrap_or_default(),
        killer_ent,
        victim_ent,
        killer_team: team_of(num(field(e, "attacker_team_num"))),
        victim_team: team_of(num(field(e, "user_team_num"))),
        weapon: text(e, "weapon").unwrap_or_default(),
        headshot: matches!(field(e, "headshot"), Some(Variant::Bool(true))),
        killer_pos: if killer_ent != 0 { pos("attacker") } else { None },
        victim_pos: pos("user"),
    }
}

fn column(v: &VarVec) -> Vec<Option<f64>> {
    match v {
        VarVec::F32(x) => x.iter().map(|v| v.map(|v| v as f64)).collect(),
        VarVec::I32(x) => x.iter().map(|v| v.map(|v| v as f64)).collect(),
        VarVec::U32(x) => x.iter().map(|v| v.map(|v| v as f64)).collect(),
        VarVec::U64(x) => x.iter().map(|v| v.map(|v| v as f64)).collect(),
        VarVec::Bool(x) => x.iter().map(|v| v.map(|v| v as u8 as f64)).collect(),
        _ => Vec::new(),
    }
}

fn samples(bytes: &[u8], huf: &Vec<(u8, u8)>, ticks: Vec<i32>) -> Result<Vec<(i32, Team, [f32; 3])>> {
    let mut s = settings(huf);
    want(&mut s, &TICK_PROPS, &[])?;
    s.wanted_ticks = ticks;
    let out: DemoOutput = Parser::new(s, ParsingMode::Normal).parse_demo(bytes)?;
    let col = |name: &str| -> Vec<Option<f64>> {
        out.prop_controller
            .prop_infos
            .iter()
            .find(|p| p.prop_friendly_name == name)
            .and_then(|p| out.df.get(&p.id))
            .and_then(|c| c.data.as_ref())
            .map(column)
            .unwrap_or_default()
    };
    let [tick, x, y, z, team, alive, warmup, frozen] =
        ["tick", "X", "Y", "Z", "team_num", "is_alive", "is_warmup_period", "is_freeze_period"].map(col);
    if tick.is_empty() {
        bail!("no player positions");
    }
    let at = |v: &Vec<Option<f64>>, i: usize| v.get(i).copied().flatten();
    Ok((0..tick.len())
        .filter_map(|i| {
            let t = team_of(at(&team, i));
            if !t.playing() || at(&alive, i) != Some(1.0) || at(&warmup, i) == Some(1.0) || at(&frozen, i) == Some(1.0) {
                return None;
            }
            Some((at(&tick, i)? as i32, t, [at(&x, i)? as f32, at(&y, i)? as f32, (at(&z, i)? + STAND_OFS) as f32]))
        })
        .collect())
}

pub fn scan(path: &Path, presence_every: f32, cancel: &dyn Fn() -> bool, on_event: &mut dyn FnMut(Event)) -> Result<Summary> {
    let map = header(path)?;
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let bytes = unsafe { memmap2::Mmap::map(&f) }.with_context(|| format!("map {}", path.display()))?;
    let huf = create_huffman_lookup_table();
    if cancel() {
        bail!("cancelled");
    }
    let mut s = settings(&huf);
    want(&mut s, &DEATH_PROPS, &["is_warmup_period"])?;
    s.wanted_events = EVENTS.map(String::from).to_vec();
    let out = Parser::new(s, ParsingMode::Normal).parse_demo(&bytes)?;
    let mut events: Vec<&GameEvent> = out.game_events.iter().collect();
    events.sort_by_key(|e| e.tick);
    let (mut round, mut used, mut ended, mut last) = (1u32, false, false, 0i32);
    let mut starts = vec![(i32::MIN, 1u32)];
    let mut ids = HashMap::new();
    for e in events {
        last = last.max(e.tick);
        if num(field(e, "is_warmup_period")) == Some(1.0) {
            continue;
        }
        match e.name.as_str() {
            "round_officially_ended" => {
                if used {
                    round += 1;
                    starts.push((e.tick, round));
                }
                used = false;
                ended = false;
            }
            "round_end" if !ended => {
                ended = true;
                used = true;
                on_event(Event::RoundEnd { winner: winner(field(e, "winner")) });
            }
            "player_death" => {
                used = true;
                on_event(Event::Death(death(e, round, &mut ids)));
            }
            _ => {}
        }
    }
    drop(out);
    let mut sum = Summary { map, frames: last.max(0) as u64, seconds: last as f32 / TICK_RATE, rounds: round, ..Default::default() };
    if presence_every > 0.0 {
        if cancel() {
            bail!("cancelled");
        }
        let step = ((presence_every * TICK_RATE).round() as i32).max(1);
        match samples(&bytes, &huf, (0..=last / step).map(|k| k * step).collect()) {
            Ok(rows) => {
                for (tick, team, pos) in rows {
                    let round = starts[starts.partition_point(|s| s.0 <= tick).max(1) - 1].1;
                    on_event(Event::Presence(Presence { round, team, pos }));
                }
            }
            Err(e) => sum.desyncs.push(format!("player positions: {e:#}")),
        }
    }
    if cancel() {
        bail!("cancelled");
    }
    Ok(sum)
}
