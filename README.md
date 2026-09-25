# bsp2img

Renders GoldSrc (Half-Life / CS 1.6) maps straight from the `.bsp`: isometric diorama views, and CS overviews with their `.txt`.

Requires opengl, dx12, or vulkan.

## GUI

Run `bsp2img` with no arguments, or `bsp2img gui de_dust2 --game C:\HLDS`.

- Top bar: current map, Open .bsp, Reload, and export progress with Cancel.
- Tabs: **Map** (game folder, map list), **Scene** (crop, lighting, roof and XY/Z cuts), **Look** (sky, background, textures, animated textures, cutaway, AO, ink, colour, styles, tilt-shift), **Camera** (iso pitch and yaw; free camera: projection, yaw, pitch, roll, distance, fov, target, presets, save/load `.cam`, use for exports), **Export** (output folder, exporter picker and its settings).
- Isometric view: drag to rotate, right-drag to pan, wheel to zoom.
- Free view: perspective or orthographic. Drag to orbit, right- or middle-drag to pan, wheel to dolly, double-click to orbit around the point under the cursor, hold right button + WASD/QE to fly (shift for faster), ctrl+click to set the focus for tilt-shift and depth of field.
- Top view: grid with world coordinates and spawns; shift+drag draws the XY crop box.
- Overview view: the exact 1024x768 overview framing.
- Exports run in the background and write the same files as the CLI into `<output>/<map>NN/`. Cancel deletes the partial files. A health report also opens in a window.
- Settings are saved to `%APPDATA%\bsp2img\gui.cfg`.

## CLI

```
bsp2img iso de_dust2 --game C:\HLDS
bsp2img iso path\to\map.bsp --sky --hull --roofs 1
bsp2img iso de_dust2 --game C:\HLDS --persp 50 --miniature
bsp2img spin de_dust2 --game C:\HLDS --camera view.cam
bsp2img overview de_dust2 --game C:\HLDS
bsp2img spin de_dust2 --game C:\HLDS --gif
bsp2img peel cs_assault --game C:\HLDS --count 3 --then-spin
bsp2img slice de_dust2 --game C:\HLDS --count 8
bsp2img timing de_dust2 --game C:\HLDS
bsp2img health "de_*" "cs_*" --game C:\HLDS
bsp2img svg de_nuke --game C:\HLDS --scale 1:200
bsp2img stl de_dust2 --game C:\HLDS --roofs 1 --print-width 200
bsp2img gltf de_dust2 --game C:\HLDS --roofs 1
bsp2img poster de_dust2 --game C:\HLDS --paper a2 --dpi 300 --top
```

In a terminal, exports show a percentage while they run.

`iso` writes `renders/<map>NN/<map>_045.png`, `_135`, `_225`, `_315` (transparent PNG). With `--persp` the names get `_persp<FOV>`; with `--camera` it writes one `<map>_cam.png`.
`spin` writes `<map>_spin.mp4`, a seamless loop of the map turning a full circle (needs `ffmpeg` on PATH). `--gif` and `--apng` add `<map>_spin.gif` and `<map>_spin.png`.
`peel` writes `<map>_peel.mp4`: roof levels lift off one at a time, from the top.
`slice` writes `<map>_slice.mp4`: a height cut rises from the floor so the map builds itself. Sliced walls show their hollow interiors.
`timing` writes `<map>_timing.png` (which team reaches each spot first, with a white line where both arrive together), `_timing_t.png` and `_timing_ct.png` (arrival times per team), and `<map>_timing.txt` (seconds to each bombsite, hostage and rescue zone). Times are for the first player of each team after freeze time, at `--speed` units/s. Movement model: steps up to 18 units, crouch-jumps up to 63, drops of any height, crouch-only areas at 1/3 speed, ladders at 200 units/s; doors and breakables are treated as open. (EXPERIMENTAL)
`poster` writes `<map>_poster_a2_045.png` (or `_top`): a print-size PNG with a title block (map name, worldspawn `message`, date, view, scale), coordinate ticks (top-down), spawn and objective markers with a legend, and a scale bar. It renders in tiles and streams rows to disk, so size is not limited by the GPU and memory stays low; AO, ink and blur have no seams.
`overview` writes `<map>.bmp`, `<map>.tga` and `<map>.txt`.
`health` writes `<map>_health.txt` and `<map>_health.png`: missing WADs, textures, sky, models and sounds; whether VIS and RAD ran; counts against engine and compiler limits (HLSDK and VHLT 34; percentages use the engine limit where known, else VHLT); spawns per team and any not on walkable ground; objectives and buy zones; overview files, BMP palette index 255 and the `.res`; walkable area and the 3 largest open spots. With several maps it also writes `<out>/health_summary.csv`. Map names accept `*` and `?`. Stock maps often have overviews that their `.res` doesn't list; that's normal.
`svg` writes `<map>_callouts.svg`: vector line art of the floor players can reach from spawns, in world units and printable to scale. Layers: `floors-N` (tinted floor per height band, lowest first; parts under a higher band get a dashed outline instead), `walls`, `objectives`, `spawns`, `grid` (hidden) and an empty `labels` layer for your own callouts. Stacked areas are split into bands at roof levels. Framing matches the `timing` and `--grid` images.
`stl` writes `<map>.stl`, a watertight binary STL of the playable area for 3D printing: walls around every spot reachable from the spawns, a base plate below the lowest floor, nothing above `--roofs`/`--zmax`. Closed doors and solid brush entities are included; sealed pockets are filled and floating parts removed.
`gltf` writes `<map>_baked.glb` (or `_separate`, `_none`) for Blender, web viewers and Sketchfab: Y up, metres (1 unit = 1 inch), with the same cuts as the images. `--lighting baked` bakes textures times lightmap into one atlas with an unlit material, so it looks the same everywhere. `separate` keeps tiled textures and puts the full-colour lightmap on UV 2 as the occlusion texture; most viewers show it greyscale, in Blender multiply it with the base colour instead. `none` writes textures only. Additive surfaces become alpha blended.

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

### Effects (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--animate-textures` | Animations play `+0`..`+9` texture sequences (10 frames/s) and warp `!` water as GoldSrc does. Stills use time 0 |
| `--ao` | Ambient occlusion; `--ao-strength` (default 1), `--ao-radius` in units (default 48) |
| `--ink` | Outlines; `--ink-width` in output pixels (default 1.5), `--ink-color` (default #000000) |
| `--saturation F` | 1 = unchanged, 0 = grey |
| `--tint #RRGGBB`, `--tint-amount F` | Blend towards a colour (default amount 0.25) |
| `--style blueprint\|comic` | blueprint: blue background, grey-blue geometry, white ink. comic: AO, black ink width 2, saturation 1.2. Other options override it |
| `--contrast F` | 1 = unchanged |
| `--tilt-shift` | Blur above and below a horizontal band; `--focus-y` (band centre, 0 top to 1 bottom, default 0.5), `--band` (sharp fraction of the height, default 0.2), `--blur` (largest radius in output pixels, default 12) |
| `--dof` | Depth of field around `--focus-dist` (default: the camera target). `--band` is then the sharp fraction of that distance. Perspective only; otherwise it falls back to `--tilt-shift` |
| `--miniature` | Tilt-shift with saturation +25% and contrast +10% |

### Camera (iso, spin, peel, slice, poster)

| Option | Effect |
|---|---|
| `--persp FOV` | Perspective with automatic framing, vertical field of view in degrees |
| `--camera FILE` | Use a `.cam` saved from the GUI Camera tab. `--size` is the longest side and the shape comes from the file. Animations orbit its target at its pitch and distance, starting at its yaw |

A `.cam` file is `key=value` lines: `proj` (`persp` or `ortho`), `target` (`x y z`), `yaw`, `pitch`, `roll`, `dist`, `fov`, `aspect`. Orthographic cameras show a view `2 * dist * tan(fov / 2)` units high.

### poster only

| Option | Effect |
|---|---|
| `--paper a0..a4\|letter\|tabloid` | Page size (default a2) |
| `--dpi N` | Print resolution (default 300); also sets text and line sizes |
| `--landscape`, `--portrait` | Orientation (default: follows the map's shape) |
| `--px W H` | Page size in pixels instead of `--paper` |
| `--top` | Top-down instead of isometric |
| `--yaw D`, `--pitch D` | Isometric angles (default 45, 35.264) |
| `--no-layout` | Map only, no border, title or legend |

### timing only

| Option | Effect |
|---|---|
| `--speed U` | Running speed in units/s (default 250, knife) |
| `--cell U` | Walk grid spacing (default 8) |
| `--interval S` | Seconds between contour lines (default 5) |
| `--size N` | Longest image side (default 1600) |

`--roofs` and `--zmax` pick which floor is shown where levels overlap; the timings themselves always cover the whole map.

### health only

| Option | Effect |
|---|---|
| `--cell U` | Walk grid spacing (default 8) |
| `--size N` | Longest side of the map thumbnail (default 600) |

### svg only

| Option | Effect |
|---|---|
| `--scale 1:N` | Print scale, 1 unit = 1 inch (default 1:100) |
| `--simplify U` | Outline tolerance in units (default 6) |
| `--bands N` | Most floor bands for stacked areas (default 2) |
| `--planes Z,...` | Split floors at these heights instead |
| `--cell U` | Walk grid spacing (default 8) |
### stl only

| Option | Effect |
|---|---|
| `--voxel U` | Voxel size in units (default 8) |
| `--wall U` | Wall kept around the walkable area (default 32) |
| `--base U` | Base plate below the lowest floor (default 16) |
| `--print-width MM` | Longest side of the print (default 200) |
| `--smooth` | Smooth surface (surface nets) instead of blocky voxels; much larger files |

### gltf only

| Option | Effect |
|---|---|
| `--lighting baked\|separate\|none` | How lighting is stored (default baked) |
| `--texel U` | Units per atlas texel with baked lighting (default 2; raised automatically if the atlas would pass 8192x8192) |
| `--nearest` | Pixelated texture filtering |

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
