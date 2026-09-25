use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::look::{Look, Tilt};
use crate::render::{COLOR_FORMAT, Gpu, attachment_view, ubuf_entry, wgpu};

pub const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const AO_KERNEL: u32 = 16;
const INK_DEPTH_STEP: f64 = 6.0;
const INK_NORMAL_DOT: f64 = 0.8;
const MAX_INK_RADIUS: f64 = 12.0;
const BLUR_RAMP: f64 = 0.3;
const MAX_BLUR_RADIUS: f64 = 64.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostU {
    px: [f32; 4],
    cam: [f32; 4],
    a: [f32; 4],
    b: [f32; 4],
    tile: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub w: u32,
    pub h: u32,
    pub upp: f64,
    pub ss: u32,
    pub depth: [f64; 4],
    pub focus: Option<f64>,
    pub tile: [f64; 4],
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ao = 0,
    AoApply = 1,
    Grade = 2,
    Ink = 3,
    Coc = 4,
    BlurH = 5,
    BlurV = 6,
}

impl Kind {
    fn writes_aux(self) -> bool {
        matches!(self, Kind::Ao | Kind::Coc)
    }
}

const KINDS: [(Kind, &str); 7] = [
    (Kind::Ao, include_str!("shaders/ao.wgsl")),
    (Kind::AoApply, include_str!("shaders/ao_apply.wgsl")),
    (Kind::Grade, include_str!("shaders/grade.wgsl")),
    (Kind::Ink, include_str!("shaders/ink.wgsl")),
    (Kind::Coc, include_str!("shaders/coc.wgsl")),
    (Kind::BlurH, include_str!("shaders/blur.wgsl")),
    (Kind::BlurV, include_str!("shaders/blur.wgsl")),
];

fn grade_on(l: &Look) -> bool {
    (l.saturation - 1.0).abs() > 1e-6 || l.tint_amount > 0.0 || (l.contrast - 1.0).abs() > 1e-6
}

fn tilt_on(l: &Look) -> bool {
    l.tilt != Tilt::Off && l.blur > 0.0
}

pub fn needed(l: &Look) -> bool {
    l.ao || l.ink || grade_on(l) || tilt_on(l)
}

pub fn reach(l: &Look, upp: f64, ss: u32) -> f64 {
    let mut m = 0.0;
    if l.ao {
        m += (l.ao_radius / upp.max(1e-6)).ceil() + 3.0;
    }
    if l.ink {
        m += ((l.ink_width * ss as f64 / 2.0).clamp(0.5, MAX_INK_RADIUS) + 0.5).ceil() + 1.0;
    }
    if tilt_on(l) {
        m += (l.blur * ss as f64).clamp(1.0, MAX_BLUR_RADIUS).ceil() + 1.0;
    }
    m
}

pub fn ao_samples(ss: u32) -> u32 {
    AO_KERNEL.div_ceil(ss * ss).max(4)
}

fn rgb(c: [u8; 3], w: f32) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, w]
}

fn steps(l: &Look, cam: &Cam) -> Vec<(Kind, [f32; 4], [f32; 4])> {
    let mut s = Vec::new();
    if l.ao {
        let bias = (cam.upp * 2.0).max(1.0);
        s.push((Kind::Ao, [l.ao_radius as f32, l.ao_strength as f32, ao_samples(cam.ss) as f32, bias as f32], [0.0; 4]));
        s.push((Kind::AoApply, [0.0; 4], [0.0; 4]));
    }
    if grade_on(l) {
        s.push((Kind::Grade, [l.saturation as f32, l.tint_amount as f32, l.contrast as f32, 0.0], rgb(l.tint, 1.0)));
    }
    if l.ink {
        let r = (l.ink_width * cam.ss as f64 / 2.0).clamp(0.5, MAX_INK_RADIUS);
        s.push((Kind::Ink, [r as f32, INK_DEPTH_STEP as f32, INK_NORMAL_DOT as f32, 0.0], rgb(l.ink_color, 1.0)));
    }
    if tilt_on(l) {
        let (mode, focus) = match (l.tilt, cam.focus) {
            (Tilt::Dof, Some(f)) => (2.0, if l.focus_dist > 0.0 { l.focus_dist } else { f }),
            _ => (1.0, 0.0),
        };
        let a = [mode, l.focus_y as f32, l.band as f32, BLUR_RAMP as f32];
        s.push((Kind::Coc, a, [focus as f32, 0.0, 0.0, 0.0]));
        let r = (l.blur * cam.ss as f64).clamp(1.0, MAX_BLUR_RADIUS) as f32;
        s.push((Kind::BlurH, [r, 1.0, 0.0, 0.0], [0.0; 4]));
        s.push((Kind::BlurV, [r, 0.0, 1.0, 0.0], [0.0; 4]));
    }
    s
}

pub struct PostTargets {
    pub normal: wgpu::TextureView,
    tmp: [wgpu::TextureView; 2],
    aux: wgpu::TextureView,
}

impl PostTargets {
    pub fn new(dev: &wgpu::Device, w: u32, h: u32) -> PostTargets {
        PostTargets {
            normal: attachment_view(dev, w, h, NORMAL_FORMAT),
            tmp: [attachment_view(dev, w, h, COLOR_FORMAT), attachment_view(dev, w, h, COLOR_FORMAT)],
            aux: attachment_view(dev, w, h, AO_FORMAT),
        }
    }

    pub fn scene(&self) -> &wgpu::TextureView {
        &self.tmp[0]
    }
}

pub struct Post {
    layout: wgpu::BindGroupLayout,
    pipes: Vec<wgpu::RenderPipeline>,
    dummy: wgpu::TextureView,
}

fn load_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

impl Post {
    pub fn new(dev: &wgpu::Device) -> Post {
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                ubuf_entry(0, wgpu::ShaderStages::FRAGMENT),
                load_entry(1, float),
                load_entry(2, wgpu::TextureSampleType::Depth),
                load_entry(3, float),
                load_entry(4, float),
            ],
        });
        let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let common = include_str!("shaders/post_common.wgsl");
        let pipes = KINDS
            .iter()
            .map(|(k, src)| {
                let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("post"),
                    source: wgpu::ShaderSource::Wgsl(format!("{common}\n{src}").into()),
                });
                let format = if k.writes_aux() { AO_FORMAT } else { COLOR_FORMAT };
                dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: None,
                    layout: Some(&pl),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        let dummy = attachment_view(dev, 1, 1, COLOR_FORMAT);
        Post { layout, pipes, dummy }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        look: &Look,
        cam: &Cam,
        t: &PostTargets,
        depth: &wgpu::TextureView,
        out: &wgpu::TextureView,
    ) {
        let dev = &gpu.device;
        let steps = steps(look, cam);
        let last_color = steps.iter().rposition(|s| !s.0.writes_aux());
        let mut cur = 0usize;
        for (i, (kind, a, b)) in steps.iter().enumerate() {
            let u = PostU {
                px: [cam.w as f32, cam.h as f32, cam.upp as f32, cam.ss as f32],
                cam: cam.depth.map(|v| v as f32),
                a: *a,
                b: *b,
                tile: cam.tile.map(|v| v as f32),
            };
            let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let writes_aux = kind.writes_aux();
            let aux_in = if writes_aux { &self.dummy } else { &t.aux };
            let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&t.tmp[cur]) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(depth) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&t.normal) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(aux_in) },
                ],
            });
            let target = if writes_aux {
                &t.aux
            } else if Some(i) == last_color {
                out
            } else {
                &t.tmp[1 - cur]
            };
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipes[*kind as usize]);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            if !writes_aux {
                cur = 1 - cur;
            }
        }
    }
}
