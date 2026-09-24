use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use wgpu::util::DeviceExt;

use crate::camera::{Basis, camera_basis, extents, ortho};
use crate::mesh::{Mesh, Mode, Vertex};
use crate::reach::HullMask;

pub use egui_wgpu::wgpu;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const NO_CLIP: [f64; 4] = [-1e9, -1e9, 1e9, 1e9];

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
}

struct SkyRes {
    pipeline: wgpu::RenderPipeline,
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
}

pub struct Renderer {
    pub gpu: Gpu,
    pub points: Vec<DVec3>,
    pub mask: Option<HullMask>,
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

fn ubuf_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
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
                ubuf_entry(2, wgpu::ShaderStages::FRAGMENT),
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
        for cull in [true, false] {
            for (blend, depth_write) in [
                (None, true),
                (Some(wgpu::BlendState { color: premul, alpha: premul }), false),
                (Some(wgpu::BlendState { color: add, alpha: add }), false),
            ] {
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
                            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x2],
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
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: COLOR_FORMAT,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                }));
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
            let Some(Some(view)) = tex_views.get(b.tex) else { continue };
            if b.verts.is_empty() {
                continue;
            }
            let vbuf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&b.verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&[b.mode as u32 as f32, b.alpha, 0.0, 0.0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &batch_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&tex_sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: ub.as_entire_binding() },
                ],
            });
            batches.push(GpuBatch { mode: b.mode, vbuf, count: b.verts.len() as u32, bind });
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
        let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
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
        self.sky = Some(SkyRes { pipeline, ubuf, bind, fov, pitch });
    }

    pub fn has_sky(&self) -> bool {
        self.sky.is_some()
    }

    pub fn points_in(&self, cuts: &Cuts) -> Vec<DVec3> {
        let [x0, y0, x1, y1] = cuts.clip;
        let mask = if cuts.use_mask { self.mask.as_ref() } else { None };
        let sel: Vec<DVec3> = self
            .points
            .iter()
            .filter(|p| {
                p.z >= cuts.zmin
                    && p.z <= cuts.zmax
                    && p.x >= x0
                    && p.x <= x1
                    && p.y >= y0
                    && p.y <= y1
                    && mask.is_none_or(|m| m.test(p.x, p.y))
            })
            .copied()
            .collect();
        if sel.is_empty() { self.points.clone() } else { sel }
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
        };
        (view, wpx, hpx)
    }

    pub fn encode(
        &self,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        view: &View,
        aspect: f64,
        cuts: &Cuts,
        cull: bool,
        clear: [f64; 4],
    ) {
        let b = &view.basis;
        let (d0, d1) = self.points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, c), p| {
            let d = p.dot(b.f);
            (a.min(d), c.max(d))
        });
        let m = ortho(b, view.cx, view.cy, view.w, view.h, d0, d1);
        let mask_on = cuts.use_mask && self.mask.is_some();
        let rect = self.mask.as_ref().map(|m| m.rect).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let fu = FrameU {
            mvp: m.as_mat4().to_cols_array(),
            clip_xy: cuts.clip.map(|v| v as f32),
            mask_rect: rect.map(|v| v as f32),
            zr: [cuts.zmin as f32, cuts.zmax as f32, if mask_on { 1.0 } else { 0.0 }, 0.0],
        };
        let q = &self.gpu.queue;
        q.write_buffer(&self.frame_buf, 0, bytemuck::bytes_of(&fu));
        if let (Some(sky), Some(yaw)) = (&self.sky, view.sky_yaw) {
            let sb = camera_basis(yaw, -sky.pitch);
            let tx = (sky.fov.to_radians() / 2.0).tan();
            let v4 = |v: DVec3| [v.x as f32, v.y as f32, v.z as f32, 0.0];
            let su = SkyU { r: v4(sb.r), u: v4(sb.u), f: v4(sb.f), tanfov: [tx as f32, (tx * aspect) as f32, 0.0, 0.0] };
            q.write_buffer(&sky.ubuf, 0, bytemuck::bytes_of(&su));
        }

        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0], g: clear[1], b: clear[2], a: clear[3] }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let (Some(sky), Some(_)) = (&self.sky, view.sky_yaw) {
            pass.set_pipeline(&sky.pipeline);
            pass.set_bind_group(0, &sky.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        pass.set_bind_group(0, &self.frame_bind, &[]);
        let base = if cull { 0 } else { 3 };
        for (pi, modes) in [(0, &[Mode::Opaque, Mode::AlphaTest][..]), (1, &[Mode::Blend][..]), (2, &[Mode::Additive][..])] {
            pass.set_pipeline(&self.pipelines[base + pi]);
            for b in self.batches.iter().filter(|b| modes.contains(&b.mode)) {
                pass.set_bind_group(1, &b.bind, &[]);
                pass.set_vertex_buffer(0, b.vbuf.slice(..));
                pass.draw(0..b.count, 0..1);
            }
        }
    }

    pub fn make_targets(&self, w: u32, h: u32, sampled: bool) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
        let dev = &self.gpu.device;
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        if sampled {
            usage |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        let color = dev.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage,
            view_formats: &[],
        });
        let depth = dev.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let cv = color.create_view(&Default::default());
        let dv = depth.create_view(&Default::default());
        (color, cv, dv)
    }

    pub fn render_view(
        &mut self,
        view: &View,
        wpx: u32,
        hpx: u32,
        ss: u32,
        cuts: &Cuts,
        cull: bool,
        bg: Option<[u8; 3]>,
    ) -> Result<image::RgbaImage> {
        let (w, h) = (wpx * ss, hpx * ss);
        if w.max(h) > self.gpu.max_dim {
            bail!("render target {w}x{h} exceeds GPU max {}; lower --size or --ss", self.gpu.max_dim);
        }
        let (color, cv, dv) = self.make_targets(w, h, false);
        let dev = self.gpu.device.clone();
        let mut enc = dev.create_command_encoder(&Default::default());
        self.encode(&mut enc, &cv, &dv, view, hpx as f64 / wpx as f64, cuts, cull, [0.0; 4]);
        let row = (w * 4).div_ceil(256) * 256;
        let buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            color.as_image_copy(),
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
        if let Some(bg) = bg {
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
