use super::*;

impl Renderer {
    pub(super) fn pick_pipe(&self) -> &(wgpu::ComputePipeline, wgpu::BindGroupLayout) {
        self.pick.get_or_init(|| {
            let dev = &self.gpu.device;
            let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
            let buffer = |ty| wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None };
            let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[
                    entry(
                        0,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                    ),
                    entry(1, buffer(wgpu::BufferBindingType::Storage { read_only: false })),
                    entry(2, buffer(wgpu::BufferBindingType::Uniform)),
                ],
            });
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/pick.wgsl").into()),
            });
            let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
            let pipe = dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(&pl),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            (pipe, layout)
        })
    }

    pub fn read_depth(&self, t: &Targets, x: u32, y: u32) -> Option<f64> {
        if x >= t.w || y >= t.h {
            return None;
        }
        let dev = &self.gpu.device;
        let lim = dev.limits();
        let compute = lim.max_compute_workgroups_per_dimension > 0 && lim.max_storage_buffers_per_shader_stage > 0;
        let size = if compute { 4 } else { ((t.w * 4).div_ceil(256) * 256 * t.h) as u64 };
        let buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = dev.create_command_encoder(&Default::default());
        let offset = if compute {
            let (pipe, layout) = self.pick_pipe();
            let out = dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let at = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&[x, y, 0u32, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&t.depth_view) },
                    wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: at.as_entire_binding() },
                ],
            });
            {
                let mut pass = enc.begin_compute_pass(&Default::default());
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            enc.copy_buffer_to_buffer(&out, 0, &buf, 0, 4);
            0
        } else {
            let row = (t.w * 4).div_ceil(256) * 256;
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
            (y * row + x * 4) as usize
        };
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
            f32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as f64
        };
        buf.unmap();
        Some(d)
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
