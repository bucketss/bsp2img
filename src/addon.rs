use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::scene::Report;

pub const TIMEOUT: Duration = Duration::from_secs(120);
pub const DIR: &str = "panorama/images/overheadmaps";

#[derive(Clone, Debug, PartialEq)]
pub struct Addon {
    pub name: String,
    pub content: PathBuf,
    pub game: PathBuf,
    pub compiler: PathBuf,
}

pub struct Plan {
    pub tga: Vec<PathBuf>,
    pub vtex: Vec<PathBuf>,
    pub txt: PathBuf,
    pub vtex_c: Vec<PathBuf>,
}

impl Plan {
    pub fn writes(&self) -> Vec<&PathBuf> {
        self.tga.iter().zip(&self.vtex).flat_map(|(a, b)| [a, b]).chain([&self.txt]).collect()
    }
}

fn sub(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

impl Addon {
    pub fn find(root: Option<&Path>, name: &str) -> Result<Addon> {
        let name = name.trim();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
            bail!("bad addon name {name:?}: give the folder name under csgo_addons");
        }
        let Some(root) = root else { bail!("CS2 install not found: pass --game to compile into an addon") };
        let compiler = root.join("bin").join("win64").join("resourcecompiler.exe");
        if !compiler.is_file() {
            bail!("resourcecompiler not found at {} (install the CS2 Workshop Tools)", compiler.display());
        }
        let game = root.join("csgo_addons").join(name);
        let content = root.parent().map(|p| p.join("content").join("csgo_addons").join(name)).unwrap_or_default();
        let missing: Vec<String> = [&content, &game].iter().filter(|p| !p.is_dir()).map(|p| p.display().to_string()).collect();
        if !missing.is_empty() {
            bail!("addon {name} must already exist in both content and game csgo_addons; missing: {}", missing.join(", "));
        }
        Ok(Addon { name: name.to_string(), content, game, compiler })
    }

    pub fn plan(&self, map: &str, stems: &[String]) -> Plan {
        let (src, out) = (sub(&self.content, DIR), sub(&self.game, DIR));
        Plan {
            tga: stems.iter().map(|s| src.join(format!("{s}_radar.tga"))).collect(),
            vtex: stems.iter().map(|s| src.join(format!("{s}_radar_tga.vtex"))).collect(),
            txt: sub(&self.game, "resource/overviews").join(format!("{map}.txt")),
            vtex_c: stems.iter().map(|s| out.join(format!("{s}_radar_tga.vtex_c"))).collect(),
        }
    }

    pub fn compile(&self, vtex: &[PathBuf], rep: &mut Report) -> Result<()> {
        let mut cmd = Command::new(&self.compiler);
        cmd.args(["-nop4", "-fshallow", "-i"]).args(vtex).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        rep.log(format!("  running {} -nop4 -fshallow -i ({} files)", self.compiler.display(), vtex.len()));
        let (code, out) = run(cmd, TIMEOUT, rep)?;
        match code {
            Some(0) => {
                for l in out.lines().filter(|l| !l.trim().is_empty()) {
                    rep.log(format!("    | {}", l.trim_end()));
                }
                Ok(())
            }
            Some(c) => bail!("resourcecompiler exited with code {c}:\n{}", out.trim_end()),
            None => bail!("resourcecompiler was stopped:\n{}", out.trim_end()),
        }
    }
}

fn run(mut cmd: Command, timeout: Duration, rep: &mut Report) -> Result<(Option<i32>, String)> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().context("starting resourcecompiler")?;
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut b);
            }
            b
        })
    };
    let so = pipe(child.stdout.take().map(|x| Box::new(x) as Box<dyn Read + Send>));
    let se = pipe(child.stderr.take().map(|x| Box::new(x) as Box<dyn Read + Send>));
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Ok(s);
        }
        let stop = rep.step(0.95).err();
        if t0.elapsed() > timeout || stop.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            break Err(stop);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::from_utf8_lossy(&so.join().unwrap_or_default()).into_owned();
    out.push_str(&String::from_utf8_lossy(&se.join().unwrap_or_default()));
    match status {
        Ok(s) => Ok((s.code(), out)),
        Err(Some(e)) => Err(e),
        Err(None) => bail!("resourcecompiler timed out after {} s:\n{}", timeout.as_secs(), out.trim_end()),
    }
}

pub fn check(vtex_c: &Path, img: &image::RgbaImage) -> Result<String> {
    let b = std::fs::read(vtex_c).with_context(|| format!("reading {}", vtex_c.display()))?;
    let t = crate::s2::texture::decode_bytes(&b, usize::MAX).with_context(|| format!("decoding {}", vtex_c.display()))?;
    if (t.w as u32, t.h as u32) != img.dimensions() {
        bail!("{} decodes to {}x{}, expected {}x{}", vtex_c.display(), t.w, t.h, img.width(), img.height());
    }
    let (mut n, mut worst) = (0usize, 0u8);
    for (a, b) in t.rgba.chunks_exact(4).zip(img.as_raw().chunks_exact(4)) {
        let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        n += (d > 0) as usize;
        worst = worst.max(d);
    }
    Ok(if n == 0 { format!("{}x{} decodes identical", t.w, t.h) } else { format!("{}x{} decodes with {n} pixels differing, max {worst}", t.w, t.h) })
}

pub fn vtex(stem: &str) -> String {
    format!(
        r#"<!-- dmx encoding keyvalues2_noids 1 format vtex 1 -->
"CDmeVtex"
{{
	"m_inputTextureArray" "element_array"
	[
		"CDmeInputTexture"
		{{
			"m_name" "string" "SheetTexture"
			"m_fileName" "string" "{DIR}/{stem}_radar.tga"
			"m_colorSpace" "string" "srgb"
			"m_typeString" "string" "2D"
			"m_imageProcessorArray" "element_array"
			[
			]
		}}
	]
	"m_outputTypeString" "string" "2D"
	"m_outputFormat" "string" "BGRA8888"
	"m_outputClearColor" "vector4" "0 0 0 0"
	"m_nOutputMinDimension" "int" "0"
	"m_nOutputMaxDimension" "int" "0"
	"m_textureOutputChannelArray" "element_array"
	[
		"CDmeTextureOutputChannel"
		{{
			"m_inputTextureArray" "string_array"
			[
				"SheetTexture"
			]
			"m_srcChannels" "string" "rgba"
			"m_dstChannels" "string" "rgba"
			"m_mipAlgorithm" "CDmeImageProcessor"
			{{
				"m_algorithm" "string" "Box"
				"m_stringArg" "string" ""
				"m_vFloat4Arg" "vector4" "0 0 0 0"
			}}
			"m_outputColorSpace" "string" "srgb"
		}}
	]
	"m_vClamp" "vector3" "0 0 0"
	"m_bNoLod" "bool" "1"
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bsp2img_addon_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("game").join("csgo")).unwrap();
        std::fs::write(d.join("game").join("csgo").join("pak01_dir.vpk"), b"").unwrap();
        d
    }

    #[test]
    fn refuses() {
        let d = tmp("refuse");
        let root = d.join("game");
        let e = Addon::find(Some(&root), "x").unwrap_err().to_string();
        assert!(e.contains("resourcecompiler not found"), "{e}");
        std::fs::create_dir_all(root.join("bin").join("win64")).unwrap();
        std::fs::write(root.join("bin").join("win64").join("resourcecompiler.exe"), b"").unwrap();
        std::fs::create_dir_all(root.join("csgo_addons").join("x")).unwrap();
        let e = Addon::find(Some(&root), "x").unwrap_err().to_string();
        assert!(e.contains("missing") && e.contains("content"), "{e}");
        assert!(Addon::find(Some(&root), "../x").is_err());
        assert!(Addon::find(None, "x").is_err());
        std::fs::create_dir_all(d.join("content").join("csgo_addons").join("x")).unwrap();
        let a = Addon::find(Some(&root), "x").unwrap();
        let p = a.plan("de_nuke", &["de_nuke".into(), "de_nuke_lower".into()]);
        let names: Vec<String> = p.writes().iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["de_nuke_radar.tga", "de_nuke_radar_tga.vtex", "de_nuke_lower_radar.tga", "de_nuke_lower_radar_tga.vtex", "de_nuke.txt"]);
        assert!(p.vtex_c[1].ends_with("panorama/images/overheadmaps/de_nuke_lower_radar_tga.vtex_c"));
        assert!(p.txt.starts_with(root.join("csgo_addons").join("x").join("resource")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn compiler_failure() {
        let d = tmp("fail");
        let a = Addon { name: "x".into(), content: d.clone(), game: d.clone(), compiler: PathBuf::from("cmd.exe") };
        let mut cmd = Command::new(&a.compiler);
        cmd.args(["/c", "echo boom& exit 3"]).stdout(Stdio::piped()).stderr(Stdio::piped());
        let (mut log, mut prog) = (|_: String| {}, |_: f32| true);
        let mut rep = Report { log: &mut log, progress: &mut prog };
        let (code, out) = run(cmd, TIMEOUT, &mut rep).unwrap();
        assert_eq!((code, out.trim()), (Some(3), "boom"));
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/c", "ping -n 5 127.0.0.1 >nul"]).stdout(Stdio::piped()).stderr(Stdio::piped());
        assert!(run(cmd, Duration::from_millis(300), &mut rep).unwrap_err().to_string().contains("timed out"));
        let missing = Addon { compiler: d.join("nope.exe"), ..a };
        assert!(missing.compile(&[], &mut rep).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
