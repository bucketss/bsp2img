# bsp2img

Renders GoldSrc (Half-Life / CS 1.6) maps straight from the `.bsp`: isometric diorama views, and CS overviews with their `.txt`.

Requires opengl, dx12, or vulkan.

## GUI

Run `bsp2img` with no arguments, or `bsp2img gui de_dust2 --game C:\HLDS`.

- Top bar: current map, Open .bsp, Reload, and export progress with Cancel.
- Tabs: **Map** (game folder, map list), **Scene** (crop, lighting, roof and XY/Z cuts), **Look** (sky, background, pixelated textures, cutaway), **Camera** (pitch, yaw), **Export** (output folder, exporter picker and its settings).
- Isometric view: drag to rotate, right-drag to pan, wheel to zoom.
- Top view: grid with world coordinates and spawns; shift+drag draws the XY crop box.
- Overview view: the exact 1024x768 overview framing.
- Exports run in the background and write the same files as the CLI into `<output>/<map>NN/`. Cancel deletes the partial files.
- Settings are saved to `%APPDATA%\bsp2img\gui.cfg`.

## CLI

```
bsp2img iso de_dust2 --game C:\HLDS
bsp2img iso path\to\map.bsp --sky --hull --roofs 1
bsp2img overview de_dust2 --game C:\HLDS
bsp2img spin de_dust2 --game C:\HLDS --gif
bsp2img peel cs_assault --game C:\HLDS --count 3 --then-spin
bsp2img slice de_dust2 --game C:\HLDS --count 8
bsp2img timing de_dust2 --game C:\HLDS
```

In a terminal, exports show a percentage while they run.

`iso` writes `renders/<map>NN/<map>_045.png`, `_135`, `_225`, `_315` (transparent PNG).
`spin` writes `<map>_spin.mp4`, a seamless loop of the map turning a full circle (needs `ffmpeg` on PATH). `--gif` and `--apng` add `<map>_spin.gif` and `<map>_spin.png`.
`peel` writes `<map>_peel.mp4`: roof levels lift off one at a time, from the top.
`slice` writes `<map>_slice.mp4`: a height cut rises from the floor so the map builds itself. Sliced walls show their hollow interiors.
`timing` writes `<map>_timing.png` (which team reaches each spot first, with a white line where both arrive together), `_timing_t.png` and `_timing_ct.png` (arrival times per team), and `<map>_timing.txt` (seconds to each bombsite, hostage and rescue zone). Times are for the first player of each team after freeze time, at `--speed` units/s. Movement model: steps up to 18 units, crouch-jumps up to 63, drops of any height, crouch-only areas at 1/3 speed, ladders at 200 units/s; doors and breakables are treated as open. (EXPERIMENTAL)
`overview` writes `<map>.bmp`, `<map>.tga` and `<map>.txt`.

Textures come from the map, then the WADs it lists, then any WAD in the mod and `valve` folders.

### Options (both commands)

| Option | Effect |
|---|---|
| `--roofs N` | Remove the N highest roof levels (levels are printed each run) |
| `--zmin/--zmax Z` | Cut away everything below/above a height |
| `--xmin/--xmax/--ymin/--ymax V` | Cut away everything beyond a world coordinate |
| `--crop X0 Y0 X1 Y1` | Keep only this XY rectangle (turns off auto crop) |
| `--hull [1\|3]` | Remove areas players can't walk to, using the collision hull |
| `--hull-pad U` | Units kept around the walkable area (default 64) |
| `--no-auto-crop` | Keep sealed rooms that can't be reached from spawns |
| `-o DIR` | Base output folder (default `renders`) |
| `--grid` | Also write a top-down `<map>_grid.png` with coordinates and removed areas |
| `--ss N` | Supersampling (default 3) |
| `--brightness`, `--light-scale` | Brighten the lighting |
| `--nearest` | Pixelated texture filtering |
| `--no-cull` | Draw back faces (disables the cutaway) |

### iso only

| Option | Effect |
|---|---|
| `--yaw A...` | View angles (default 45 135 225 315) |
| `--pitch P` | Degrees looking down (35.264 true isometric, 30 for 2:1) |
| `--size N` | Longest image side (default 2048) |
| `--sky [NAME]` | Skybox backdrop; an explicit NAME is added to filenames |
| `--bg #RRGGBB` | Solid background |
| `--all-styles` | Include switchable lights |

### spin only

| Option | Effect |
|---|---|
| `--seconds S` | Seconds per full turn (default 12) |
| `--fps F` | Frame rate (default 60; GIF rounds to 50, 33.3, 25...) |
| `--start A` | Yaw of the first frame (default 45) |
| `--ccw` | Turn the other way |
| `--pitch P`, `--sky [NAME]` | As for iso |
| `--size N` | Longest image side (default 720) |
| `--bg #RRGGBB` | Background (default #202020; the APNG stays transparent unless set) |
| `--gif` | Also write an animated GIF |
| `--apng` | Also write an animated PNG |
| `--no-mp4` | Skip the MP4 |

### peel and slice

| Option | Effect |
|---|---|
| `--count N` | peel: roof levels to lift (default 0 = all but the lowest). slice: steps (default 0 = continuous sweep) |
| `--hold S` | Pause at each step (peel 0.5, slice 0.4) |
| `--seconds-per S` | peel: seconds to lift each level (default 1.5) |
| `--reverse` | peel: play backwards, roofs drop into place |
| `--seconds S` | slice: sweep duration with `--count 0` (default 6) |
| `--then-spin` | Follow with a full turn in the same file (`_peel_spin`, `_slice_spin`) |
| `--spin-seconds S`, `--ccw` | The turn for `--then-spin` (default 12) |
| `--start A` | Yaw of the view (default 45) |
| `--size`, `--fps`, `--pitch`, `--sky`, `--bg`, `--gif`, `--apng`, `--no-mp4` | As for spin |

`--roofs` and `--zmax` set the starting cut. The camera stays fixed while geometry is removed.

### timing only

| Option | Effect |
|---|---|
| `--speed U` | Running speed in units/s (default 250, knife) |
| `--cell U` | Walk grid spacing (default 8) |
| `--interval S` | Seconds between contour lines (default 5) |
| `--size N` | Longest image side (default 1600) |

`--roofs` and `--zmax` pick which floor is shown where levels overlap; the timings themselves always cover the whole map.

### overview only

| Option | Effect |
|---|---|
| `--from-txt FILE` | Reuse ZOOM/ORIGIN/ROTATED from an existing `.txt` |
| `--margin F` | Border around the map (default 0.04) |
| `--png` | Also write a transparent PNG |

Filenames carry the cuts, e.g. `aim_map_city1_hull_roof01_xmin-1776_zmax0_045.png`.

## Build

- Rust 1.85+
- Windows: MSVC C++ Build Tools

```
cargo build --release
```
