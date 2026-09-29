use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Block(Block),
}

pub type Block = Vec<(String, Value)>;

impl Value {
    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    pub fn block(&self) -> Option<&Block> {
        match self {
            Value::Block(b) => Some(b),
            Value::Str(_) => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        self.str()?.trim().parse().ok()
    }
}

pub fn get<'a>(b: &'a Block, key: &str) -> Option<&'a Value> {
    b.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

pub fn num(b: &Block, key: &str) -> Option<f64> {
    get(b, key)?.num()
}

#[derive(Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    Str(String),
}

fn tokens(text: &str) -> Result<Vec<(Tok, usize)>> {
    let b = text.as_bytes();
    let ch = |i: usize| text[i..].chars().next().unwrap_or(' ');
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < b.len() {
        match b[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                out.push((Tok::Open, line));
                i += 1;
            }
            b'}' => {
                out.push((Tok::Close, line));
                i += 1;
            }
            b'"' => {
                let start = line;
                i += 1;
                let s0 = i;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                if i >= b.len() {
                    bail!("line {start}: unterminated string");
                }
                out.push((Tok::Str(text[s0..i].to_string()), start));
                i += 1;
            }
            _ if ch(i).is_whitespace() => i += ch(i).len_utf8(),
            _ => {
                let s0 = i;
                while i < b.len() {
                    let c = ch(i);
                    if c.is_whitespace() || matches!(c, '{' | '}' | '"') || (c == '/' && b.get(i + 1) == Some(&b'/')) {
                        break;
                    }
                    i += c.len_utf8();
                }
                let s = &text[s0..i];
                if !(s.starts_with('[') && s.ends_with(']')) {
                    out.push((Tok::Str(s.to_string()), line));
                }
            }
        }
    }
    Ok(out)
}

fn block(toks: &[(Tok, usize)], i: &mut usize, depth: usize) -> Result<Block> {
    let mut out = Block::new();
    while *i < toks.len() {
        let (t, line) = &toks[*i];
        *i += 1;
        let key = match t {
            Tok::Close if depth > 0 => return Ok(out),
            Tok::Close => bail!("line {line}: unexpected }}"),
            Tok::Open => bail!("line {line}: {{ without a key"),
            Tok::Str(s) => s.clone(),
        };
        match toks.get(*i) {
            Some((Tok::Str(v), _)) => {
                *i += 1;
                out.push((key, Value::Str(v.clone())));
            }
            Some((Tok::Open, _)) => {
                *i += 1;
                out.push((key, Value::Block(block(toks, i, depth + 1)?)));
            }
            Some((Tok::Close, _)) if depth > 0 => {
                *i += 1;
                return Ok(out);
            }
            _ => bail!("line {line}: \"{key}\" has no value"),
        }
    }
    Ok(out)
}

pub fn parse(text: &str) -> Result<Block> {
    let toks = tokens(text)?;
    let mut i = 0;
    block(&toks, &mut i, 0)
}

pub fn parse_bytes(b: &[u8]) -> Result<Block> {
    let text = String::from_utf8_lossy(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b));
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_comments() {
        let t = "// head\n\"de_x\"\n{\n\t\"pos_x\"\t\"-1046.3943\" // c\n\tscale 3\n\t\"verticalsections\"\n\t{\n\t\t\"default\" // d\n\t\t{\n\t\t\t\"AltitudeMax\" \"16384\"\n\t\t}\t}\n}";
        let b = parse(t).unwrap();
        let m = b[0].1.block().unwrap();
        assert_eq!(b[0].0, "de_x");
        assert_eq!(num(m, "POS_X"), Some(-1046.3943));
        assert_eq!(num(m, "scale"), Some(3.0));
        let vs = get(m, "verticalsections").unwrap().block().unwrap();
        assert_eq!(num(vs[0].1.block().unwrap(), "altitudemax"), Some(16384.0));
    }

    #[test]
    fn unclosed_block_is_accepted() {
        let b = parse("\"a\" { \"b\" \"1\" ").unwrap();
        assert_eq!(num(b[0].1.block().unwrap(), "b"), Some(1.0));
        assert!(parse("\"a\" }").is_err());
        assert!(parse("\"a\" \"b").is_err());
        let b = parse("\"a\" { \"b\" \"1\" CREDITS: x y } \"c\" \"2\"").unwrap();
        assert_eq!(b[0].1.block().unwrap().len(), 2);
        assert_eq!(b[1], ("c".to_string(), Value::Str("2".into())));
    }
}
