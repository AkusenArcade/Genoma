//! Particles (PRD §5.4, §8.3): each one's position, size and fade are a
//! function of its age and its seed, computed in the vertex shader. The CPU
//! keeps only when each was born, in a ring, and writes a slot when a
//! particle is born, nothing per frame. There is no compute pass and no
//! position buffer to update.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

/// One emitter, as the main thread describes it.
pub struct Emitter {
    /// Stable across frames, so its births are uploaded only when they change.
    pub key: u64,
    /// Changes whenever `births` does.
    pub generation: u64,
    /// Seconds since the scene's epoch, for each slot; negative is empty.
    pub births: std::sync::Arc<Vec<f32>>,
    pub params: EmitterParams,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct EmitterParams {
    /// Physical pixels: origin x, y, target x, y (target used when `mode` is 1).
    pub points: [f32; 4],
    /// Now (seconds since the epoch), lifetime, speed (px/s), spread (radians).
    pub motion: [f32; 4],
    /// Direction (radians, 0 is up), gravity (px/s², down), size (px), mode.
    pub shape: [f32; 4],
    pub color: [f32; 4],
    /// x0, y0, x1, y1, physical pixels.
    pub clip: [f32; 4],
    pub viewport: [f32; 4],
    /// The carried path's control point, physical pixels, and whether it is
    /// set: particles then follow the same curve as a thread.
    pub bend: [f32; 4],
}

const SHADER: &str = r#"
struct Params {
    points: vec4<f32>,
    motion: vec4<f32>,
    shape: vec4<f32>,
    color: vec4<f32>,
    clip: vec4<f32>,
    viewport: vec4<f32>,
    bend: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> births: array<f32>;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) radius: f32,
};

// A particle's own randomness: three numbers in [0, 1) from its slot and
// its birth, so a slot reborn is a new particle.
fn hash(n: f32) -> f32 { return fract(sin(n) * 43758.5453); }

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) slot: u32) -> Varyings {
    var out: Varyings;
    let born = births[slot];
    let life = p.motion.y;
    let age = p.motion.x - born;
    if (born < 0.0 || age < 0.0 || age > life) {
        out.position = vec4<f32>(2.0, 2.0, 0.0, 1.0);
        out.alpha = 0.0;
        return out;
    }
    let seed = f32(slot) * 12.9898 + born * 78.233;
    let r1 = hash(seed);
    let r2 = hash(seed + 1.7);
    let r3 = hash(seed + 3.1);
    let t = age / life;
    var at: vec2<f32>;
    if (p.shape.w > 0.5) {
        // Carried to a destination: along the line, swaying sideways and
        // drawn in at the end, arriving when its life does.
        let start = p.points.xy;
        let end = p.points.zw;
        let along = end - start;
        let side = normalize(vec2<f32>(-along.y, along.x) + vec2<f32>(1e-5, 0.0));
        let ease = t * t * (3.0 - 2.0 * t);
        let sway = sin(t * 3.14159) * (r1 - 0.5) * p.motion.w * length(along) * 0.25;
        var on = start + along * ease;
        if (p.bend.z > 0.5) {
            let u = 1.0 - ease;
            on = u * u * start + 2.0 * u * ease * p.bend.xy + ease * ease * end;
        }
        at = on + side * sway;
    } else {
        // Ballistic: a direction within the spread, a speed, and gravity.
        let angle = p.shape.x + (r1 - 0.5) * p.motion.w;
        let speed = p.motion.z * (0.6 + 0.4 * r2);
        let v = vec2<f32>(sin(angle), -cos(angle)) * speed;
        at = p.points.xy + v * age + vec2<f32>(0.0, 0.5 * p.shape.y * age * age);
    }
    // In over the first fifth, out over the last half.
    out.alpha = clamp(t * 5.0, 0.0, 1.0) * clamp((1.0 - t) * 2.0, 0.0, 1.0) * p.color.a;
    let size = p.shape.z * (0.7 + 0.6 * r3);
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u)) * 2.0 - 1.0;
    let half = size * 0.5 + 1.0;
    let pixel = at + corner * half;
    out.position = vec4<f32>(pixel / p.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.local = corner * half;
    out.radius = size * 0.5;
    return out;
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    let q = in.position.xy;
    if (q.x < p.clip.x || q.y < p.clip.y || q.x >= p.clip.z || q.y >= p.clip.w) { discard; }
    let cover = clamp(in.radius - length(in.local) + 0.5, 0.0, 1.0) * in.alpha;
    return vec4<f32>(p.color.rgb * cover, cover);
}
"#;

struct Slot {
    params: wgpu::Buffer,
    births: wgpu::Buffer,
    capacity: usize,
    group: wgpu::BindGroup,
    generation: u64,
    count: u32,
}

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    slots: HashMap<u64, Slot>,
    draws: Vec<u64>,
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particles"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particles"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particles"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Pipeline { pipeline, layout, slots: HashMap::new(), draws: Vec::new() }
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2], emitters: &[Emitter]) {
        self.draws.clear();
        for e in emitters {
            let n = e.births.len().max(1);
            let rebuild = self.slots.get(&e.key).is_none_or(|s| s.capacity < n);
            if rebuild {
                let params = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle params"),
                    size: std::mem::size_of::<EmitterParams>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let births = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle births"),
                    size: (n * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("particles"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: births.as_entire_binding() },
                    ],
                });
                self.slots.insert(e.key, Slot { params, births, capacity: n, group, generation: u64::MAX, count: 0 });
            }
            let slot = self.slots.get_mut(&e.key).expect("made above");
            let mut params = e.params;
            params.viewport = [viewport[0], viewport[1], 0.0, 0.0];
            queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&params));
            // The births travel only when one was added.
            if slot.generation != e.generation && !e.births.is_empty() {
                queue.write_buffer(&slot.births, 0, bytemuck::cast_slice(&e.births));
                slot.generation = e.generation;
            }
            slot.count = e.births.len() as u32;
            self.draws.push(e.key);
        }
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for key in &self.draws {
            let slot = &self.slots[key];
            if slot.count > 0 {
                pass.set_bind_group(0, &slot.group, &[]);
                pass.draw(0..4, 0..slot.count);
            }
        }
    }
}
