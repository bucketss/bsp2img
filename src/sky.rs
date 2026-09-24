use std::path::{Path, PathBuf};

use anyhow::Result;

const SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];

pub fn find_sky(name: &str, dirs: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let mut faces = Vec::new();
    for suf in SUFFIXES {
        let mut found = None;
        'dirs: for d in dirs {
            let env = d.join("gfx").join("env");
            let Ok(rd) = std::fs::read_dir(&env) else { continue };
            let names: Vec<(String, PathBuf)> =
                rd.flatten().map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path())).collect();
            for ext in [".tga", ".bmp"] {
                let want = format!("{name}{suf}{ext}").to_lowercase();
                if let Some((_, p)) = names.iter().find(|(n, _)| *n == want) {
                    found = Some(p.clone());
                    break 'dirs;
                }
            }
        }
        faces.push(found?);
    }
    Some(faces)
}

pub fn load_sky(paths: &[PathBuf], lut: &[u8; 256]) -> Result<(u32, Vec<u8>)> {
    let imgs: Vec<image::RgbaImage> =
        paths.iter().map(|p: &PathBuf| Ok(image::open(Path::new(p))?.to_rgba8())).collect::<Result<_>>()?;
    let size = imgs.iter().map(|im| im.width().max(im.height())).max().unwrap_or(1);
    let mut out = Vec::with_capacity((size * size * 4 * 6) as usize);
    for im in imgs {
        let im = if im.dimensions() != (size, size) {
            image::imageops::resize(&im, size, size, image::imageops::FilterType::CatmullRom)
        } else {
            im
        };
        for p in im.pixels() {
            out.extend_from_slice(&[lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize], 255]);
        }
    }
    Ok((size, out))
}
