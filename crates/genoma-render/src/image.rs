//! Images: textured quads on the shared device, for icons rasterised by the
//! main thread at the exact physical size they are shown (never scaled, as
//! Bioma's `Icon.qml` insists: a scaled stroke smears).

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Quad {
    origin: [f32; 2],
    size: [f32; 2],
}

const SHADER: &str = r#"
struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @location(0) origin: vec2<f32>, @location(1) size: vec2<f32>) -> Varyings {
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    let pixel = origin + corner * size;
    var out: Varyings;
    out.position = vec4<f32>(pixel / globals.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    // Already premultiplied, as tiny-skia rasterises.
    return textureSample(image, smp, in.uv);
}
"#;

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<u64, wgpu::BindGroup>,
    quads: wgpu::Buffer,
    capacity: usize,
    draws: Vec<u64>,
    last_viewport: [f32; 2],
    last_quads: Vec<u8>,
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image globals"),
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
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image texture"),
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
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                })],
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Linear: an icon placed on whole physical pixels samples its own
        // texels exactly; linear only matters for the half-pixel case.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let capacity = 8;
        let quads = Self::quad_buffer(device, capacity);
        Pipeline {
            pipeline,
            globals,
            globals_group,
            texture_layout,
            sampler,
            textures: HashMap::new(),
            quads,
            capacity,
            draws: Vec::new(),
            last_viewport: [0.0; 2],
            last_quads: Vec::new(),
        }
    }

    fn quad_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image quads"),
            size: (capacity * std::mem::size_of::<Quad>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        images: &[crate::Image],
    ) {
        self.draws.clear();
        if images.is_empty() {
            return;
        }
        if images.len() > self.capacity {
            self.capacity = images.len().next_power_of_two();
            self.quads = Self::quad_buffer(device, self.capacity);
            self.last_quads.clear();
        }
        let mut quads = Vec::with_capacity(images.len());
        for image in images {
            if !self.textures.contains_key(&image.key) {
                let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("image"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    texture.as_image_copy(),
                    &image.pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(image.width * 4),
                        rows_per_image: Some(image.height),
                    },
                    size,
                );
                let view = texture.create_view(&Default::default());
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("image"),
                    layout: &self.texture_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                });
                self.textures.insert(image.key, group);
            }
            quads.push(Quad { origin: image.origin, size: [image.width as f32, image.height as f32] });
            self.draws.push(image.key);
        }
        if viewport != self.last_viewport {
            queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&[viewport[0], viewport[1], 0.0, 0.0]));
            self.last_viewport = viewport;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&quads);
        if bytes != self.last_quads.as_slice() {
            queue.write_buffer(&self.quads, 0, bytes);
            self.last_quads.clear();
            self.last_quads.extend_from_slice(bytes);
        }
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_group, &[]);
        pass.set_vertex_buffer(0, self.quads.slice(..));
        for (i, key) in self.draws.iter().enumerate() {
            pass.set_bind_group(1, &self.textures[key], &[]);
            pass.draw(0..4, i as u32..i as u32 + 1);
        }
    }
}
