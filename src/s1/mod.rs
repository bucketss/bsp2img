pub mod bsp;

use std::path::Path;

use anyhow::{Result, bail};

use crate::bsp::Bsp;
use crate::mesh::Mesh;
use crate::scene::LoadOpts;

pub fn is_source(path: &Path) -> bool {
    bsp::is_source(path)
}

pub fn load(path: &Path, _opts: &LoadOpts, _max_dim: u32, _log: &mut dyn FnMut(String)) -> Result<(Bsp, Mesh)> {
    let map = bsp::Map::open(path)?;
    bail!("Source BSP v{} ({}): rendering is not implemented yet", map.version, path.display())
}
