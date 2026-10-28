//! The `rect` primitive (PRD §5.2): a rounded rectangle with a fill and the
//! lit border, both from one distance field, every instance in one draw call.

use bytemuck::{Pod, Zeroable};

/// One rounded rectangle, in physical pixels of its target.
///
/// The rim is Bioma's lit border: a band `rim` pixels wide inside the edge,
/// running from `rim_top` at the top to `rim_bottom` at mid-height and staying
/// there below. Colours are straight (not premultiplied) linear RGBA as the
/// compositor blends them.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Rect {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub radius: f32,
    pub rim: f32,
    /// How much of the height the fill covers, from the bottom: 1 is all of
    /// it, a lower value is a liquid's surface. The rim is never cut.
    pub level: f32,
    /// 1 erases what is under the shape instead of painting it: a tissue's
    /// band is cut out where its cells are, so glass never sits on glass.
    pub cut: f32,
    pub fill: [f32; 4],
    pub rim_top: [f32; 4],
    pub rim_bottom: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    cutter: wgpu::RenderPipeline,
    runs: Vec<(bool, std::ops::Range<u32>)>,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

const SHADER: &str = r#"
struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) shape: vec4<f32>,      // radius, rim, level, cut
    @location(3) fill: vec4<f32>,
    @location(4) rim_top: vec4<f32>,
    @location(5) rim_bottom: vec4<f32>,
};

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,      // pixels from the rectangle's centre
    @location(1) half: vec2<f32>,
    @location(2) shape: vec4<f32>,
    @location(3) fill: vec4<f32>,
    @location(4) rim_top: vec4<f32>,
    @location(5) rim_bottom: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vertex: u32, instance: Instance) -> Varyings {
    // Two triangles over the rectangle, one pixel larger on every side so the
    // antialiased edge is never cut by the quad.
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    let pixel = instance.origin - vec2<f32>(1.0) + corner * (instance.size + vec2<f32>(2.0));
    let clip = pixel / globals.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: Varyings;
    out.position = vec4<f32>(clip, 0.0, 1.0);
    out.half = instance.size * 0.5;
    out.local = pixel - instance.origin - out.half;
    out.shape = instance.shape;
    out.fill = instance.fill;
    out.rim_top = instance.rim_top;
    out.rim_bottom = instance.rim_bottom;
    return out;
}

// Signed distance to a rounded box: negative inside, in pixels.
fn rounded_box(p: vec2<f32>, half: vec2<f32>, radius: f32) -> f32 {
    let r = min(radius, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    let d = rounded_box(in.local, in.half, in.shape.x);
    let rim = in.shape.y;

    // Coverage over one pixel of distance: the edge is analytic at any scale.
    let outer = clamp(0.5 - d, 0.0, 1.0);
    // Inset by the rim width: the inner edge is the same field offset, so its
    // radius is concentric by construction.
    let inner = clamp(0.5 - (d + rim), 0.0, 1.0);

    // The light reaches the bottom colour at mid-height and stays there.
    let height = in.local.y + in.half.y;
    let t = clamp(height / max(in.half.y, 1.0), 0.0, 1.0);
    let rim_colour = mix(in.rim_top, in.rim_bottom, t);

    // The liquid's surface, antialiased like every other edge.
    let from_bottom = in.half.y - in.local.y;
    let surface = clamp(in.shape.z * in.half.y * 2.0 - from_bottom + 0.5, 0.0, 1.0);

    let rim_alpha = rim_colour.a * (outer - inner);
    let fill_alpha = in.fill.a * inner * surface;
    if (in.shape.w > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, outer);
    }
    // Premultiplied out: the surface is composited as premultiplied alpha.
    return vec4<f32>(rim_colour.rgb * rim_alpha + in.fill.rgb * fill_alpha, rim_alpha + fill_alpha);
}
"#;

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rect globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rect globals"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let make = |blend: wgpu::BlendState| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Rect>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x4,
                        3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
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
        let pipeline = make(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        // Destination-out: what is drawn is the shape's coverage taken away.
        let erase = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let cutter = make(wgpu::BlendState { color: erase, alpha: erase });
        let capacity = 16;
        let instances = Self::instance_buffer(device, capacity);
        Pipeline { pipeline, cutter, runs: Vec::new(), globals, bind_group, instances, capacity, count: 0 }
    }

    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect instances"),
            size: (capacity * std::mem::size_of::<Rect>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        rects: &[Rect],
    ) {
        if rects.len() > self.capacity {
            self.capacity = rects.len().next_power_of_two();
            self.instances = Self::instance_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&Globals { viewport, _pad: [0.0; 2] }));
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(rects));
        self.count = rects.len() as u32;
        // Consecutive rects of one kind share a draw call.
        self.runs.clear();
        for (i, r) in rects.iter().enumerate() {
            let cut = r.cut > 0.5;
            match self.runs.last_mut() {
                Some((c, range)) if *c == cut => range.end = i as u32 + 1,
                _ => self.runs.push((cut, i as u32..i as u32 + 1)),
            }
        }
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (cut, range) in &self.runs {
            pass.set_pipeline(if *cut { &self.cutter } else { &self.pipeline });
            pass.draw(0..4, range.clone());
        }
    }
}
