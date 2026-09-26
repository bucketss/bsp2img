# bsp2img

Renders GoldSrc (Half-Life / CS 1.6) maps from the `.bsp`. Needs OpenGL, DX12 or Vulkan.

## GUI

`bsp2img` or `bsp2img gui <map>`. Settings are saved to `%APPDATA%\bsp2img\gui.cfg`.

## Commands

`bsp2img <command> <map...> [options]`

| Command | Output |
|---|---|
| `iso` | Isometric PNGs, one per yaw |
| `spin` | Looping turntable MP4 (needs `ffmpeg`), optional GIF/APNG |
| `peel` | Animation lifting roof levels off one by one |
| `slice` | Animation raising a height cut from floor to sky |
| `poster` | Print-size tiled PNG with title block, legend and scale bar |
| `overview` | CS overview `.bmp`, `.tga` and `.txt` |
| `timing` | Rush timing maps and `.txt` per team (experimental) |
| `kills` | Kill and presence heatmaps, `.csv` and `.txt` from HLTV demos |
| `health` | Asset, compile, engine limit and spawn report; `health_summary.csv` for several maps |
| `svg` | Vector callout map of reachable floors, walls, objectives and spawns |
| `stl` | Watertight 3D-printable diorama |
| `gltf` | `.glb` with textures and lighting, Y up, metres |
| `obj` | `.obj`, `.mtl` and PNGs, same options as `gltf` (no `separate` lighting) |
| `gui` | Open the GUI |

## Options

### All commands

| Option | Effect |
|---|---|
| `--game DIR` | Game or mod folder, allows map names |
| `--wad-dir DIR` | Extra WAD folder |
| `-o DIR` | Output folder (default `renders`) |
| `--roofs N` | Remove the N highest roof levels |
| `--zmin/--zmax Z` | Height cuts |
| `--xmin/--xmax/--ymin/--ymax V` | XY cuts |
| `--crop X0 Y0 X1 Y1` | Keep only this XY rectangle |
| `--hull [1\|3]` | Remove areas players can't reach |
| `--hull-pad U` | Units kept around walkable area (64) |
| `--no-auto-crop` | Keep sealed rooms |
| `--grid` | Also write a top-down grid PNG (iso, overview) |
| `--ss N` | Supersampling (3) |
| `--brightness`, `--light-scale` | Brighten lighting |
| `--gamma`, `--texgamma`, `--lightgamma` | Gamma (2.5, 2, 2.5) |
| `--all-styles` | Include switchable lights |
| `--nearest` | Pixelated textures |
| `--no-cull` | Draw back faces |

### iso

| Option | Effect |
|---|---|
| `--yaw A...` | View angles (45 135 225 315) |
| `--pitch P` | Degrees down (35.264) |
| `--size N` | Longest side (2048) |
| `--sky [NAME]` | Skybox backdrop; `--sky-fov`, `--sky-pitch` |
| `--bg #RRGGBB` | Background colour |

### spin

| Option | Effect |
|---|---|
| `--seconds S` | Seconds per turn (12) |
| `--fps F` | Frame rate (60) |
| `--start A` | First yaw (45) |
| `--ccw` | Reverse direction |
| `--size N` | Longest side (720) |
| `--gif`, `--apng`, `--no-mp4` | Output formats |
| `--day-cycle` | Sweep time of day; `--day-from`, `--day-to`, `--no-turn` |

Also `--pitch`, `--sky`, `--bg`.

### peel, slice

| Option | Effect |
|---|---|
| `--count N` | Roof levels (peel) or steps (slice); 0 = all / continuous |
| `--hold S` | Pause per step |
| `--seconds-per S` | peel: seconds per level (1.5) |
| `--reverse` | peel: roofs drop into place |
| `--seconds S` | slice: sweep length (6) |
| `--then-spin` | Follow with a full turn; `--spin-seconds`, `--ccw` |

Also the spin output options.

### Effects (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--animate-textures` | Texture sequences and water warp |
| `--ao` | Ambient occlusion; `--ao-strength`, `--ao-radius` |
| `--ink` | Outlines; `--ink-width`, `--ink-color` |
| `--saturation F`, `--contrast F` | Colour grade |
| `--tint #RRGGBB`, `--tint-amount F` | Colour tint |
| `--style blueprint\|comic` | Presets |
| `--tilt-shift` | Blur outside a band; `--focus-y`, `--band`, `--blur` |
| `--dof` | Depth of field (perspective); `--focus-dist` |
| `--miniature` | Tilt-shift with extra saturation and contrast |

### Relighting (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--relight [A]` | Sun and shadows from `light_environment`; A blends with baked (1) |
| `--time HH:MM` | Time of day |
| `--sun-az D`, `--sun-el D` | Sun direction |
| `--keep-lights` | Keep baked lamps in shadow (artistic) |
| `--shadow-res N` | Shadow map size (4096) |

### Camera (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--persp FOV` | Perspective |
| `--camera FILE` | `.cam` saved from the GUI |

### Exploded floors (iso, spin, peel, slice, poster, svg)

| Option | Effect |
|---|---|
| `--explode N` | Split at the N largest level gaps |
| `--explode-at Z,...` | Split at these heights |
| `--explode-gap U` | Lift per floor (256) |
| `--explode-guides` | Corner guide lines |

### poster

| Option | Effect |
|---|---|
| `--paper a0..a4\|letter\|tabloid` | Page size (a2) |
| `--dpi N` | Resolution (300) |
| `--landscape`, `--portrait` | Orientation |
| `--px W H` | Size in pixels |
| `--top` | Top-down |
| `--yaw`, `--pitch` | Iso angles |
| `--no-layout` | Map only |

### overview

| Option | Effect |
|---|---|
| `--from-txt FILE` | Reuse framing from a `.txt` |
| `--margin F` | Border (0.04) |
| `--png` | Also write a PNG |

### timing

| Option | Effect |
|---|---|
| `--speed U` | Units/s (250) |
| `--cell U` | Grid spacing (8) |
| `--interval S` | Contour spacing (5) |
| `--size N` | Longest side (1600) |

### kills

| Option | Effect |
|---|---|
| `--demos PATH...` | Demo files, folders or globs |
| `--weapon W,...` | Only these weapons |
| `--team t\|ct\|both` | Victim's team (both) |
| `--headshots` | Only headshots |
| `--lines` | Killer-to-victim lines |
| `--rounds A-B` | Only these rounds |
| `--radius U` | Heatmap radius (96) |
| `--presence-every S` | Presence sampling (0.5, 0 = off) |
| `--size N` | Longest side (1600) |

Only demos recorded on the map are used. With no map, every map in the demos that `--game` has a `.bsp` for, plus `kills_summary.csv`. Kills on floors under the visible one are left out.

### health

| Option | Effect |
|---|---|
| `--cell U` | Grid spacing (8) |
| `--size N` | Thumbnail size (600) |

### svg

| Option | Effect |
|---|---|
| `--scale 1:N` | Print scale (1:100) |
| `--simplify U` | Outline tolerance (6) |
| `--bands N` | Floor bands (2) |
| `--planes Z,...` | Split heights |
| `--cell U` | Grid spacing (8) |

### stl

| Option | Effect |
|---|---|
| `--voxel U` | Voxel size (8) |
| `--wall U` | Wall thickness (32) |
| `--base U` | Base plate (16) |
| `--print-width MM` | Longest side (200) |
| `--smooth` | Smooth surface |

### gltf, obj

| Option | Effect |
|---|---|
| `--lighting baked\|separate\|none` | Lighting mode (baked) |
| `--texel U` | Units per atlas texel (2) |

## Build

Rust 1.85+, MSVC Build Tools on Windows. `cargo build --release`
