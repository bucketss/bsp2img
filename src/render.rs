use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use glam::{DMat4, DVec3};
use wgpu::util::DeviceExt;

use std::sync::{Arc, Mutex};

use crate::camera::{Basis, camera_basis, extents, ortho, persp};
use crate::clip::clip_tris_z;
use crate::explode::Explode;
use crate::look::Look;
use crate::mesh::{Batch, Mesh, Mode, Vertex};
use crate::post::{Cam, NORMAL_FORMAT, Post, PostTargets};
use crate::reach::HullMask;
use crate::sun::{Sun, SunEnv, baked_sun, sun_at};

pub use egui_wgpu::wgpu;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const NO_CLIP: [f64; 4] = [-1e9, -1e9, 1e9, 1e9];
pub const ANIM_FPS: f64 = 10.0;
const PERSP_NEAR: f64 = 4.0;
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

#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub max_dim: u32,
}

impl Gpu {
    pub fn headless() -> Result<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context("no GPU adapter found")?;
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: limits.max_texture_dimension_2d,
                max_buffer_size: limits.max_buffer_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }))?;
        catch_errors(&device);
        Ok(Gpu { device, queue, max_dim: limits.max_texture_dimension_2d })
    }

    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Gpu {
        catch_errors(&device);
        let max_dim = device.limits().max_texture_dimension_2d;
        Gpu { device, queue, max_dim }
    }

    pub fn guard<T>(&self, what: impl FnOnce() -> String, f: impl FnOnce() -> T) -> Result<T> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let v = f();
        match pollster::block_on(scope.pop()) {
            Some(e) => bail!("GPU out of memory ({}); try smaller --size/--ss, --tex-max, --crop or no --explode [{e}]", what()),
            None => Ok(v),
        }
    }
}

fn catch_errors(dev: &wgpu::Device) {
    dev.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| crate::prof::gpu_error(e.to_string())));
}

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

struct GpuBatch {
    mode: Mode,
    vbuf: wgpu::Buffer,
    count: u32,
    bind: wgpu::BindGroup,
    frames: Vec<wgpu::BindGroup>,
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
    tex_views: Vec<Option<wgpu::TextureView>>,
    batch_layout: wgpu::BindGroupLayout,
    frame_layout: wgpu::BindGroupLayout,
    light_layout: wgpu::BindGroupLayout,
    samplers: [wgpu::Sampler; 2],
    lmap_view: wgpu::TextureView,
    shmask_view: wgpu::TextureView,
    lamp_buf: wgpu::Buffer,
    hdr: Option<HdrInfo>,
    casters: Option<(wgpu::Buffer, u32, wgpu::BindGroup)>,
    dummy_view: wgpu::TextureView,
    linear_clamp: wgpu::Sampler,
    cmp_sampler: wgpu::Sampler,
    pipelines: Vec<wgpu::RenderPipeline>,
    shadow_pipe: wgpu::RenderPipeline,
    batches: Vec<GpuBatch>,
    points: Vec<DVec3>,
}

pub struct Renderer {
    pub gpu: Gpu,
    sc: Arc<GpuScene>,
    nearest: bool,
    pub points: Vec<DVec3>,
    pub mask: Option<HullMask>,
    orig: Option<Vec<DVec3>>,
    pt_band: Vec<u16>,
    explode: Option<Explode>,
    exploded: Option<Vec<GpuBatch>>,
    guides: Option<Guides>,
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
    pts_cache: Mutex<Option<(String, Arc<Vec<DVec3>>)>>,
    pub sun_env: SunEnv,
}

fn mip_chain(w: u32, h: u32, rgba: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    loop {
        let (pw, ph, prev) = out.last().map_or((w, h, rgba), |(a, b, c)| (*a, *b, &c[..]));
        if pw == 1 && ph == 1 {
            break;
        }
        let (nw, nh) = ((pw / 2).max(1), (ph / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(pw - 1);
                    let sy = (y * 2 + dy).min(ph - 1);
                    let i = ((sy * pw + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += prev[i + c] as u32;
                    }
                }
                let o = ((y * nw + x) * 4) as usize;
                for c in 0..4 {
                    next[o + c] = ((acc[c] + 2) / 4) as u8;
                }
            }
        }
        out.push((nw, nh, next));
    }
    out
}

fn upload_texture(
    gpu: &Gpu,
    w: u32,
    h: u32,
    layers: u32,
    format: wgpu::TextureFormat,
    levels: &[(u32, u32, &[u8])],
    bpp: u32,
) -> wgpu::Texture {
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: layers },
        mip_level_count: levels.len() as u32 / layers.max(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let per_layer = levels.len() / layers.max(1) as usize;
    for (i, (lw, lh, data)) in levels.iter().enumerate() {
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: (i % per_layer) as u32,
                origin: wgpu::Origin3d { x: 0, y: 0, z: (i / per_layer) as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * bpp), rows_per_image: Some(*lh) },
            wgpu::Extent3d { width: *lw, height: *lh, depth_or_array_layers: 1 },
        );
    }
    tex
}

fn sampler(gpu: &Gpu, repeat: bool, nearest: bool, aniso: bool) -> wgpu::Sampler {
    let am = if repeat { wgpu::AddressMode::Repeat } else { wgpu::AddressMode::ClampToEdge };
    let f = if nearest { wgpu::FilterMode::Nearest } else { wgpu::FilterMode::Linear };
    gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: am,
        address_mode_v: am,
        address_mode_w: am,
        mag_filter: f,
        min_filter: f,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        anisotropy_clamp: if aniso && !nearest { 16 } else { 1 },
        ..Default::default()
    })
}

fn tex_entry(binding: u32, dim: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

pub fn ubuf_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn color_targets(blend: Option<wgpu::BlendState>, normals: bool, normal_write: bool) -> Vec<Option<wgpu::ColorTargetState>> {
    let mut t = vec![Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })];
    if normals {
        t.push(Some(wgpu::ColorTargetState {
            format: NORMAL_FORMAT,
            blend: None,
            write_mask: if normal_write { wgpu::ColorWrites::ALL } else { wgpu::ColorWrites::empty() },
        }));
    }
    t
}

fn texture(dev: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
    dev.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

pub fn attachment_view(dev: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::TextureView {
    texture(dev, w, h, format, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
        .create_view(&Default::default())
}

#[allow(clippy::too_many_arguments)]
fn make_batch(
    st: &mut Stager,
    layout: &wgpu::BindGroupLayout,
    tex_views: &[Option<wgpu::TextureView>],
    mesh: &Mesh,
    b: &Batch,
    verts: &[Vertex],
    band: usize,
) -> Option<GpuBatch> {
    let Some(Some(view)) = tex_views.get(b.tex) else { return None };
    if verts.is_empty() {
        return None;
    }
    let vbuf = st.buffer(wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(verts));
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
    let u = [
        b.mode as u32 as f32, b.alpha, water, 0.0, warp[0], warp[1], warp[2], warp[3], band as f32, 0.0, has2, 0.0, t[0], t[1], t[2], 1.0,
        uv2[0], uv2[1], uv2[2], uv2[3], uv3[0], uv3[1], uv3[2], uv3[3], on3, second3, if tmask.is_some() { 1.0 } else { 0.0 }, 0.0,
    ];
    let ub = st.buffer(wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&u));
    let dev = &st.gpu.device;
    let make = |v: &wgpu::TextureView| batch_bind(dev, layout, [v, l2.unwrap_or(v), l3.unwrap_or(v), tmask.unwrap_or(v)], &ub);
    let frames = match mesh.anim.get(b.tex) {
        Some(Some(seq)) => seq.iter().filter_map(|&i| tex_views.get(i).and_then(|v| v.as_ref())).map(make).collect(),
        _ => Vec::new(),
    };
    Some(GpuBatch { mode: b.mode, vbuf, count: verts.len() as u32, bind: make(view), frames })
}

fn batch_bind(dev: &wgpu::Device, layout: &wgpu::BindGroupLayout, v: [&wgpu::TextureView; 4], ub: &wgpu::Buffer) -> wgpu::BindGroup {
    dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(v[0]) },
            wgpu::BindGroupEntry { binding: 2, resource: ub.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(v[1]) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(v[2]) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(v[3]) },
        ],
    })
}

fn vpos(v: &Vertex) -> DVec3 {
    DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)
}

const UPLOAD_CHUNK: usize = 64 << 20;
const UPLOAD_FLUSH: u64 = 256 << 20;

struct Stager<'a> {
    gpu: &'a Gpu,
    pending: u64,
}

impl Stager<'_> {
    fn buffer(&mut self, usage: wgpu::BufferUsages, data: &[u8]) -> wgpu::Buffer {
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (data.len() as u64).max(4).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for (i, c) in data.chunks(UPLOAD_CHUNK).enumerate() {
            self.write(&buf, (i * UPLOAD_CHUNK) as u64, c);
        }
        buf
    }

    fn write(&mut self, buf: &wgpu::Buffer, offset: u64, data: &[u8]) {
        let n = data.len() / 4 * 4;
        if n > 0 {
            self.gpu.queue.write_buffer(buf, offset, &data[..n]);
        }
        if n < data.len() {
            let mut tail = [0u8; 4];
            tail[..data.len() - n].copy_from_slice(&data[n..]);
            self.gpu.queue.write_buffer(buf, offset + n as u64, &tail);
        }
        self.add(data.len() as u64);
    }

    fn texture(&mut self, w: u32, h: u32, format: wgpu::TextureFormat, levels: &[(u32, u32, &[u8])], bpp: u32) -> wgpu::TextureView {
        let t = upload_texture(self.gpu, w, h, 1, format, levels, bpp);
        self.add(levels.iter().map(|l| l.2.len() as u64).sum());
        t.create_view(&Default::default())
    }

    fn add(&mut self, n: u64) {
        self.pending += n;
        if self.pending >= UPLOAD_FLUSH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        self.gpu.queue.submit([]);
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        self.pending = 0;
    }
}

impl GpuScene {
    pub fn new(gpu: &Gpu, mesh: &Mesh) -> Result<Arc<GpuScene>> {
        let bytes: usize = mesh.batches.iter().map(|b| b.verts.len()).sum::<usize>() * std::mem::size_of::<Vertex>();
        gpu.guard(|| format!("{} MB of geometry", bytes >> 20), || Arc::new(Self::upload(gpu, mesh)))
    }

    fn upload(gpu: &Gpu, mesh: &Mesh) -> GpuScene {
        let mut tm = crate::prof::Timer::new("upload");
        let dev = &gpu.device;
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/world.wgsl").into()),
        });
        let frame_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                ubuf_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
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
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x3, 1 => Float32x2, 2 => Float32x2, 3 => Float32, 4 => Float32x3,
                5 => Float32x2, 6 => Unorm16x2, 7 => Unorm8x4, 8 => Uint32, 9 => Unorm8x4
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
                            buffers: &[Some(vbuf_layout.clone())],
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

        let shadow_layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&frame_layout), Some(&batch_layout)],
            immediate_size: 0,
        });
        let shadow_pipe = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(vbuf_layout.clone())],
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
                entry_point: Some("fs_shadow"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        tm.lap("pipelines");
        let mut st = Stager { gpu, pending: 0 };
        let tex_views: Vec<Option<wgpu::TextureView>> = mesh
            .textures
            .iter()
            .map(|t| {
                t.as_ref().map(|im| {
                    let mips = mip_chain(im.w, im.h, &im.rgba);
                    let levels: Vec<(u32, u32, &[u8])> =
                        std::iter::once((im.w, im.h, &im.rgba[..])).chain(mips.iter().map(|(w, h, d)| (*w, *h, &d[..]))).collect();
                    st.texture(im.w, im.h, wgpu::TextureFormat::Rgba8Unorm, &levels, 4)
                })
            })
            .collect();
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
            batches.extend(make_batch(&mut st, &batch_layout, &tex_views, mesh, b, &b.verts, 0));
        }
        let casters = (!mesh.casters.is_empty()).then(|| {
            let vbuf = st.buffer(wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&mesh.casters));
            let ub = st.buffer(wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&[0.0f32; 28]));
            (vbuf, mesh.casters.len() as u32, batch_bind(dev, &batch_layout, [&dummy_view; 4], &ub))
        });
        st.flush();
        tm.lap("geometry");
        GpuScene {
            gpu: gpu.clone(),
            tex_views,
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
        let frame_buf = ubuf(std::mem::size_of::<FrameU>());
        let shadow_frame_buf = ubuf(std::mem::size_of::<FrameU>());
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
            exploded: None,
            guides: None,
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
            sun_env: SunEnv::default(),
            nearest,
            sc,
            gpu,
        }
    }

    pub fn scene(&self) -> &Arc<GpuScene> {
        &self.sc
    }

    fn batches(&self) -> &[GpuBatch] {
        self.exploded.as_deref().unwrap_or(&self.sc.batches)
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
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/sky.wgsl").into()),
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

    pub fn points_in(&self, cuts: &Cuts) -> Arc<Vec<DVec3>> {
        let key = format!("{cuts:?}{:?}", self.explode);
        let mut cache = self.pts_cache.lock().unwrap();
        if let Some((k, v)) = cache.as_ref().filter(|(k, _)| *k == key) {
            let _ = k;
            return v.clone();
        }
        let keep = self.keeps(cuts);
        let base = self.orig.as_ref().unwrap_or(&self.points);
        let sel: Vec<DVec3> = base.iter().zip(&self.points).filter(|(p, _)| keep(p)).map(|(_, q)| *q).collect();
        let v = Arc::new(if sel.is_empty() { self.points.clone() } else { sel });
        *cache = Some((key, v.clone()));
        v
    }

    pub fn lift(&self, z: f64) -> f64 {
        self.explode.as_ref().map_or(0.0, |e| e.planes.partition_point(|&p| p < z) as f64 * e.gap)
    }

    pub fn z_range(&self, cuts: &Cuts) -> (f64, f64) {
        let keep = self.keeps(cuts);
        let base = self.orig.as_ref().unwrap_or(&self.points);
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
            self.exploded = None;
            match e {
                None => {
                    self.orig = None;
                    self.pt_band = Vec::new();
                    self.points = self.sc.points.clone();
                }
                Some(x) => {
                    let mut batches = Vec::new();
                    let mut st = Stager { gpu: &self.gpu, pending: 0 };
                    let (lay, tv) = (&self.sc.batch_layout, &self.sc.tex_views);
                    let (mut orig, mut band) = (Vec::new(), Vec::new());
                    for b in &mesh.batches {
                        for (k, verts) in clip_tris_z(&b.verts, &x.planes) {
                            orig.extend(verts.iter().map(vpos));
                            band.extend(std::iter::repeat_n(k as u16, verts.len()));
                            batches.extend(make_batch(&mut st, lay, tv, mesh, b, &verts, k));
                        }
                    }
                    st.flush();
                    self.orig = Some(orig);
                    self.pt_band = band;
                    self.exploded = Some(batches);
                }
            }
            *self.pts_cache.lock().unwrap() = None;
        }
        self.guides = e.filter(|x| x.guides).map(|x| self.make_guides(x.planes.len()));
        self.explode = e.cloned();
        *self.pts_cache.lock().unwrap() = None;
        if let (Some(orig), Some(x)) = (&self.orig, &self.explode) {
            self.points = orig.iter().zip(&self.pt_band).map(|(p, &b)| *p + DVec3::Z * (b as f64 * x.gap)).collect();
        }
    }

    fn make_guides(&self, planes: usize) -> Guides {
        let dev = &self.gpu.device;
        let vbuf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (planes * GUIDE_VERTS * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tex = upload_texture(&self.gpu, 1, 1, 1, wgpu::TextureFormat::Rgba8Unorm, &[(1, 1, &GUIDE_COLOR[..])], 4)
            .create_view(&Default::default());
        let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&[
                0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0,
            ]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = batch_bind(dev, &self.sc.batch_layout, [&tex; 4], &ub);
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

    fn depth_range(&self, b: &Basis) -> (f64, f64) {
        self.points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, c), p| {
            let d = p.dot(b.f);
            (a.min(d), c.max(d))
        })
    }

    fn persp_range(&self, b: &Basis, eye: DVec3) -> (f64, f64) {
        let (z0, z1) = self.points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, c), p| {
            let d = (*p - eye).dot(b.f);
            (a.min(d), c.max(d))
        });
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

    pub fn read_depth(&self, t: &Targets, x: u32, y: u32) -> Option<f64> {
        if x >= t.w || y >= t.h {
            return None;
        }
        let dev = &self.gpu.device;
        let row = (t.w * 4).div_ceil(256) * 256;
        let buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * t.h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = dev.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.depth,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(t.h) },
            },
            wgpu::Extent3d { width: t.w, height: t.h, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        dev.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let d = {
            let data = slice.get_mapped_range().ok()?;
            let i = (y * row + x * 4) as usize;
            f32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as f64
        };
        buf.unmap();
        Some(d)
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
            view_r: v4(b.r, self.explode.as_ref().map_or(0.0, |e| e.gap as f32)),
            view_u: v4(b.u, 0.0),
            eye: match view.persp {
                Some(p) => v4(p.eye, 1.0),
                None => [0.0; 4],
            },
        };
        let q = &self.gpu.queue;
        q.write_buffer(&self.frame_buf, 0, bytemuck::bytes_of(&fu));
        let guide = self.guides.as_ref().and_then(|g| {
            let gv = self.guide_verts(g, view, guide_w, cuts);
            if gv.is_empty() {
                return None;
            }
            q.write_buffer(&g.vbuf, 0, bytemuck::cast_slice(&gv));
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
        pass.set_bind_group(0, &self.frame_bind, &[]);
        pass.set_bind_group(2, light, &[]);
        let base = ni * 8 + if cull { 0 } else { 4 };
        let tick = (time * ANIM_FPS).floor().max(0.0) as usize;
        for (pi, modes) in
            [(0, &[Mode::Opaque, Mode::AlphaTest][..]), (3, &[Mode::Mod2x][..]), (1, &[Mode::Blend][..]), (2, &[Mode::Additive][..])]
        {
            pass.set_pipeline(&self.sc.pipelines[base + pi]);
            for b in self.batches().iter().filter(|b| modes.contains(&b.mode)) {
                let bind = if anim && !b.frames.is_empty() { &b.frames[tick % b.frames.len()] } else { &b.bind };
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, b.vbuf.slice(..));
                pass.draw(0..b.count, 0..1);
            }
            if let (0, Some((g, n))) = (pi, &guide) {
                pass.set_pipeline(&self.sc.pipelines[ni * 8 + 4]);
                pass.set_bind_group(1, &g.bind, &[]);
                pass.set_vertex_buffer(0, g.vbuf.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
    }

    pub fn sun(&self, look: &Look) -> Option<Sun> {
        look.relit().then(|| sun_at(&self.sun_env, look))
    }

    fn prepare_light(&self, look: &Look, cuts: &Cuts) -> (wgpu::BindGroup, [f32; 4]) {
        let q = &self.gpu.queue;
        let sun = self.sun(look);
        if sun.is_none() && self.sc.hdr.is_none() {
            q.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&LightU::zeroed()));
            return (self.light_off.clone(), [1.0; 4]);
        }
        let res = if look.shadow_res == 0 { SHADOW_RES } else { look.shadow_res };
        let base = baked_sun(&self.sun_env);
        let a = sun.map(|s| self.shadow_map(s.dir, cuts, res));
        let b = match &a {
            Some(a) if !look.keep_lights && self.sc.hdr.is_none() => a.clone(),
            _ => self.shadow_map(base.dir, cuts, res),
        };
        let a = a.unwrap_or_else(|| b.clone());
        let f = |v: [f64; 3], w: f32| [v[0] as f32, v[1] as f32, v[2] as f32, w];
        let v = |d: DVec3, w: f32| [d.x as f32, d.y as f32, d.z as f32, w];
        let sh = |s: &ShadowMap| [1.0 / s.res as f32, (SHADOW_NORMAL * s.texel) as f32, (SHADOW_DEPTH * s.texel / s.depth) as f32, 0.0];
        let (hdr, hsun) = match &self.sc.hdr {
            Some(h) => ([h.curve[0], h.curve[1], h.curve[2], 1.0], [h.sun[0], h.sun[1], h.sun[2], h.lamps as f32]),
            None => ([0.0; 4], [0.0; 4]),
        };
        let lu = LightU {
            mvp: a.mvp.as_mat4().to_cols_array(),
            dir: sun.map_or([0.0; 4], |s| v(s.dir, look.relight.clamp(0.0, 1.0) as f32)),
            color: sun.map_or([0.0; 4], |s| f(s.color, if look.keep_lights { 1.0 } else { 0.0 })),
            ambient: sun.map_or([0.0; 4], |s| f(s.ambient, KEEP_MARGIN)),
            shadow: sh(&a),
            mvp0: b.mvp.as_mat4().to_cols_array(),
            dir0: v(base.dir, 0.0),
            color0: f(base.color, 0.0),
            ambient0: f(base.ambient, 0.0),
            shadow0: sh(&b),
            hdr,
            sun: hsun,
        };
        q.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&lu));
        let bind = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.sc.light_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.light_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&a.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sc.cmp_sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&b.view) },
            ],
        });
        let tint = match sun {
            Some(s) => {
                let r = look.relight.clamp(0.0, 1.0);
                f(s.sky_tint.map(|c| 1.0 + (c - 1.0) * r), 1.0)
            }
            None => [1.0; 4],
        };
        (bind, tint)
    }

    fn shadow_map(&self, dir: DVec3, cuts: &Cuts, res: u32) -> ShadowMap {
        let res = res.clamp(256, self.gpu.max_dim);
        let key = format!("{:?}{res}{cuts:?}{}{:?}{}", dir.to_array(), self.points.len(), self.explode, self.mask.is_some());
        let mut cache = self.shadow.lock().unwrap();
        if let Some(i) = cache.iter().position(|s| s.key == key) {
            let s = cache.remove(i);
            cache.push(s.clone());
            return s;
        }
        let dev = &self.gpu.device;
        let f = -dir.normalize();
        let r = f.cross(DVec3::Z);
        let r = if r.length() < 1e-6 { DVec3::X } else { r.normalize() };
        let b = Basis { r, u: r.cross(f), f };
        let e = extents(&self.points_in(cuts), &b);
        let side = (e[0].1 - e[0].0).max(e[1].1 - e[1].0) + 2.0 * SHADOW_PAD;
        let (d0, d1) = (e[2].0 - SHADOW_PAD, e[2].1 + SHADOW_PAD);
        let m = ortho(&b, (e[0].0 + e[0].1) / 2.0, (e[1].0 + e[1].1) / 2.0, side, side, d0, d1);
        let reuse = (cache.len() >= SHADOW_CACHE).then(|| cache.remove(0)).filter(|s| s.res == res);
        let view = match reuse {
            Some(s) => s.view,
            None => texture(dev, res, res, DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
                .create_view(&Default::default()),
        };
        let mask_on = cuts.use_mask && self.mask.is_some();
        let rect = self.mask.as_ref().map(|m| m.rect).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let v4 = |v: DVec3, w: f32| [v.x as f32, v.y as f32, v.z as f32, w];
        let fu = FrameU {
            mvp: m.as_mat4().to_cols_array(),
            clip_xy: cuts.clip.map(|v| v as f32),
            mask_rect: rect.map(|v| v as f32),
            zr: [cuts.zmin as f32, cuts.zmax as f32, if mask_on { 1.0 } else { 0.0 }, 0.0],
            view_dir: v4(b.f, 0.0),
            view_r: v4(b.r, self.explode.as_ref().map_or(0.0, |e| e.gap as f32)),
            view_u: v4(b.u, 0.0),
            eye: [0.0; 4],
        };
        self.gpu.queue.write_buffer(&self.shadow_frame_buf, 0, bytemuck::bytes_of(&fu));
        let mut enc = dev.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.sc.shadow_pipe);
            pass.set_bind_group(0, &self.shadow_frame_bind, &[]);
            for b in self.batches().iter().filter(|b| matches!(b.mode, Mode::Opaque | Mode::AlphaTest)) {
                pass.set_bind_group(1, &b.bind, &[]);
                pass.set_vertex_buffer(0, b.vbuf.slice(..));
                pass.draw(0..b.count, 0..1);
            }
            if let (Some((vbuf, n, bind)), None) = (&self.sc.casters, &self.explode) {
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        self.gpu.queue.submit([enc.finish()]);
        let s = ShadowMap { key, res, view, mvp: m, texel: side / res as f64, depth: d1 - d0 + 64.0 };
        cache.push(s.clone());
        s
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

    #[allow(clippy::too_many_arguments)]
    pub fn render_view(
        &mut self,
        view: &View,
        wpx: u32,
        hpx: u32,
        ss: u32,
        cuts: &Cuts,
        look: &Look,
        time: f64,
    ) -> Result<image::RgbaImage> {
        let (w, h) = (wpx * ss, hpx * ss);
        if w.max(h) > self.gpu.max_dim {
            bail!("render target {w}x{h} exceeds GPU max {}; lower --size or --ss", self.gpu.max_dim);
        }
        let t = self.make_targets(w, h, crate::post::needed(look))?;
        let px = self.render_crop(&t, view, None, ss, cuts, look, time)?;
        let mut px = if ss > 1 { downsample(&px, w, h, wpx, hpx)? } else { px };
        unpremultiply(&mut px);
        if let Some(bg) = look.bg {
            composite_bg(&mut px, bg);
        }
        Ok(image::RgbaImage::from_raw(wpx, hpx, px).unwrap())
    }
}

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    pub fn render_crop(
        &self,
        t: &Targets,
        view: &View,
        crop: Option<Crop>,
        ss: u32,
        cuts: &Cuts,
        look: &Look,
        time: f64,
    ) -> Result<Vec<u8>> {
        let mut tm = crate::prof::Timer::new("render");
        let (w, h) = (t.w, t.h);
        let dev = self.gpu.device.clone();
        let mut enc = dev.create_command_encoder(&Default::default());
        self.draw_crop(&mut enc, t, view, crop, cuts, look, ss, time, [0.0; 4]);
        tm.lap("encode");
        let row = (w * 4).div_ceil(256) * 256;
        let buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            t.color.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        dev.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        tm.lap("gpu");
        let mut px = vec![0u8; (w * h * 4) as usize];
        {
            let data = slice.get_mapped_range()?;
            for y in 0..h as usize {
                let s = y * row as usize;
                px[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(&data[s..s + w as usize * 4]);
            }
        }
        buf.unmap();
        Ok(px)
    }
}

pub fn downsample(px: &[u8], w: u32, h: u32, nw: u32, nh: u32) -> Result<Vec<u8>> {
    use fast_image_resize as fr;
    let src = fr::images::ImageRef::new(w, h, px, fr::PixelType::U8x4)?;
    let mut dst = fr::images::Image::new(nw, nh, fr::PixelType::U8x4);
    let mut rs = fr::Resizer::new();
    rs.resize(
        &src,
        &mut dst,
        &fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)).use_alpha(false),
    )?;
    Ok(dst.into_vec())
}

pub fn unpremultiply(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 {
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
        } else if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

pub fn composite_bg(px: &mut [u8], bg: [u8; 3]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in 0..3 {
            p[c] = ((p[c] as u32 * a + bg[c] as u32 * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
}

pub fn parse_color(s: &str) -> Result<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        bail!("colour must be #RRGGBB: {s}");
    }
    let v = u32::from_str_radix(h, 16).context("bad colour")?;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}
