fn main() {
    println!("cargo:rerun-if-changed=bsp2img.rc");
    println!("cargo:rerun-if-changed=arctic.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("bsp2img.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
