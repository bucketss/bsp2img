# bsp2img

Renders GoldSrc (Half-Life / CS 1.6) maps straight from the `.bsp`: isometric diorama views, and CS overviews with their `.txt`.

Requires opengl, dx12, or vulkan.

MP4 output requires ffmpeg on PATH.

## Build

- Rust 1.85+
- Windows: MSVC C++ Build Tools

```
cargo build --release
```
