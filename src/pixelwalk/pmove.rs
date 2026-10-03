use super::hull::{PLAYER_MINS, PwMap};
use super::trace::{PmTrace, player_trace};
use super::vec::{V3, angle_vectors, cross, dot, length, normalize};
use super::world::Physent;

const MOVETYPE_WALK: i32 = 3;
const MAX_CLIP_PLANES: usize = 5;
const STOP_EPSILON: f32 = 0.1;

pub struct Pm<'a> {
    pub map: &'a PwMap,
    pub phys: &'a [Physent],
    pub usehull: usize,
    pub eps: f32,
    pub origin: V3,
    pub velocity: V3,
    pub basevelocity: V3,
    pub angles: V3,
    pub forward: V3,
    pub right: V3,
    pub up: V3,
    pub fmove: f32,
    pub smove: f32,
    pub frametime: f32,
    pub gravity: f32,
    pub friction: f32,
    pub onground: i32,
    pub blocked: i32,
    pub floor_ent: i32,
    pub wall_ent: i32,
    pub movetype: i32,
    pub waterlevel: i32,
    pub waterjumptime: f32,
    pub dead: bool,
    pub maxspeed: f32,
    pub mv_gravity: f32,
    pub mv_bounce: f32,
    pub mv_accelerate: f32,
    pub mv_airaccelerate: f32,
    pub mv_stopspeed: f32,
    pub mv_friction: f32,
    pub mv_edgefriction: f32,
    pub mv_stepsize: f32,
    pub mv_maxvelocity: f32,
}

impl<'a> Pm<'a> {
    pub fn new(map: &'a PwMap, phys: &'a [Physent], usehull: usize, eps: f32) -> Pm<'a> {
        Pm {
            map,
            phys,
            usehull,
            eps,
            origin: [0.0; 3],
            velocity: [0.0; 3],
            basevelocity: [0.0; 3],
            angles: [0.0; 3],
            forward: [0.0; 3],
            right: [0.0; 3],
            up: [0.0; 3],
            fmove: 0.0,
            smove: 0.0,
            frametime: 0.01,
            gravity: 1.0,
            friction: 1.0,
            onground: -1,
            blocked: 0,
            floor_ent: -1,
            wall_ent: -1,
            movetype: MOVETYPE_WALK,
            waterlevel: 0,
            waterjumptime: 0.0,
            dead: false,
            maxspeed: 250.0,
            mv_gravity: 800.0,
            mv_bounce: 1.0,
            mv_accelerate: 5.0,
            mv_airaccelerate: 10.0,
            mv_stopspeed: 75.0,
            mv_friction: 4.0,
            mv_edgefriction: 2.0,
            mv_stepsize: 18.0,
            mv_maxvelocity: 2000.0,
        }
    }

    fn trace(&self, start: &V3, end: &V3) -> PmTrace {
        player_trace(self.map, self.phys, self.usehull, self.eps, start, end)
    }

    fn check_velocity(&mut self) {
        let mx = self.mv_maxvelocity;
        for i in 0..3 {
            if self.velocity[i].is_nan() {
                self.velocity[i] = 0.0;
            }
            if self.origin[i].is_nan() {
                self.origin[i] = 0.0;
            }
            if self.velocity[i] > mx {
                self.velocity[i] = mx;
            } else if self.velocity[i] < -mx {
                self.velocity[i] = -mx;
            }
        }
    }

    fn add_correct_gravity(&mut self) {
        if self.waterjumptime != 0.0 {
            return;
        }
        let g = if self.gravity != 0.0 { self.gravity } else { 1.0 };
        self.velocity[2] -= g * self.mv_gravity * 0.5 * self.frametime;
        self.velocity[2] += self.basevelocity[2] * self.frametime;
        self.basevelocity[2] = 0.0;
        self.check_velocity();
    }

    fn fixup_gravity_velocity(&mut self) {
        if self.waterjumptime != 0.0 {
            return;
        }
        let g = if self.gravity != 0.0 { self.gravity } else { 1.0 };
        self.velocity[2] -= self.mv_gravity * self.frametime * g * 0.5;
        self.check_velocity();
    }

    fn fly_move(&mut self) -> i32 {
        let numbumps = 4;
        let mut blocked = 0;
        let mut numplanes = 0usize;
        let mut planes = [[0f32; 3]; MAX_CLIP_PLANES];
        let mut original_velocity = self.velocity;
        let primal_velocity = self.velocity;
        let mut new_velocity = [0f32; 3];
        let mut all_fraction = 0.0f32;
        let mut time_left = self.frametime;
        for _ in 0..numbumps {
            if self.velocity[0] == 0.0 && self.velocity[1] == 0.0 && self.velocity[2] == 0.0 {
                break;
            }
            let end: V3 = std::array::from_fn(|i| self.origin[i] + time_left * self.velocity[i]);
            let trace = self.trace(&self.origin.clone(), &end);
            all_fraction += trace.fraction;
            if trace.allsolid {
                self.velocity = [0.0; 3];
                self.blocked |= 4;
                return 4;
            }
            if trace.fraction > 0.0 {
                self.origin = trace.endpos;
                original_velocity = self.velocity;
                numplanes = 0;
            }
            if trace.fraction == 1.0 {
                break;
            }
            if trace.normal[2] > 0.7 {
                blocked |= 1;
                self.floor_ent = trace.ent;
            } else {
                self.wall_ent = trace.ent;
            }
            if trace.normal[2] == 0.0 {
                blocked |= 2;
            }
            time_left -= time_left * trace.fraction;
            if numplanes >= MAX_CLIP_PLANES {
                self.velocity = [0.0; 3];
                break;
            }
            planes[numplanes] = trace.normal;
            numplanes += 1;
            if numplanes == 1 && self.movetype == MOVETYPE_WALK && (self.onground == -1 || self.friction != 1.0) {
                for plane in planes.iter().take(numplanes) {
                    if plane[2] > 0.7 {
                        clip_velocity(&original_velocity, plane, &mut new_velocity, 1.0);
                        original_velocity = new_velocity;
                    } else {
                        clip_velocity(&original_velocity, plane, &mut new_velocity, 1.0 + self.mv_bounce * (1.0 - self.friction));
                    }
                }
                self.velocity = new_velocity;
                original_velocity = new_velocity;
            } else {
                let mut i = 0;
                while i < numplanes {
                    let mut v = [0f32; 3];
                    clip_velocity(&original_velocity, &planes[i], &mut v, 1.0);
                    self.velocity = v;
                    let mut j = 0;
                    while j < numplanes {
                        if j != i && dot(&self.velocity, &planes[j]) < 0.0 {
                            break;
                        }
                        j += 1;
                    }
                    if j == numplanes {
                        break;
                    }
                    i += 1;
                }
                if i == numplanes {
                    if numplanes != 2 {
                        self.velocity = [0.0; 3];
                        break;
                    }
                    let dir = cross(&planes[0], &planes[1]);
                    let d = dot(&dir, &self.velocity);
                    self.velocity = [dir[0] * d, dir[1] * d, dir[2] * d];
                }
                if dot(&self.velocity, &primal_velocity) <= 0.0 {
                    self.velocity = [0.0; 3];
                    break;
                }
            }
        }
        if all_fraction == 0.0 {
            self.velocity = [0.0; 3];
        }
        self.blocked |= blocked;
        blocked
    }

    pub fn categorize_position(&mut self) {
        let point = [self.origin[0], self.origin[1], self.origin[2] - 2.0];
        if self.velocity[2] > 180.0 {
            self.onground = -1;
            return;
        }
        let tr = self.trace(&self.origin.clone(), &point);
        self.onground = if tr.normal[2] < 0.7 { -1 } else { tr.ent };
        if self.onground != -1 {
            self.waterjumptime = 0.0;
            if self.waterlevel < 2 && !tr.startsolid && !tr.allsolid {
                self.origin = tr.endpos;
            }
        }
    }

    fn accelerate(&mut self, wishdir: &V3, wishspeed: f32, accel: f32) {
        if self.dead || self.waterjumptime != 0.0 {
            return;
        }
        let currentspeed = dot(&self.velocity, wishdir);
        let addspeed = wishspeed - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        let mut accelspeed = accel * self.frametime * wishspeed * self.friction;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            self.velocity[i] += accelspeed * wishdir[i];
        }
    }

    fn air_accelerate(&mut self, wishdir: &V3, wishspeed: f32, accel: f32) {
        if self.dead || self.waterjumptime != 0.0 {
            return;
        }
        let wishspd = if wishspeed > 30.0 { 30.0 } else { wishspeed };
        let currentspeed = dot(&self.velocity, wishdir);
        let addspeed = wishspd - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        let mut accelspeed = accel * wishspeed * self.frametime * self.friction;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            self.velocity[i] += accelspeed * wishdir[i];
        }
    }

    fn friction_step(&mut self) {
        if self.waterjumptime != 0.0 {
            return;
        }
        let vel = self.velocity;
        let speed = (vel[0] * vel[0] + vel[1] * vel[1] + vel[2] * vel[2]).sqrt();
        if speed < 0.1 {
            return;
        }
        let mut drop = 0.0f32;
        if self.onground != -1 {
            let sx = self.origin[0] + vel[0] / speed * 16.0;
            let sy = self.origin[1] + vel[1] / speed * 16.0;
            let sz = self.origin[2] + PLAYER_MINS[self.usehull][2];
            let start = [sx, sy, sz];
            let stop = [sx, sy, sz - 34.0];
            let tr = self.trace(&start, &stop);
            let mut friction = if tr.fraction == 1.0 { self.mv_friction * self.mv_edgefriction } else { self.mv_friction };
            friction *= self.friction;
            let control = if speed < self.mv_stopspeed { self.mv_stopspeed } else { speed };
            drop += friction * control * self.frametime;
        }
        let mut newspeed = speed - drop;
        if newspeed < 0.0 {
            newspeed = 0.0;
        }
        newspeed /= speed;
        for v in self.velocity.iter_mut() {
            *v *= newspeed;
        }
    }

    fn wish(&mut self) -> (V3, V3, f32) {
        self.forward[2] = 0.0;
        self.right[2] = 0.0;
        normalize(&mut self.forward);
        normalize(&mut self.right);
        let mut wishvel = [0f32; 3];
        for i in 0..2 {
            wishvel[i] = self.forward[i] * self.fmove + self.right[i] * self.smove;
        }
        wishvel[2] = 0.0;
        let mut wishdir = wishvel;
        let mut wishspeed = normalize(&mut wishdir);
        if wishspeed > self.maxspeed {
            let s = self.maxspeed / wishspeed;
            wishvel = [wishvel[0] * s, wishvel[1] * s, wishvel[2] * s];
            wishspeed = self.maxspeed;
        }
        (wishvel, wishdir, wishspeed)
    }

    fn air_move(&mut self) {
        let (_, wishdir, wishspeed) = self.wish();
        self.air_accelerate(&wishdir, wishspeed, self.mv_airaccelerate);
        for i in 0..3 {
            self.velocity[i] += self.basevelocity[i];
        }
        self.fly_move();
    }

    fn walk_move(&mut self) {
        let (_, wishdir, wishspeed) = self.wish();
        self.velocity[2] = 0.0;
        self.accelerate(&wishdir, wishspeed, self.mv_accelerate);
        self.velocity[2] = 0.0;
        for i in 0..3 {
            self.velocity[i] += self.basevelocity[i];
        }
        if length(&self.velocity) < 1.0 {
            self.velocity = [0.0; 3];
            return;
        }
        let oldonground = self.onground;
        let mut dest = [
            self.origin[0] + self.velocity[0] * self.frametime,
            self.origin[1] + self.velocity[1] * self.frametime,
            self.origin[2],
        ];
        let trace = self.trace(&self.origin.clone(), &dest);
        if trace.fraction == 1.0 {
            self.origin = trace.endpos;
            return;
        }
        if oldonground == -1 && self.waterlevel == 0 {
            return;
        }
        if self.waterjumptime != 0.0 {
            return;
        }
        let original = self.origin;
        let originalvel = self.velocity;
        self.fly_move();
        let down = self.origin;
        let downvel = self.velocity;
        self.origin = original;
        self.velocity = originalvel;
        dest = self.origin;
        dest[2] += self.mv_stepsize;
        let trace = self.trace(&self.origin.clone(), &dest);
        if !trace.startsolid && !trace.allsolid {
            self.origin = trace.endpos;
        }
        self.fly_move();
        dest = self.origin;
        dest[2] -= self.mv_stepsize;
        let trace = self.trace(&self.origin.clone(), &dest);
        let mut usedown = trace.normal[2] < 0.7;
        if !usedown {
            if !trace.startsolid && !trace.allsolid {
                self.origin = trace.endpos;
            }
            self.up = self.origin;
            let downdist = (down[0] - original[0]) * (down[0] - original[0]) + (down[1] - original[1]) * (down[1] - original[1]);
            let updist =
                (self.up[0] - original[0]) * (self.up[0] - original[0]) + (self.up[1] - original[1]) * (self.up[1] - original[1]);
            if downdist > updist {
                usedown = true;
            } else {
                self.velocity[2] = downvel[2];
            }
        }
        if usedown {
            self.origin = down;
            self.velocity = downvel;
        }
    }

    pub fn player_move_frame(&mut self) {
        self.blocked = 0;
        self.floor_ent = -1;
        self.wall_ent = -1;
        let (f, r, u) = angle_vectors(&self.angles);
        self.forward = f;
        self.right = r;
        self.up = u;
        self.add_correct_gravity();
        if self.onground != -1 {
            self.velocity[2] = 0.0;
            self.friction_step();
        }
        self.check_velocity();
        if self.onground != -1 {
            self.walk_move();
        } else {
            self.air_move();
        }
        self.categorize_position();
        self.check_velocity();
        self.fixup_gravity_velocity();
        if self.onground != -1 {
            self.velocity[2] = 0.0;
        }
    }
}

fn clip_velocity(input: &V3, normal: &V3, out: &mut V3, overbounce: f32) -> i32 {
    let angle = normal[2];
    let mut blocked = 0;
    if angle > 0.0 {
        blocked |= 1;
    }
    if angle == 0.0 {
        blocked |= 2;
    }
    let backoff = dot(input, normal) * overbounce;
    for i in 0..3 {
        let change = input[i] - normal[i] * backoff;
        out[i] = change;
        if out[i] > -STOP_EPSILON && out[i] < STOP_EPSILON {
            out[i] = 0.0;
        }
    }
    blocked
}
