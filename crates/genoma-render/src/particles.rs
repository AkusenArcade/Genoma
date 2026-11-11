//! Particles (PRD §5.4, §8.3): each one's position, size and fade are a
//! function of its age and its seed, computed in the vertex shader. The CPU
//! keeps only when each was born, in a ring, and writes a slot when a
//! particle is born, nothing per frame.
//!
//! An emitter whose particles feel forces (attractors, repulsors) or meet
//! walls cannot be worked out from its age alone: it is simulated. Each of
//! its particles keeps a position and a velocity in a buffer of its own, a
//! compute pass steps them to the frame's time, and the vertex shader reads
//! them there. The others stay as they were, with no buffer to step.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

/// One emitter, as the main thread describes it.
#[derive(Clone)]
pub struct Emitter {
    /// Stable across frames, so its births are uploaded only when they change.
    pub key: u64,
    /// Changes whenever `births` does.
    pub generation: u64,
    /// Seconds since the scene's epoch, for each slot; negative is empty.
    pub births: std::sync::Arc<Vec<f32>>,
    pub params: EmitterParams,
    /// A sub-emitter's parent's key: when the parent is simulated, where
    /// each of its particles died is read from its buffer.
    pub parent: Option<u64>,
    /// The picture each particle is drawn as, tinted by its colour, instead
    /// of a dot.
    pub sprite: Option<Sprite>,
}

/// A particle's picture: premultiplied RGBA, kept by the images' key.
#[derive(Clone)]
pub struct Sprite {
    pub key: u64,
    pub pixels: std::sync::Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
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
    /// The colour at the end of a life (straight RGBA), its alpha negative
    /// when there is none and `color` holds throughout.
    pub color_end: [f32; 4],
    /// The diameter at the end of a life (px); the trail, seconds of the
    /// path drawn behind each particle (0 none); the burst, particles born
    /// where each of the parent's dies (0: not a sub-emitter); how much of
    /// its speed a simulated particle keeps off a wall, 0…1.
    pub life: [f32; 4],
    /// A sub-emitter's parent, as its own `points`, `motion`, `shape` and
    /// `bend`: where each of its particles dies is worked out from them.
    pub parent: [[f32; 4]; 4],
    /// Simulated (1 or 0); the parent simulated; how many `forces`; how
    /// many `walls`.
    pub sim: [f32; 4],
    /// Points that pull (or, negative, push): x, y, physical pixels; the
    /// pull, px/s²; how far it reaches, fading to nothing there, px.
    pub forces: [[f32; 4]; FORCES],
    /// Rounded rectangles particles bounce off, two rows each: x0, y0, x1,
    /// y1, physical pixels; then the corners' radius, px.
    pub walls: [[f32; 4]; WALLS * 2],
    /// Drawn as its sprite (1) rather than a dot; how fast each turns at
    /// most, radians a second, either way.
    pub look: [f32; 4],
    /// Depth, as a rect's (see [`crate::Rect::transform`]).
    pub transform: [[f32; 4]; 4],
}

/// Attractors and walls an emitter feels at most.
pub const FORCES: usize = 8;
pub const WALLS: usize = 8;

impl Emitter {
    /// How many particles it draws at most: a slot each, or for a
    /// sub-emitter a burst for each of its parent's slots.
    pub fn instances(&self) -> u32 {
        let burst = self.params.life[2].max(0.0) as u32;
        self.births.len() as u32 * burst.max(1)
    }
}

/// What both the drawing and the stepping read: the emitter, its births,
/// and a sub-emitter's parent's particles.
const PARAMS: &str = r#"
struct Params {
    points: vec4<f32>,
    motion: vec4<f32>,
    shape: vec4<f32>,
    color: vec4<f32>,
    clip: vec4<f32>,
    viewport: vec4<f32>,
    bend: vec4<f32>,
    color_end: vec4<f32>,
    life: vec4<f32>,
    parent_points: vec4<f32>,
    parent_motion: vec4<f32>,
    parent_shape: vec4<f32>,
    parent_bend: vec4<f32>,
    sim: vec4<f32>,
    forces: array<vec4<f32>, 8>,
    walls: array<vec4<f32>, 16>,
    look: vec4<f32>,
    transform: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> births: array<f32>;
@group(0) @binding(3) var<storage, read> parent_state: array<vec4<f32>>;
"#;

/// Where particles are born and how they move when nothing but their age
/// says: shared by the drawing and the stepping.
const COMMON: &str = r#"
// A particle's own randomness: numbers in [0, 1) from a seed made of its
// slot and its birth, so a slot reborn is a new particle.
fn hash(n: f32) -> f32 { return fract(sin(n) * 43758.5453); }

// The velocity a ballistic particle is thrown at: a direction within the
// spread, a speed.
fn thrown(motion: vec4<f32>, shape: vec4<f32>, r1: f32, r2: f32) -> vec2<f32> {
    let angle = shape.x + (r1 - 0.5) * motion.w;
    let speed = motion.z * (0.6 + 0.4 * r2);
    return vec2<f32>(sin(angle), -cos(angle)) * speed;
}

// Where a particle of an emitter (its points, motion, shape and bend) is
// `age` seconds into a life of `life`, its randomness r1, r2.
fn place(points: vec4<f32>, motion: vec4<f32>, shape: vec4<f32>, bend: vec4<f32>, r1: f32, r2: f32, age: f32, life: f32) -> vec2<f32> {
    if (shape.w > 0.5) {
        // Carried to a destination: along the line, swaying sideways and
        // drawn in at the end, arriving when its life does.
        let t = clamp(age / life, 0.0, 1.0);
        let start = points.xy;
        let end = points.zw;
        let along = end - start;
        let side = normalize(vec2<f32>(-along.y, along.x) + vec2<f32>(1e-5, 0.0));
        let ease = t * t * (3.0 - 2.0 * t);
        let sway = sin(t * 3.14159) * (r1 - 0.5) * motion.w * length(along) * 0.25;
        var on = start + along * ease;
        if (bend.z > 0.5) {
            let u = 1.0 - ease;
            on = u * u * start + 2.0 * u * ease * bend.xy + ease * ease * end;
        }
        return on + side * sway;
    }
    // Ballistic: thrown, and gravity.
    return points.xy + thrown(motion, shape, r1, r2) * age + vec2<f32>(0.0, 0.5 * shape.y * age * age);
}

// Who a particle is: when it was born, its seed, and where from. A
// sub-emitter's are born in bursts where each of the parent's dies, from
// the point it died at.
struct Who {
    ok: bool,
    born: f32,
    seed: f32,
    origin: vec4<f32>,
};

fn who(instance: u32) -> Who {
    var w: Who;
    w.ok = false;
    let burst = u32(max(p.life.z, 0.0));
    if (burst > 0u) {
        let slot = instance / burst;
        if (slot >= arrayLength(&births)) {
            return w;
        }
        let parent_born = births[slot];
        if (parent_born < 0.0) {
            return w;
        }
        let parent_life = p.parent_motion.y;
        let parent_seed = f32(slot) * 12.9898 + parent_born * 78.233;
        var end: vec2<f32>;
        if (p.sim.y > 0.5) {
            end = parent_state[slot * 2u].xy;
        } else {
            end = place(p.parent_points, p.parent_motion, p.parent_shape, p.parent_bend, hash(parent_seed), hash(parent_seed + 1.7), parent_life, parent_life);
        }
        w.origin = vec4<f32>(end, end);
        w.born = parent_born + parent_life;
        w.seed = parent_seed + f32(instance % burst + 1u) * 91.7;
    } else {
        if (instance >= arrayLength(&births)) {
            return w;
        }
        w.born = births[instance];
        if (w.born < 0.0) {
            return w;
        }
        w.origin = p.points;
        w.seed = f32(instance) * 12.9898 + w.born * 78.233;
    }
    w.ok = true;
    return w;
}
"#;

/// Drawing: a quad round each particle, a dot or a segment with its trail.
const DRAW: &str = r#"
@group(0) @binding(2) var<storage, read> state: array<vec4<f32>>;
@group(1) @binding(0) var sprite: texture_2d<f32>;
@group(1) @binding(1) var sprite_sampler: sampler;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) alpha: f32,
    @location(1) radius: f32,
    // The pixel before the matrix, where the clip and the shape are
    // measured (as the rects').
    @location(2) unturned: vec2<f32>,
    // The particle as a segment, tail to head, before the matrix: a dot when
    // they are one point.
    @location(3) @interpolate(flat) tail: vec2<f32>,
    @location(4) @interpolate(flat) head: vec2<f32>,
    @location(5) @interpolate(flat) color: vec4<f32>,
    @location(6) @interpolate(flat) trailing: f32,
    // A sprite's place in its picture, 0…1 inside it.
    @location(7) uv: vec2<f32>,
};

fn hidden() -> Varyings {
    var out: Varyings;
    out.position = vec4<f32>(2.0, 2.0, 0.0, 1.0);
    out.alpha = 0.0;
    return out;
}

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Varyings {
    var out: Varyings;
    let life = p.motion.y;
    let w = who(instance);
    if (!w.ok) {
        return hidden();
    }
    let age = p.motion.x - w.born;
    if (age < 0.0 || age > life) {
        return hidden();
    }
    let r1 = hash(w.seed);
    let r2 = hash(w.seed + 1.7);
    let r3 = hash(w.seed + 3.1);
    let t = age / life;
    let trail = max(p.life.y, 0.0);
    var head: vec2<f32>;
    var tail: vec2<f32>;
    if (p.sim.x > 0.5) {
        // Stepped: where the compute pass left it, the trail along its
        // velocity.
        let at = state[instance * 2u];
        if (state[instance * 2u + 1u].z < 0.5) {
            return hidden();
        }
        head = at.xy;
        tail = head - at.zw * min(trail, age);
    } else {
        head = place(w.origin, p.motion, p.shape, p.bend, r1, r2, age, life);
        // The trail: where it was that long ago, or since it was born.
        tail = head;
        if (trail > 0.0) {
            tail = place(w.origin, p.motion, p.shape, p.bend, r1, r2, max(age - trail, 0.0), life);
        }
    }
    var color = p.color;
    if (p.color_end.a >= 0.0) {
        color = mix(p.color, p.color_end, t);
    }
    // In over the first fifth, out over the last half.
    out.alpha = clamp(t * 5.0, 0.0, 1.0) * clamp((1.0 - t) * 2.0, 0.0, 1.0) * color.a;
    let size = mix(p.shape.z, p.life.x, t) * (0.7 + 0.6 * r3);
    if (p.look.x > 0.5) {
        // A sprite: an upright square round the head, wide enough for the
        // picture turned any way; turning at its own rate from its own
        // angle.
        let r4 = hash(w.seed + 4.7);
        let angle = r4 * 6.2831853 + (r4 - 0.5) * 2.0 * p.look.y * age;
        let reach = size * 0.75;
        let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u)) * 2.0 - 1.0;
        let pixel = head + corner * reach;
        let turned = vec4<f32>(pixel, 0.0, 1.0) * p.transform;
        out.position = vec4<f32>(((turned.xy / turned.w) / p.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0)) * turned.w, 0.0, turned.w);
        let c = cos(-angle);
        let s = sin(-angle);
        let local = corner * reach;
        out.uv = vec2<f32>(local.x * c - local.y * s, local.x * s + local.y * c) / size + vec2<f32>(0.5);
        out.unturned = pixel;
        out.color = color;
        out.trailing = 0.0;
        return out;
    }
    out.uv = vec2<f32>(-1.0);
    // A quad round the segment, a pixel past its edge for the antialiasing.
    let span = head - tail;
    let length_ = length(span);
    var along = vec2<f32>(1.0, 0.0);
    if (length_ > 1e-3) {
        along = span / length_;
    }
    let across = vec2<f32>(-along.y, along.x);
    let half = size * 0.5 + 1.0;
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u)) * 2.0 - 1.0;
    let pixel = (head + tail) * 0.5 + along * corner.x * (length_ * 0.5 + half) + across * corner.y * half;
    // Depth, as the rects have it: w carries the perspective.
    let turned = vec4<f32>(pixel, 0.0, 1.0) * p.transform;
    out.position = vec4<f32>(((turned.xy / turned.w) / p.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0)) * turned.w, 0.0, turned.w);
    out.unturned = pixel;
    out.tail = tail;
    out.head = head;
    out.radius = size * 0.5;
    out.color = color;
    out.trailing = select(0.0, 1.0, trail > 0.0);
    return out;
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    let q = in.unturned;
    if (q.x < p.clip.x || q.y < p.clip.y || q.x >= p.clip.z || q.y >= p.clip.w) { discard; }
    if (p.look.x > 0.5) {
        // Its picture, premultiplied, tinted by its colour; nothing past
        // its edges.
        let inside = all(in.uv >= vec2<f32>(0.0)) && all(in.uv <= vec2<f32>(1.0));
        let texel = textureSampleLevel(sprite, sprite_sampler, clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
        let a = select(0.0, in.alpha, inside);
        return vec4<f32>(texel.rgb * in.color.rgb, texel.a) * a;
    }
    // Along the segment, 0 at the tail and 1 at the head: a trail narrows
    // and fades toward its tail. A dot (no trail yet, or none at all) is
    // its head throughout.
    let span = in.head - in.tail;
    let s = select(1.0, clamp(dot(q - in.tail, span) / max(dot(span, span), 1e-6), 0.0, 1.0), dot(span, span) > 1e-4);
    let d = length(q - (in.tail + span * s));
    var radius = in.radius;
    var fade = 1.0;
    if (in.trailing > 0.5) {
        radius = in.radius * (0.35 + 0.65 * s);
        fade = s;
    }
    let cover = clamp(radius - d + 0.5, 0.0, 1.0) * in.alpha * fade;
    return vec4<f32>(in.color.rgb * cover, cover);
}
"#;

/// Stepping a simulated emitter's particles to the frame's time, at most a
/// hundred and twentieth of a second at a time: gravity, the pull of each
/// force, then the walls.
const STEP: &str = r#"
@group(0) @binding(2) var<storage, read_write> state: array<vec4<f32>>;

fn pulled(at: vec2<f32>) -> vec2<f32> {
    var a = vec2<f32>(0.0, p.shape.y);
    let n = u32(p.sim.z);
    for (var i = 0u; i < n; i = i + 1u) {
        let f = p.forces[i];
        let d = f.xy - at;
        let dist = length(d);
        if (dist > 1e-3) {
            // Whole near, nothing at its reach; eased in the last few
            // pixels, so a particle over the point is not flung.
            let fall = 1.0 - smoothstep(0.0, max(f.w, 1.0), dist);
            let soft = clamp(dist / 12.0, 0.0, 1.0);
            a = a + d / dist * f.z * fall * soft;
        }
    }
    return a;
}

fn wall(q: vec2<f32>, i: u32) -> f32 {
    let r = p.walls[i * 2u];
    let c = (r.xy + r.zw) * 0.5;
    let h = (r.zw - r.xy) * 0.5;
    let k = min(p.walls[i * 2u + 1u].x, min(h.x, h.y));
    let e = abs(q - c) - h + vec2<f32>(k);
    return length(max(e, vec2<f32>(0.0))) + min(max(e.x, e.y), 0.0) - k;
}

@compute @workgroup_size(64)
fn simulate(@builtin(global_invocation_id) id: vec3<u32>) {
    let instance = id.x;
    if (instance * 2u + 1u >= arrayLength(&state)) {
        return;
    }
    let w = who(instance);
    if (!w.ok) {
        return;
    }
    let life = p.motion.y;
    var s = state[instance * 2u];
    var m = state[instance * 2u + 1u];
    // Newly born: thrown as an unsimulated one is.
    if (m.z < 0.5 || m.x != w.born) {
        s = vec4<f32>(w.origin.xy, thrown(p.motion, p.shape, hash(w.seed), hash(w.seed + 1.7)));
        m = vec4<f32>(w.born, w.born, 1.0, 0.0);
    }
    let until = min(p.motion.x, w.born + life);
    let radius = p.shape.z * 0.5;
    let bounce = clamp(p.life.w, 0.0, 1.0);
    let walls = u32(p.sim.w);
    var t = m.y;
    var steps = 0u;
    loop {
        if (t >= until - 1e-5 || steps >= 48u) {
            break;
        }
        let dt = min(until - t, 1.0 / 120.0);
        var v = s.zw + pulled(s.xy) * dt;
        var at = s.xy + v * dt;
        for (var j = 0u; j < walls; j = j + 1u) {
            let d = wall(at, j) - radius;
            if (d < 0.0) {
                // Out along the wall's normal, the velocity into it turned
                // back as much as `bounce` keeps, the rest slowed a little.
                let e = 0.5;
                let normal = normalize(vec2<f32>(
                    wall(at + vec2<f32>(e, 0.0), j) - wall(at - vec2<f32>(e, 0.0), j),
                    wall(at + vec2<f32>(0.0, e), j) - wall(at - vec2<f32>(0.0, e), j),
                ) + vec2<f32>(1e-6, 0.0));
                at = at - normal * d;
                let into = dot(v, normal);
                if (into < 0.0) {
                    let slide = (v - normal * into) * 0.98;
                    v = slide - normal * into * bounce;
                }
            }
        }
        s = vec4<f32>(at, v);
        t = t + dt;
        steps = steps + 1u;
    }
    // Too far behind (a surface that slept): caught up where it is.
    if (t < until - 1e-5) {
        t = until;
    }
    m.y = t;
    state[instance * 2u] = s;
    state[instance * 2u + 1u] = m;
}
"#;

/// The drawing module, as compiled.
pub(crate) fn draw_source() -> String {
    format!("{PARAMS}{DRAW}{COMMON}")
}

/// The stepping module, as compiled.
pub(crate) fn step_source() -> String {
    format!("{PARAMS}{STEP}{COMMON}")
}

struct Slot {
    params: wgpu::Buffer,
    births: wgpu::Buffer,
    /// Each particle's position and velocity, then its birth as last seen,
    /// how far it has been stepped and whether it is set: two rows each.
    state: wgpu::Buffer,
    /// Births and instances the buffers hold.
    capacity: (usize, usize),
    draw_group: wgpu::BindGroup,
    step_group: wgpu::BindGroup,
    generation: u64,
    count: u32,
    simulated: bool,
    /// Bumped when its buffers are made again; a sub-emitter's groups name
    /// the parent's state as it was then.
    made: u64,
    parent_made: Option<(u64, u64)>,
}

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    stepper: wgpu::ComputePipeline,
    draw_layout: wgpu::BindGroupLayout,
    step_layout: wgpu::BindGroupLayout,
    /// What a group with no parent binds in its parent's place.
    nothing: wgpu::Buffer,
    slots: HashMap<u64, Slot>,
    /// Each draw: the emitter's key and its sprite's.
    draws: Vec<u64>,
    sprites: Vec<Option<u64>>,
    made: u64,
}

impl Pipeline {
    /// `textures` is the images' layout: a sprite is bound as an image is.
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat, textures: &wgpu::BindGroupLayout) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particles"),
            source: wgpu::ShaderSource::Wgsl(draw_source().into()),
        });
        let step_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particles step"),
            source: wgpu::ShaderSource::Wgsl(step_source().into()),
        });
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let drawing = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles"),
            entries: &[
                entry(0, drawing, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::ShaderStages::VERTEX, read),
                entry(2, wgpu::ShaderStages::VERTEX, read),
                entry(3, wgpu::ShaderStages::VERTEX, read),
            ],
        });
        let computing = wgpu::ShaderStages::COMPUTE;
        let step_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles step"),
            entries: &[
                entry(0, computing, wgpu::BufferBindingType::Uniform),
                entry(1, computing, read),
                entry(2, computing, wgpu::BufferBindingType::Storage { read_only: false }),
                entry(3, computing, read),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particles"),
            bind_group_layouts: &[Some(&draw_layout), Some(textures)],
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
        let step_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particles step"),
            bind_group_layouts: &[Some(&step_layout)],
            ..Default::default()
        });
        let stepper = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("particles step"),
            layout: Some(&step_pipeline_layout),
            module: &step_module,
            entry_point: Some("simulate"),
            compilation_options: Default::default(),
            cache: None,
        });
        let nothing = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles: no parent"),
            size: 32,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Pipeline { pipeline, stepper, draw_layout, step_layout, nothing, slots: HashMap::new(), draws: Vec::new(), sprites: Vec::new(), made: 0 }
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2], emitters: &[Emitter]) {
        self.draws.clear();
        self.sprites.clear();
        for e in emitters {
            let births = e.births.len().max(1);
            let instances = (e.instances() as usize).max(1);
            let rebuild = self.slots.get(&e.key).is_none_or(|s| s.capacity.0 < births || s.capacity.1 < instances);
            // A sub-emitter reads its parent's state, as it is now.
            let parent = e.parent.and_then(|k| self.slots.get(&k).map(|s| (k, s.made, s.state.clone())));
            let parent_made = parent.as_ref().map(|(k, made, _)| (*k, *made));
            let regroup = rebuild || self.slots.get(&e.key).is_some_and(|s| s.parent_made != parent_made);
            if rebuild {
                let params = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle params"),
                    size: std::mem::size_of::<EmitterParams>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let births_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle births"),
                    size: (births * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                // Zeroed: no particle set.
                let state = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particle state"),
                    size: (instances * 32) as u64,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                });
                self.made += 1;
                let (draw_group, step_group) = self.groups(device, &params, &births_buffer, &state, parent.as_ref().map(|p| &p.2));
                self.slots.insert(
                    e.key,
                    Slot {
                        params,
                        births: births_buffer,
                        state,
                        capacity: (births, instances),
                        draw_group,
                        step_group,
                        generation: u64::MAX,
                        count: 0,
                        simulated: false,
                        made: self.made,
                        parent_made,
                    },
                );
            } else if regroup {
                let slot = &self.slots[&e.key];
                let (draw_group, step_group) = self.groups(device, &slot.params, &slot.births, &slot.state, parent.as_ref().map(|p| &p.2));
                let slot = self.slots.get_mut(&e.key).expect("there");
                (slot.draw_group, slot.step_group, slot.parent_made) = (draw_group, step_group, parent_made);
            }
            let slot = self.slots.get_mut(&e.key).expect("made above");
            let mut params = e.params;
            params.viewport = [viewport[0], viewport[1], 0.0, 0.0];
            // A parent read from a buffer that is not there yet: worked out
            // from its motion instead.
            if parent.is_none() {
                params.sim[1] = 0.0;
            }
            queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&params));
            // The births travel only when one was added.
            if slot.generation != e.generation && !e.births.is_empty() {
                queue.write_buffer(&slot.births, 0, bytemuck::cast_slice(&e.births));
                slot.generation = e.generation;
            }
            slot.count = e.instances();
            slot.simulated = params.sim[0] > 0.5;
            self.draws.push(e.key);
            self.sprites.push(e.sprite.as_ref().map(|s| s.key));
        }
    }

    fn groups(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        births: &wgpu::Buffer,
        state: &wgpu::Buffer,
        parent: Option<&wgpu::Buffer>,
    ) -> (wgpu::BindGroup, wgpu::BindGroup) {
        let parent = parent.unwrap_or(&self.nothing);
        let make = |layout: &wgpu::BindGroupLayout, label| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: births.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: state.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: parent.as_entire_binding() },
                ],
            })
        };
        (make(&self.draw_layout, "particles"), make(&self.step_layout, "particles step"))
    }

    /// Steps the simulated emitters just uploaded to their time, in their
    /// order (a parent before its sub-emitters). A frame drawn again at the
    /// same time steps nothing.
    pub(crate) fn simulate(&self, encoder: &mut wgpu::CommandEncoder) {
        let stepped: Vec<&Slot> = self.draws.iter().map(|k| &self.slots[k]).filter(|s| s.simulated && s.count > 0).collect();
        if stepped.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("particles step"), timestamp_writes: None });
        pass.set_pipeline(&self.stepper);
        for slot in stepped {
            pass.set_bind_group(0, &slot.step_group, &[]);
            pass.dispatch_workgroups(slot.count.div_ceil(64), 1, 1);
        }
    }

    /// `images` holds the sprites' textures (`ensure`d before `upload`).
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, only: std::ops::Range<usize>, images: &crate::image::Pipeline) {
        if self.draws.is_empty() || only.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for i in only.start.min(self.draws.len())..only.end.min(self.draws.len()) {
            let slot = &self.slots[&self.draws[i]];
            if slot.count > 0 {
                pass.set_bind_group(0, &slot.draw_group, &[]);
                pass.set_bind_group(1, images.bound(self.sprites[i]), &[]);
                pass.draw(0..4, 0..slot.count);
            }
        }
    }
}
