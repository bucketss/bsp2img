use super::bits::{Bits, text};

pub const DT_BYTE: u32 = 1;
pub const DT_SHORT: u32 = 2;
pub const DT_FLOAT: u32 = 4;
pub const DT_INTEGER: u32 = 8;
pub const DT_ANGLE: u32 = 16;
pub const DT_TIMEWINDOW_8: u32 = 32;
pub const DT_TIMEWINDOW_BIG: u32 = 64;
pub const DT_STRING: u32 = 128;
pub const DT_SIGNED: u32 = 0x8000_0000;

pub const SLOT_NAMES: [&str; 10] = [
    "origin[0]",
    "origin[1]",
    "origin[2]",
    "angles[0]",
    "angles[1]",
    "angles[2]",
    "effects",
    "solid",
    "sequence",
    "gaitsequence",
];
pub const NSLOT: usize = SLOT_NAMES.len();
pub const EFFECTS: usize = 6;
pub const SOLID: usize = 7;
const NONE: u8 = u8::MAX;

pub type Slots = [f32; NSLOT];

#[derive(Clone, Debug)]
pub struct Field {
    pub kind: u32,
    pub bits: u32,
    pub pre: f64,
    pub post: f64,
    slot: u8,
}

impl Field {
    pub fn new(name: &str, kind: u32, bits: u32, pre: f64, post: f64) -> Field {
        let slot = SLOT_NAMES.iter().position(|s| *s == name).map_or(NONE, |i| i as u8);
        Field { kind, bits, pre, post, slot }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub fields: Vec<Field>,
}

pub enum Val {
    Num(f64),
    Str(Vec<u8>),
}

fn scale(d: f64, f: &Field) -> f64 {
    let mut d = d;
    if (f.pre <= 0.9999 || f.pre >= 1.0001) && f.pre != 0.0 {
        d /= f.pre;
    }
    if (f.post <= 0.9999 || f.post >= 1.0001) && f.post != 0.0 {
        d *= f.post;
    }
    d
}

impl Table {
    pub fn meta() -> Table {
        let f = |n: &str, k: u32, b: u32, pre: f64| Field::new(n, k, b, pre, 1.0);
        Table {
            fields: vec![
                f("fieldType", DT_INTEGER, 32, 1.0),
                f("fieldName", DT_STRING, 1, 1.0),
                f("fieldOffset", DT_INTEGER, 16, 1.0),
                f("fieldSize", DT_INTEGER, 8, 1.0),
                f("significant_bits", DT_INTEGER, 8, 1.0),
                f("premultiply", DT_FLOAT, 32, 4000.0),
                f("postmultiply", DT_FLOAT, 32, 4000.0),
            ],
        }
    }

    pub fn read_with(&self, b: &mut Bits, mut out: impl FnMut(usize, Val)) -> u32 {
        let n = b.bits(3) as usize;
        let mut mask = [0u8; 8];
        for m in mask.iter_mut().take(n) {
            *m = b.bits(8) as u8;
        }
        let mut odd = 0;
        for (i, f) in self.fields.iter().enumerate().take(64) {
            if mask[i >> 3] & (1 << (i & 7)) == 0 {
                continue;
            }
            let signed = f.kind & DT_SIGNED != 0;
            let v = match f.kind & !DT_SIGNED {
                DT_BYTE | DT_SHORT | DT_INTEGER | DT_FLOAT => {
                    let raw = if signed { b.sbits(f.bits) as f64 } else { b.bits(f.bits) as f64 };
                    let d = scale(raw, f);
                    Val::Num(match f.kind & !DT_SIGNED {
                        DT_BYTE if signed => d as i8 as f64,
                        DT_BYTE => d as u8 as f64,
                        DT_SHORT if signed => d as i16 as f64,
                        DT_SHORT => d as u16 as f64,
                        DT_INTEGER if signed => d as i32 as f64,
                        DT_INTEGER => d as u32 as f64,
                        _ => d as f32 as f64,
                    })
                }
                DT_ANGLE => Val::Num(b.bits(f.bits) as f64 * (360.0 / (1u64 << f.bits) as f64)),
                DT_TIMEWINDOW_8 => Val::Num(b.sbits(8) as f64),
                DT_TIMEWINDOW_BIG => Val::Num(b.sbits(f.bits) as f64),
                DT_STRING => Val::Str(b.string()),
                _ => {
                    odd += 1;
                    continue;
                }
            };
            out(i, v);
        }
        odd
    }

    pub fn read(&self, b: &mut Bits, s: &mut Slots) -> u32 {
        self.read_with(b, |i, v| {
            let slot = self.fields[i].slot;
            if slot != NONE
                && let Val::Num(x) = v
            {
                s[slot as usize] = x as f32;
            }
        })
    }

    pub fn parse_description(b: &mut Bits, count: usize) -> Table {
        let meta = Table::meta();
        let mut fields = Vec::with_capacity(count);
        for _ in 0..count {
            let (mut name, mut kind, mut bits, mut pre, mut post) = (String::new(), 0u32, 0u32, 0.0, 0.0);
            meta.read_with(b, |i, v| match (i, v) {
                (0, Val::Num(x)) => kind = x as u32,
                (1, Val::Str(s)) => name = text(&s),
                (4, Val::Num(x)) => bits = x as u32,
                (5, Val::Num(x)) => pre = x,
                (6, Val::Num(x)) => post = x,
                _ => {}
            });
            fields.push(Field::new(&name, kind, bits, pre, post));
        }
        Table { fields }
    }
}
