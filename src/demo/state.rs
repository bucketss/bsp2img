use super::delta::{EFFECTS, SOLID};
use super::messages::{MAX_PLAYERS, Msg, Parser};

const EF_NODRAW: u32 = 128;
const SOLID_SLIDEBOX: f32 = 3.0;
const MAX_FREEZE: f32 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash, PartialOrd, Ord)]
pub enum Team {
    #[default]
    None,
    T,
    Ct,
    Spec,
}

impl Team {
    pub fn key(self) -> &'static str {
        match self {
            Team::T => "T",
            Team::Ct => "CT",
            Team::Spec => "SPEC",
            Team::None => "",
        }
    }

    pub fn playing(self) -> bool {
        matches!(self, Team::T | Team::Ct)
    }
}

#[derive(Clone, Debug)]
pub struct Death {
    pub time: f32,
    pub round: u32,
    pub killer: Option<String>,
    pub victim: String,
    pub killer_ent: u8,
    pub victim_ent: u8,
    pub killer_team: Team,
    pub victim_team: Team,
    pub weapon: String,
    pub headshot: bool,
    pub killer_pos: Option<[f32; 3]>,
    pub victim_pos: Option<[f32; 3]>,
}

#[derive(Clone, Debug)]
pub struct Presence {
    pub round: u32,
    pub team: Team,
    pub pos: [f32; 3],
}

#[derive(Clone, Debug)]
pub enum Event {
    Death(Death),
    Presence(Presence),
    RoundEnd { winner: Option<Team> },
}

#[derive(Clone, Default)]
struct Player {
    name: String,
    team: Team,
    model_team: Team,
    seen: Option<([f32; 3], f32)>,
}

pub struct Game {
    players: Vec<Player>,
    pub round: u32,
    round_used: bool,
    ended: bool,
    frozen_since: Option<f32>,
    roundtime_now: bool,
    pub map: String,
    next_sample: f32,
    pub every: f32,
    pub time: f32,
}

fn winner_of(text: &str) -> Option<Option<Team>> {
    Some(match text {
        "#Terrorists_Win" | "#Target_Bombed" | "#VIP_Assassinated" | "#Terrorists_Escaped" | "#VIP_Not_Escaped"
        | "#Hostages_Not_Rescued" => Some(Team::T),
        "#CTs_Win" | "#Bomb_Defused" | "#VIP_Escaped" | "#CTs_PreventEscape" | "#Escaping_Terrorists_Neutralized"
        | "#All_Hostages_Rescued" | "#Target_Saved" | "#Terrorists_Not_Escaped" => Some(Team::Ct),
        "#Round_Draw" | "#Game_Commencing" => None,
        _ => return None,
    })
}

fn team_of_model(model: &str) -> Team {
    match model.to_ascii_lowercase().as_str() {
        "terror" | "leet" | "arctic" | "guerilla" | "militia" => Team::T,
        "urban" | "gsg9" | "sas" | "gign" | "spetsnaz" | "vip" => Team::Ct,
        _ => Team::None,
    }
}

fn info_value(info: &str, key: &str) -> Option<String> {
    let mut it = info.trim_start_matches('\\').split('\\');
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k == key {
            return Some(v.to_string());
        }
    }
    None
}

impl Game {
    pub fn new(every: f32) -> Game {
        Game {
            players: vec![Player::default(); MAX_PLAYERS + 1],
            round: 1,
            round_used: false,
            ended: false,
            frozen_since: None,
            roundtime_now: false,
            map: String::new(),
            next_sample: 0.0,
            every,
            time: 0.0,
        }
    }

    fn pos(&self, ent: u8) -> Option<[f32; 3]> {
        let (pos, t) = self.players.get(ent as usize)?.seen?;
        (self.time - t <= 1.0).then_some(pos)
    }

    fn name(&self, ent: u8) -> String {
        self.players.get(ent as usize).map(|p| p.name.clone()).unwrap_or_default()
    }

    fn team(&self, ent: u8) -> Team {
        self.players.get(ent as usize).map_or(Team::None, |p| if p.team == Team::None { p.model_team } else { p.team })
    }

    fn end(&mut self, winner: Option<Team>, out: &mut dyn FnMut(Event)) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.round_used = true;
        out(Event::RoundEnd { winner });
    }

    pub fn frame(&mut self, time: f32, p: &mut Parser, out: &mut dyn FnMut(Event)) {
        self.time = time;
        self.roundtime_now = false;
        for (ent, s) in p.players.iter().enumerate().take(p.maxclients.min(MAX_PLAYERS) + 1) {
            if let Some(s) = s {
                self.players[ent].seen = Some(([s[0], s[1], s[2]], time));
            }
        }
        let msgs = std::mem::take(&mut p.out);
        for m in &msgs {
            match m {
                Msg::ServerInfo { map, .. } => {
                    self.map = map.trim_start_matches("maps/").trim_end_matches(".bsp").to_string();
                }
                Msg::UserInfo { slot, info, .. } => {
                    if let Some(pl) = self.players.get_mut(*slot as usize + 1) {
                        pl.name = info_value(info, "name").unwrap_or_default();
                        pl.model_team = team_of_model(&info_value(info, "model").unwrap_or_default());
                    }
                }
                Msg::TeamInfo { ent, team } => {
                    if let Some(pl) = self.players.get_mut(*ent as usize) {
                        pl.team = match team.as_str() {
                            "TERRORIST" => Team::T,
                            "CT" => Team::Ct,
                            "SPECTATOR" => Team::Spec,
                            _ => Team::None,
                        };
                    }
                }
                Msg::Hltv { ent: 0, value } if *value & 128 != 0 => {
                    if self.round_used {
                        self.round += 1;
                    }
                    self.round_used = false;
                    self.ended = false;
                    self.frozen_since = (!self.roundtime_now).then_some(time);
                }
                Msg::RoundTime => {
                    self.frozen_since = None;
                    self.roundtime_now = true;
                }
                Msg::SendAudio(s) => match s.as_str() {
                    "%!MRAD_terwin" => self.end(Some(Team::T), out),
                    "%!MRAD_ctwin" => self.end(Some(Team::Ct), out),
                    "%!MRAD_rounddraw" => self.end(None, out),
                    _ => {}
                },
                Msg::TextMsg { text, .. } => {
                    if let Some(w) = winner_of(text) {
                        self.end(w, out);
                    }
                }
                Msg::Death { killer, victim, headshot, weapon } => {
                    self.round_used = true;
                    let d = Death {
                        time,
                        round: self.round,
                        killer: (*killer != 0).then(|| self.name(*killer)),
                        victim: self.name(*victim),
                        killer_ent: *killer,
                        victim_ent: *victim,
                        killer_team: self.team(*killer),
                        victim_team: self.team(*victim),
                        weapon: weapon.clone(),
                        headshot: *headshot,
                        killer_pos: if *killer != 0 { self.pos(*killer) } else { None },
                        victim_pos: self.pos(*victim),
                    };
                    out(Event::Death(d));
                }
                _ => {}
            }
        }
        p.out = msgs;
        p.out.clear();
        if self.every > 0.0 && time >= self.next_sample {
            self.next_sample = if time - self.next_sample > self.every { time + self.every } else { self.next_sample + self.every };
            if self.frozen_since.is_none_or(|t| time - t > MAX_FREEZE) {
                for ent in 1..=p.maxclients.min(MAX_PLAYERS) {
                    let team = self.team(ent as u8);
                    let Some(s) = p.players[ent] else { continue };
                    if !team.playing() || s[SOLID] != SOLID_SLIDEBOX || s[EFFECTS] as u32 & EF_NODRAW != 0 {
                        continue;
                    }
                    out(Event::Presence(Presence { round: self.round, team, pos: [s[0], s[1], s[2]] }));
                }
            }
        }
    }
}
