We want to add the following features to bsp2img

### **glTF export.** 
Lets you open any map, with its textures and baked lighting, in Blender, a browser or Sketchfab.

### **Roof-peel and slice animations.**
Cheap follow-ons to `spin` that reuse the roof levels and Z cuts Lifts roof levels off one by one, or moves a Z cut from the floor up to the sky so the map appears to build itself. Works well as an intro before a spin. Accepts number of roofs or number of slices.

### Animated textures and water warp

Plays `+0` to `+9` texture sequences and adds the classic sine warp to `!water`. Just a simple on/off for this I think is easiest.

## New looks

Changes to the renderer and post-processing. The depth buffer already exists and just isn't read back.

### Ambient occlusion and ink outlines

Screen-space AO darkens corners and the bases of crates. Edge detection on depth draws crisp outlines. Together they give a blueprint or comic style, which reads well on flatly lit maps.

**Builds on:** a second pass over the `Depth32Float` target that `make_targets` already creates.

### Tilt-shift miniature

A perspective camera plus depth-based blur so the map looks like a tabletop model photographed with a macro lens.

In general, we want to give users full control of camera view in gui mode so they can manipulate it as needed.

**Builds on:** `camera.rs` only has `ortho`; a perspective matrix is a small addition. The sky shader already works with a field of view.

### Exploded floors

For multi-level maps like cs_assault, de_nuke and de_vertigo, each floor is split off and floated above the one below, so every level is visible in one image.

**Builds on:** the roof level boundaries decide where to split; each band is drawn with its own vertical offset.

### Relighting: sun, shadows, time of day

Ignores the baked lightmaps and lights the map from the angle and colour in `light_environment`, with shadow maps. That gives golden-hour or night versions of a map, or an animated day cycle.

### Map health report

A one-page summary for each map: missing textures and WADs, whether VIS was run (a missing VIS usually means a leak), how close it is to engine limits (faces, clipnodes, lightmap and texture data), spawn counts per team, and the largest open areas.

**Builds on:** most counts are already logged at load time. This formats them and compares against the engine's hard limits.

### Poster mode

Renders in tiles to get past the GPU's texture size limit (16k and beyond), and adds a title block, coordinate border and legend. The output is print-ready.

**Builds on:** `View` centre and size make tiling a loop; `render_view` already bails when the target exceeds `max_dim`.

### Vector callout maps

M

Projects floor and wall outlines into SVG line art that can be scaled and edited. Good for strategy boards and printouts.

### 3D-printable diorama

L

Converts the playable area's solid space into voxels using the clipnode hulls and turns that into a watertight STL mesh with marching cubes, so you can print de_dust2 for your desk.

---

# Implementation plan

Written 2026-09-24 against the code in `C:\Users\servor\rust\bsp2img` (single commit "Initial commit", which includes `spin` and `timing`). Nothing below has been built yet.

## 1. Ground rules for the implementing agent

- Read `C:\Users\servor\.claude\CLAUDE.md` first. In short: no code comments, no AI attribution anywhere, commit messages of 1-5 words with no body, terse public text.
- Build with `cargo build --release`. If `bsp2img.exe` is running (the user often has the GUI open), the build fails with "Access is denied". Ask the user to close it, or build with `CARGO_TARGET_DIR` pointed at a scratch folder.
- There is no test suite. Verify every feature by rendering real maps into a scratch folder and looking at the images. Test data:
  - Game: `F:\servers\cs-coftest` (490 de/cs/as maps in `cstrike\maps`).
  - Useful maps: `aim_map` (tiny, symmetric), `de_dust2`, `de_nuke` (ladders, stacked sites), `cs_assault` and `de_vertigo` (multi-level), `cs_office` (vents, breakables), `de_aztec` (water).
  - HLTV demos: `F:\servers\cs-coftest\cstrike\hltv\*.dem` (10 files, 207 MB). `2026-0904-0050-de_oc2.log` is the matching server log for one of them.
  - Server logs: `cstrike\logs\L*.log`. The kill lines have no coordinates.
- Every export follows the same pattern. Keep to it:
  1. `XOpts` struct plus `export_x(r, bsp, name, cut_tag, cuts, &opts, out, log)` in the feature's own module.
  2. `XArgs` in `cli.rs`, with `#[command(flatten)] c: Common`, plus `run_x`.
  3. A `Job::X` and its settings in the GUI.
  4. A README section.
  5. Output goes to `run_dir(out, name)`, filenames go through `free_name`, messages go through the `Log` callback.
- Keep CLI and GUI in step: anything the CLI can export, the GUI can export too.
- Run `bsp2img timing` across all maps after touching `reach.rs`, `bsp.rs` or the nav code. The loop used last time took about 10 minutes and caught ~60 edge cases.

## 2. Difficulty ranking

| Rank | Feature | Size | Main difficulty |
|---|---|---|---|
| 1 | Coplanar face z-fighting fix | S | Finding overlapping coplanar faces at mesh build, one vertex attribute |
| 2 | Roof-peel and slice animations | S | Only per-frame cuts on top of the `spin` pipeline |
| 3 | Map health report | S-M | Mostly counting. Needs the visibility lump and a checked table of engine limits |
| 4 | Animated textures and water warp | M | Finding frame sequences, a time uniform, the GoldSrc warp formula |
| 5 | Ambient occlusion and ink outlines | M | Building the first GPU post-processing chain (reused by later features) |
| 6 | Exploded floors | M | CPU triangle clipping at level boundaries |
| 7 | Vector callout maps (SVG) | M | Marching squares over the nav grid, one layer per level |
| 8 | Poster mode | M | Tiled rendering, streaming PNG, seams with post effects, layout |
| 9 | glTF export | M | Format plumbing, axis and unit conversion, baking the lightmap |
| 10 | Full camera control, perspective, tilt-shift | M-L | Camera refactor touches every view, export and GUI input path |
| 11 | Relighting (sun, shadows, time of day) | L | Vertex normals, shadow maps, a shading model that keeps interiors lit |
| 12 | 3D-printable diorama | L | Voxelising, meshing and guaranteeing a watertight result |

## 3. Order of work

Grouped by shared foundations, not strictly by difficulty. Each phase leaves the program releasable.

| Phase | Contents | Why here |
|---|---|---|
| 0 | GUI restructure, background jobs, shared look options | Every later feature adds UI and long-running exports |
| 1 | Nav module split, roof-peel/slice, health report, z-fighting fix | Quick wins that reuse existing code |
| 2 | Renderer groundwork (normals, time, post chain), animated textures, AO and ink | Later effects depend on this plumbing |
| 3 | Camera model, perspective, GUI camera control, tilt-shift | Needs the post chain from phase 2 |
| 4 | Mesh clipping utility, exploded floors | Clipping is reused by glTF and STL |
| 5 | Poster, SVG callouts, glTF | Output formats built on phases 2-4 |
| 6 | Relighting | Needs normals (2) and camera (3) |
| 7 | STL diorama | Standalone, large |

---

## Phase 0: GUI restructure and background jobs

### 0.1 Problems today

- **Clutter:** `gui.rs` (905 lines) has one scrolling left panel with Map, Crop, Cuts, Look and Export stacked. Export already holds four exporters (isometric, animation, rush timings, overview) and this list adds eight more.
- **Frozen UI:** `run_job` runs exports on the UI thread. Animations and timing take seconds to minutes, so the window stops responding, the spinner doesn't move, and there's no progress or cancel.
- **Lost settings:** only `game` and `out` persist in `%APPDATA%\bsp2img\gui.cfg`.

### 0.2 Proposed layout: tabs

```
+-------------------------------------------------------------------------------+
| de_dust2 (F:\...\maps)   [Open .bsp] [Reload]      [#####-----] Exporting 43% [Cancel] |
+----------------------------+--------------------------------------------------+
| [Map][Scene][Look][Camera][Export] |  Iso | Top | Overview | Free    yaw 45  ...  |
|----------------------------|                                                  |
|  (contents of the active   |                 preview                          |
|   tab, scrolls on its own) |                                                  |
|                            |                                                  |
+----------------------------+--------------------------------------------------+
| log (collapsible)                                                             |
+-------------------------------------------------------------------------------+
```

- **Top bar:** a new `egui::Panel::top`. It shows the current map, Open .bsp and Reload (moved out of the Map section), and a job progress bar with Cancel on the right. Load progress also shows here.
- **Left panel:** a tab strip made of `selectable_value` buttons (the pattern the view-mode bar already uses; no new crate), with the active tab inside its own `ScrollArea`. The active tab persists.

| Tab | Contents |
|---|---|
| Map | Game folder, filter, map list (taller, now that it has the space), Open .bsp. |
| Scene | Auto crop, hull crop and pad, lighting sliders, Reload with these. Then roofs, Z/XY cuts, Clear cuts, and later exploded-floor settings. |
| Look | Sky, background, cutaway, pixelated textures. From phase 2 on: animated textures, AO, ink, tilt-shift, relighting. Grouped into collapsible sections: Background, Textures, Effects, Lighting. |
| Camera | Added in phase 3. Until then, pitch and the 4 yaw presets move here from the Export section. |
| Export | Output folder at the top, then an output picker (a vertical list or a `ComboBox`) grouped as below. Only the chosen exporter's settings and its one button are shown. |

Export picker groups:
- **Images:** Isometric, Poster.
- **Animation:** Spin, Roof peel, Slice.
- **Counter-Strike:** Overview.
- **Analysis:** Rush timings, Kill heatmap, Health report.
- **3D and vector:** glTF, STL diorama, SVG callouts.

- **Central area:** the view-mode bar gets a fourth mode, "Free" (perspective, phase 3). Later analysis results can show as preview overlays; see the optional item in 1.3.
- **Log:** `Panel::bottom` with a collapse toggle. Keep it auto-scrolling.

Why tabs rather than dockable panes: `egui_dock` adds a dependency and window-management complexity that a single-user tool doesn't need. A tab strip and an export picker cut the visible controls to about a fifth without hiding anything more than one click deep.

### 0.3 Code structure

Split `src/gui.rs` into `src/gui/`:

| File | Contents |
|---|---|
| `mod.rs` | App struct, `run`, eframe glue, top bar and log |
| `tabs.rs` | One `fn tab_map/tab_scene/tab_look/tab_camera(&mut App, ui)` per tab |
| `export_ui.rs` | Export picker and each exporter's settings form |
| `preview.rs` | `ensure_preview`, `central`, `handle_input`, `overlay_top`, `to_world`, `to_screen` |
| `jobs.rs` | Background job runner |
| `cfg.rs` | Settings persistence |

`build_plan.md` already describes a `gui/` module split that was never done. Update it.

### 0.4 Background jobs

- Keep the scene in an `Arc<Scene>` (`App.scene: Option<Arc<Scene>>`). `Scene` holds only plain data, so it is Send and Sync.
- `Job` becomes an enum carrying a snapshot of its settings (`Job::Spin(SpinOpts)`, ...). The worker never reads `App`.
- The worker is a `std::thread::spawn` that:
  1. Builds its own `Renderer` with `scene.renderer(&gpu, nearest)`. `Gpu` is Clone and the wgpu device and queue can be shared across threads. Uploading textures again takes milliseconds.
  2. Applies the same sky (`set_sky`) and mask as the preview. Add `Scene::job_renderer(&self, gpu, nearest, sky: Option<(String, fov, pitch)>)` so the CLI and GUI build renderers the same way.
  3. Sends `Msg::Log`, `Msg::Progress(f32, String)` and `Msg::JobDone(Result<PathBuf, String>)` over the existing channel.
- Add an `Arc<AtomicBool>` cancel flag.
- Export functions need progress and cancel. Replace the `Log` callback argument with a small context:
  ```
  pub struct Report<'a> { pub log: &'a mut dyn FnMut(String), pub progress: &'a mut dyn FnMut(f32) -> bool }
  ```
  `progress` returns false when cancelled. Exporters call it per frame or tile and stop cleanly, deleting any partial output file.
  - Update the call sites in `export.rs`, `spin.rs`, `timing.rs`, `overview.rs` and `cli.rs`. The CLI passes a progress closure that prints a percentage on one line.
- Only one job runs at a time. Export buttons stay disabled while a job runs, but preview interaction keeps working.

### 0.5 Settings persistence

Save every GUI setting that has a CLI equivalent (iso, spin, overview and timing options, look options, active tab, chosen exporter) to `gui.cfg`. Keep the current `key=value` format. Add a `fn kv(&self) -> Vec<(String, String)>` and a `fn set(&mut self, k, v)` to each options struct rather than pulling in serde.

### 0.6 Shared look options

Create `src/look.rs` with `pub struct Look { bg, sky, cull, nearest, ... }`. It grows in phases 2, 3 and 6 (`anim_textures`, `ao`, `ink`, `tilt_shift`, `relight`, ...). Flatten a matching `LookArgs` into the iso, spin, peel, slice and poster CLI args, so every image export gets the same effects. The phase 0 fields are only the existing ones: bg, sky name, fov and pitch, cull, nearest.

**Check:**
- Every tab renders.
- Every existing export works from the GUI, and the window stays responsive during a 720-frame spin.
- Cancel stops a spin within one frame and leaves no half-written file.
- Settings survive a restart.

---

## Phase 1: Quick wins

### 1.1 Move the nav grid into its own module (prep)

Move `Nav`, `Node`, the blocker handling and `Nav::build/flood/nearest/nodes_in/cols_in` from `timing.rs` to `src/nav.rs`, public. `timing.rs` keeps zones, the field and drawing. The health report (1.3), SVG (5.2), STL (7) and heatmaps (8) all reuse the walk grid.

Add:
- `Nav::floor_at(x, y, zmax) -> Option<usize>`: the highest node in a column below `zmax`.
- `Nav::clearance() -> Vec<f32>`: per node, the distance to the nearest non-walkable neighbour. Use a 2D chamfer or Euclidean distance transform per floor, following the `orth` links.

**Check:** `timing` output for dust2 is unchanged (compare the `.txt` and diff the images).

### 1.2 Roof-peel and slice animations

**Goal:** two new animation kinds on the `spin` pipeline. Peel lifts roof levels off one at a time. Slice raises a Z cut from the floor to the sky so the map appears to build itself. The user wants a count for each: the number of roofs for peel, the number of slices for slice.

**Approach:**
1. In `spin.rs`, split `SpinOpts` into a shared `AnimOpts` (size, ss, pitch, start yaw, fps, bg, cull, mp4/gif/apng) and a kind:
   ```
   enum Anim { Spin { seconds, ccw }, Peel { roofs: usize, seconds_per: f64, hold: f64, reverse: bool, then_spin: bool }, Slice { slices: u32, seconds: f64, hold: f64, then_spin: bool } }
   ```
   `export_spin` becomes `export_anim`. The frame loop asks the kind for `(View, Cuts)` per frame instead of only a view.
2. **Framing:** compute it once, from `points_in` with the **uncut** Z range, so the camera never moves while geometry disappears. Use `spin_views` with a single yaw for peel and slice. If `then_spin` is set, use the spin's multi-yaw framing for the whole clip, so the intro and the spin share one canvas.
3. **Peel:** `scene.levels` is sorted high to low, each entry `(lo, hi, area)`. For level k = 0..roofs, move `zmax` smoothly (ease-in-out) from `levels[k].1 + 1` down to `levels[k].0 - 1` over `seconds_per`, then hold for `hold` seconds. `roofs` is clamped to `levels.len() - 1`, as `roof_zmax` does. `reverse` plays it backwards (roofs drop into place).
4. **Slice:** the Z range comes from `points_in(cuts)`. With `slices == 0`, `zmax` sweeps continuously from zmin to zmax. With `slices = N`, it moves in N equal steps with a hold at each, and the step itself eases over 0.3 s.
5. **then_spin:** after the last hold, keep the final cuts and append a full turn (the existing Spin frames) to the same encoder stream.

**CLI:**
- `bsp2img peel <map> --count N [--seconds-per 1.5] [--hold 0.5] [--reverse] [--then-spin]`
- `bsp2img slice <map> --count N [--seconds 6] [--hold 0.4] [--then-spin]`

Use `--count`, because `--roofs` already exists in `Common` as a static cut and would clash. The static `--roofs` still applies as a starting cut.

**GUI:** Export > Animation, with a kind picker (Spin / Roof peel / Slice) that shows the matching fields.

**Pitfall:** with cutaway culling on, sliced walls show their hollow interiors. That's acceptable. Solid caps on cut faces are out of scope; mention it in the README.

**Check:**
- A peel of cs_assault and de_nuke with `--count 3`: at each hold the frame must match a static `iso --roofs k` render at the same yaw and size. Compare the extracted frame with the PNG; small encoding differences only.
- A slice of dust2 with `--count 0` and `--count 8`.
- `--then-spin` produces one continuous file.

### 1.3 Map health report

**Goal:** a one-page summary per map, and a summary table across many maps.

**Data to gather:**
1. **Missing assets:** WADs and textures (the `TextureSource` misses already logged in `Scene::load`), the sky (`find_sky`), and the `models`, `sprites` and `sounds` named in entity keys (`model`, `message` for `ambient_generic`, `noise*`) that don't exist under the search dirs.
2. **Compile health:** parse lump 4 (`LUMP_VISIBILITY`, add the constant to `bsp.rs`) for its size only.
   - VIS size 0 means VIS was not run. That usually means a leak, or `-novis`.
   - Also report lightmap data size, whether any face has lightofs -1 while not special (sign of `-nolight`), and entity count.
3. **Engine limits:** a table of counts against limits. Get the limits from the GoldSrc SDK `bspfile.h` and from VHLT's limits. **Verify each value before using it and cite the source in a code constant name, not a comment.** Candidates:

   | Item | Candidate limit |
   |---|---|
   | Models | 512 |
   | Planes | 32767 |
   | Vertices | 65535 |
   | Nodes | 32767 |
   | Texinfo | 32767 |
   | Faces | 65535 |
   | Clipnodes | 32767 |
   | Leaves | 8192 |
   | Marksurfaces | 65535 |
   | Edges | 256000 |
   | Surfedges | 512000 |
   | Miptex bytes | 0x200000 (VHLT uses 0x400000) |
   | Lighting bytes | 0x200000, engine-dependent |
   | Visibility bytes | 0x200000 |
   | Entity string | 128 KiB |

   Show a percentage with green below 75%, amber from 75% to 90%, red above 90%. Where the engine limit and the compiler limit differ, show both.
4. **Gameplay setup:**
   - T and CT spawn counts (`info_player_deathmatch` and `info_player_start`). Warn below 16 per team for a 32-slot server.
   - Spawns that don't snap to walkable ground (from `Nav::nearest`).
   - Objectives present (bombsites, hostages, rescue zones, VIP).
   - Buy zones (`func_buyzone`; if missing, CS creates default buy zones around spawns, so note it rather than error).
5. **Overview readiness** (see memory notes in the user's global memory):
   - `overviews/<map>.txt`, `.bmp` and `.tga` present in the game dir or its `_downloads`.
   - BMP palette index 255 unused.
   - Map's `.res` lists the overview files.
6. **Largest open areas:** from `Nav::clearance()`, the 3 spots with the largest clearance radius, as world XY and radius in units. Big open spaces favour snipers. Also report total walkable area in m², at 1 unit = 1 inch.

**Output:**
- `<map>_health.txt`.
- `<map>_health.png`: a card with a top-down thumbnail from `grid_frame` (size 600), the open-area spots circled, and the limit bars drawn with the `grid.rs` helpers.
- With several maps (`bsp2img health de_* --game ...`, where the shell or a new glob helper expands the list), also write `health_summary.csv` in the base output folder, one row per map.

**GUI:** Export > Analysis > Health report. Also show the text result in a scrollable `egui::Window` after the job finishes.

**Optional:** show the timing heatmap as a toggleable overlay in the Top preview. `export_timing` already produces the per-pixel field, so expose `timing::field(...)` and upload it as an egui texture.

**Check:**
- Run over all 490 maps.
- Spot-check a map compiled without VIS, if one exists (sort the CSV by VIS size), and de_pophouse (spawns not on ground).
- Look at the CSV for any limit over 100%, which would mean a wrong limit value.

### 1.4 Coplanar face z-fighting

**Cause (measured 2026-09-24):** the compiler's CSG removes overlapping world faces, so the flicker comes almost entirely from brush entities sitting flush on world faces or on each other. Coplanar overlapping faces (exact convex-polygon intersection area above 4 square units) counted per map:

| Map | Entity pairs | Main culprits | Faces overlapping in one model |
|---|---|---|---|
| de_inferno | 27 | `func_illusionary` on `func_wall` (20) | 0 |
| cs_assault | 18 | `func_button` on `func_wall` | 2 |
| de_aztec | 154 | `func_water` against world walls (102) | 12 |
| cs_italy | 4 | `func_illusionary` on world | 0 |
| cs_office | 0 | — | 42 |
| de_dust2, de_nuke, aim_map | 0 | — | 0 |

The throwaway script that produced this used the Python reader from `C:\Users\servor\py\bsp2iso\goldsrc.py`:
- Bucket faces by rounded plane normal and distance.
- Within each bucket, clip every convex pair with Sutherland-Hodgman and keep pairs whose overlap area exceeds 4 square units.

Port that logic to Rust.

**Fix:** push the smaller face of each overlapping pair slightly toward the camera, so one face wins cleanly and consistently. In practice the smaller face is the overlay: a sign, a button, trim.
1. **Detect,** in `mesh.rs` at build time, over the faces actually drawn (after skip classes and textures):
   - Bucket faces by plane (normal rounded to 1e-3, distance rounded to 0.25 units, facing direction included).
   - Within a bucket, test pairs with a bounding-box reject, then exact convex overlap area (faces are convex, so a 2D Sutherland-Hodgman clip is enough).
   - Buckets are small, so the O(n²) inside a bucket is fine. Run buckets in parallel with rayon.
2. **Rank:** a face's level is the number of overlapping faces in its bucket that have a larger area, capped at 4. Nested overlays stack correctly: a sign on a trim on a wall gives 0, 1, 2. For equal areas, the higher model index wins (entities over world).
3. **Store:** add `bias: f32` (the level) to `mesh::Vertex`. Update the vertex layout in `render.rs`. If phase 2.1's normal attribute has already landed, add this next to it.
4. **Shader** (`world.wgsl` `vs`):
   ```
   clip = mvp * vec4(pos - view_dir * bias * 0.25, 1)
   ```
   `view_dir` is the camera forward vector (`basis.f`), passed in `FrameU`. Keep `o.world = pos` (unnudged), so Z/XY cuts and the hull mask still use true positions. In ortho the nudge is along the view direction, so nothing moves on screen; it only changes depth. 0.25 units is far above Depth32Float's resolution over any map depth range. Once phase 3 adds perspective, use `normalize(pos - eye)` instead of `basis.f`.
5. **Log:** add `coplanar overlaps: N faces nudged` to the load log, next to the batch count.

**Why not wgpu's `DepthBiasState`:** it's fixed per pipeline, so batches would have to be split by entity and every pipeline variant doubled. It also can't handle overlaps inside one model. A per-vertex attribute handles both cases with one code path.

**Check:**
- Render close-up iso crops (GUI zoom, or `--crop` around the entity) of each map above before and after, at `--ss 1` so supersampling doesn't hide shimmer.
  - The overlay should show cleanly: inferno signs, assault buttons.
  - Rotating in the GUI preview should no longer flicker.
- de_dust2 renders must be byte-identical to before (no overlaps found there).
- de_aztec: the `func_water` sides are mostly buried in walls. Confirm the water surface itself doesn't change.

---

## Phase 2: Renderer groundwork and the first effects

### 2.1 Groundwork (do first, as one commit)

1. **Vertex normals:** add `normal: [f32; 3]` to `mesh::Vertex` from `bsp.face_normal(fi)`. Update the vertex layout in `render.rs` (`vertex_attr_array![... 3 => Float32x3]`) and both shader entry points. Needed by AO, ink, relighting and glTF.
2. **Time:** add `time: f32` to `FrameU` (use the spare lane in `zr.w`, or a new vec4) and a `time` argument to `Renderer::encode` and `render_view`. Stills pass 0. Animation exports pass `frame / fps`.
3. **Post-processing chain:** today's path is render at `ss`×, read back, then Lanczos downsample on the CPU (`render_view`). Post effects must run on the GPU **at the supersampled resolution, before readback**.
   - Give the depth texture `TEXTURE_BINDING` usage in `make_targets`.
   - Add a second colour attachment, a view-space normal target in `Rgba16Float`, written by `fs` as a second output. This needs `targets: &[color, normal]` in the pipelines.
   - Add `src/post.rs`: a list of fullscreen-triangle passes, ping-ponging between two colour textures. Each pass has a WGSL file in `shaders/` and a small uniform block.
   - `Renderer::draw(enc, targets, view, look, time)` runs the world pass, then the post passes enabled in `Look`.
   - `render_view` (exports) and `central()` (GUI preview) both call `draw`, so previews match exports. Today the preview calls `encode` directly.
   - Ortho linear depth is `d * D + near`, with `D` from `camera::ortho`. Pass `near`, `D` and units-per-pixel to post shaders so their radii are in world units.

### 2.2 Animated textures and water warp

**Goal:** one on/off switch (`Look.anim_textures`, CLI `--animate-textures`, GUI Look > Textures). It cycles `+0..+9` texture sequences and warps `!` textures the way GoldSrc does.

**Approach:**
1. **Sequences:** in `mesh.rs`, when a miptex name starts with `+0`..`+9`, collect every miptex whose name matches `+<digit><rest>`. Look in the BSP's embedded textures first, then the WADs through `TextureSource::find`. The frames are often not referenced by any face, so load them explicitly. Store `anim: Vec<Option<Vec<usize>>>` indexed like `textures`. Ignore the alternate `+a..+j` sequence (it's switched by entity state).
2. **Frame rate:** GoldSrc advances animated textures at 10 frames/s: `frame = floor(time * 10) % n`. Confirm against `R_TextureAnimation` in the Quake/GoldSrc source before relying on it. The renderer keeps one bind group per frame for animated batches and picks one per draw.
3. **Water:** GoldSrc warps turbulent (`!`) surfaces in texel space:
   ```
   s' = s + 8 * sin(t * 0.125 + time)
   t' = t + 8 * sin(s * 0.125 + time)
   ```
   Then it scales by 1/64. Confirm `EmitWaterPolys` and the `TURBSCALE` sine table in Quake's `gl_warp.c`. Implement it in the fragment shader:
   - Flag water batches in the batch uniform (`bu.z = 1`).
   - Get texel coordinates with `uv * vec2<f32>(textureDimensions(tex))`, then warp.
   - Water has no lightmap (`special`), which the mesh already handles.
4. **GUI preview:** while the switch is on, call `ctx.request_repaint()` every frame and put `time` in the preview cache key, rounded to 1/30 s.

**Check:**
- Find maps with animated textures by scanning miptex names for a `+0` prefix across all 490 maps (a throwaway CLI flag or a small loop is fine).
- Render a short spin of one such map and of de_aztec (water) with the switch on, and confirm the textures move.
- Stills with the switch off must be byte-identical to before.

### 2.3 Ambient occlusion and ink outlines

**Goal:** darker corners and crisp lines. Presets give a blueprint or comic look.

**Approach (two post passes from 2.1):**
- **SSAO:** 16 samples in a normal-oriented hemisphere with a world-space radius (`--ao-radius`, default 48 units). Convert to screen space with units-per-pixel. Add a 4x4 rotation noise and a 4x4 blur, and multiply the result into the colour.
  - Options: `--ao` (on), `--ao-strength` (default 1.0), `--ao-radius`.
  - Because it runs at `ss`× resolution, scale sample counts with `ss` or the pass gets expensive: at ss 3 and size 2048 it's 6144², around 37 M pixels × 16 samples. Test the time.
- **Ink:** Sobel on linear depth plus normal discontinuity (dot product below 0.8). Line width is `--ink-width` output pixels × `ss`.
  - Options: `--ink`, `--ink-width 1.5`, `--ink-color #RRGGBB`.
  - Draw silhouettes against a transparent background too (alpha 1 on the line).
- **Presets** (`--style blueprint|comic`):
  - blueprint: background `#1d3b6e`, colour desaturated and tinted blue at 25%, white ink, AO off.
  - comic: AO on, black ink width 2, saturation +20%.

**GUI:** Look > Effects: AO checkbox and sliders, Ink checkbox, width, colour, style preset.

**Check:**
- dust2 and aim_map iso renders with each option alone and together, at ss 1 and ss 3. Lines must be the same visual width at both.
- Check the transparent-background PNG edges.
- Check render time at the default size and log it.

---

## Phase 3: Camera control, perspective and tilt-shift

### 3.1 Camera model

- Add `src/camera.rs::Camera`:
  ```
  { target: DVec3, yaw, pitch, roll(0), dist, proj: Proj::Ortho { upp } | Proj::Persp { fov_y } }
  ```
  - Add `fn basis()` (reuse `camera_basis`), `fn eye()` and `fn matrix(aspect, near, far) -> DMat4`.
  - Add `persp()` alongside `ortho()`, with wgpu 0..1 depth. Consider reverse-Z for precision on large maps.
- `View` gains `proj` and `eye`. `Renderer::encode` picks the matrix from `View`.
  - Near and far come from the scene points' distance along the view direction.
  - The sky pass already takes a field of view. In perspective, feed the camera fov and basis instead of the fixed `sky_fov`/`sky_pitch`, so the sky lines up with the view.
- Cuts, masks and back-face culling work unchanged because they use world positions.
- Framing helpers (`iso_view`, `spin_views`, `grid_frame`) stay ortho-only. Add `Camera::frame_points(points, aspect)` for perspective framing.

### 3.2 Full camera control in the GUI

- **Free mode:** a new preview mode (perspective by default, with an ortho toggle).

  | Input | Action |
  |---|---|
  | Left-drag | Orbit around the target |
  | Right-drag or middle-drag | Pan |
  | Wheel | Dolly (perspective) or zoom (ortho) |
  | Double-click on geometry | Set the orbit target to that point. Read the depth under the cursor by copying one texel from the preview depth target |
  | Hold right button + WASD/QE | Fly, at a speed scaled by map size |

- **Camera tab:**
  - Projection radio. Numeric yaw, pitch, distance, fov and target X/Y/Z (DragValues).
  - Presets: iso 45/135/225/315, top, overview framing, "frame map".
  - "Save camera…" and "Load camera…" (`.cam` text file using the `key=value` format).
  - "Use this camera for exports". When on, iso and poster exports use the GUI camera instead of automatic framing, and spin orbits around `target` at the camera's pitch and distance.
- Iso mode keeps its current behaviour, so existing users aren't surprised.

**CLI:** `--camera FILE` on iso, spin, peel, slice and poster. `--persp FOV` switches automatic framing to perspective.

### 3.3 Tilt-shift miniature

- Two post passes:
  1. Blur amount: in `--tilt-shift` mode, from the screen-space distance to a horizontal focus band (`--focus-y 0.5`, `--band 0.2`). This works in ortho too, and is the classic fake. In `--dof` mode (perspective only), from `|linear depth - focus depth|` instead.
  2. A separable variable-radius blur (two passes, max radius `--blur 12` px × `ss`).
- Add `--miniature`, which turns on tilt-shift with saturation +25% and contrast +10%.
- **GUI:** Look > Effects > Tilt-shift, with the band drawn as a translucent overlay while the slider is dragged. In Free mode, ctrl+click sets the focus distance.

**Check:**
- Iso and perspective renders of dust2.
- A spin with `--miniature`: the band must stay fixed on screen.
- A perspective render's sky must line up with the geometry at 3 yaw values.

---

## Phase 4: Mesh clipping and exploded floors

### 4.1 Clipping utility (reused by 5.3 and 7)

`src/clip.rs`:
- `clip_tris_z(verts: &[Vertex], planes: &[f64]) -> Vec<(band, Vec<Vertex>)>` splits triangles at horizontal planes (Sutherland-Hodgman per triangle, interpolating pos/uv/lm/normal).
- `clip_tris_cuts(verts, &Cuts, mask) -> Vec<Vertex>` does on the CPU what the shader does per fragment today: z range, XY box, hull mask (cells tested at triangle centroid after splitting large triangles to the mask cell size).

### 4.2 Exploded floors

**Goal:** split multi-level maps into bands and float each band above the one below.

**Approach:**
1. **Bands:** planes at chosen Z values. By default, one plane below each of the top N roof levels (`levels[k].0 - 1`, the same value `roof_zmax` uses).
   - `--explode N` takes the N planes with the largest vertical gaps between levels. `--explode-at z1,z2` sets them explicitly.
   - The GUI shows the level list with a checkbox per level.
2. **Build:** `Renderer::new_exploded(gpu, mesh, planes, gap)` builds vertex buffers from `clip_tris_z` and adds `band * gap` to z (`--explode-gap`, default 256). Build new buffers when the settings change; a dust2 rebuild takes under 100 ms.
   - `Renderer.points` must hold the offset points, so framing includes the lifted bands.
   - Cuts (z range) apply before the offset. Pass original Z as an extra vertex attribute, or cut in `clip_tris_cuts` first.
3. Ceilings of the lower band face down, so cutaway culling hides them when viewed from above. That's the intended look.
4. Optional: thin vertical guide lines at the four corners of each band's footprint (drawn as quads) so the stack reads as one building.

**Check:** cs_assault, de_nuke and de_vertigo as iso stills and a spin. Check that no triangles are lost at band edges (no gaps along walls).

---

## Phase 5: Large-format and interchange outputs

### 5.1 Poster mode

**Goal:** print-ready images larger than the GPU texture limit, with a title block, coordinate border and legend.

**Approach:**
1. **Size:** `--paper a0..a4|letter|tabloid --dpi 300 [--landscape]`, or `--px W H`. Resolve to pixel width and height.
2. **Framing:** iso (`--yaw`, `--pitch`), top-down, or `--camera FILE`, over the drawable area (page minus border).
3. **Tiles:** tile size is `floor(gpu.max_dim / ss)` minus margin. Each tile renders a sub-`View` (shift `cx`/`cy`, scale `w`/`h`) with a margin of `max(post-effect kernel radius) × ss` pixels on each side. After post-processing it's cropped back, so AO, ink and blur have no seams.
4. **Streaming:** render one row of tiles at a time and write finished rows with `png::StreamWriter`, so a 16k × 16k poster never sits in memory whole (that would be 1 GiB RGBA).
5. **Layout:** drawn with tiny-skia into the border strips and over each tile row, using the `grid.rs` helpers.
   - Title: map name, and the `message` key from worldspawn if present.
   - Coordinate ticks along the edges.
   - Legend: spawns, objectives.
   - Scale bar in metres (1 unit = 1 inch = 0.0254 m).
   - Date.
   - Font sizes in points convert with `dpi`.
6. **Report:** progress per tile.

**CLI:** `bsp2img poster de_dust2 --paper a2 --dpi 300 --top`. **GUI:** Export > Images > Poster.

**Check:**
- An A2 at 300 dpi (7016 × 4961) of dust2, top and iso.
- A forced tiny tile size (hidden `--tile 512` for testing) must produce the same image as a single-tile render of a small poster: pixel diff within Lanczos rounding, and no seams with AO and ink on.

### 5.2 Vector callout maps (SVG)

**Goal:** editable line art for strategy boards and print.

**Approach:**
1. From `Nav` (1.1), take the walkable nodes reachable from spawns and group them into floor bands. Use the exploded-floor planes if set, otherwise the roof levels.
2. Per band, rasterise walkable columns into a bitmap at the nav cell size (8 units). Run marching squares for the outlines, then Douglas-Peucker simplification (tolerance `--simplify 6` units). Holes are pillars and crates.
3. **SVG layers** (`<g id=...>`), in world units, with a `viewBox` that flips Y:
   - `floors-<band>`: filled polygons with a light tint per band. Lower bands drawn first; where an upper band covers them, dashed outline only.
   - `walls`: outlines.
   - `objectives`: bombsite and rescue rectangles or circles with labels, from `timing::zones` (move it to `nav.rs` or `grid.rs`).
   - `spawns`: dots.
   - `grid`: the coordinate grid, hidden by default.
   - `labels`: empty, for the user's own callout names.
   - Set `width`/`height` in mm so it prints to scale with `--scale 1:100`.
4. **Output:** `<map>_callouts.svg`, written by hand with `format!` (no crate needed).

**GUI:** Export > 3D and vector > SVG callouts.

**Check:** open in a browser and in Inkscape (user). Overlay it on the top render at the same framing to confirm alignment. Try dust2, nuke and office.

### 5.3 glTF export

**Goal:** a `.glb` that opens with correct textures and baked lighting in Blender, browsers and Sketchfab.

**Approach:**
1. **Crate:** `gltf-json` (Apache/MIT) to build the document. Pack the GLB by hand (12-byte header, JSON chunk, BIN chunk). Check the crate's current version and licence first.
2. **Geometry:**
   - Apply cuts on the CPU with `clip_tris_cuts` (4.1), so `--roofs` and the other cuts export what the user sees.
   - Convert GoldSrc (Z up, X forward, inches) to glTF (Y up, metres): `(x, y, z) -> (x, z, -y) * 0.0254`. The determinant is +1, so winding is unchanged. Confirm by eye in a viewer.
   - One primitive per batch.
3. **Materials:**
   - `{` textures: alphaMode MASK with cutoff 0.5.
   - Blend: alphaMode BLEND with `baseColorFactor.a = alpha`.
   - Additive: BLEND (glTF has no additive mode; note the approximation).
   - Sampler: repeat. Linear, or nearest when `--nearest`.
4. **Lighting modes (`--lighting`):**
   - `baked` (default): every face gets a unique region in a new atlas at `--texel` units per texel (default 2). Each texel is `texture(uv) × lightmap(lm)`, sampled on the CPU with bilinear filtering. UVs are replaced by atlas UVs. This looks right in every viewer, at the cost of texture tiling detail.
     - Size check: total face area / texel² for dust2. If the atlas would exceed 8192², split it over several atlases (one material each) or raise `--texel` automatically and log it.
   - `separate`: tiled textures on TEXCOORD_0, and the lightmap atlas on TEXCOORD_1 as `occlusionTexture` (`texCoord: 1`). Viewers show it greyscale. Also embed the full-colour atlas as an extra image so Blender users can wire it up. Say this in the README.
   - `none`: textures only.
5. **Embedded images:** PNGs from `mesh.textures` (after the tex gamma LUT, so colours match the renders). Deduplicate by texture index.

**CLI:** `bsp2img gltf de_dust2 [--lighting baked|separate|none] [--texel 2]`. **GUI:** Export > 3D and vector > glTF.

**Check:**
- Validate with the Khronos glTF validator (`npx gltf-validator file.glb`, or the web version) with no errors.
- Open dust2 and aim_map in https://gltf-viewer.donmccurdy.com (user) and in Blender (user).
- Confirm scale: a player-height doorway (~72 units) should be about 1.8 m.

---

## Phase 6: Relighting

**Goal:** light the map from `light_environment` (sun direction and colour) with shadows. This gives golden-hour and night looks and an animated day cycle.

**Approach:**
1. **Parse `light_environment`:**
   - `angles` (pitch yaw roll), overridden by `pitch`.
   - `_light` "R G B brightness".
   - `_diffuse_light` if present (VHLT sky light).
   - Fall back to pitch -60, yaw 45 and warm white if absent.
2. **Shadow map:** before the world pass, draw a depth pass from the sun direction (ortho over the scene bounds) into a Depth32 texture (`--shadow-res`, default 4096, 8192 for posters). Sample it in `fs` with 3x3 PCF and a slope-scaled bias. Tune for acne and peter-panning on dust2's long walls.
3. **Shading:**
   ```
   lit = albedo * mix(lightmap, ambient + sun * max(dot(N, L), 0) * shadow, relight_amount)
   ```
   - `ambient` is `sky_color * (0.5 + 0.5 * N.z)`.
   - `relight_amount` (`--relight 0..1`, default 1) blends baked and relit.
   - `--keep-lights`: where the fragment is in shadow and the lightmap luminance is above its local sun estimate, keep the lightmap, so interiors and lamps survive at night. Treat this as an artistic control and say so in the README.
4. **Time of day:** `--time HH:MM` maps to sun elevation and azimuth along a simple arc rotated to the map's `light_environment` yaw at noon, plus a colour temperature ramp:
   - Night: blue, low intensity.
   - Dawn/dusk: orange.
   - Noon: neutral.

   Tint the skybox by the same colour. `--day-cycle` becomes an animation kind (reusing phase 1.2's `Anim`) that sweeps time over the clip.
5. **GUI:** Look > Lighting: Baked / Relit / Blend, a time slider, sun azimuth and elevation overrides, a keep-lights checkbox. The preview updates live (the shadow map is cached per sun direction).

**Check:** dust2 at 06:00, 12:00, 18:30 and 23:00. de_nuke (large interiors) with and without `--keep-lights`. A 10-second day-cycle spin.

---

## Phase 7: 3D-printable diorama (STL)

**Goal:** a watertight STL of the playable area with roofs removed, sized for a desktop printer.

**Approach:**
1. **Solid test:** hull 0 via `point_leaf` contents (CONTENTS_SOLID), generalised per model (brush entities use `models[i].headnode[0]` into `nodes`). Include the same blocking entity classes as the nav grid, plus `func_door` (closed doors print better).
2. **Region:** the XY footprint of reachable nav columns, dilated by `--wall 32` units. Voxels outside the footprint are empty. Above `zmax` (from `--roofs`/`--zmax`) is empty. Below the lowest floor, fill solid down to a base plate of `--base 16` units.
3. **Grid:** `--voxel 8` units by default. dust2 is about 560 × 690 × 125 cells, 48 M voxels, a 6 MB bitset. Classify in parallel with rayon.
4. **Mesh:**
   - Default: greedy-meshed voxel faces (blocky, always watertight, crisp architecture).
   - `--smooth`: surface nets (the `fast-surface-nets` crate, MIT/Apache).
   - Either way, weld vertices and check that every edge is shared by exactly 2 triangles. Report and fail if not.
5. **Scale:** `--print-width 200` (mm across the longest side). Warn when a voxel is under 0.8 mm at that scale (walls too thin to print).
6. **Output:** binary STL (50 bytes per triangle). Also log the triangle count and bounding size in mm.

**CLI:** `bsp2img stl de_dust2 --roofs 1 --print-width 200`. **GUI:** Export > 3D and vector > STL diorama.

**Check:**
- The manifold check passes on aim_map, dust2, nuke and vertigo.
- Open in PrusaSlicer or Cura (user) and confirm no repair prompts.
- File size stays reasonable (dust2 blocky under 100 MB).

---
# SAVE THIS FOR A FUTURE UPDATE

## Phase 8: Kill and death heatmaps from HLTV demos  SAVE THIS FOR A FUTURE UPDATE

# SAVE THIS FOR A FUTURE UPDATE

### 8.1 Spike (time-boxed; report back before continuing)

**Goal:** prove the demos can be parsed well enough to get player positions at each death.

Facts established while writing this plan:
- The files are `HLDEMO`, demo protocol 5, `cstrike`.
- The server logs have no kill coordinates, so the demos are the only source.

Steps:
1. **Container:** header (map name, game dir, directory offset), directory entries, then frames. Frame types:

   | Type | Meaning |
   |---|---|
   | 0, 1 | Network data |
   | 2 | Start |
   | 3 | Console command |
   | 4 | Client data |
   | 5 | Next section |
   | 6 | Event |
   | 7 | Weapon animation |
   | 8 | Sound |
   | 9 | Demo buffer |

2. **Network messages (types 0/1):** a sequence of `svc_*` messages. Every one must be parsed to stay in sync. The ones that matter:
   - `svc_deltadescription`: delta field tables, sent in the demo itself.
   - `svc_spawnbaseline`.
   - `svc_packetentities` and `svc_deltapacketentities`: bit-level delta-encoded `entity_state_t` containing `origin`.
   - `svc_newusermsg`: registers user message ids and sizes, including `DeathMsg`.
   - `svc_updateuserinfo`: names and teams.
   - `svc_time`.
3. **Deaths:** the `DeathMsg` user message carries killer index, victim index, headshot and weapon name. Positions are the killer's and victim's entity origins (entity index = player slot + 1) from the entity state current in that frame.
4. **References:** use open-source parsers for the formats, and read their licences before borrowing structure; don't copy code.
   - hlviewer.js: MIT, a complete GoldSrc demo and delta parser in JavaScript.
   - coldemoplayer (C#).
   - Valve's `delta.lst` in `valve/` and `cstrike/` of the server install.
5. **Validation:** `cstrike\hltv\2026-0904-0050-de_oc2.log` is the server log for the de_oc2 demo in the same folder. Parsed death count, killer and victim names and weapons must match its `killed` lines.

The spike succeeds when positions for every death in 2 demos are extracted and plotted as dots on a `grid_frame` top-down render, and the dots sit on walkable floor.

### 8.2 Feature (after the spike)

- **Module:** `src/demo/` with `container.rs`, `bits.rs` (bit reader), `delta.rs`, `messages.rs` and `events.rs`. Export `fn deaths(path) -> Result<(String map, Vec<Death>)>` and `fn positions(path, every_n_frames) -> ...` (for a "where players spend time" heatmap).
- **CLI:** `bsp2img kills de_dust2 --demos "F:\...\hltv\*.dem"`. Only demos whose header map name matches are used; warn about mismatches and skip them.
- **Output**, reusing the timing drawing path (`grid_frame`, the darkened base render, the `grid.rs` helpers):
  - `<map>_kills.png`: kernel-density heatmap of victim positions, with T deaths and CT deaths as separate colour ramps or separate images.
  - Optional killer-to-victim lines (`--lines`).
  - Weapon filter (`--weapon awp`).
  - `<map>_presence.png`: time spent per area.
  - `<map>_kills.csv`: all events with coordinates.
- **GUI:** Export > Analysis > Kill heatmap, with a demo folder picker and a list of matching demos with checkboxes.

**Check:**
- Death counts match the server log for de_oc2.
- Dots fall on walkable areas (sample `Nav::floor_at`; flag any that are more than 32 units off).
- Aggregate at least 3 demos of one map.

---

## Appendix: new CLI subcommands at a glance

| Command | Phase |
|---|---|
| `peel`, `slice` | 1 |
| `health` | 1 |
| `poster` | 5 |
| `svg` | 5 |
| `gltf` | 5 |
| `stl` | 7 |
| `kills` | 8 |

Look flags added to iso, spin, peel, slice and poster:

| Flags | Phase |
|---|---|
| `--animate-textures` | 2 |
| `--ao`, `--ink`, `--style` | 2 |
| `--camera`, `--persp` | 3 |
| `--tilt-shift`, `--dof`, `--miniature` | 3 |
| `--explode*` | 4 |
| `--relight`, `--time`, `--keep-lights` | 6 |
