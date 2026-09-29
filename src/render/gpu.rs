use super::*;

#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub max_dim: u32,
}

impl Gpu {
    pub fn headless() -> Result<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context("no GPU adapter found")?;
        crate::prof::note(|| {
            let i = adapter.get_info();
            format!("adapter {} ({:?})", i.name, i.backend)
        });
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC,
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

pub(super) fn catch_errors(dev: &wgpu::Device) {
    dev.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| crate::prof::gpu_error(e.to_string())));
}

pub(super) fn mip_chain(w: u32, h: u32, rgba: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    let masked = rgba.chunks_exact(4).all(|p| p[3] == 0 || p[3] == 255) && rgba.chunks_exact(4).any(|p| p[3] == 0);
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
                let mut wacc = [0u32; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(pw - 1);
                    let sy = (y * 2 + dy).min(ph - 1);
                    let i = ((sy * pw + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += prev[i + c] as u32;
                    }
                    for c in 0..3 {
                        wacc[c] += prev[i + c] as u32 * prev[i + 3] as u32;
                    }
                }
                let o = ((y * nw + x) * 4) as usize;
                for c in 0..4 {
                    next[o + c] = if masked && c < 3 && acc[3] > 0 { ((wacc[c] + acc[3] / 2) / acc[3]) as u8 } else { ((acc[c] + 2) / 4) as u8 };
                }
            }
        }
        out.push((nw, nh, next));
    }
    out
}

pub(super) fn upload_texture(
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

pub(super) fn sampler(gpu: &Gpu, repeat: bool, nearest: bool, aniso: bool) -> wgpu::Sampler {
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

pub(super) fn tex_entry(binding: u32, dim: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
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

pub(super) fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

pub(super) fn color_targets(blend: Option<wgpu::BlendState>, normals: bool, normal_write: bool) -> Vec<Option<wgpu::ColorTargetState>> {
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

pub(super) fn texture(dev: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
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
pub(super) const UPLOAD_CHUNK: usize = 64 << 20;
pub(super) const UPLOAD_FLUSH: u64 = 256 << 20;

pub(super) struct Stager<'a> {
    pub(super) gpu: &'a Gpu,
    pub(super) pending: u64,
}

impl Stager<'_> {
    pub(super) fn buffer(&mut self, usage: wgpu::BufferUsages, data: &[u8]) -> wgpu::Buffer {
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

    pub(super) fn write(&mut self, buf: &wgpu::Buffer, offset: u64, data: &[u8]) {
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

    pub(super) fn packed<T: Pod + Send>(&mut self, usage: wgpu::BufferUsages, verts: &[Vertex], f: impl Fn(&Vertex) -> T + Sync) -> wgpu::Buffer {
        let size = std::mem::size_of::<T>();
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: ((verts.len() * size) as u64).max(4).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let per = UPLOAD_CHUNK / size;
        let mut tmp: Vec<T> = Vec::with_capacity(per.min(verts.len()));
        for (i, c) in verts.chunks(per).enumerate() {
            c.par_iter().map(&f).collect_into_vec(&mut tmp);
            self.write(&buf, (i * per * size) as u64, bytemuck::cast_slice(&tmp));
        }
        buf
    }

    pub(super) fn texture(&mut self, w: u32, h: u32, format: wgpu::TextureFormat, levels: &[(u32, u32, &[u8])], bpp: u32) -> wgpu::TextureView {
        let t = upload_texture(self.gpu, w, h, 1, format, levels, bpp);
        self.add(levels.iter().map(|l| l.2.len() as u64).sum());
        t.create_view(&Default::default())
    }

    pub(super) fn compressed(&mut self, c: &crate::s2::texture::Compressed) -> wgpu::TextureView {
        let (format, bb) = match c.format {
            1 => (wgpu::TextureFormat::Bc1RgbaUnorm, 8),
            2 => (wgpu::TextureFormat::Bc3RgbaUnorm, 16),
            _ => (wgpu::TextureFormat::Bc7RgbaUnorm, 16),
        };
        let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: c.w as u32, height: c.h as u32, depth_or_array_layers: 1 },
            mip_level_count: c.levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (l, data) in c.levels.iter().enumerate() {
            let (bw, bh) = (((c.w >> l).max(1)).div_ceil(4) as u32, ((c.h >> l).max(1)).div_ceil(4) as u32);
            let n = (bw * bh) as usize * bb;
            self.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: l as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &data[..n.min(data.len())],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bw * bb as u32), rows_per_image: Some(bh) },
                wgpu::Extent3d { width: bw * 4, height: bh * 4, depth_or_array_layers: 1 },
            );
            self.add(n as u64);
        }
        tex.create_view(&Default::default())
    }

    pub(super) fn add(&mut self, n: u64) {
        self.pending += n;
        if self.pending >= UPLOAD_FLUSH {
            self.flush();
        }
    }

    pub(super) fn flush(&mut self) {
        self.gpu.queue.submit([]);
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        self.pending = 0;
    }
}
