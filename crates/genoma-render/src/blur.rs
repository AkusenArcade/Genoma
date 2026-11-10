//! Blur for a subtree drawn into a texture of its own (`blur_radius`): a
//! Gaussian, separable, two passes; a wide one is done on the texture
//! halved first, as many times as it takes, which the eye cannot tell from
//! the whole and costs a fraction.

pub(crate) const SOURCE: &str = r#"
struct Params {
    // One texel of the source, 0…1 units.
    texel: vec2<f32>,
    // The pass's direction, in texels: (1, 0) or (0, 1); (0, 0) copies.
    direction: vec2<f32>,
    sigma: f32,
    taps: f32,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: Out;
    out.position = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    if (params.taps < 0.5) {
        return textureSample(source, smp, in.uv);
    }
    // Pairs of taps read between two texels at once, weighted as both.
    var sum = textureSample(source, smp, in.uv);
    var total = 1.0;
    let two = 2.0 * params.sigma * params.sigma;
    let n = i32(params.taps);
    for (var k = 1; k <= n; k = k + 2) {
        let a = exp(-f32(k * k) / two);
        let b = exp(-f32((k + 1) * (k + 1)) / two);
        let w = a + b;
        let at = (f32(k) * a + f32(k + 1) * b) / w;
        let offset = params.direction * params.texel * at;
        sum = sum + (textureSample(source, smp, in.uv + offset) + textureSample(source, smp, in.uv - offset)) * w;
        total = total + 2.0 * w;
    }
    return sum / total;
}
"#;

/// The widest a pass blurs, in its own texels, before the texture is
/// halved instead.
const MOST: f32 = 4.0;

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
}

/// A texture the blur reads or writes.
pub(crate) struct Sheet {
    _texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
}

impl Sheet {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: [u32; 2], label: &str) -> Sheet {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Sheet { _texture: texture, view, size: [size[0].max(1), size[1].max(1)] }
    }
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("blur"), source: wgpu::ShaderSource::Wgsl(SOURCE.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blur"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Pipeline { pipeline, layout, sampler, format }
    }

    fn pass(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, from: &Sheet, to: &Sheet, direction: [f32; 2], sigma: f32) {
        use wgpu::util::DeviceExt;
        let taps = if direction == [0.0, 0.0] { 0.0 } else { (sigma * 3.0).ceil().max(1.0) };
        let params: [f32; 8] = [1.0 / from.size[0] as f32, 1.0 / from.size[1] as f32, direction[0], direction[1], sigma, taps, 0.0, 0.0];
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blur"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&from.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &to.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// `source` blurred over `sigma` (its own pixels): halved while the
    /// blur would be wider than a pass reads well, then blurred across and
    /// down. The result may be smaller than the source; drawn stretched to
    /// the source's size, the eye cannot tell.
    pub(crate) fn blur(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, source: Sheet, sigma: f32) -> Sheet {
        let mut sheet = source;
        let mut sigma = sigma;
        while sigma > MOST && sheet.size[0] > 8 && sheet.size[1] > 8 {
            let half = Sheet::new(device, self.format, [sheet.size[0].div_ceil(2), sheet.size[1].div_ceil(2)], "blur half");
            self.pass(device, encoder, &sheet, &half, [0.0, 0.0], 0.0);
            sheet = half;
            sigma /= 2.0;
        }
        let across = Sheet::new(device, self.format, sheet.size, "blur across");
        self.pass(device, encoder, &sheet, &across, [1.0, 0.0], sigma);
        let down = Sheet::new(device, self.format, sheet.size, "blur down");
        self.pass(device, encoder, &across, &down, [0.0, 1.0], sigma);
        down
    }
}
