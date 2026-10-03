pub mod draw3d;
pub mod finder;
pub mod hull;
pub mod output;
pub mod pmove;
pub mod present;
pub mod seams;
pub mod trace;
pub mod vec;
pub mod world;

use anyhow::{Result, bail};

use crate::bsp::{Bsp, Engine};
pub use finder::{Find, FinderConfig};

#[derive(Clone, Debug)]
pub struct FinderOpts {
    pub standing: bool,
    pub duck: bool,
    pub min_samples: i32,
    pub threads: usize,
}

impl Default for FinderOpts {
    fn default() -> Self {
        FinderOpts { standing: true, duck: true, min_samples: 2, threads: 0 }
    }
}

pub struct Found {
    pub finds: Vec<Find>,
    pub seams: usize,
}

pub fn find(bsp: &Bsp, opts: &FinderOpts) -> Result<Found> {
    if bsp.engine != Engine::GoldSrc {
        bail!("pixelwalks are only supported for GoldSrc maps");
    }
    let run = || {
        let map = hull::PwMap::new(bsp);
        let wm = world::build_world(bsp, &map);
        let seams = seams::enumerate_seams(&map, &wm);
        let cfg = FinderConfig { standing: opts.standing, duck: opts.duck, min_samples: opts.min_samples, ..Default::default() };
        let finds = finder::run_finder(&map, &wm, &seams, &cfg);
        Found { finds, seams: seams.len() }
    };
    if opts.threads > 0 {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.threads).build()?;
        Ok(pool.install(run))
    } else {
        Ok(run())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn golden_blocks(text: &str) -> Vec<(String, usize, Vec<String>)> {
        let mut out: Vec<(String, usize, Vec<String>)> = Vec::new();
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("=== ").and_then(|h| h.strip_suffix(" ===")) {
                let (path, rest) = h.rsplit_once(" : ").unwrap();
                let n: usize = rest.split_whitespace().next().unwrap().parse().unwrap();
                let stem = path.rsplit(['\\', '/']).next().unwrap().trim_end_matches(".bsp").to_string();
                out.push((stem, n, Vec::new()));
            } else if line.starts_with("  [")
                && let Some(b) = out.last_mut()
            {
                b.2.push(line.to_string());
            }
        }
        out
    }

    #[test]
    #[ignore]
    fn golden() {
        let Ok(dir) = std::env::var("PIXELWALK_GOLDEN") else { return };
        let maps_dir = std::env::var("PIXELWALK_MAPDIR").unwrap_or_else(|_| r"F:\servers\cs-coftest\cstrike\maps".into());
        let only = std::env::var("PIXELWALK_MAPS").ok();
        let threads = std::env::var("PIXELWALK_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(0);
        let reference = std::env::var("PIXELWALK_REF").map_or_else(|_| Path::new(&dir).join("cs-coftest.txt"), Into::into);
        let text = std::fs::read_to_string(reference).unwrap();
        let (mut ok, mut bad) = (0, 0);
        for (stem, n, lines) in golden_blocks(&text) {
            if only.as_ref().is_some_and(|o| !o.split(',').any(|m| m.eq_ignore_ascii_case(&stem))) {
                continue;
            }
            let path = Path::new(&maps_dir).join(format!("{stem}.bsp"));
            let bsp = Bsp::load(&path).unwrap();
            let t0 = std::time::Instant::now();
            let found = find(&bsp, &FinderOpts { threads, ..Default::default() }).unwrap();
            let got: Vec<String> = found.finds.iter().map(output::raw_line).collect();
            let secs = t0.elapsed().as_secs_f64();
            if got.len() == n && got == lines {
                ok += 1;
                println!("{stem}: {n} spans match ({} seams, {secs:.1}s)", found.seams);
            } else {
                bad += 1;
                println!("{stem}: MISMATCH got {} want {n} ({secs:.1}s)", got.len());
                for k in 0..got.len().max(lines.len()) {
                    let (g, w) = (got.get(k).map_or("", |s| s.as_str()), lines.get(k).map_or("", |s| s.as_str()));
                    if g != w {
                        println!("  got  {g}\n  want {w}");
                    }
                }
            }
        }
        println!("{ok} maps match, {bad} differ");
        assert_eq!(bad, 0);
    }
}
