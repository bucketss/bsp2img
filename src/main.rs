mod bsp;
mod camera;
mod cli;
mod clip;
mod explode;
mod export;
mod gltf;
mod grid;
mod gui;
mod health;
mod light;
mod look;
mod mesh;
mod nav;
mod overview;
mod paths;
mod post;
mod quant;
mod reach;
mod render;
mod scene;
mod sky;
mod svg;
mod spin;
mod stl;
mod timing;
mod wad;

use clap::Parser;

#[cfg(windows)]
fn detach_console() {
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn FreeConsole() -> i32;
    }
    let mut ids = [0u32; 2];
    unsafe {
        if GetConsoleProcessList(ids.as_mut_ptr(), 2) == 1 {
            FreeConsole();
        }
    }
}

#[cfg(not(windows))]
fn detach_console() {}

fn main() {
    let cli = cli::Cli::parse();
    if cli.cmd.is_none() {
        detach_console();
    }
    let res = match &cli.cmd {
        Some(cli::Cmd::Iso(a)) => cli::run_iso(a),
        Some(cli::Cmd::Overview(a)) => cli::run_overview(a),
        Some(cli::Cmd::Spin(a)) => cli::run_spin(a),
        Some(cli::Cmd::Peel(a)) => cli::run_peel(a),
        Some(cli::Cmd::Slice(a)) => cli::run_slice(a),
        Some(cli::Cmd::Timing(a)) => cli::run_timing(a),
        Some(cli::Cmd::Health(a)) => cli::run_health(a),
        Some(cli::Cmd::Svg(a)) => cli::run_svg(a),
        Some(cli::Cmd::Stl(a)) => cli::run_stl(a),
        Some(cli::Cmd::Gltf(a)) => cli::run_gltf(a),
        Some(cli::Cmd::Gui(a)) => gui::run(a.map.clone(), a.game.clone(), &a.view),
        None => gui::run(None, None, "iso"),
    };
    if let Err(e) = res {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
