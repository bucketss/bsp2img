use super::delta::{EFFECTS, USEHULL};
use super::messages::{MAX_PLAYERS, Msg, Parser};

const EF_NODRAW: u32 = 128;

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
    pub time: f32,
    pub round: u32,
    pub ent: u8,
    pub team: Team,
    pub pos: [f32; 3],
    pub ducked: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Death(Death),
    Presence(Presence),
    RoundStart { time: f32, round: u32 },
    RoundEnd { time: f32, round: u32, winner: Option<Team>, reason: String },
}

#[derive(Clone, Default)]
struct Player {
    name: String,
    team: Team,
    alive: bool,
    seen: Option<([f32; 3], f32)>,
}

pub struct Game {
    players: Vec<Player>,
    pub round: u32,
    round_used: bool,
    ended: bool,
    frozen: bool,
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
            frozen: false,
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
        self.players.get(ent as usize).map_or(Team::None, |p| p.team)
    }

    fn end(&mut self, winner: Option<Team>, reason: String, out: &mut dyn FnMut(Event)) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.round_used = true;
        out(Event::RoundEnd { time: self.time, round: self.round, winner, reason });
    }

    pub fn frame(&mut self, time: f32, p: &mut Parser, out: &mut dyn FnMut(Event)) {
        self.time = time;
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
                        if !pl.team.playing() {
                            pl.alive = false;
                        }
                    }
                }
                Msg::Hltv { ent: 0, value } if *value & 128 != 0 => {
                    if self.round_used {
                        self.round += 1;
                    }
                    self.round_used = false;
                    self.ended = false;
                    self.frozen = true;
                    for pl in self.players.iter_mut() {
                        pl.alive = pl.team.playing();
                    }
                    out(Event::RoundStart { time, round: self.round });
                }
                Msg::Hltv { ent, value } if *ent > 0 && *value & 128 != 0 => {
                    if let Some(pl) = self.players.get_mut(*ent as usize) {
                        pl.alive = *value & 127 > 0 && pl.team.playing();
                    }
                }
                Msg::ScoreAttrib { ent, flags } => {
                    if let Some(pl) = self.players.get_mut(*ent as usize)
                        && flags & 1 != 0
                    {
                        pl.alive = false;
                    }
                }
                Msg::RoundTime(_) if self.frozen => {
                    self.frozen = false;
                }
                Msg::SendAudio(s) => match s.as_str() {
                    "%!MRAD_terwin" => self.end(Some(Team::T), String::new(), out),
                    "%!MRAD_ctwin" => self.end(Some(Team::Ct), String::new(), out),
                    "%!MRAD_rounddraw" => self.end(None, String::new(), out),
                    _ => {}
                },
                Msg::TextMsg { text, .. } => {
                    if let Some(w) = winner_of(text) {
                        self.end(w, text.trim_start_matches('#').to_string(), out);
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
                    if let Some(pl) = self.players.get_mut(*victim as usize) {
                        pl.alive = false;
                    }
                    out(Event::Death(d));
                }
                _ => {}
            }
        }
        p.out = msgs;
        p.out.clear();
        if self.every > 0.0 && time >= self.next_sample {
            self.next_sample = if time - self.next_sample > self.every { time + self.every } else { self.next_sample + self.every };
            if !self.frozen {
                for ent in 1..=p.maxclients.min(MAX_PLAYERS) {
                    let pl = &self.players[ent];
                    let Some(s) = p.players[ent] else { continue };
                    if !pl.alive || !pl.team.playing() || s[EFFECTS] as u32 & EF_NODRAW != 0 {
                        continue;
                    }
                    out(Event::Presence(Presence {
                        time,
                        round: self.round,
                        ent: ent as u8,
                        team: pl.team,
                        pos: [s[0], s[1], s[2]],
                        ducked: s[USEHULL] != 0.0,
                    }));
                }
            }
        }
    }
}
