use super::*;

impl Renderer {
    pub fn sun(&self, look: &Look) -> Option<Sun> {
        look.relit().then(|| sun_at(&self.sun_env, look))
    }

    pub(super) fn prepare_light(&self, look: &Look, cuts: &Cuts) -> (wgpu::BindGroup, [f32; 4]) {
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
        let mut shadow0 = sh(&b);
        shadow0[3] = if sun.is_some() { self.tex_gamma } else { 0.0 };
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
            shadow0,
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

    pub(super) fn shadow_map(&self, dir: DVec3, cuts: &Cuts, res: u32) -> ShadowMap {
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
            view_r: v4(b.r, 0.0),
            view_u: v4(b.u, 0.0),
            eye: [0.0; 4],
        };
        let bands = self.write_bands(&self.shadow_frame_buf, fu);
        let cl = Cull::new(&m, cuts);
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
            pass.set_bind_group(0, &self.shadow_frame_bind, &[0]);
            let mut cur = None;
            for b in self.sc.batches.iter().filter(|b| matches!(b.mode, Mode::Opaque | Mode::AlphaTest)) {
                let full = b.mode == Mode::AlphaTest;
                if cur != Some(full) {
                    pass.set_pipeline(&self.sc.shadow_pipe[full as usize]);
                    cur = Some(full);
                }
                pass.set_bind_group(1, &b.bind, &[]);
                pass.set_vertex_buffer(0, b.pbuf.slice(..));
                if full {
                    pass.set_vertex_buffer(1, b.abuf.slice(..));
                }
                pass.set_index_buffer(b.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for w in bands.iter().filter(|w| w.meets(b.zr)) {
                    let rs = cl.ranges(b, w);
                    if !rs.is_empty() {
                        pass.set_bind_group(0, &self.shadow_frame_bind, &[w.off]);
                    }
                    for r in rs {
                        pass.draw_indexed(r, 0, 0..1);
                    }
                }
            }
            pass.set_bind_group(0, &self.shadow_frame_bind, &[0]);
            if let (Some((vbuf, ibuf, n, bind)), None) = (&self.sc.casters, &self.explode) {
                pass.set_pipeline(&self.sc.shadow_pipe[0]);
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*n, 0, 0..1);
            }
        }
        self.gpu.queue.submit([enc.finish()]);
        let s = ShadowMap { key, res, view, mvp: m, texel: side / res as f64, depth: d1 - d0 + 64.0 };
        cache.push(s.clone());
        s
    }
}
