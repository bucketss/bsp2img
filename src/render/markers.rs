use glam::DVec3;

use super::wgpu::util::DeviceExt;
use super::{Packed, Renderer, View, batch_bind, upload_texture, wgpu};
use crate::mesh::Vertex;

const XRAY_ALPHA: f32 = 0.3;

#[derive(Clone, Debug)]
pub struct MarkLine {
    pub a: DVec3,
    pub b: DVec3,
    pub color: [u8; 4],
    pub px: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Markers {
    pub tris: Vec<(DVec3, [u8; 4])>,
    pub lines: Vec<MarkLine>,
}

impl Markers {
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty() && self.lines.is_empty()
    }
}

pub(super) struct MarkerRes {
    m: Markers,
    pub(super) vbuf: wgpu::Buffer,
    pub(super) cap: usize,
    pub(super) bind: wgpu::BindGroup,
    pub(super) xray: wgpu::BindGroup,
}

fn uniform(alpha: f32) -> [f32; 52] {
    let mut u = [0.0f32; 52];
    u[..25].copy_from_slice(&[2.0, alpha, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    u[28..30].copy_from_slice(&[1.0, 1.0]);
    u
}

impl Renderer {
    pub fn set_markers(&mut self, m: Option<Markers>) {
        self.markers = m.filter(|m| !m.is_empty()).map(|m| {
            let dev = &self.gpu.device;
            let cap = m.tris.len() + m.lines.len() * 6;
            let vbuf = dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (cap * (12 + std::mem::size_of::<Packed>())) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let tex = upload_texture(&self.gpu, 1, 1, 1, wgpu::TextureFormat::Rgba8Unorm, &[(1, 1, &[255u8; 4][..])], 4)
                .create_view(&Default::default());
            let ub = |a: f32| {
                dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&uniform(a)),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
            };
            let bind = batch_bind(dev, &self.sc.batch_layout, [&tex; 5], &ub(1.0));
            let xray = batch_bind(dev, &self.sc.batch_layout, [&tex; 5], &ub(XRAY_ALPHA));
            MarkerRes { m, vbuf, cap, bind, xray }
        });
    }

    pub(super) fn write_markers(&self, mr: &MarkerRes, view: &View, unit: f64) -> u32 {
        let f = view.basis.f;
        let nrm = (-f).as_vec3().to_array();
        let v = |p: DVec3, c: [u8; 4]| Vertex { pos: p.as_vec3().to_array(), uv: [0.5, 0.5], normal: nrm, paint: c, ..Vertex::BLANK };
        let mut out: Vec<Vertex> = mr.m.tris.iter().map(|&(p, c)| v(p, c)).collect();
        for l in &mr.m.lines {
            let d = l.b - l.a;
            let mut side = d.cross(f);
            if side.length_squared() < 1e-12 {
                side = view.basis.r;
            }
            let side = side.normalize() * (l.px * unit / 2.0);
            let q = [v(l.a - side, l.color), v(l.a + side, l.color), v(l.b + side, l.color), v(l.b - side, l.color)];
            out.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
        let pos: Vec<[f32; 3]> = out.iter().map(|v| v.pos).collect();
        let attr: Vec<Packed> = out.iter().map(Packed::new).collect();
        let q = &self.gpu.queue;
        q.write_buffer(&mr.vbuf, 0, bytemuck::cast_slice(&pos));
        q.write_buffer(&mr.vbuf, (mr.cap * 12) as u64, bytemuck::cast_slice(&attr));
        out.len() as u32
    }
}
