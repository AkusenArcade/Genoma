//! Simulations a shader node runs (`compute`): WGSL of the interface's own,
//! stepped on the GPU between frames, its state kept there.
//!
//! The file holds either function, or both:
//!
//! ```wgsl
//! fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32>
//! fn agent(i: i32, me: Agent) -> Agent
//! ```
//!
//! `step` is a grid's: every cell's four numbers, worked out from what the
//! grid held a step before (`g_cell(c)` reads any cell, wrapping at the
//! edges): reaction-diffusion, waves, a fluid. `agent` is an agent's: where
//! it is, its velocity and four numbers of its own, from what every agent
//! was a step before (`g_agent(j)`): boids. Both read `sim.frame` (0 the
//! first step, to set things up), `sim.dt`, `sim.grid`, `sim.agents`,
//! `shader.size`, `shader.time`, `shader.pointer`, `value(i)` and the `g_`
//! library.
//!
//! The node's `shade(p)` then shows it: `g_state(p)` is the grid at a point
//! (logical units from the node's corner, between cells blended),
//! `g_cell(c)` one cell, `g_agent(j)` an agent. Agents are also drawn by
//! Genoma, as dots stretched along their velocity, when the node gives
//! them a size.
//!
//! Two of each are kept, the one read and the one written, swapped after
//! every step. A grid or a number of agents that changes starts again from
//! nothing.

use std::{collections::HashMap, sync::Arc};

use bytemuck::{Pod, Zeroable};

/// One node's simulation, as the main thread describes it.
#[derive(Clone)]
pub struct Sim {
    /// The node's own, stable across frames: its state is kept by it.
    pub key: u64,
    /// The interface's WGSL, and its key (changes with it).
    pub source: Arc<String>,
    pub source_key: u64,
    /// Cells across and down; 0 for no grid.
    pub grid: [u32; 2],
    pub agents: u32,
    /// Steps taken since it started, as the clock says: the renderer takes
    /// those it has not taken yet, a few at most a frame.
    pub steps: u64,
    /// Seconds a step stands for.
    pub dt: f32,
    /// How large Genoma draws each agent, physical pixels (0 not drawn),
    /// and in what colour (straight RGBA).
    pub agent_size: f32,
    pub agent_color: [f32; 4],
}

/// Steps taken at most in one frame: a surface that slept does not catch
/// up all at once.
const MOST: u64 = 8;

/// The step's own numbers are bound at offsets of this many bytes.
const STEP_STRIDE: u64 = 256;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Step {
    grid: [i32; 2],
    agents: i32,
    frame: u32,
    dt: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Shared {
    size: [f32; 2],
    time: f32,
    scale: f32,
    pointer: [f32; 4],
    values: [[f32; 4]; crate::shader::VALUES / 4],
}

/// Before the interface's WGSL in a compute module.
const PRELUDE: &str = r#"
struct GenomaShared {
    size: vec2<f32>,
    time: f32,
    scale: f32,
    pointer: vec4<f32>,
    values: array<vec4<f32>, 256>,
};
struct GenomaStep {
    grid: vec2<i32>,
    agents: i32,
    frame: u32,
    dt: f32,
};
@group(0) @binding(0) var<uniform> shader: GenomaShared;
@group(0) @binding(1) var<uniform> sim: GenomaStep;
@group(1) @binding(0) var genoma_before: texture_2d<f32>;
@group(1) @binding(1) var genoma_after: texture_storage_2d<rgba32float, write>;
@group(1) @binding(2) var<storage, read> genoma_agents_before: array<vec4<f32>>;
@group(1) @binding(3) var<storage, read_write> genoma_agents_after: array<vec4<f32>>;

struct Agent {
    at: vec2<f32>,
    velocity: vec2<f32>,
    data: vec4<f32>,
};

fn value(i: i32) -> f32 {
    if (i < 0 || i >= 1024) { return 0.0; }
    return shader.values[i / 4][i % 4];
}

fn g_grid() -> vec2<i32> {
    return sim.grid;
}

// A cell as the grid held it a step before, wrapping round the edges.
fn g_cell(c: vec2<i32>) -> vec4<f32> {
    let n = max(sim.grid, vec2<i32>(1));
    return textureLoad(genoma_before, ((c % n) + n) % n, 0);
}

// The grid at `p`, logical units from the node's corner, blended between
// the four nearest cells.
fn g_state(p: vec2<f32>) -> vec4<f32> {
    let f = p / max(shader.size, vec2<f32>(1.0)) * vec2<f32>(max(sim.grid, vec2<i32>(1))) - 0.5;
    let i = vec2<i32>(floor(f));
    let t = fract(f);
    let top = mix(g_cell(i), g_cell(i + vec2<i32>(1, 0)), t.x);
    let bottom = mix(g_cell(i + vec2<i32>(0, 1)), g_cell(i + vec2<i32>(1, 1)), t.x);
    return mix(top, bottom, t.y);
}

fn g_agents() -> i32 {
    return sim.agents;
}

// An agent as it was a step before.
fn g_agent(i: i32) -> Agent {
    let j = u32(clamp(i, 0, max(sim.agents - 1, 0))) * 2u;
    let a = genoma_agents_before[j];
    return Agent(a.xy, a.zw, genoma_agents_before[j + 1u]);
}
"#;

const STEP_ENTRY: &str = r#"
@compute @workgroup_size(8, 8)
fn genoma_step(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = vec2<i32>(id.xy);
    if (c.x >= sim.grid.x || c.y >= sim.grid.y) {
        return;
    }
    textureStore(genoma_after, c, step(c, textureLoad(genoma_before, c, 0)));
}
"#;

const MOVE_ENTRY: &str = r#"
@compute @workgroup_size(64)
fn genoma_move(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    if (i >= sim.agents) {
        return;
    }
    let a = agent(i, g_agent(i));
    genoma_agents_after[u32(i) * 2u] = vec4<f32>(a.at, a.velocity);
    genoma_agents_after[u32(i) * 2u + 1u] = a.data;
}
"#;

/// Whether the interface's WGSL has a grid's step, an agent's move.
fn has(source: &str) -> (bool, bool) {
    (source.contains("fn step("), source.contains("fn agent("))
}

/// The whole compute module: the prelude, the library, the interface's
/// WGSL and an entry point for each function it has.
pub(crate) fn compute_source(source: &str) -> String {
    let (step, moves) = has(source);
    let mut text = format!("{PRELUDE}\n{}\n{source}\n", crate::shader::LIBRARY);
    if step {
        text.push_str(STEP_ENTRY);
    }
    if moves {
        text.push_str(MOVE_ENTRY);
    }
    text
}

/// A simulation's WGSL checked as the device will compile it: what is
/// wrong and where, its line counted in `source` itself.
pub fn check(source: &str) -> Result<(), String> {
    let (step, moves) = has(source);
    if !step && !moves {
        return Err("it has neither fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32> nor fn agent(i: i32, me: Agent) -> Agent".into());
    }
    crate::shader::check_module(&compute_source(source), source)
}

/// The simulations Genoma ships, by the name a `compute` gives them after
/// `genoma:`.
const BUILT_IN: &[(&str, &str)] = &[
    ("reaction-diffusion", include_str!("shaders/reaction-diffusion.wgsl")),
    ("waves", include_str!("shaders/waves.wgsl")),
    ("boids", include_str!("shaders/boids.wgsl")),
];

pub fn built_in(name: &str) -> Option<&'static str> {
    BUILT_IN.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// What a node's simulation keeps between frames.
struct Run {
    grid: [u32; 2],
    agents: u32,
    source_key: u64,
    /// Which of the two holds the latest.
    current: usize,
    /// Steps taken.
    done: u64,
    shared: wgpu::Buffer,
    steps: wgpu::Buffer,
    /// The compute groups: the numbers, then each way round (reading 0
    /// writing 1, and back).
    numbers: wgpu::BindGroup,
    turns: [wgpu::BindGroup; 2],
    /// What `shade` and the agents' drawing read, as each holds the latest.
    shown: [wgpu::BindGroup; 2],
    /// Steps to take this frame, from which frame number.
    pending: (u64, u64),
    used: u64,
}

pub(crate) struct Sims {
    /// What a fragment reads: a grid and the agents.
    pub(crate) shown_layout: wgpu::BindGroupLayout,
    /// Bound for a shader with no simulation.
    pub(crate) nothing: wgpu::BindGroup,
    numbers_layout: wgpu::BindGroupLayout,
    turn_layout: wgpu::BindGroupLayout,
    compute_layout: wgpu::PipelineLayout,
    /// Each source's pipelines: its step's and its move's, none when it
    /// does not compile.
    pipelines: HashMap<u64, (Option<wgpu::ComputePipeline>, Option<wgpu::ComputePipeline>)>,
    runs: HashMap<u64, Run>,
    uploads: u64,
}

impl Sims {
    pub(crate) fn new(device: &wgpu::Device) -> Sims {
        let shown_stages = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let grid_entry = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let buffer_entry = |binding, visibility, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let shown_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simulation shown"),
            entries: &[grid_entry(0, shown_stages), buffer_entry(1, shown_stages, true)],
        });
        let compute = wgpu::ShaderStages::COMPUTE;
        let numbers_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simulation numbers"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: compute,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: compute,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Step>() as u64),
                    },
                    count: None,
                },
            ],
        });
        let turn_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simulation turn"),
            entries: &[
                grid_entry(0, compute),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: compute,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                buffer_entry(2, compute, true),
                buffer_entry(3, compute, false),
            ],
        });
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("simulation"),
            bind_group_layouts: &[Some(&numbers_layout), Some(&turn_layout)],
            ..Default::default()
        });
        let (texture, buffer) = Self::state(device, [1, 1], 1);
        let nothing = Self::shown_group(device, &shown_layout, &texture, &buffer);
        Sims { shown_layout, nothing, numbers_layout, turn_layout, compute_layout, pipelines: HashMap::new(), runs: HashMap::new(), uploads: 0 }
    }

    /// One of a run's two: a grid (at least a cell) and room for the
    /// agents (at least one), zeroed.
    fn state(device: &wgpu::Device, grid: [u32; 2], agents: u32) -> (wgpu::Texture, wgpu::Buffer) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("simulation grid"),
            size: wgpu::Extent3d { width: grid[0].max(1), height: grid[1].max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("simulation agents"),
            size: u64::from(agents.max(1)) * 32,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        (texture, buffer)
    }

    fn shown_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, texture: &wgpu::Texture, buffer: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("simulation shown"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&texture.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
            ],
        })
    }

    fn compile(&self, device: &wgpu::Device, source: &str) -> (Option<wgpu::ComputePipeline>, Option<wgpu::ComputePipeline>) {
        let (step, moves) = has(source);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("g.shader compute"),
            source: wgpu::ShaderSource::Wgsl(compute_source(source).into()),
        });
        let entry = |name: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("g.shader compute"),
                layout: Some(&self.compute_layout),
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipelines = (step.then(|| entry("genoma_step")), moves.then(|| entry("genoma_move")));
        match pollster::block_on(scope.pop()) {
            None => pipelines,
            Some(e) => {
                log::error!("g.shader compute does not compile: {e}");
                (None, None)
            }
        }
    }

    /// The node's simulation made ready for this frame: its state (made
    /// again when its grid or its agents changed), its numbers, and the
    /// steps it is owed. `shared` is what its shader is given.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, s: &Sim, size: [f32; 2], time: f32, scale: f32, pointer: [f32; 4], values: &[f32]) {
        self.uploads += 1;
        if !self.pipelines.contains_key(&s.source_key) {
            let compiled = self.compile(device, &s.source);
            self.pipelines.insert(s.source_key, compiled);
        }
        let fresh = self.runs.get(&s.key).is_none_or(|r| r.grid != s.grid || r.agents != s.agents || r.source_key != s.source_key);
        if fresh {
            let a = Self::state(device, s.grid, s.agents);
            let b = Self::state(device, s.grid, s.agents);
            let shared = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("simulation shared"),
                size: std::mem::size_of::<Shared>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let steps = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("simulation steps"),
                size: MOST * STEP_STRIDE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let numbers = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("simulation numbers"),
                layout: &self.numbers_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: shared.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &steps,
                            offset: 0,
                            size: wgpu::BufferSize::new(std::mem::size_of::<Step>() as u64),
                        }),
                    },
                ],
            });
            let turn = |from: &(wgpu::Texture, wgpu::Buffer), to: &(wgpu::Texture, wgpu::Buffer)| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("simulation turn"),
                    layout: &self.turn_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&from.0.create_view(&Default::default())) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&to.0.create_view(&Default::default())) },
                        wgpu::BindGroupEntry { binding: 2, resource: from.1.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: to.1.as_entire_binding() },
                    ],
                })
            };
            let turns = [turn(&a, &b), turn(&b, &a)];
            let shown = [Self::shown_group(device, &self.shown_layout, &a.0, &a.1), Self::shown_group(device, &self.shown_layout, &b.0, &b.1)];
            self.runs.insert(
                s.key,
                Run { grid: s.grid, agents: s.agents, source_key: s.source_key, current: 0, done: 0, shared, steps, numbers, turns, shown, pending: (0, 0), used: 0 },
            );
        }
        let run = self.runs.get_mut(&s.key).expect("made above");
        run.used = self.uploads;
        let mut shared = Shared { size, time, scale, pointer, values: [[0.0; 4]; crate::shader::VALUES / 4] };
        for (j, v) in values.iter().take(crate::shader::VALUES).enumerate() {
            shared.values[j / 4][j % 4] = *v;
        }
        queue.write_buffer(&run.shared, 0, bytemuck::bytes_of(&shared));
        // A clock gone back (the node made again) starts the count afresh.
        if s.steps < run.done {
            run.done = s.steps;
        }
        let owed = (s.steps - run.done).min(MOST);
        run.pending = (owed, run.done);
        if owed > 0 {
            let mut bytes = vec![0u8; (owed * STEP_STRIDE) as usize];
            for i in 0..owed {
                let step = Step {
                    grid: [s.grid[0] as i32, s.grid[1] as i32],
                    agents: s.agents as i32,
                    frame: (run.done + i) as u32,
                    dt: s.dt,
                    _pad: [0.0; 3],
                };
                let at = (i * STEP_STRIDE) as usize;
                bytes[at..at + std::mem::size_of::<Step>()].copy_from_slice(bytemuck::bytes_of(&step));
            }
            queue.write_buffer(&run.steps, 0, &bytes);
            // A surface that slept is not caught up: what it missed is let go.
            run.done = s.steps;
        }
        if self.uploads.is_multiple_of(256) {
            let now = self.uploads;
            self.runs.retain(|_, r| now - r.used < 4096);
        }
    }

    /// Takes the steps each simulation uploaded this frame is owed.
    pub(crate) fn simulate(&mut self, encoder: &mut wgpu::CommandEncoder, keys: impl Iterator<Item = u64>) {
        // Those owed steps and able to take them.
        let due: Vec<u64> = keys
            .filter(|k| {
                self.runs.get(k).is_some_and(|r| {
                    r.pending.0 > 0 && self.pipelines.get(&r.source_key).is_some_and(|(s, m)| s.is_some() || m.is_some())
                })
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("simulation"), timestamp_writes: None });
        for key in due {
            let Some(run) = self.runs.get_mut(&key) else { continue };
            let owed = std::mem::take(&mut run.pending.0);
            let Some((step, moves)) = self.pipelines.get(&run.source_key) else { continue };
            for i in 0..owed {
                pass.set_bind_group(0, &run.numbers, &[(i * STEP_STRIDE) as u32]);
                pass.set_bind_group(1, &run.turns[run.current], &[]);
                if let Some(moves) = moves
                    && run.agents > 0
                {
                    pass.set_pipeline(moves);
                    pass.dispatch_workgroups(run.agents.div_ceil(64), 1, 1);
                }
                if let Some(step) = step
                    && run.grid[0] > 0
                    && run.grid[1] > 0
                {
                    pass.set_pipeline(step);
                    pass.dispatch_workgroups(run.grid[0].div_ceil(8), run.grid[1].div_ceil(8), 1);
                }
                run.current ^= 1;
            }
        }
    }

    /// What a node's shader reads of its simulation now.
    pub(crate) fn shown(&self, key: Option<u64>) -> &wgpu::BindGroup {
        key.and_then(|k| self.runs.get(&k)).map_or(&self.nothing, |r| &r.shown[r.current])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_built_in_simulation_steps_and_shades() {
        for (name, source) in super::BUILT_IN {
            super::check(source).unwrap_or_else(|e| panic!("{name}, stepped: {e}"));
            crate::shader::check(source).unwrap_or_else(|e| panic!("{name}, shaded: {e}"));
        }
    }

    #[test]
    fn a_mistake_is_told_at_its_own_line() {
        let wrong = "fn step(cell: vec2<i32>, me: vec4<f32>) -> vec4<f32> {\n    return nope;\n}\n";
        let said = super::check(wrong).unwrap_err();
        assert!(said.starts_with("line 2"), "{said}");
        assert!(super::check("fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }").is_err(), "neither step nor move");
    }

    #[test]
    fn a_shade_reads_the_grid_and_the_agents() {
        let both = "fn shade(p: vec2<f32>) -> vec4<f32> { return g_state(p) + g_cell(vec2<i32>(1)) + vec4<f32>(g_agent(0).at, f32(g_agents()), 1.0); }";
        crate::shader::check(both).unwrap();
    }
}
