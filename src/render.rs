use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use glam::{DMat4, DVec3};
use wgpu::util::DeviceExt;

use std::sync::Mutex;

use crate::camera::{Basis, camera_basis, extents, ortho, persp};
use crate::clip::clip_tris_z;
use crate::explode::Explode;
use crate::look::Look;
use crate::mesh::{Batch, Mesh, Mode, Vertex};
use crate::post::{Cam, NORMAL_FORMAT, Post, PostTargets};
use crate::reach::HullMask;

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
        Ok(Gpu { device, queue, max_dim: limits.max_texture_dimension_2d })
    }

    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Gpu {
        let max_dim = device.limits().max_texture_dimension_2d;
        Gpu { device, queue, max_dim }
    }
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

pub struct Renderer {
    pub gpu: Gpu,
    pub points: Vec<DVec3>,
    pub mask: Option<HullMask>,
    orig: Option<Vec<DVec3>>,
    pt_band: Vec<u16>,
    explode: Option<Explode>,
    guides: Option<Guides>,
    tex_views: Vec<Option<wgpu::TextureView>>,
    batch_layout: wgpu::BindGroupLayout,
    tex_sampler: wgpu::Sampler,
    frame_layout: wgpu::BindGroupLayout,
    frame_buf: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,
    lmap_view: wgpu::TextureView,
    mask_view: Option<wgpu::TextureView>,
    dummy_view: wgpu::TextureView,
    linear_clamp: wgpu::Sampler,
    pipelines: Vec<wgpu::RenderPipeline>,
    batches: Vec<GpuBatch>,
    sky: Option<SkyRes>,
    post: Post,
}

fn mip_chain(w: u32, h: u32, rgba: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = vec![(w, h, rgba.to_vec())];
    loop {
        let (pw, ph, ref prev) = *out.last().unwrap();
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
    levels: &[(u32, u32, Vec<u8>)],
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
    gpu: &Gpu,
    layout: &wgpu::BindGroupLayout,
    samp: &wgpu::Sampler,
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
    let dev = &gpu.device;
    let vbuf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let (water, warp) = match b.warp {
        Some(w) => (1.0, w),
        None => (0.0, [0.0; 4]),
    };
    let u = [b.mode as u32 as f32, b.alpha, water, 0.0, warp[0], warp[1], warp[2], warp[3], band as f32, 0.0, 0.0, 0.0];
    let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&u),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let make = |v: &wgpu::TextureView| batch_bind(dev, layout, v, samp, &ub);
    let frames = match mesh.anim.get(b.tex) {
        Some(Some(seq)) => seq.iter().filter_map(|&i| tex_views.get(i).and_then(|v| v.as_ref())).map(make).collect(),
        _ => Vec::new(),
    };
    Some(GpuBatch { mode: b.mode, vbuf, count: verts.len() as u32, bind: make(view), frames })
}

fn batch_bind(
    dev: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    v: &wgpu::TextureView,
    samp: &wgpu::Sampler,
    ub: &wgpu::Buffer,
) -> wgpu::BindGroup {
    dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(v) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(samp) },
            wgpu::BindGroupEntry { binding: 2, resource: ub.as_entire_binding() },
        ],
    })
}

fn vpos(v: &Vertex) -> DVec3 {
    DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)
}

impl Renderer {
    pub fn new(gpu: &Gpu, mesh: &Mesh, nearest: bool) -> Renderer {
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
            ],
        });
        let batch_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0, wgpu::TextureViewDimension::D2),
                samp_entry(1),
                ubuf_entry(2, wgpu::ShaderStages::VERTEX_FRAGMENT),
            ],
        });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&frame_layout), Some(&batch_layout)],
            immediate_size: 0,
        });

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
        let mut pipelines = Vec::new();
        for normals in [false, true] {
            for cull in [true, false] {
                for (blend, depth_write) in [
                    (None, true),
                    (Some(wgpu::BlendState { color: premul, alpha: premul }), false),
                    (Some(wgpu::BlendState { color: add, alpha: add }), false),
                ] {
                    let targets = color_targets(blend, normals, depth_write);
                    pipelines.push(dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: None,
                        layout: Some(&layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs"),
                            compilation_options: Default::default(),
                            buffers: &[Some(wgpu::VertexBufferLayout {
                                array_stride: std::mem::size_of::<Vertex>() as u64,
                                step_mode: wgpu::VertexStepMode::Vertex,
                                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x2, 3 => Float32, 4 => Float32x3],
                            })],
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

        let tex_sampler = sampler(gpu, true, nearest, true);
        let mut tex_views: Vec<Option<wgpu::TextureView>> = Vec::new();
        for t in &mesh.textures {
            tex_views.push(t.as_ref().map(|im| {
                let levels = mip_chain(im.w, im.h, &im.rgba);
                upload_texture(gpu, im.w, im.h, 1, wgpu::TextureFormat::Rgba8Unorm, &levels, 4)
                    .create_view(&Default::default())
            }));
        }
        let a = &mesh.atlas;
        let lmap_view =
            upload_texture(gpu, a.w, a.h, 1, wgpu::TextureFormat::Rgba8Unorm, &[(a.w, a.h, a.rgba.clone())], 4)
                .create_view(&Default::default());
        let dummy_view = upload_texture(gpu, 1, 1, 1, wgpu::TextureFormat::R8Unorm, &[(1, 1, vec![255])], 1)
            .create_view(&Default::default());
        let linear_clamp = sampler(gpu, false, false, false);

        let mut batches = Vec::new();
        for b in &mesh.batches {
            batches.extend(make_batch(gpu, &batch_layout, &tex_sampler, &tex_views, mesh, b, &b.verts, 0));
        }

        let frame_buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<FrameU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind =
            Self::make_frame_bind(dev, &frame_layout, &frame_buf, &lmap_view, &dummy_view, &linear_clamp);
        Renderer {
            gpu: gpu.clone(),
            points: mesh.points.clone(),
            mask: None,
            orig: None,
            pt_band: Vec::new(),
            explode: None,
            guides: None,
            tex_views,
            batch_layout,
            tex_sampler,
            frame_layout,
            frame_buf,
            frame_bind,
            lmap_view,
            mask_view: None,
            dummy_view,
            linear_clamp,
            pipelines,
            batches,
            sky: None,
            post: Post::new(dev),
        }
    }

    fn make_frame_bind(
        dev: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        buf: &wgpu::Buffer,
        lmap: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        samp: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(lmap) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(samp) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(mask) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(samp) },
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
                &[(m.nx as u32, m.ny as u32, m.texture())],
                1,
            )
            .create_view(&Default::default())
        });
        self.mask = mask;
        let mv = self.mask_view.as_ref().unwrap_or(&self.dummy_view);
        self.frame_bind = Self::make_frame_bind(
            &self.gpu.device,
            &self.frame_layout,
            &self.frame_buf,
            &self.lmap_view,
            mv,
            &self.linear_clamp,
        );
    }

    pub fn animated(&self) -> bool {
        self.batches.iter().any(|b| b.frames.len() > 1)
    }

    pub fn set_sky(&mut self, faces: Option<(u32, Vec<u8>)>, fov: f64, pitch: f64) {
        let Some((size, data)) = faces else {
            self.sky = None;
            return;
        };
        let gpu = &self.gpu;
        let dev = &gpu.device;
        let n = (size * size * 4) as usize;
        let levels: Vec<(u32, u32, Vec<u8>)> = (0..6).map(|i| (size, size, data[i * n..(i + 1) * n].to_vec())).collect();
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
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.linear_clamp) },
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

    pub fn points_in(&self, cuts: &Cuts) -> Vec<DVec3> {
        let keep = self.keeps(cuts);
        let base = self.orig.as_ref().unwrap_or(&self.points);
        let sel: Vec<DVec3> = base.iter().zip(&self.points).filter(|(p, _)| keep(p)).map(|(_, q)| *q).collect();
        if sel.is_empty() { self.points.clone() } else { sel }
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
            let mut batches = Vec::new();
            let (gpu, lay, samp, tv) = (&self.gpu, &self.batch_layout, &self.tex_sampler, &self.tex_views);
            match e {
                None => {
                    for b in &mesh.batches {
                        batches.extend(make_batch(gpu, lay, samp, tv, mesh, b, &b.verts, 0));
                    }
                    self.orig = None;
                    self.pt_band = Vec::new();
                    self.points = mesh.points.clone();
                }
                Some(x) => {
                    let (mut orig, mut band) = (Vec::new(), Vec::new());
                    for b in &mesh.batches {
                        for (k, verts) in clip_tris_z(&b.verts, &x.planes) {
                            orig.extend(verts.iter().map(vpos));
                            band.extend(std::iter::repeat_n(k as u16, verts.len()));
                            batches.extend(make_batch(gpu, lay, samp, tv, mesh, b, &verts, k));
                        }
                    }
                    self.orig = Some(orig);
                    self.pt_band = band;
                }
            }
            self.batches = batches;
        }
        self.guides = e.filter(|x| x.guides).map(|x| self.make_guides(x.planes.len()));
        self.explode = e.cloned();
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
        let tex = upload_texture(&self.gpu, 1, 1, 1, wgpu::TextureFormat::Rgba8Unorm, &[(1, 1, GUIDE_COLOR.to_vec())], 4)
            .create_view(&Default::default());
        let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&[0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = batch_bind(dev, &self.batch_layout, &tex, &self.tex_sampler, &ub);
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
        let v = |p: DVec3| Vertex { pos: p.as_vec3().to_array(), uv: [0.5, 0.5], lm: [0.0, 0.0], bias: 0.0, normal: n };
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
        cuts: &Cuts,
        cull: bool,
        clear: [f64; 4],
        anim: bool,
        time: f64,
        guide_w: f64,
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
            let su = SkyU { r: v4(sb.r, 0.0), u: v4(sb.u, 0.0), f: v4(sb.f, 0.0), tanfov };
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
        let base = ni * 6 + if cull { 0 } else { 3 };
        let tick = (time * ANIM_FPS).floor().max(0.0) as usize;
        for (pi, modes) in [(0, &[Mode::Opaque, Mode::AlphaTest][..]), (1, &[Mode::Blend][..]), (2, &[Mode::Additive][..])] {
            pass.set_pipeline(&self.pipelines[base + pi]);
            for b in self.batches.iter().filter(|b| modes.contains(&b.mode)) {
                let bind = if anim && !b.frames.is_empty() { &b.frames[tick % b.frames.len()] } else { &b.bind };
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, b.vbuf.slice(..));
                pass.draw(0..b.count, 0..1);
            }
            if let (0, Some((g, n))) = (pi, &guide) {
                pass.set_pipeline(&self.pipelines[ni * 6 + 3]);
                pass.set_bind_group(1, &g.bind, &[]);
                pass.set_vertex_buffer(0, g.vbuf.slice(..));
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
        let aspect = t.h as f64 / t.w as f64;
        let anim = look.anim_textures;
        let (m, depth) = self.projection(view, t.w, t.h);
        let guide_w = GUIDE_PX * ss.max(1) as f64 * view.w / t.w.max(1) as f64;
        let Some(pt) = t.post.as_ref().filter(|_| crate::post::needed(look)) else {
            self.encode_world(enc, &t.color_view, None, &t.depth_view, view, &m, aspect, cuts, look.cull, clear, anim, time, guide_w);
            return;
        };
        let scene = pt.scene();
        self.encode_world(enc, scene, Some(&pt.normal), &t.depth_view, view, &m, aspect, cuts, look.cull, clear, anim, time, guide_w);
        let focus = view.persp.map(|p| p.focus);
        let cam = Cam { w: t.w, h: t.h, upp: view.w / t.w as f64, ss, depth, focus };
        self.post.run(&self.gpu, enc, look, &cam, pt, &t.depth_view, &t.color_view);
    }

    pub fn make_targets(&self, w: u32, h: u32, post: bool) -> Targets {
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
        let t = self.make_targets(w, h, crate::post::needed(look));
        let dev = self.gpu.device.clone();
        let mut enc = dev.create_command_encoder(&Default::default());
        self.draw(&mut enc, &t, view, cuts, look, ss, time, [0.0; 4]);
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
        let mut px = vec![0u8; (w * h * 4) as usize];
        {
            let data = slice.get_mapped_range()?;
            for y in 0..h as usize {
                let s = y * row as usize;
                px[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(&data[s..s + w as usize * 4]);
            }
        }
        buf.unmap();
        let mut px = if ss > 1 { downsample(&px, w, h, wpx, hpx)? } else { px };
        unpremultiply(&mut px);
        if let Some(bg) = look.bg {
            composite_bg(&mut px, bg);
        }
        Ok(image::RgbaImage::from_raw(wpx, hpx, px).unwrap())
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
