//! Images: textured quads on the shared device, for icons rasterised by the
//! main thread at the exact physical size they are shown (never scaled, as
//! Bioma's `Icon.qml` insists: a scaled stroke smears).

use std::collections::HashMap;

use crate::SurfaceId;

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Quad {
    origin: [f32; 2],
    size: [f32; 2],
    /// Alpha, the corners' radius, then the fade toward a clip's foot.
    alpha: [f32; 4],
    clip: [f32; 4],
    uv: [f32; 4],
    m: [[f32; 4]; 4],
    /// Saturation, contrast, brightness; then the tint and its strength.
    filter: [f32; 4],
    tint: [f32; 4],
}

pub(crate) const SHADER: &str = r#"
struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;
// A mask over the same rectangle: its alpha multiplies the picture's. One
// white texel where there is none.
@group(2) @binding(0) var mask_image: texture_2d<f32>;
@group(2) @binding(1) var mask_smp: sampler;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) clip: vec4<f32>,
    @location(3) local: vec2<f32>,
    @location(4) shape: vec3<f32>,
    @location(5) fade: vec2<f32>,
    // The pixel before the matrix, where the clip and the fade are measured
    // (as the rects': a tilted subtree is cut by its turned parent).
    @location(6) unturned: vec2<f32>,
    @location(7) grade: vec4<f32>,
    @location(8) tint: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @location(0) origin: vec2<f32>, @location(1) size: vec2<f32>, @location(2) alpha: vec4<f32>, @location(3) clip: vec4<f32>,
      @location(4) m0: vec4<f32>, @location(5) m1: vec4<f32>, @location(6) m2: vec4<f32>, @location(7) m3: vec4<f32>,
      @location(8) uv: vec4<f32>, @location(9) grade: vec4<f32>, @location(10) tint: vec4<f32>) -> Varyings {
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    let pixel = origin + corner * size;
    // Depth, as the rects have it: w carries the perspective.
    let p = vec4<f32>(pixel, 0.0, 1.0);
    let q = vec4<f32>(dot(m0, p), dot(m1, p), dot(m2, p), dot(m3, p));
    var out: Varyings;
    out.position = vec4<f32>((q.xy / q.w) / globals.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0) * vec4<f32>(q.w, q.w, 1.0, q.w);
    out.uv = mix(uv.xy, uv.zw, corner);
    out.alpha = alpha.x;
    out.clip = clip;
    out.local = corner * size;
    out.shape = vec3<f32>(size, alpha.y);
    out.fade = alpha.zw;
    out.unturned = pixel;
    out.grade = grade;
    out.tint = tint;
    return out;
}

// The picture's colours changed as the node asks, on the straight (not
// premultiplied) sRGB values: saturation and contrast around grey,
// brightness a multiplier, a tint the luminance in its hue.
fn filtered(c: vec4<f32>, grade: vec4<f32>, tint: vec4<f32>) -> vec4<f32> {
    if (all(grade.xyz == vec3<f32>(1.0)) && tint.w == 0.0) || c.a <= 0.0 {
        return c;
    }
    let weights = vec3<f32>(0.2126, 0.7152, 0.0722);
    var rgb = c.rgb / c.a;
    let luma = dot(rgb, weights);
    rgb = mix(vec3<f32>(luma), rgb, grade.x);
    rgb = (rgb - vec3<f32>(0.5)) * grade.y + vec3<f32>(0.5);
    rgb = rgb * grade.z;
    if tint.w > 0.0 {
        let hue = tint.rgb / max(dot(tint.rgb, weights), 1e-3);
        rgb = mix(rgb, dot(rgb, weights) * hue, tint.w);
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * c.a, c.a);
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    let p = in.unturned;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }
    // Already premultiplied, as tiny-skia rasterises; the corners cut here,
    // antialiased across a pixel.
    var cover = 1.0;
    let r = min(in.shape.z, min(in.shape.x, in.shape.y) * 0.5);
    if (r > 0.0) {
        let half = in.shape.xy * 0.5;
        let q = abs(in.local - half) - (half - vec2<f32>(r));
        let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
        cover = clamp(0.5 - d, 0.0, 1.0);
    }
    if (in.fade.y > 0.0) {
        cover *= clamp((in.fade.x - p.y) / in.fade.y, 0.0, 1.0);
    }
    let second = textureSample(mask_image, mask_smp, in.uv);
    let c = filtered(textureSample(image, smp, in.uv), in.grade, in.tint);
    if in.grade.w > 0.5 {
        // Overlaid on what is under (the second texture): multiplied where
        // that is dark, screened where it is light, as much as it is there.
        let s = c.rgb / max(c.a, 1e-4);
        let b = second.rgb / max(second.a, 1e-4);
        let low = 2.0 * s * b;
        let high = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - s) * (vec3<f32>(1.0) - b);
        let ov = select(high, low, b < vec3<f32>(0.5));
        let rgb = mix(s, ov, second.a);
        let a = c.a * in.alpha * cover;
        return vec4<f32>(rgb * a, a);
    }
    return c * in.alpha * cover * second.a;
}
"#;

pub(crate) struct Pipeline {
    /// Normal, multiply, screen, add (see [`crate::Image::blend`]).
    pipelines: [wgpu::RenderPipeline; 4],
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// One white texel: the mask of an image that has none.
    blank: wgpu::BindGroup,
    /// Each key's texture, and the upload that last drew it.
    textures: HashMap<u64, (wgpu::BindGroup, u64)>,
    /// Each key's texture's size in bytes, for the overlay's count.
    sizes: HashMap<u64, u64>,
    uploads: u64,
    /// What each surface's last frame drew. The compositor keeps showing
    /// that frame, and the surface may draw it again at any time with the
    /// pixels already let go: a texture listed here is never dropped. A
    /// still wallpaper drew nothing for a thousand uploads of the busy
    /// surfaces, lost its picture, and showed black while it was decoded
    /// again whenever anything made it draw (a settings page, a new picture).
    shown: HashMap<SurfaceId, Vec<u64>>,
    /// Pictures drawn on the GPU for a surface's groups (blurred, masked,
    /// post-processed), by key: the surface. One that moves is a new key
    /// each frame, so each is let go as soon as its surface stops showing
    /// it (`release`) instead of waiting out a thousand uploads.
    adopted: HashMap<u64, SurfaceId>,
    quads: wgpu::Buffer,
    capacity: usize,
    /// Each quad's texture, its mask's, and its blend.
    draws: Vec<(u64, Option<u64>, u8)>,
    last_viewport: [f32; 2],
    last_quads: Vec<u8>,
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Pipeline {
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
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout), Some(&texture_layout)],
            ..Default::default()
        });
        let make = |blend: wgpu::BlendState| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4,
                        8 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4, 9 => Float32x4, 10 => Float32x4],
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
        // What a node's `blend` asks, premultiplied: normal (and overlay,
        // which the shader works out against what is under), multiply,
        // screen, add.
        let over = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add };
        let colour = |src_factor, dst_factor| wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor, dst_factor, operation: wgpu::BlendOperation::Add },
            alpha: over,
        };
        let pipelines = [
            make(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            make(colour(wgpu::BlendFactor::Dst, wgpu::BlendFactor::OneMinusSrcAlpha)),
            make(colour(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc)),
            make(colour(wgpu::BlendFactor::One, wgpu::BlendFactor::One)),
        ];
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
        let white = wgpu::util::DeviceExt::create_texture_with_data(
            device,
            queue,
            &wgpu::TextureDescriptor {
                label: Some("image blank mask"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255, 255, 255, 255],
        );
        let view = white.create_view(&Default::default());
        let blank = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image blank mask"),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        Pipeline {
            pipelines,
            globals,
            globals_group,
            texture_layout,
            sampler,
            blank,
            textures: HashMap::new(),
            sizes: HashMap::new(),
            uploads: 0,
            shown: HashMap::new(),
            adopted: HashMap::new(),
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

    /// What a surface shows, when the last upload carried only part of it
    /// (a frame drawn again only where it changed): the rest stays too.
    pub(crate) fn showing(&mut self, id: SurfaceId, keys: impl Iterator<Item = u64>) {
        self.shown.insert(id, keys.collect());
    }

    /// A surface gone: what it showed may go too.
    /// The textures it keeps: how many, and their bytes.
    pub(crate) fn memory(&self) -> (usize, u64) {
        (self.textures.len(), self.sizes.values().sum())
    }

    pub(crate) fn forget(&mut self, id: SurfaceId) {
        self.shown.remove(&id);
    }

    /// Whether a texture is kept for `key`.
    pub(crate) fn has(&self, key: u64) -> bool {
        self.textures.contains_key(&key)
    }

    /// A texture drawn on the GPU (a blurred subtree), kept by `key` as an
    /// uploaded one is, and let go the same way.
    pub(crate) fn adopt(&mut self, id: SurfaceId, device: &wgpu::Device, key: u64, view: &wgpu::TextureView, bytes: u64) {
        self.sizes.insert(key, bytes);
        self.adopted.insert(key, id);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.textures.insert(key, (group, self.uploads));
    }

    /// The pictures drawn for `id`'s groups that its frame no longer
    /// shows, let go.
    pub(crate) fn release(&mut self, id: SurfaceId, keep: &std::collections::HashSet<u64>) {
        let gone: Vec<u64> = self.adopted.iter().filter(|(k, s)| **s == id && !keep.contains(*k)).map(|(k, _)| *k).collect();
        for key in gone {
            self.adopted.remove(&key);
            self.textures.remove(&key);
            self.sizes.remove(&key);
        }
    }

    pub(crate) fn upload(
        &mut self,
        id: SurfaceId,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        images: &[crate::Image],
    ) {
        self.draws.clear();
        // A texture no surface has drawn for a while is dropped: a text
        // rasterised while tilted leaves one per thing it said. The pixels
        // travel with every list, so a key seen again is uploaded again.
        self.uploads += 1;
        if self.uploads % 256 == 0 {
            let now = self.uploads;
            let shown: std::collections::HashSet<u64> = self.shown.values().flatten().copied().collect();
            self.textures.retain(|key, (_, used)| now - *used < 1024 || shown.contains(key));
            let textures = &self.textures;
            self.sizes.retain(|key, _| textures.contains_key(key));
            let mut resident = RESIDENT.lock().unwrap();
            resident.retain(|key| self.textures.contains_key(key));
        }
        self.shown.insert(id, images.iter().flat_map(|i| std::iter::once(i.key).chain(i.mask)).collect());
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
                // Sent by its key alone, its pixels let go on the CPU side,
                // and no longer here: not drawn this once, and the scene
                // decodes it again, as the key is no longer resident.
                if image.pixels.is_empty() {
                    continue;
                }
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
                self.textures.insert(image.key, (group, 0));
                self.sizes.insert(image.key, u64::from(image.width) * u64::from(image.height) * 4);
                RESIDENT.lock().unwrap().insert(image.key);
            }
            if let Some((_, used)) = self.textures.get_mut(&image.key) {
                *used = self.uploads;
            }
            quads.push(Quad {
                origin: image.origin,
                size: image.size,
                alpha: [image.alpha, image.radius, image.fade[0], image.fade[1]],
                clip: image.clip,
                uv: image.uv,
                m: image.transform,
                // The fourth: 1 when the second texture is what is under, to
                // be overlaid, rather than a mask.
                filter: [image.filter[0], image.filter[1], image.filter[2], if image.blend == crate::Image::OVERLAY { 1.0 } else { 0.0 }],
                tint: [image.filter[4], image.filter[5], image.filter[6], image.filter[7]],
            });
            self.draws.push((image.key, image.mask, image.blend));
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

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, only: std::ops::Range<usize>) {
        if self.draws.is_empty() || only.is_empty() {
            return;
        }
        let mut current = u8::MAX;
        pass.set_bind_group(0, &self.globals_group, &[]);
        pass.set_vertex_buffer(0, self.quads.slice(..));
        for (i, (key, mask, blend)) in self.draws.iter().enumerate() {
            if !only.contains(&i) {
                continue;
            }
            let wanted = match *blend {
                1..=3 => *blend,
                _ => 0,
            };
            if wanted != current {
                pass.set_pipeline(&self.pipelines[wanted as usize]);
                current = wanted;
            }
            pass.set_bind_group(1, &self.textures[key].0, &[]);
            let mask = mask.and_then(|m| self.textures.get(&m)).map_or(&self.blank, |t| &t.0);
            pass.set_bind_group(2, mask, &[]);
            pass.draw(0..4, i as u32..i as u32 + 1);
        }
    }
}

/// The images on the GPU, by key: a scene lets a picture's pixels go once
/// its key is here, and sends the key alone from then on. A wallpaper was
/// kept twice, in a texture and in the scene's cache.
static RESIDENT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Whether the image with this key is on the GPU now.
pub fn resident(key: u64) -> bool {
    RESIDENT.lock().unwrap().contains(&key)
}
