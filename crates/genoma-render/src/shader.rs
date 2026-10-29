//! Shader nodes (`g.shader`): a rectangle whose every pixel is a function
//! the interface writes in WGSL, fed by up to 128 numbers it binds. What a
//! distance field of rects cannot say, a field of its own can: Bioma's audio
//! band melts its drops together, and shakes a membrane.
//!
//! The interface's file holds one function,
//!
//! ```wgsl
//! fn shade(p: vec2<f32>) -> vec4<f32>
//! ```
//!
//! `p` in logical units from the node's top-left corner; the result
//! premultiplied, sRGB-encoded as every other colour. The prelude gives it
//! `shader.size` (logical), `shader.scale` (physical pixels per logical
//! unit), `shader.time` (seconds its clock has run) and `value(i)`, the
//! numbers bound, eased as the node asks. Derivatives (`dpdx`) are per
//! physical pixel. The node's opacity and clip are applied after it.
//!
//! A file that does not compile draws nothing and says why, once.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

/// One shader node, as the main thread describes it.
pub struct Shader {
    /// The interface's WGSL.
    pub source: std::sync::Arc<String>,
    /// Changes whenever `source` does: pipelines are kept by it.
    pub key: u64,
    /// Top-left corner, physical pixels.
    pub origin: [f32; 2],
    /// Logical units.
    pub size: [f32; 2],
    pub scale: f32,
    pub time: f32,
    pub alpha: f32,
    /// x0, y0, x1, y1, physical pixels.
    pub clip: [f32; 4],
    /// At most [`VALUES`]; the rest read 0.
    pub values: Vec<f32>,
}

pub const VALUES: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    origin: [f32; 2],
    size: [f32; 2],
    clip: [f32; 4],
    viewport: [f32; 2],
    scale: f32,
    time: f32,
    alpha: f32,
    _pad: [f32; 3],
    values: [[f32; 4]; VALUES / 4],
}

/// Uniforms are bound at offsets of this many bytes, the largest alignment
/// any device asks for.
const STRIDE: u64 = 768;

const PRELUDE: &str = r#"
struct Shader {
    origin: vec2<f32>,
    size: vec2<f32>,
    clip: vec4<f32>,
    viewport: vec2<f32>,
    scale: f32,
    time: f32,
    alpha: f32,
    values: array<vec4<f32>, 32>,
};
@group(0) @binding(0) var<uniform> shader: Shader;

fn value(i: i32) -> f32 {
    if (i < 0 || i >= 128) { return 0.0; }
    return shader.values[i / 4][i % 4];
}

@vertex
fn genoma_vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    let pixel = shader.origin + corner * shader.size * shader.scale;
    return vec4<f32>(pixel / shader.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn genoma_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let q = position.xy;
    let c = shader.clip;
    // Shaded before the clip is tested: derivatives need every pixel of a quad.
    let colour = shade((q - shader.origin) / shader.scale);
    if (q.x < c.x || q.y < c.y || q.x >= c.z || q.y >= c.w) {
        discard;
    }
    return colour * shader.alpha;
}
"#;

pub(crate) struct Pipeline {
    format: wgpu::TextureFormat,
    layout: wgpu::PipelineLayout,
    group_layout: wgpu::BindGroupLayout,
    /// Each source's pipeline, or none when it would not compile.
    pipelines: HashMap<u64, (Option<wgpu::RenderPipeline>, u64)>,
    uploads: u64,
    uniforms: wgpu::Buffer,
    group: wgpu::BindGroup,
    capacity: usize,
    draws: Vec<u64>,
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shader"),
            bind_group_layouts: &[Some(&group_layout)],
            ..Default::default()
        });
        let capacity = 4;
        let (uniforms, group) = Self::buffer(device, &group_layout, capacity);
        Pipeline {
            format,
            layout,
            group_layout,
            pipelines: HashMap::new(),
            uploads: 0,
            uniforms,
            group,
            capacity,
            draws: Vec::new(),
        }
    }

    fn buffer(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, capacity: usize) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shader uniforms"),
            size: capacity as u64 * STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                }),
            }],
        });
        (buffer, group)
    }

    /// The source's pipeline, compiled under an error scope: a mistake in the
    /// interface's WGSL is reported, never a panic of the device.
    fn compile(&self, device: &wgpu::Device, source: &str) -> Option<wgpu::RenderPipeline> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("g.shader"),
            source: wgpu::ShaderSource::Wgsl(format!("{PRELUDE}\n{source}").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("g.shader"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("genoma_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("genoma_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        match pollster::block_on(scope.pop()) {
            None => Some(pipeline),
            Some(e) => {
                log::error!("g.shader does not compile: {e}");
                None
            }
        }
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2], shaders: &[crate::Shader]) {
        self.draws.clear();
        self.uploads += 1;
        if self.uploads % 256 == 0 {
            let now = self.uploads;
            self.pipelines.retain(|_, (_, used)| now - *used < 1024);
        }
        if shaders.is_empty() {
            return;
        }
        if shaders.len() > self.capacity {
            self.capacity = shaders.len().next_power_of_two();
            (self.uniforms, self.group) = Self::buffer(device, &self.group_layout, self.capacity);
        }
        let mut bytes = vec![0u8; shaders.len() * STRIDE as usize];
        for (i, s) in shaders.iter().enumerate() {
            if !self.pipelines.contains_key(&s.key) {
                let pipeline = self.compile(device, &s.source);
                self.pipelines.insert(s.key, (pipeline, 0));
            }
            let entry = self.pipelines.get_mut(&s.key).expect("inserted above");
            entry.1 = self.uploads;
            let mut values = [[0.0; 4]; VALUES / 4];
            for (j, v) in s.values.iter().take(VALUES).enumerate() {
                values[j / 4][j % 4] = *v;
            }
            let u = Uniforms {
                origin: s.origin,
                size: s.size,
                clip: s.clip,
                viewport,
                scale: s.scale,
                time: s.time,
                alpha: s.alpha,
                _pad: [0.0; 3],
                values,
            };
            let at = i * STRIDE as usize;
            bytes[at..at + std::mem::size_of::<Uniforms>()].copy_from_slice(bytemuck::bytes_of(&u));
            self.draws.push(s.key);
        }
        queue.write_buffer(&self.uniforms, 0, &bytes);
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, only: std::ops::Range<usize>) {
        for (i, key) in self.draws.iter().enumerate() {
            if !only.contains(&i) {
                continue;
            }
            let Some((Some(pipeline), _)) = self.pipelines.get(key) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.group, &[(i as u64 * STRIDE) as u32]);
            pass.draw(0..4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use wgpu::naga;

    /// The prelude with `source` after it, validated as the device would.
    pub(crate) fn validate(source: &str) -> Result<(), String> {
        let text = format!("{}\n{source}", super::PRELUDE);
        let module = naga::front::wgsl::parse_str(&text).map_err(|e| e.emit_to_string(&text))?;
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .map_err(|e| e.emit_to_string(&text))?;
        Ok(())
    }

    #[test]
    fn prelude_takes_a_shade() {
        validate("fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(value(0), p.x / shader.size.x, dpdx(p.y), 1.0); }").unwrap();
    }

    /// `GENOMA_SHADER=file.wgsl cargo test -p genoma-render shader_file`:
    /// an interface's shader checked without drawing it.
    #[test]
    fn shader_file() {
        let Ok(path) = std::env::var("GENOMA_SHADER") else { return };
        let source = std::fs::read_to_string(&path).unwrap();
        if let Err(e) = validate(&source) {
            panic!("{e}");
        }
    }
}
