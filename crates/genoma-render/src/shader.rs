//! Shader nodes (`g.shader`): a rectangle whose every pixel is a function
//! the interface writes in WGSL, fed by up to 1024 numbers it binds. What a
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
//! unit), `shader.time` (seconds its clock has run), `shader.pointer`
//! (x, y from the node's corner; z 1 over the surface; w 1 while a button
//! is held: a shader that reads it is drawn again as the pointer moves)
//! and `value(i)`, the numbers bound, eased as the node asks. Derivatives (`dpdx`) are per
//! physical pixel. The node's opacity and clip are applied after it.
//!
//! A file that does not compile draws nothing and says why, once.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

/// One shader node, as the main thread describes it.
#[derive(Clone)]
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
    /// Faded toward a clip's foot, as a rect's.
    pub fade: [f32; 4],
    /// At most [`VALUES`]; the rest read 0.
    pub values: Vec<f32>,
    /// Depth, as a rect's (see [`crate::Rect::transform`]).
    pub transform: [[f32; 4]; 4],
    /// The pointer from the node's corner, logical units; whether it is
    /// over the surface; whether a button is held.
    pub pointer: [f32; 4],
    /// What is under the node (`backdrop = true`): the drawn group's key,
    /// and where its texture lies from the node's corner, logical units:
    /// x, y, width, height.
    pub backdrop: Option<(u64, [f32; 4])>,
}

pub const VALUES: usize = 1024;

/// Noise, distance fields and colour, in every shader after the prelude
/// (`shaders/library.wgsl`), each name prefixed `g_`.
const LIBRARY: &str = include_str!("shaders/library.wgsl");

/// The effects Genoma ships, by the name a `src` gives them after
/// `genoma:`: `src = "genoma:ripple"`.
const BUILT_IN: &[(&str, &str)] = &[
    ("noise", include_str!("shaders/noise.wgsl")),
    ("ripple", include_str!("shaders/ripple.wgsl")),
    ("dissolve", include_str!("shaders/dissolve.wgsl")),
    ("vignette", include_str!("shaders/vignette.wgsl")),
    ("grain", include_str!("shaders/grain.wgsl")),
    ("glass", include_str!("shaders/glass.wgsl")),
];

/// A built-in effect's WGSL, by its name without `genoma:`.
pub fn built_in(name: &str) -> Option<&'static str> {
    BUILT_IN.iter().find(|(n, _)| *n == name).map(|(_, source)| *source)
}

/// The whole module a shader node compiles to: the prelude, the library,
/// then its own source.
fn module_source(source: &str) -> String {
    format!("{PRELUDE}\n{LIBRARY}\n{source}")
}

/// A shader's WGSL checked as the device will compile it, Genoma's prelude
/// and library before it: what is wrong, and where, its line counted in
/// `source` itself (`line 12: …`).
pub fn check(source: &str) -> Result<(), String> {
    use std::error::Error as _;
    use wgpu::naga;
    let text = module_source(source);
    let start = text.len() - source.len();
    let lines_before = text[..start].matches('\n').count() as u32;
    let place = |at: Option<naga::SourceLocation>| match at {
        Some(l) if l.offset as usize >= start => format!("line {}: ", l.line_number - lines_before),
        Some(_) => "in what Genoma puts before it: ".to_owned(),
        None => String::new(),
    };
    // Of the places an error names, the first in the source itself.
    let mine = |spans: Vec<naga::Span>| {
        spans.into_iter().find(|s| s.to_range().is_some_and(|r| r.start >= start)).map(|s| s.location(&text))
    };
    let module = naga::front::wgsl::parse_str(&text).map_err(|e| {
        let at = mine(e.labels().map(|(span, _)| span).collect()).or_else(|| e.location(&text));
        format!("{}{}", place(at), e.message())
    })?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| {
            let mut said = e.as_inner().to_string();
            let mut cause = e.as_inner().source();
            while let Some(c) = cause {
                said.push_str(": ");
                said.push_str(&c.to_string());
                cause = c.source();
            }
            let at = mine(e.spans().map(|(span, _)| *span).collect()).or_else(|| e.location(&text));
            format!("{}{said}", place(at))
        })?;
    Ok(())
}

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
    fade_end: f32,
    fade_span: f32,
    _pad: f32,
    pointer: [f32; 4],
    backdrop: [f32; 4],
    transform: [[f32; 4]; 4],
    values: [[f32; 4]; VALUES / 4],
}

/// Uniforms are bound at offsets of this many bytes: their size rounded up
/// to 256, the largest alignment any device asks for.
const STRIDE: u64 = ((std::mem::size_of::<Uniforms>() as u64 + 255) / 256) * 256;

const PRELUDE: &str = r#"
struct Shader {
    origin: vec2<f32>,
    size: vec2<f32>,
    clip: vec4<f32>,
    viewport: vec2<f32>,
    scale: f32,
    time: f32,
    alpha: f32,
    fade_end: f32,
    fade_span: f32,
    // The pointer: x, y from the node's corner, logical units; z 1 while it
    // is over the surface; w 1 while a button is held.
    pointer: vec4<f32>,
    // What is under it: x, y, width, height from the node's corner, logical
    // units; a width of 0 is nothing.
    backdrop: vec4<f32>,
    transform: mat4x4<f32>,
    values: array<vec4<f32>, 256>,
};
@group(0) @binding(0) var<uniform> shader: Shader;
@group(1) @binding(0) var genoma_backdrop: texture_2d<f32>;
@group(1) @binding(1) var genoma_backdrop_sampler: sampler;

// What the surface drew under the node at `p` (logical units from its
// corner), premultiplied; transparent where there is nothing, or for a
// shader without `backdrop = true`. Read anywhere: a glass bends it.
fn g_backdrop(p: vec2<f32>) -> vec4<f32> {
    let b = shader.backdrop;
    if (b.z <= 0.0) {
        return vec4<f32>(0.0);
    }
    let uv = (p - b.xy) / b.zw;
    return textureSampleLevel(genoma_backdrop, genoma_backdrop_sampler, uv, 0.0);
}

// The pixel before the node's matrix rides beside the position: the
// interface's `shade` is given it, and the clip and the fade are measured
// on it, so a tilted shader is drawn and cut as its turned parent is.
struct GenomaVaryings {
    @builtin(position) position: vec4<f32>,
    @location(0) unturned: vec2<f32>,
};

fn value(i: i32) -> f32 {
    if (i < 0 || i >= 1024) { return 0.0; }
    return shader.values[i / 4][i % 4];
}

@vertex
fn genoma_vs(@builtin(vertex_index) vertex: u32) -> GenomaVaryings {
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    let pixel = shader.origin + corner * shader.size * shader.scale;
    // Depth, as the rects have it: w carries the perspective.
    let t = vec4<f32>(pixel, 0.0, 1.0) * shader.transform;
    let ndc = (t.xy / t.w) / shader.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: GenomaVaryings;
    out.position = vec4<f32>(ndc * t.w, 0.0, t.w);
    out.unturned = pixel;
    return out;
}

@fragment
fn genoma_fs(in: GenomaVaryings) -> @location(0) vec4<f32> {
    let q = in.unturned;
    let c = shader.clip;
    // Shaded before the clip is tested: derivatives need every pixel of a quad.
    let colour = shade((q - shader.origin) / shader.scale);
    if (q.x < c.x || q.y < c.y || q.x >= c.z || q.y >= c.w) {
        discard;
    }
    var fade = 1.0;
    if (shader.fade_span > 0.0) {
        fade = clamp((shader.fade_end - q.y) / shader.fade_span, 0.0, 1.0);
    }
    return colour * shader.alpha * fade;
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
    /// Each draw's backdrop, by the group's key.
    backdrops: Vec<Option<u64>>,
    /// What is under a node, drawn: its texture bound for the shader, and
    /// when last used.
    under: HashMap<u64, (wgpu::BindGroup, u64)>,
    under_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// A transparent texel, for a shader that reads nothing under it.
    nothing: wgpu::BindGroup,
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
        let under_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader backdrop"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shader backdrop"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shader backdrop nothing"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let nothing = Self::under_group(device, &under_layout, &sampler, &blank.create_view(&Default::default()));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shader"),
            bind_group_layouts: &[Some(&group_layout), Some(&under_layout)],
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
            backdrops: Vec::new(),
            under: HashMap::new(),
            under_layout,
            sampler,
            nothing,
        }
    }

    fn under_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader backdrop"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// Whether what is under a node is kept for `key`.
    pub(crate) fn has_backdrop(&self, key: u64) -> bool {
        self.under.contains_key(&key)
    }

    /// What is under a node, drawn into `view`, for the shaders that read it.
    pub(crate) fn adopt_backdrop(&mut self, device: &wgpu::Device, key: u64, view: &wgpu::TextureView) {
        let group = Self::under_group(device, &self.under_layout, &self.sampler, view);
        self.under.insert(key, (group, self.uploads));
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
            source: wgpu::ShaderSource::Wgsl(module_source(source).into()),
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

    /// Shaders still shown though not uploaded this time (a frame drawn
    /// again only where it changed): their pipelines stay.
    pub(crate) fn touch(&mut self, keys: impl Iterator<Item = u64>) {
        for key in keys {
            if let Some((_, used)) = self.pipelines.get_mut(&key) {
                *used = self.uploads;
            }
        }
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2], shaders: &[crate::Shader]) {
        self.draws.clear();
        self.backdrops.clear();
        self.uploads += 1;
        if self.uploads % 256 == 0 {
            let now = self.uploads;
            self.pipelines.retain(|_, (_, used)| now - *used < 1024);
            self.under.retain(|_, (_, used)| now - *used < 1024);
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
                fade_end: s.fade[0],
                fade_span: s.fade[1],
                _pad: 0.0,
                pointer: s.pointer,
                backdrop: s.backdrop.map_or([0.0; 4], |(_, b)| b),
                transform: s.transform,
                values,
            };
            let at = i * STRIDE as usize;
            bytes[at..at + std::mem::size_of::<Uniforms>()].copy_from_slice(bytemuck::bytes_of(&u));
            self.draws.push(s.key);
            self.backdrops.push(s.backdrop.map(|(k, _)| k));
            if let Some((k, _)) = s.backdrop
                && let Some(entry) = self.under.get_mut(&k)
            {
                entry.1 = self.uploads;
            }
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
            let under = self.backdrops.get(i).copied().flatten().and_then(|k| self.under.get(&k)).map_or(&self.nothing, |(g, _)| g);
            pass.set_bind_group(1, under, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use wgpu::naga;

    /// The prelude and the library with `source` after them, validated as
    /// the device would.
    pub(crate) fn validate(source: &str) -> Result<(), String> {
        let text = super::module_source(source);
        let module = naga::front::wgsl::parse_str(&text).map_err(|e| e.emit_to_string(&text))?;
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .map_err(|e| e.emit_to_string(&text))?;
        Ok(())
    }

    #[test]
    fn a_mistake_is_told_at_its_own_line() {
        let wrong = "fn shade(p: vec2<f32>) -> vec4<f32> {\n    let a = 1.0;\n    return vec4<f32>(a, b, 0.0, 1.0);\n}\n";
        let said = super::check(wrong).unwrap_err();
        assert!(said.starts_with("line 3: "), "{said}");
        assert!(said.contains('b'), "names what is missing: {said}");
        let bad_type = "fn shade(p: vec2<f32>) -> vec4<f32> {\n    return 1.0;\n}\n";
        let said = super::check(bad_type).unwrap_err();
        assert!(said.contains("line"), "a validation error has its line too: {said}");
        super::check("fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(p, 0.0, 1.0); }").unwrap();
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
    #[test]
    fn every_built_in_effect_compiles_with_the_library() {
        for (name, source) in super::BUILT_IN {
            validate(source).unwrap_or_else(|e| panic!("genoma:{name}: {e}"));
        }
        assert!(super::built_in("ripple").is_some() && super::built_in("nothing").is_none());
    }

    #[test]
    fn the_library_is_there_for_a_shader_of_ones_own() {
        let own = "fn shade(p: vec2<f32>) -> vec4<f32> {\n    let n = g_simplex(p) + g_worley(p).x + g_value_noise(p) + g_fbm(p, 3);\n    let d = g_smax(g_sd_rounded_box(p, vec2<f32>(4.0), 1.0), -g_sd_segment(p, vec2<f32>(0.0), vec2<f32>(1.0)), 2.0);\n    return g_over(g_premul(vec4<f32>(n, 0.0, 0.0, g_cover(d))), g_rgba(0));\n}\n";
        validate(own).unwrap();
    }
}
