# bsp2img

Renders GoldSrc, Source and CS2 maps.

## Commands

`bsp2img <command> <map...> [options]` (maps may be globs)

| Command | Output |
|---|---|
| `gui` | GUI (also `bsp2img` alone) |
| `iso` | Isometric PNGs |
| `spin` | Turntable MP4, GIF, APNG |
| `peel` | Roof levels lifting off |
| `slice` | Rising height cut |
| `poster` | Print-size poster PNG |
| `overview` | Overview / radar images and `.txt` |
| `timing` | Rush timing maps |
| `kills` | Kill heatmaps from demos |
| `wallbang` | GoldSrc wallbang maps |
| `pixelwalk` | GoldSrc pixelwalk maps |
| `health` | Map health report |
| `svg` | Vector callout map |
| `stl` | 3D-printable diorama |
| `gltf` | `.glb` model |
| `obj` | `.obj` model |

## Options

### All commands

| Option | Effect |
|---|---|
| `--game DIR` | Game or mod folder |
| `--wad-dir DIR` | Extra WAD folder |
| `-o DIR` | Output folder (renders) |
| `--roofs N` | Remove the N highest roof levels |
| `--zmin/--zmax Z` | Height cuts |
| `--xmin/--xmax/--ymin/--ymax V` | XY cuts |
| `--crop X0 Y0 X1 Y1` | XY rectangle |
| `--hull [1\|3]` | Remove unreachable areas |
| `--hull-pad U` | Padding around walkable area (64) |
| `--no-auto-crop` | Keep sealed rooms |
| `--grid` | Also write a grid PNG |
| `--ss N` | Supersampling (3) |
| `--brightness`, `--light-scale` | Brighten lighting |
| `--gamma`, `--texgamma`, `--lightgamma` | Gamma (2.5, 2, 2.5) |
| `--all-styles` | Include switchable lights |
| `--tex-max N` | CS2 and Source texture limit (512) |
| `--nearest` | Pixelated textures |
| `--no-cull` | Draw back faces |
| `--timings` | Log phase times |

### iso

| Option | Effect |
|---|---|
| `--yaw A...` | View angles (45 135 225 315) |
| `--pitch P` | Degrees down (35.264) |
| `--size N` | Longest side (2048) |
| `--sky [NAME]` | Skybox; `--sky-fov`, `--sky-pitch` |
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
| `--day-cycle` | Time of day sweep; `--day-from`, `--day-to`, `--no-turn` |

Also `--pitch`, `--sky`, `--bg`.

### peel, slice

| Option | Effect |
|---|---|
| `--count N` | Levels or steps (0 = all) |
| `--hold S` | Pause per step |
| `--seconds-per S` | peel: seconds per level (1.5) |
| `--reverse` | peel: roofs drop in |
| `--seconds S` | slice: sweep length (6) |
| `--then-spin` | Then a full turn; `--spin-seconds`, `--ccw` |

Also the spin output options.

### Effects (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--animate-textures` | Animated textures and water |
| `--ao` | Ambient occlusion; `--ao-strength`, `--ao-radius` |
| `--ink` | Outlines; `--ink-width`, `--ink-color` |
| `--saturation F`, `--contrast F` | Colour grade |
| `--tint #RRGGBB`, `--tint-amount F` | Colour tint |
| `--style blueprint\|comic` | Presets |
| `--tilt-shift` | Tilt-shift blur; `--focus-y`, `--band`, `--blur` |
| `--dof` | Depth of field; `--focus-dist` |
| `--miniature` | Tilt-shift preset |

### Relighting (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--relight [A]` | Sun and shadows (1) |
| `--time HH:MM` | Time of day |
| `--sun-az D`, `--sun-el D` | Sun direction |
| `--keep-lights` | Keep baked lamps in shadow |
| `--shadow-res N` | Shadow map size (4096) |

### Camera (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--persp FOV` | Perspective |
| `--camera FILE` | `.cam` from the GUI |

### overview

| Option | Effect |
|---|---|
| `--from-txt FILE` | Reuse framing |
| `--margin F` | Border (0.04) |
| `--png` | Also write a PNG |
| `--fit` | Fit to geometry |
| `--sections auto\|none\|Z,...` | Radar sections |
| `--tga` | CS2: also write TGAs |
| `--official` | Also write official radars |
| `--compile ADDON` | CS2: compile into an addon |

### timing, kills

| Option | Effect |
|---|---|
| `--base render\|radar` | Backdrop (render) |

### wallbang

| Option | Effect |
|---|---|
| `--weapon NAME` | ak47, awp, m4a1, m4a1_sil, deagle, scout (ak47) |
| `--stance stand\|duck\|both` | Shooter stance (both) |
| `--yaw-step D` | Yaw step (2) |
| `--pitch A..B/S` | Pitch range and step (-20..20/4) |
| `--spot-cell U` | Shooter spacing (64) |
| `--max-hit U` | Max wall distance (2000) |
| `--skip-translucent A` | Ignore translucent brush entities |
| `--skip-doors` | Ignore doors |
| `--min-thickness U` | Min wall thickness (0) |
| `--slab U` | Ground thickness (256) |
| `--materials FILE` | Override `materials.txt` |
| `--style lines\|areas\|both` | 2D style (both) |
| `--sigma PX` | Area smoothing (2) |
| `--base render\|radar` | Backdrop (render) |
| `--sections auto\|none\|Z,...` | Per-level images (none) |
| `--others off\|faint` | Other levels (off) |
| `--pixelwalks` | Pixelwalk layer |
| `--damage` | Colour by headshot kill |
| `--3d` | Also write iso renders |
| `--gltf`, `--obj` | Also write a model |
| `--csv` | Write every shot |

### pixelwalk

| Option | Effect |
|---|---|
| `--stance stand\|duck\|both` | Stances (both) |
| `--min-samples N` | Min sub-pixel hits (2) |
| `--merge-gap U` | Merge distance (64) |
| `--sections auto\|none\|Z,...` | Per-level images (none) |
| `--base render\|radar` | Backdrop (render) |
| `--labels` | Number the pixelwalks |
| `--3d` | Also write iso renders |
| `--gltf`, `--obj` | Also write a model |

`health --pixelwalks` adds a pixelwalk count.

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
| `--lighting baked\|separate\|none` | Lighting (baked) |
| `--texel U` | Units per texel (2) |

## Build

`cargo build --release`
