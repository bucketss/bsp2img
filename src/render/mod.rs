use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use glam::{DMat4, DVec3};
use rayon::prelude::*;
use wgpu::util::DeviceExt;

use std::sync::{Arc, Mutex};

use crate::camera::{Basis, camera_basis, extents, ortho, persp, span};
use crate::explode::{Explode, band_points};
use crate::look::Look;
use crate::mesh::{Batch, Chunk, Mesh, Mode, Vertex};
use crate::post::{Cam, NORMAL_FORMAT, Post, PostTargets};
use crate::reach::HullMask;
use crate::sun::{Sun, SunEnv, baked_sun, sun_at};

pub use egui_wgpu::wgpu;

mod gpu;
mod readback;
mod shadow;

use gpu::*;
pub use gpu::{Gpu, attachment_view, ubuf_entry};
pub use readback::{composite_bg, downsample, parse_color, unpremultiply};

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const NO_CLIP: [f64; 4] = [-1e9, -1e9, 1e9, 1e9];
pub const ANIM_FPS: f64 = 10.0;
const PERSP_NEAR: f64 = 4.0;
const BACK_BIAS_CONST: i32 = -2;
const BACK_BIAS_SLOPE: f32 = -1.0;
const GLASS_BACK_BIAS_CONST: i32 = 4;
const GLASS_BACK_BIAS_SLOPE: f32 = 2.0;
const PERSP_MARGIN: f64 = 32.0;
const GUIDE_PX: f64 = 1.5;
const GUIDE_COLOR: [u8; 4] = [0xe8, 0xe8, 0xe8, 0xff];
const GUIDE_VERTS: usize = 24;
pub const SHADOW_RES: u32 = 4096;
const SHADOW_PAD: f64 = 32.0;
const SHADOW_NORMAL: f64 = 1.0;
const SHADOW_DEPTH: f64 = 0.75;
const SHADOW_SLOPE: f32 = 1.5;
const KEEP_MARGIN: f32 = 0.1;
const SHADOW_CACHE: usize = 2;
const BAND_EPS: f64 = 0.01;
const NO_BAND: f32 = -3.0e38;
const BAND_SLOTS: usize = 16;
const CULL_PAD: f64 = 2.0;

const TEX_CHUNK: usize = 64;

type RangeKey = ([f64; 6], (f64, f64));

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameU {
    mvp: [f32; 16],
    clip_xy: [f32; 4],
    mask_rect: [f32; 4],
    zr: [f32; 4],
    view_dir: [f32; 4],
    view_r: [f32; 4],
    view_u: [f32; 4],
    eye: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyU {
    r: [f32; 4],
    u: [f32; 4],
    f: [f32; 4],
    tanfov: [f32; 4],
    tint: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightU {
    mvp: [f32; 16],
    dir: [f32; 4],
    color: [f32; 4],
    ambient: [f32; 4],
    shadow: [f32; 4],
    mvp0: [f32; 16],
    dir0: [f32; 4],
    color0: [f32; 4],
    ambient0: [f32; 4],
    shadow0: [f32; 4],
    hdr: [f32; 4],
    sun: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LampU {
    pos: [f32; 4],
    color: [f32; 4],
    dir: [f32; 4],
    cone: [f32; 4],
}

struct HdrInfo {
    sun: [f32; 3],
    curve: [f32; 3],
    lamps: u32,
}

#[derive(Clone)]
struct ShadowMap {
    key: String,
    res: u32,
    view: wgpu::TextureView,
    mvp: DMat4,
    texel: f64,
    depth: f64,
}

struct GpuChunk {
    first: u32,
    count: u32,
    lo: DVec3,
    hi: DVec3,
}

struct Cull {
    m: DMat4,
    zmin: f64,
    zmax: f64,
    clip: [f64; 4],
}

impl Cull {
    fn new(m: &DMat4, cuts: &Cuts) -> Cull {
        Cull { m: *m, zmin: cuts.zmin, zmax: cuts.zmax, clip: cuts.clip }
    }

    fn keeps(&self, c: &GpuChunk, w: &Band) -> bool {
        let [x0, y0, x1, y1] = self.clip;
        if c.hi.z < self.zmin || c.lo.z > self.zmax || c.hi.z <= w.lo as f64 || c.lo.z > w.hi as f64 {
            return false;
        }
        if c.hi.x < x0 || c.lo.x > x1 || c.hi.y < y0 || c.lo.y > y1 {
            return false;
        }
        let lift = DVec3::Z * w.lift;
        let (lo, hi) = (c.lo - DVec3::splat(CULL_PAD) + lift, c.hi + DVec3::splat(CULL_PAD) + lift);
        let mut out = [true; 6];
        for i in 0..8 {
            let p = DVec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
            let q = self.m * p.extend(1.0);
            let o = [q.x < -q.w, q.x > q.w, q.y < -q.w, q.y > q.w, q.z < 0.0, q.z > q.w];
            for k in 0..6 {
                out[k] &= o[k];
            }
        }
        !out.contains(&true)
    }

    fn ranges(&self, b: &GpuBatch, w: &Band) -> Vec<std::ops::Range<u32>> {
        let mut out: Vec<std::ops::Range<u32>> = Vec::new();
        for c in b.chunks.iter().filter(|c| self.keeps(c, w)) {
            match out.last_mut() {
                Some(r) if r.end == c.first => r.end += c.count,
                _ => out.push(c.first..c.first + c.count),
            }
        }
        out
    }
}

struct GpuBatch {
    mode: Mode,
    chunks: Vec<GpuChunk>,
    pbuf: wgpu::Buffer,
    abuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    zr: [f32; 2],
    bind: wgpu::BindGroup,
    frames: Vec<wgpu::BindGroup>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Packed {
    uv: [f32; 2],
    uv2: [f32; 2],
    lm: [f32; 2],
    nb: u32,
    blend: [u16; 4],
    paint: [u8; 4],
    light: u32,
    mask: [u8; 4],
}

impl Packed {
    fn new(v: &Vertex) -> Packed {
        let [x, y, z] = v.normal;
        let l1 = x.abs() + y.abs() + z.abs();
        let (mut ox, mut oy) = if l1 > 0.0 { (x / l1, y / l1) } else { (0.0, 0.0) };
        if z < 0.0 {
            let s = |c: f32| if c >= 0.0 { 1.0 } else { -1.0 };
            (ox, oy) = ((1.0 - oy.abs()) * s(ox), (1.0 - ox.abs()) * s(oy));
        }
        let q = |c: f32| ((c.clamp(-1.0, 1.0) * 0.5 + 0.5) * 16383.0).round() as u32;
        let bias = (v.bias.round().clamp(0.0, 15.0) as u32) << 28;
        Packed {
            uv: v.uv,
            uv2: v.uv2,
            lm: v.lm,
            nb: q(ox) | (q(oy) << 14) | bias,
            blend: v.blend,
            paint: v.paint,
            light: v.light,
            mask: v.mask,
        }
    }
}

struct Band {
    off: u32,
    lo: f32,
    hi: f32,
    lift: f64,
}

impl Band {
    fn meets(&self, zr: [f32; 2]) -> bool {
        zr[1] > self.lo && zr[0] <= self.hi
    }
}

struct SkyRes {
    pipelines: [wgpu::RenderPipeline; 2],
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    fov: f64,
    pitch: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Cuts {
    pub zmin: f64,
    pub zmax: f64,
    pub clip: [f64; 4],
    pub use_mask: bool,
}

impl Default for Cuts {
    fn default() -> Self {
        Cuts { zmin: -1e9, zmax: 1e9, clip: NO_CLIP, use_mask: true }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub basis: Basis,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub sky_yaw: Option<f64>,
    pub persp: Option<Persp>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Persp {
    pub eye: DVec3,
    pub fov_y: f64,
    pub focus: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    fn apply(&self, m: DMat4, tw: u32, th: u32) -> (DMat4, [f64; 4]) {
        if self.x == 0 && self.y == 0 && self.w == tw && self.h == th {
            return (m, [1.0, 1.0, 0.0, 0.0]);
        }
        let (fw, fh) = (self.w as f64, self.h as f64);
        let a0 = 2.0 * self.x as f64 / fw - 1.0;
        let a1 = 2.0 * (self.x + tw) as f64 / fw - 1.0;
        let b1 = 1.0 - 2.0 * self.y as f64 / fh;
        let b0 = 1.0 - 2.0 * (self.y + th) as f64 / fh;
        let (sx, sy) = (2.0 / (a1 - a0), 2.0 / (b1 - b0));
        let (ox, oy) = ((a0 + a1) / (a1 - a0), (b0 + b1) / (b1 - b0));
        let c = DMat4::from_cols(
            glam::DVec4::new(sx, 0.0, 0.0, 0.0),
            glam::DVec4::new(0.0, sy, 0.0, 0.0),
            glam::DVec4::new(0.0, 0.0, 1.0, 0.0),
            glam::DVec4::new(-ox, -oy, 0.0, 1.0),
        );
        (c * m, [1.0 / sx, 1.0 / sy, (a0 + a1) / 2.0, (b0 + b1) / 2.0])
    }
}

pub struct Targets {
    pub w: u32,
    pub h: u32,
    pub color: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    post: Option<PostTargets>,
}

impl Targets {
    pub fn has_post(&self) -> bool {
        self.post.is_some()
    }
}

struct Guides {
    vbuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    feet: Mutex<Option<(String, Vec<Option<[f64; 4]>>)>>,
}

pub struct GpuScene {
    gpu: Gpu,
    batch_layout: wgpu::BindGroupLayout,
    frame_layout: wgpu::BindGroupLayout,
    light_layout: wgpu::BindGroupLayout,
    samplers: [wgpu::Sampler; 2],
    lmap_view: wgpu::TextureView,
    shmask_view: wgpu::TextureView,
    lamp_buf: wgpu::Buffer,
    hdr: Option<HdrInfo>,
    casters: Option<(wgpu::Buffer, wgpu::Buffer, u32, wgpu::BindGroup)>,
    dummy_view: wgpu::TextureView,
    linear_clamp: wgpu::Sampler,
    cmp_sampler: wgpu::Sampler,
    pipelines: Vec<wgpu::RenderPipeline>,
    shadow_pipe: [wgpu::RenderPipeline; 2],
    batches: Vec<GpuBatch>,
    points: Arc<[DVec3]>,
}

pub struct Renderer {
    pub gpu: Gpu,
    sc: Arc<GpuScene>,
    nearest: bool,
    pub points: Arc<[DVec3]>,
    pub mask: Option<HullMask>,
    orig: Option<Vec<DVec3>>,
    pt_band: Vec<u16>,
    explode: Option<Explode>,
    guides: Option<Guides>,
    frame_stride: u64,
    band_slots: usize,
    frame_buf: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,
    mask_view: Option<wgpu::TextureView>,
    sky: Option<SkyRes>,
    post: Post,
    light_buf: wgpu::Buffer,
    light_off: wgpu::BindGroup,
    shadow_frame_buf: wgpu::Buffer,
    shadow_frame_bind: wgpu::BindGroup,
    shadow: Mutex<Vec<ShadowMap>>,
    pts_cache: Mutex<Option<(String, Arc<[DVec3]>)>>,
    range_cache: Mutex<Option<RangeKey>>,
    pick: std::sync::OnceLock<(wgpu::ComputePipeline, wgpu::BindGroupLayout)>,
    pub sun_env: SunEnv,
    pub tex_gamma: f32,
}

fn make_batch(
    st: &mut Stager,
    layout: &wgpu::BindGroupLayout,
    tex_views: &[Option<wgpu::TextureView>],
    mesh: &Mesh,
    b: &Batch,
    verts: &[Vertex],
    idx: &[u32],
) -> Option<GpuBatch> {
    let Some(Some(view)) = tex_views.get(b.tex) else { return None };
    if idx.is_empty() {
        return None;
    }
    let pbuf = st.packed(wgpu::BufferUsages::VERTEX, verts, |v| v.pos);
    let abuf = st.packed(wgpu::BufferUsages::VERTEX, verts, Packed::new);
    let ibuf = st.buffer(wgpu::BufferUsages::INDEX, bytemuck::cast_slice(idx));
    let (water, warp) = match b.warp {
        Some(w) => (1.0, w),
        None => (0.0, [0.0; 4]),
    };
    let t = b.tint;
    let layer = b.layer2.and_then(|(i, uv)| tex_views.get(i).and_then(|v| v.as_ref()).map(|v| (v, uv)));
    let (l2, uv2) = match layer {
        Some((v, uv)) => (Some(v), uv),
        None => (None, [1.0, 1.0, 0.0, 0.0]),
    };
    let has2 = if l2.is_some() { 1.0 } else { 0.0 };
    let layer3 = b.layer3.and_then(|(i, uv, second)| tex_views.get(i).and_then(|v| v.as_ref()).map(|v| (v, uv, second)));
    let (l3, uv3, on3, second3) = match layer3 {
        Some((v, uv, sec)) => (Some(v), uv, 1.0, if sec { 1.0 } else { 0.0 }),
        None => (None, [1.0, 1.0, 0.0, 0.0], 0.0, 0.0),
    };
    let tmask = b.tint_mask.and_then(|i| tex_views.get(i).and_then(|v| v.as_ref()));
    let four = b.four.as_ref();
    let l4 = four.and_then(|f| f.tex4).and_then(|(i, uv)| tex_views.get(i).and_then(|v| v.as_ref()).map(|v| (v, uv)));
    let uv4 = l4.map_or([1.0, 1.0, 0.0, 0.0], |(_, uv)| uv);
    let fw = four.map_or([0.0; 20], |f| {
        let mut a = [0.0; 20];
        a[..8].copy_from_slice(&f.lum);
        a[8..14].copy_from_slice(&f.blend);
        a[14] = if f.mul3 { 1.0 } else { 0.0 };
        a[15] = if f.mul4 { 1.0 } else { 0.0 };
        a[16] = 1.0;
        a[17..20].copy_from_slice(&f.lumblend);
        a
    });
    let mut u = vec![
        b.mode as u32 as f32, b.alpha, water, 0.0, warp[0], warp[1], warp[2], warp[3], 1.0, 0.0, has2, 0.0, t[0], t[1], t[2], 1.0,
        uv2[0], uv2[1], uv2[2], uv2[3], uv3[0], uv3[1], uv3[2], uv3[3], on3, second3, if tmask.is_some() { 1.0 } else { 0.0 }, 0.0,
    ];
    u.extend_from_slice(&uv4);
    u.extend_from_slice(&fw[..8]);
    u.extend_from_slice(&fw[8..12]);
    u.extend_from_slice(&[fw[12], fw[13], fw[14], fw[15]]);
    u.extend_from_slice(&[fw[16], fw[17], fw[18], fw[19]]);
    let ub = st.buffer(wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&u));
    let dev = &st.gpu.device;
    let make = |v: &wgpu::TextureView| batch_bind(dev, layout, [v, l2.unwrap_or(v), l3.unwrap_or(v), tmask.unwrap_or(v), l4.map_or(v, |l| l.0)], &ub);
    let frames = match mesh.anim.get(b.tex) {
        Some(Some(seq)) => seq.iter().filter_map(|&i| tex_views.get(i).and_then(|v| v.as_ref())).map(make).collect(),
        _ => Vec::new(),
    };
    let zr = verts.iter().fold([f32::INFINITY, f32::NEG_INFINITY], |r, v| [r[0].min(v.pos[2]), r[1].max(v.pos[2])]);
    let whole = [Chunk::new(verts, 0, idx.len())];
    let chunks = if b.chunks.is_empty() { &whole[..] } else { &b.chunks[..] };
    let dv = |p: [f32; 3]| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
    let chunks = chunks.iter().map(|c| GpuChunk { first: c.first, count: c.count, lo: dv(c.lo), hi: dv(c.hi) }).collect();
    Some(GpuBatch { mode: b.mode, chunks, pbuf, abuf, ibuf, zr, bind: make(view), frames })
}

fn batch_bind(dev: &wgpu::Device, layout: &wgpu::BindGroupLayout, v: [&wgpu::TextureView; 5], ub: &wgpu::Buffer) -> wgpu::BindGroup {
    dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(v[0]) },
            wgpu::BindGroupEntry { binding: 2, resource: ub.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(v[1]) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(v[2]) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(v[3]) },
            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(v[4]) },
        ],
    })
}

impl GpuScene {
    pub fn new(gpu: &Gpu, mesh: &Mesh) -> Result<Arc<GpuScene>> {
        let bytes: usize = mesh.batches.iter().map(|b| b.verts.len() * (12 + std::mem::size_of::<Packed>()) + b.idx.len() * 4).sum();
        gpu.guard(|| format!("{} MB of geometry", bytes >> 20), || Arc::new(Self::upload(gpu, mesh)))
    }

    fn upload(gpu: &Gpu, mesh: &Mesh) -> GpuScene {
        let mut tm = crate::prof::Timer::new("upload");
        let dev = &gpu.device;
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/world.wgsl").into()),
        });
        let frame_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<FrameU>() as u64),
                    },
                    count: None,
                },
                tex_entry(1, wgpu::TextureViewDimension::D2),
                samp_entry(2),
                tex_entry(3, wgpu::TextureViewDimension::D2),
                samp_entry(4),
                tex_entry(5, wgpu::TextureViewDimension::D2),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                samp_entry(7),
            ],
        });
        let batch_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0, wgpu::TextureViewDimension::D2),
                ubuf_entry(2, wgpu::ShaderStages::VERTEX_FRAGMENT),
                tex_entry(3, wgpu::TextureViewDimension::D2),
                tex_entry(4, wgpu::TextureViewDimension::D2),
                tex_entry(5, wgpu::TextureViewDimension::D2),
                tex_entry(6, wgpu::TextureViewDimension::D2),
            ],
        });
        let light_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                ubuf_entry(0, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&frame_layout), Some(&batch_layout), Some(&light_layout)],
            immediate_size: 0,
        });
        let pos_layout = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
        };
        let attr_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Packed>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![
                1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Uint32, 5 => Unorm16x4, 6 => Unorm8x4, 7 => Uint32, 8 => Unorm8x4
            ],
        };

        let premul = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let mod2x = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };
        let keep = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let mut pipelines = Vec::new();
        for normals in [false, true] {
            for cull in [true, false] {
                for (blend, depth_write) in [
                    (None, true),
                    (Some(wgpu::BlendState { color: premul, alpha: premul }), false),
                    (Some(wgpu::BlendState { color: add, alpha: add }), false),
                    (Some(wgpu::BlendState { color: mod2x, alpha: keep }), false),
                ] {
                    let targets = color_targets(blend, normals, depth_write);
                    pipelines.push(dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: None,
                        layout: Some(&layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs"),
                            compilation_options: Default::default(),
                            buffers: &[Some(pos_layout.clone()), Some(attr_layout.clone())],
                        },
                        primitive: wgpu::PrimitiveState {
                            topology: wgpu::PrimitiveTopology::TriangleList,
                            front_face: wgpu::FrontFace::Ccw,
                            cull_mode: if cull { Some(wgpu::Face::Back) } else { None },
                            ..Default::default()
                        },
                        depth_stencil: Some(wgpu::DepthStencilState {
                            format: DEPTH_FORMAT,
                            depth_write_enabled: Some(depth_write),
                            depth_compare: Some(wgpu::CompareFunction::Less),
                            stencil: Default::default(),
                            bias: Default::default(),
                        }),
                        multisample: Default::default(),
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some(if normals { "fs_n" } else { "fs" }),
                            compilation_options: Default::default(),
                            targets: &targets,
                        }),
                        multiview_mask: None,
                        cache: None,
                    }));
                }
            }
        }

        for normals in [false, true] {
            for back in [false, true] {
                let targets = color_targets(None, normals, true);
                pipelines.push(dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: None,
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[Some(pos_layout.clone()), Some(attr_layout.clone())],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: Some(if back { wgpu::Face::Front } else { wgpu::Face::Back }),
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::Less),
                        stencil: Default::default(),
                        bias: if back {
                            wgpu::DepthBiasState { constant: BACK_BIAS_CONST, slope_scale: BACK_BIAS_SLOPE, clamp: 0.0 }
                        } else {
                            Default::default()
                        },
                    }),
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(if normals { "fs_n" } else { "fs" }),
                        compilation_options: Default::default(),
                        targets: &targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                }));
            }
        }

        for normals in [false, true] {
            for back in [false, true] {
                let targets = color_targets(Some(wgpu::BlendState { color: premul, alpha: premul }), normals, false);
                pipelines.push(dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: None,
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[Some(pos_layout.clone()), Some(attr_layout.clone())],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: Some(if back { wgpu::Face::Front } else { wgpu::Face::Back }),
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(false),
                        depth_compare: Some(wgpu::CompareFunction::Less),
                        stencil: Default::default(),
                        bias: if back {
                            wgpu::DepthBiasState { constant: GLASS_BACK_BIAS_CONST, slope_scale: GLASS_BACK_BIAS_SLOPE, clamp: 0.0 }
                        } else {
                            Default::default()
                        },
                    }),
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(if normals { "fs_n" } else { "fs" }),
                        compilation_options: Default::default(),
                        targets: &targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                }));
            }
        }

        let shadow_layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&frame_layout), Some(&batch_layout)],
            immediate_size: 0,
        });
        let shadow_pipe = |pos_only: bool| {
            let full = [Some(pos_layout.clone()), Some(attr_layout.clone())];
            dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shadow"),
                layout: Some(&shadow_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if pos_only { "vs_pos" } else { "vs" }),
                    compilation_options: Default::default(),
                    buffers: if pos_only { &full[..1] } else { &full },
                },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState { constant: 0, slope_scale: SHADOW_SLOPE, clamp: 0.0 },
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if pos_only { "fs_shadow_pos" } else { "fs_shadow" }),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let shadow_pipe = [shadow_pipe(true), shadow_pipe(false)];
        tm.lap("pipelines");
        let mut st = Stager { gpu, pending: 0 };
        let bc = gpu.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let comp = |i: usize| mesh.compressed.get(i).and_then(|c| c.as_ref()).filter(|_| bc);
        let mut tex_views: Vec<Option<wgpu::TextureView>> = Vec::with_capacity(mesh.textures.len());
        for chunk in (0..mesh.textures.len()).collect::<Vec<_>>().chunks(TEX_CHUNK) {
            let plain: Vec<Option<(std::borrow::Cow<crate::mesh::Image>, Vec<(u32, u32, Vec<u8>)>)>> = chunk
                .par_iter()
                .map(|&i| {
                    if comp(i).is_some() {
                        return None;
                    }
                    let im = mesh.texture(i)?;
                    let mips = mip_chain(im.w, im.h, &im.rgba);
                    Some((im, mips))
                })
                .collect();
            tex_views.extend(plain.iter().zip(chunk).map(|(p, &i)| match (p, comp(i)) {
                (Some((im, mips)), _) => {
                    let levels: Vec<(u32, u32, &[u8])> =
                        std::iter::once((im.w, im.h, &im.rgba[..])).chain(mips.iter().map(|(w, h, d)| (*w, *h, &d[..]))).collect();
                    Some(st.texture(im.w, im.h, wgpu::TextureFormat::Rgba8Unorm, &levels, 4))
                }
                (None, Some(c)) => Some(st.compressed(c)),
                (None, None) => None,
            }));
        }
        tm.lap("textures");
        let a = &mesh.atlas;
        let lmap_view = match &mesh.hdr {
            Some(h) => st.texture(h.w, h.h, wgpu::TextureFormat::Rgb9e5Ufloat, &[(h.w, h.h, bytemuck::cast_slice(&h.irradiance))], 4),
            None => st.texture(a.w, a.h, wgpu::TextureFormat::Rgba8Unorm, &[(a.w, a.h, &a.rgba)], 4),
        };
        let dummy_view = st.texture(1, 1, wgpu::TextureFormat::R8Unorm, &[(1, 1, &[255])], 1);
        let shmask_view = match mesh.hdr.as_ref().and_then(|h| h.shadows.as_ref()) {
            Some(m) => st.texture(m.w, m.h, wgpu::TextureFormat::Rgba8Unorm, &[(m.w, m.h, &m.rgba)], 4),
            None => st.texture(1, 1, wgpu::TextureFormat::Rgba8Unorm, &[(1, 1, &[255; 4])], 4),
        };
        let mut lamps: Vec<LampU> = mesh
            .hdr
            .iter()
            .flat_map(|h| &h.lights)
            .map(|l| LampU {
                pos: [l.pos[0], l.pos[1], l.pos[2], l.range],
                color: [l.color[0], l.color[1], l.color[2], l.kind as f32],
                dir: [l.dir[0], l.dir[1], l.dir[2], l.channel as f32],
                cone: [l.cone[0], l.cone[1], 0.0, 0.0],
            })
            .collect();
        let hdr = mesh.hdr.as_ref().map(|h| HdrInfo { sun: h.sun, curve: h.curve, lamps: lamps.len() as u32 });
        if lamps.is_empty() {
            lamps.push(LampU::zeroed());
        }
        let lamp_buf = st.buffer(wgpu::BufferUsages::STORAGE, bytemuck::cast_slice(&lamps));
        tm.lap("lightmap");
        let mut batches = Vec::new();
        for b in &mesh.batches {
            batches.extend(make_batch(&mut st, &batch_layout, &tex_views, mesh, b, &b.verts, &b.idx));
        }
        let casters = (!mesh.caster_idx.is_empty()).then(|| {
            let vbuf = st.packed(wgpu::BufferUsages::VERTEX, &mesh.casters, |v| v.pos);
            let ibuf = st.buffer(wgpu::BufferUsages::INDEX, bytemuck::cast_slice(&mesh.caster_idx));
            let ub = st.buffer(wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&[0.0f32; 52]));
            (vbuf, ibuf, mesh.caster_idx.len() as u32, batch_bind(dev, &batch_layout, [&dummy_view; 5], &ub))
        });
        st.flush();
        tm.lap("geometry");
        GpuScene {
            gpu: gpu.clone(),
            batch_layout,
            frame_layout,
            light_layout,
            samplers: [sampler(gpu, true, false, true), sampler(gpu, true, true, true)],
            lmap_view,
            shmask_view,
            lamp_buf,
            hdr,
            casters,
            dummy_view,
            linear_clamp: sampler(gpu, false, false, false),
            cmp_sampler: dev.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
            pipelines,
            shadow_pipe,
            batches,
            points: mesh.points.clone(),
        }
    }

    pub fn animated(&self) -> bool {
        self.batches.iter().any(|b| b.frames.len() > 1)
    }
}

impl Renderer {
    pub fn new(gpu: &Gpu, mesh: &Mesh, nearest: bool) -> Result<Renderer> {
        Ok(Renderer::with_scene(GpuScene::new(gpu, mesh)?, nearest))
    }

    pub fn with_scene(sc: Arc<GpuScene>, nearest: bool) -> Renderer {
        let gpu = sc.gpu.clone();
        let dev = &gpu.device;
        let ubuf = |size: usize| {
            dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let align = dev.limits().min_uniform_buffer_offset_alignment as u64;
        let frame_stride = (std::mem::size_of::<FrameU>() as u64).next_multiple_of(align);
        let frame_buf = ubuf(frame_stride as usize * BAND_SLOTS);
        let shadow_frame_buf = ubuf(frame_stride as usize * BAND_SLOTS);
        let light_buf = ubuf(std::mem::size_of::<LightU>());
        let frame_bind = Self::make_frame_bind(&sc, &frame_buf, &sc.dummy_view, nearest);
        let shadow_frame_bind = Self::make_frame_bind(&sc, &shadow_frame_buf, &sc.dummy_view, nearest);
        let off_depth = texture(dev, 1, 1, DEPTH_FORMAT, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let light_off = Self::make_light_bind(dev, &sc.light_layout, &light_buf, &off_depth, &sc.cmp_sampler);
        Renderer {
            points: sc.points.clone(),
            mask: None,
            orig: None,
            pt_band: Vec::new(),
            explode: None,
            guides: None,
            frame_stride,
            band_slots: BAND_SLOTS,
            frame_buf,
            frame_bind,
            mask_view: None,
            sky: None,
            post: Post::new(dev),
            light_buf,
            light_off,
            shadow_frame_buf,
            shadow_frame_bind,
            shadow: Mutex::new(Vec::new()),
            pts_cache: Mutex::new(None),
            range_cache: Mutex::new(None),
            pick: std::sync::OnceLock::new(),
            sun_env: SunEnv::default(),
            tex_gamma: 0.0,
            nearest,
            sc,
            gpu,
        }
    }

    pub fn scene(&self) -> &Arc<GpuScene> {
        &self.sc
    }

    pub fn set_nearest(&mut self, nearest: bool) {
        if nearest != self.nearest {
            self.nearest = nearest;
            self.rebind();
        }
    }

    fn rebind(&mut self) {
        let mv = self.mask_view.as_ref().unwrap_or(&self.sc.dummy_view);
        self.frame_bind = Self::make_frame_bind(&self.sc, &self.frame_buf, mv, self.nearest);
        self.shadow_frame_bind = Self::make_frame_bind(&self.sc, &self.shadow_frame_buf, mv, self.nearest);
    }

    fn make_light_bind(
        dev: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        buf: &wgpu::Buffer,
        depth: &wgpu::TextureView,
        samp: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(depth) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(samp) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(depth) },
            ],
        })
    }

    fn make_frame_bind(sc: &GpuScene, buf: &wgpu::Buffer, mask: &wgpu::TextureView, nearest: bool) -> wgpu::BindGroup {
        sc.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &sc.frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<FrameU>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&sc.lmap_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sc.linear_clamp) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(mask) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&sc.linear_clamp) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&sc.shmask_view) },
                wgpu::BindGroupEntry { binding: 6, resource: sc.lamp_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&sc.samplers[nearest as usize]) },
            ],
        })
    }

    pub fn set_mask(&mut self, mask: Option<HullMask>) {
        self.mask_view = mask.as_ref().map(|m| {
            upload_texture(
                &self.gpu,
                m.nx as u32,
                m.ny as u32,
                1,
                wgpu::TextureFormat::R8Unorm,
                &[(m.nx as u32, m.ny as u32, &m.texture())],
                1,
            )
            .create_view(&Default::default())
        });
        self.mask = mask;
        self.rebind();
        self.shadow.lock().unwrap().clear();
        *self.pts_cache.lock().unwrap() = None;
    }

    pub fn animated(&self) -> bool {
        self.sc.animated()
    }

    pub fn set_sky(&mut self, faces: Option<(u32, Vec<u8>)>, fov: f64, pitch: f64) {
        let Some((size, data)) = faces else {
            self.sky = None;
            return;
        };
        let gpu = &self.gpu;
        let dev = &gpu.device;
        let n = (size * size * 4) as usize;
        let levels: Vec<(u32, u32, &[u8])> = (0..6).map(|i| (size, size, &data[i * n..(i + 1) * n])).collect();
        let tex = upload_texture(gpu, size, size, 6, wgpu::TextureFormat::Rgba8Unorm, &levels, 4);
        let view = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/sky.wgsl").into()),
        });
        let bl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                ubuf_entry(0, wgpu::ShaderStages::FRAGMENT),
                tex_entry(1, wgpu::TextureViewDimension::D2Array),
                samp_entry(2),
            ],
        });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bl)],
            immediate_size: 0,
        });
        let pipeline = |normals: bool| {
            dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if normals { "fs_n" } else { "fs" }),
                    compilation_options: Default::default(),
                    targets: &color_targets(None, normals, true),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipelines = [pipeline(false), pipeline(true)];
        let ubuf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<SkyU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sc.linear_clamp) },
            ],
        });
        self.sky = Some(SkyRes { pipelines, ubuf, bind, fov, pitch });
    }

    pub fn set_sky_angles(&mut self, fov: f64, pitch: f64) {
        if let Some(s) = &mut self.sky {
            (s.fov, s.pitch) = (fov, pitch);
        }
    }

    fn keeps<'a>(&'a self, cuts: &'a Cuts) -> impl Fn(&DVec3) -> bool + 'a {
        let [x0, y0, x1, y1] = cuts.clip;
        let mask = if cuts.use_mask { self.mask.as_ref() } else { None };
        move |p: &DVec3| {
            p.z >= cuts.zmin
                && p.z <= cuts.zmax
                && p.x >= x0
                && p.x <= x1
                && p.y >= y0
                && p.y <= y1
                && mask.is_none_or(|m| m.test(p.x, p.y))
        }
    }

    pub fn points_in(&self, cuts: &Cuts) -> Arc<[DVec3]> {
        let key = format!("{cuts:?}{:?}", self.explode);
        let mut cache = self.pts_cache.lock().unwrap();
        if let Some((k, v)) = cache.as_ref().filter(|(k, _)| *k == key) {
            let _ = k;
            return v.clone();
        }
        let keep = self.keeps(cuts);
        let base: &[DVec3] = self.orig.as_deref().unwrap_or(&self.points);
        let sel: Vec<DVec3> = base.iter().zip(self.points.iter()).filter(|(p, _)| keep(p)).map(|(_, q)| *q).collect();
        let v: Arc<[DVec3]> = if sel.is_empty() || sel.len() == self.points.len() { self.points.clone() } else { sel.into() };
        *cache = Some((key, v.clone()));
        v
    }

    pub fn lift(&self, z: f64) -> f64 {
        self.explode.as_ref().map_or(0.0, |e| e.planes.partition_point(|&p| p < z) as f64 * e.gap)
    }

    pub fn z_range(&self, cuts: &Cuts) -> (f64, f64) {
        let keep = self.keeps(cuts);
        let base: &[DVec3] = self.orig.as_deref().unwrap_or(&self.points);
        let span = |it: &mut dyn Iterator<Item = &DVec3>| {
            it.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.z), b.max(p.z)))
        };
        let r = span(&mut base.iter().filter(|p| keep(p)));
        if r.0 <= r.1 { r } else { span(&mut base.iter()) }
    }

    pub fn set_explode(&mut self, mesh: &Mesh, e: Option<&Explode>) {
        let e = e.filter(|x| !x.planes.is_empty());
        if self.explode.as_ref() == e {
            return;
        }
        if self.explode.as_ref().map(|x| &x.planes) != e.map(|x| &x.planes) {
            match e {
                None => {
                    self.orig = None;
                    self.pt_band = Vec::new();
                    self.points = self.sc.points.clone();
                }
                Some(x) => {
                    let (orig, band) = band_points(&self.sc.points, &mesh.batches, &x.planes, mesh.point_cell);
                    self.orig = Some(orig);
                    self.pt_band = band;
                    self.reserve_bands(x.planes.len() + 1);
                }
            }
        }
        self.guides = e.filter(|x| x.guides).map(|x| self.make_guides(x.planes.len()));
        self.explode = e.cloned();
        *self.pts_cache.lock().unwrap() = None;
        *self.range_cache.lock().unwrap() = None;
        if let (Some(orig), Some(x)) = (&self.orig, &self.explode) {
            self.points = orig.iter().zip(&self.pt_band).map(|(p, &b)| *p + DVec3::Z * (b as f64 * x.gap)).collect();
        }
    }

    fn reserve_bands(&mut self, n: usize) {
        if n <= self.band_slots {
            return;
        }
        let size = self.frame_stride * n as u64;
        let make = || {
            self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        (self.frame_buf, self.shadow_frame_buf) = (make(), make());
        self.band_slots = n;
        self.rebind();
    }

    fn write_bands(&self, buf: &wgpu::Buffer, fu: FrameU) -> Vec<Band> {
        let planes: &[f64] = self.explode.as_ref().map_or(&[], |e| &e.planes);
        let gap = self.explode.as_ref().map_or(0.0, |e| e.gap);
        (0..=planes.len())
            .map(|k| {
                let lo = if k == 0 { f64::NEG_INFINITY } else { planes[k - 1] + BAND_EPS };
                let hi = planes.get(k).map_or(f64::INFINITY, |p| p + BAND_EPS);
                let mut f = fu;
                f.zr[1] = f.zr[1].min(hi as f32);
                f.view_u[3] = if k == 0 { NO_BAND } else { lo as f32 };
                f.view_r[3] = (k as f64 * gap) as f32;
                let off = k as u64 * self.frame_stride;
                self.gpu.queue.write_buffer(buf, off, bytemuck::bytes_of(&f));
                Band { off: off as u32, lo: lo as f32, hi: hi as f32, lift: k as f64 * gap }
            })
            .collect()
    }

    fn make_guides(&self, planes: usize) -> Guides {
        let dev = &self.gpu.device;
        let vbuf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (planes * GUIDE_VERTS * (12 + std::mem::size_of::<Packed>())) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tex = upload_texture(&self.gpu, 1, 1, 1, wgpu::TextureFormat::Rgba8Unorm, &[(1, 1, &GUIDE_COLOR[..])], 4)
            .create_view(&Default::default());
        let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&{
                let mut u = [0.0f32; 52];
                u[..25].copy_from_slice(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
                u[28..30].copy_from_slice(&[1.0, 1.0]);
                u
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = batch_bind(dev, &self.sc.batch_layout, [&tex; 5], &ub);
        Guides { vbuf, bind, feet: Mutex::new(None) }
    }

    fn guide_verts(&self, g: &Guides, view: &View, width: f64, cuts: &Cuts) -> Vec<Vertex> {
        let (Some(e), Some(orig)) = (&self.explode, &self.orig) else { return Vec::new() };
        let key = format!("{cuts:?}");
        let mut cache = g.feet.lock().unwrap();
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            let keep = self.keeps(cuts);
            let mut feet: Vec<Option<[f64; 4]>> = vec![None; e.planes.len() + 1];
            for (p, &b) in orig.iter().zip(&self.pt_band) {
                if !keep(p) {
                    continue;
                }
                let f = feet[b as usize].get_or_insert([p.x, p.y, p.x, p.y]);
                *f = [f[0].min(p.x), f[1].min(p.y), f[2].max(p.x), f[3].max(p.y)];
            }
            *cache = Some((key, feet));
        }
        let feet = &cache.as_ref().unwrap().1;
        let b = &view.basis;
        let mut side = DVec3::new(b.f.y, -b.f.x, 0.0);
        if side.length() < 1e-6 {
            side = b.r;
        }
        let side = side.normalize() * (width / 2.0);
        let n = (-b.f).as_vec3().to_array();
        let v = |p: DVec3| Vertex { pos: p.as_vec3().to_array(), uv: [0.5, 0.5], normal: n, ..Vertex::BLANK };
        let mut out = Vec::new();
        for k in 1..feet.len() {
            let (Some(f), Some(_)) = (feet[k], feet[k - 1]) else { continue };
            let z0 = e.planes[k - 1] + (k - 1) as f64 * e.gap;
            let z1 = e.planes[k - 1] + k as f64 * e.gap;
            for (x, y) in [(f[0], f[1]), (f[2], f[1]), (f[2], f[3]), (f[0], f[3])] {
                let (lo, hi) = (DVec3::new(x, y, z0), DVec3::new(x, y, z1));
                let q = [v(lo - side), v(lo + side), v(hi + side), v(hi - side)];
                out.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
            }
        }
        out
    }

    pub fn iso_view(&self, yaw: f64, pitch: f64, upp: f64, pad: u32, cuts: &Cuts) -> (View, u32, u32) {
        let b = camera_basis(yaw, pitch);
        let e = extents(&self.points_in(cuts), &b);
        let wpx = ((e[0].1 - e[0].0) / upp).ceil() as u32 + 2 * pad;
        let hpx = ((e[1].1 - e[1].0) / upp).ceil() as u32 + 2 * pad;
        let view = View {
            basis: b,
            cx: (e[0].0 + e[0].1) / 2.0,
            cy: (e[1].0 + e[1].1) / 2.0,
            w: wpx as f64 * upp,
            h: hpx as f64 * upp,
            sky_yaw: Some(yaw),
            persp: None,
        };
        (view, wpx, hpx)
    }

    fn cached_range(&self, f: DVec3, eye: Option<DVec3>) -> (f64, f64) {
        let e = eye.unwrap_or(DVec3::NAN);
        let key = [f.x, f.y, f.z, e.x, e.y, e.z];
        let same = |k: &[f64; 6]| k.iter().zip(&key).all(|(a, b)| a.to_bits() == b.to_bits());
        if let Some((_, r)) = self.range_cache.lock().unwrap().as_ref().filter(|(k, _)| same(k)) {
            return *r;
        }
        let r = match eye {
            None => span(&self.points, |p| p.dot(f)),
            Some(eye) => span(&self.points, |p| (*p - eye).dot(f)),
        };
        *self.range_cache.lock().unwrap() = Some((key, r));
        r
    }

    fn depth_range(&self, b: &Basis) -> (f64, f64) {
        self.cached_range(b.f, None)
    }

    fn persp_range(&self, b: &Basis, eye: DVec3) -> (f64, f64) {
        let (z0, z1) = self.cached_range(b.f, Some(eye));
        let near = (z0 - PERSP_MARGIN).max(PERSP_NEAR);
        let far = (z1 + PERSP_MARGIN).max(near + PERSP_MARGIN);
        (near, far)
    }

    fn projection(&self, view: &View, w: u32, h: u32) -> (DMat4, [f64; 4]) {
        let b = &view.basis;
        match view.persp {
            None => {
                let (d0, d1) = self.depth_range(b);
                (ortho(b, view.cx, view.cy, view.w, view.h, d0, d1), [(d0 + d1) / 2.0, d1 - d0 + 64.0, 0.0, 0.0])
            }
            Some(p) => {
                let (near, far) = self.persp_range(b, p.eye);
                let k = 2.0 * (p.fov_y.to_radians() / 2.0).tan() / h as f64;
                (persp(b, p.eye, p.fov_y, w as f64 / h as f64, near, far), [near, far, k, 1.0])
            }
        }
    }

    pub fn unproject(&self, view: &View, w: u32, h: u32, x: f64, y: f64, d: f64) -> Option<(DVec3, f64)> {
        if !(0.0..1.0).contains(&d) {
            return None;
        }
        let b = &view.basis;
        let nx = (x + 0.5) / w as f64 * 2.0 - 1.0;
        let ny = 1.0 - (y + 0.5) / h as f64 * 2.0;
        match view.persp {
            Some(p) => {
                let (near, far) = self.persp_range(b, p.eye);
                let z = near * far / (far - d * (far - near));
                let ty = (p.fov_y.to_radians() / 2.0).tan();
                let ray = b.f + b.r * (nx * ty * w as f64 / h as f64) + b.u * (ny * ty);
                Some((p.eye + ray * z, z))
            }
            None => {
                let (d0, d1) = self.depth_range(b);
                let pf = (d - 0.5) * (d1 - d0 + 64.0) + (d0 + d1) / 2.0;
                let pt = b.r * (view.cx + nx * view.w / 2.0) + b.u * (view.cy + ny * view.h / 2.0) + b.f * pf;
                Some((pt, pf))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_world(
        &self,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        normal: Option<&wgpu::TextureView>,
        depth: &wgpu::TextureView,
        view: &View,
        m: &DMat4,
        aspect: f64,
        sky_crop: [f64; 4],
        cuts: &Cuts,
        cull: bool,
        clear: [f64; 4],
        anim: bool,
        time: f64,
        guide_w: f64,
        light: &wgpu::BindGroup,
        tint: [f32; 4],
    ) {
        let b = &view.basis;
        let mask_on = cuts.use_mask && self.mask.is_some();
        let rect = self.mask.as_ref().map(|m| m.rect).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let v4 = |v: DVec3, w: f32| [v.x as f32, v.y as f32, v.z as f32, w];
        let fu = FrameU {
            mvp: m.as_mat4().to_cols_array(),
            clip_xy: cuts.clip.map(|v| v as f32),
            mask_rect: rect.map(|v| v as f32),
            zr: [cuts.zmin as f32, cuts.zmax as f32, if mask_on { 1.0 } else { 0.0 }, if anim { time as f32 } else { 0.0 }],
            view_dir: v4(b.f, if anim { 1.0 } else { 0.0 }),
            view_r: v4(b.r, 0.0),
            view_u: v4(b.u, 0.0),
            eye: match view.persp {
                Some(p) => v4(p.eye, 1.0),
                None => [0.0; 4],
            },
        };
        let q = &self.gpu.queue;
        let bands = self.write_bands(&self.frame_buf, fu);
        let cl = Cull::new(m, cuts);
        let guide = self.guides.as_ref().and_then(|g| {
            let gv = self.guide_verts(g, view, guide_w, cuts);
            if gv.is_empty() {
                return None;
            }
            let pos: Vec<[f32; 3]> = gv.iter().map(|v| v.pos).collect();
            let attr: Vec<Packed> = gv.iter().map(Packed::new).collect();
            q.write_buffer(&g.vbuf, 0, bytemuck::cast_slice(&pos));
            q.write_buffer(&g.vbuf, (gv.len() * 12) as u64, bytemuck::cast_slice(&attr));
            Some((g, gv.len() as u32))
        });
        if let (Some(sky), Some(yaw)) = (&self.sky, view.sky_yaw) {
            let (sb, tanfov) = match view.persp {
                Some(p) => {
                    let ty = (p.fov_y.to_radians() / 2.0).tan();
                    (*b, [(ty / aspect) as f32, ty as f32, 0.0, 0.0])
                }
                None => {
                    let tx = (sky.fov.to_radians() / 2.0).tan();
                    (camera_basis(yaw, -sky.pitch), [tx as f32, (tx * aspect) as f32, 0.0, 0.0])
                }
            };
            let [sx, sy, ox, oy] = sky_crop;
            let (tx, ty) = (tanfov[0] as f64, tanfov[1] as f64);
            let f = sb.f + sb.r * (tx * ox) + sb.u * (ty * oy);
            let tanfov = [(tx * sx) as f32, (ty * sy) as f32, 0.0, 0.0];
            let su = SkyU { r: v4(sb.r, 0.0), u: v4(sb.u, 0.0), f: v4(f, 0.0), tanfov, tint };
            q.write_buffer(&sky.ubuf, 0, bytemuck::bytes_of(&su));
        }

        let mut attachments = vec![Some(wgpu::RenderPassColorAttachment {
            view: color,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0], g: clear[1], b: clear[2], a: clear[3] }),
                store: wgpu::StoreOp::Store,
            },
        })];
        if let Some(n) = normal {
            attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: n,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            }));
        }
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let ni = normal.is_some() as usize;
        if let (Some(sky), Some(_)) = (&self.sky, view.sky_yaw) {
            pass.set_pipeline(&sky.pipelines[ni]);
            pass.set_bind_group(0, &sky.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        pass.set_bind_group(0, &self.frame_bind, &[0]);
        pass.set_bind_group(2, light, &[]);
        let base = ni * 8 + if cull { 0 } else { 4 };
        let tick = (time * ANIM_FPS).floor().max(0.0) as usize;
        for (pi, modes) in
            [(0, &[Mode::Opaque, Mode::AlphaTest][..]), (3, &[Mode::Mod2x][..]), (1, &[Mode::Blend][..]), (2, &[Mode::Additive][..])]
        {
            let sets: &[usize] = match (pi, cull) {
                (0, false) => &[16 + ni * 2, 17 + ni * 2],
                (1, false) => &[20 + ni * 2, 21 + ni * 2],
                _ => &[base + pi],
            };
            for &pipe in sets {
                pass.set_pipeline(&self.sc.pipelines[pipe]);
                for b in self.sc.batches.iter().filter(|b| modes.contains(&b.mode)) {
                    let bind = if anim && !b.frames.is_empty() { &b.frames[tick % b.frames.len()] } else { &b.bind };
                    pass.set_bind_group(1, bind, &[]);
                    pass.set_vertex_buffer(0, b.pbuf.slice(..));
                    pass.set_vertex_buffer(1, b.abuf.slice(..));
                    pass.set_index_buffer(b.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for w in bands.iter().filter(|w| w.meets(b.zr)) {
                        let rs = cl.ranges(b, w);
                        if !rs.is_empty() {
                            pass.set_bind_group(0, &self.frame_bind, &[w.off]);
                        }
                        for r in rs {
                            pass.draw_indexed(r, 0, 0..1);
                        }
                    }
                }
            }
            if let (0, Some((g, n))) = (pi, &guide) {
                pass.set_bind_group(0, &self.frame_bind, &[0]);
                pass.set_pipeline(&self.sc.pipelines[ni * 8 + 4]);
                pass.set_bind_group(1, &g.bind, &[]);
                pass.set_vertex_buffer(0, g.vbuf.slice(..(*n as u64 * 12)));
                pass.set_vertex_buffer(1, g.vbuf.slice((*n as u64 * 12)..));
                pass.draw(0..*n, 0..1);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        enc: &mut wgpu::CommandEncoder,
        t: &Targets,
        view: &View,
        cuts: &Cuts,
        look: &Look,
        ss: u32,
        time: f64,
        clear: [f64; 4],
    ) {
        self.draw_crop(enc, t, view, None, cuts, look, ss, time, clear);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_crop(
        &self,
        enc: &mut wgpu::CommandEncoder,
        t: &Targets,
        view: &View,
        crop: Option<Crop>,
        cuts: &Cuts,
        look: &Look,
        ss: u32,
        time: f64,
        clear: [f64; 4],
    ) {
        let c = crop.unwrap_or(Crop { x: 0, y: 0, w: t.w, h: t.h });
        let aspect = c.h as f64 / c.w as f64;
        let anim = look.anim_textures;
        let (m, depth) = self.projection(view, c.w, c.h);
        let (m, sky_crop) = c.apply(m, t.w, t.h);
        let guide_w = GUIDE_PX * ss.max(1) as f64 * view.w / c.w.max(1) as f64;
        let (light, tint) = self.prepare_light(look, cuts);
        let Some(pt) = t.post.as_ref().filter(|_| crate::post::needed(look)) else {
            self.encode_world(enc, &t.color_view, None, &t.depth_view, view, &m, aspect, sky_crop, cuts, look.cull, clear, anim, time, guide_w, &light, tint);
            return;
        };
        let scene = pt.scene();
        self.encode_world(enc, scene, Some(&pt.normal), &t.depth_view, view, &m, aspect, sky_crop, cuts, look.cull, clear, anim, time, guide_w, &light, tint);
        let focus = view.persp.map(|p| p.focus);
        let tile = [c.x as f64, c.y as f64, c.w as f64, c.h as f64];
        let cam = Cam { w: t.w, h: t.h, upp: view.w / c.w as f64, ss, depth, focus, tile };
        self.post.run(&self.gpu, enc, look, &cam, pt, &t.depth_view, &t.color_view);
    }

    pub fn make_targets(&self, w: u32, h: u32, post: bool) -> Result<Targets> {
        self.gpu.guard(|| format!("{w}x{h} render targets"), || self.targets(w, h, post))
    }

    fn targets(&self, w: u32, h: u32, post: bool) -> Targets {
        let dev = &self.gpu.device;
        let color = texture(
            dev,
            w,
            h,
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            dev,
            w,
            h,
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let color_view = color.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        Targets { w, h, color, color_view, depth, depth_view, post: post.then(|| PostTargets::new(dev, w, h)) }
    }

}

