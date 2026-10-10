const SIM_SOURCES: &[&str] = &[
    "src/wallbang/cache.rs",
    "src/wallbang/cs2.rs",
    "src/wallbang/hull0.rs",
    "src/wallbang/mod.rs",
    "src/wallbang/shots.rs",
    "src/wallbang/sim.rs",
    "src/wallbang/sim_s2.rs",
    "src/wallbang/texture.rs",
    "src/s2/bullet.rs",
    "src/s2/entities.rs",
    "src/s2/nav.rs",
    "src/s2/phys.rs",
    "src/s2/surfprop.rs",
    "src/nav.rs",
    "src/navmesh.rs",
    "src/phys.rs",
];

fn main() {
    println!("cargo:rerun-if-changed=bsp2img.rc");
    println!("cargo:rerun-if-changed=arctic.ico");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in SIM_SOURCES {
        println!("cargo:rerun-if-changed={f}");
        let d = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        for b in d.into_iter().filter(|&b| b != b'\r').chain(f.bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=BSP2IMG_SIM_SRC={h:016x}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("bsp2img.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
