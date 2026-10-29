//! The device and the targets, owned by the render thread.

use std::{collections::HashMap, ffi::c_void, ptr::NonNull};

use crate::{DrawList, Error, Fonts, Layer, SurfaceId, image, particles, raw_handles, rect, shader};

pub(crate) struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    fonts: Fonts,
    rects: Option<rect::Pipeline>,
    images: Option<image::Pipeline>,
    particles: Option<particles::Pipeline>,
    shaders: Option<shader::Pipeline>,
    text: Option<Text>,
    targets: HashMap<SurfaceId, Target>,
}

/// Glyphon's pieces, made once the surface format is known.
struct Text {
    atlas: glyphon::TextAtlas,
    renderer: glyphon::TextRenderer,
    /// The raised layer's, drawn after the base layer's shapes and text.
    raised: glyphon::TextRenderer,
    swash: glyphon::SwashCache,
    cache: glyphon::Cache,
}

struct Target {
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    size: Option<(u32, u32)>,
    viewport: Option<glyphon::Viewport>,
}

impl Gpu {
    /// Opens the device on the adapter at `pci_bus_id` (`0000:03:00.0`), the
    /// compositor's main device when the caller knows it. Without a match it
    /// takes Vulkan's first hardware adapter and says so: drawing on another
    /// GPU than the compositor's copies every frame between the two.
    pub(crate) fn new(pci_bus_id: Option<&str>, fonts: Fonts) -> Result<Gpu, Error> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        for adapter in &adapters {
            let info = adapter.get_info();
            log::debug!(
                "adapter: {} ({:?}, pci {:?}, {})",
                info.name,
                info.device_type,
                info.device_pci_bus_id,
                info.driver
            );
        }

        let wanted = pci_bus_id.and_then(|id| {
            adapters
                .iter()
                .find(|a| a.get_info().device_pci_bus_id.eq_ignore_ascii_case(id))
        });
        let adapter = match (pci_bus_id, wanted) {
            (_, Some(adapter)) => adapter.clone(),
            (wanted, None) => {
                let fallback = adapters
                    .iter()
                    .find(|a| a.get_info().device_type != wgpu::DeviceType::Cpu)
                    .or(adapters.first())
                    .ok_or(Error::NoAdapter)?
                    .clone();
                log::warn!(
                    "compositor's device {:?} not among the adapters; using {}",
                    wanted,
                    fallback.get_info().name
                );
                fallback
            }
        };
        let info = adapter.get_info();
        log::info!("drawing on {} (pci {})", info.name, info.device_pci_bus_id);

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("genoma"),
            ..Default::default()
        }))
        .map_err(Error::Device)?;

        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            fonts,
            rects: None,
            images: None,
            particles: None,
            shaders: None,
            text: None,
            targets: HashMap::new(),
        })
    }

    pub(crate) fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    /// # Safety
    /// See [`crate::Renderer::create_target`].
    pub(crate) unsafe fn create_target(
        &mut self,
        id: SurfaceId,
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
    ) -> Result<(), Error> {
        let surface = unsafe { self.instance.create_surface_unsafe(raw_handles(display, surface)) }
            .map_err(Error::Surface)?;
        let caps = surface.get_capabilities(&self.adapter);
        // A shell surface is mostly transparent: the compositor must blend it
        // as premultiplied, or every soft edge gets a dark fringe.
        if !caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            return Err(Error::NoAlpha);
        }
        // Non-sRGB, so the blending happens in the same space the compositor
        // and every other client blend in.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .unwrap_or(caps.formats[0]);
        self.targets.insert(id, Target { surface, format, size: None, viewport: None });
        Ok(())
    }

    pub(crate) fn destroy_target(&mut self, id: SurfaceId) {
        self.targets.remove(&id);
    }

    pub(crate) fn draw(&mut self, id: SurfaceId, width: u32, height: u32, list: &DrawList) {
        let started = std::time::Instant::now();
        self.draw_inner(id, width, height, list);
        let took = started.elapsed();
        log::trace!("render {} us", took.as_micros());
        if took > std::time::Duration::from_millis(20) {
            log::warn!(
                "surface {id}: rendered in {} ms ({} rects, {} images, {} texts)",
                took.as_millis(),
                list.rects.len(),
                list.images.len(),
                list.texts.len()
            );
        }
    }

    fn draw_inner(&mut self, id: SurfaceId, width: u32, height: u32, list: &DrawList) {
        let Some(target) = self.targets.get_mut(&id) else {
            log::warn!("draw for unknown surface {id}");
            return;
        };
        if target.size != Some((width, height)) {
            target.surface.configure(
                &self.device,
                &wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: target.format,
                    width,
                    height,
                    // Fifo: one frame per frame callback, never ahead of it.
                    present_mode: wgpu::PresentMode::Fifo,
                    desired_maximum_frame_latency: 1,
                    alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                    view_formats: vec![],
                    color_space: wgpu::SurfaceColorSpace::Auto,
                },
            );
            target.size = Some((width, height));
        }

        // How long the buffer took to come back: a frame the eye catches late
        // because the GPU or the compositor held it, said apart from one that
        // took long to draw.
        let asked = std::time::Instant::now();
        let acquired = target.surface.get_current_texture();
        let waited = asked.elapsed();
        if waited > std::time::Duration::from_millis(8) {
            log::warn!("surface {id}: waited {} ms for a buffer", waited.as_millis());
        }
        let frame = match acquired {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => {
                log::warn!("no frame to draw into: {other:?}");
                target.size = None;
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let format = target.format;
        let rects = self.rects.get_or_insert_with(|| rect::Pipeline::new(&self.device, format));
        rects.upload(&self.device, &self.queue, [width as f32, height as f32], &list.rects);
        let images = self.images.get_or_insert_with(|| image::Pipeline::new(&self.device, format));
        images.upload(&self.device, &self.queue, [width as f32, height as f32], &list.images);
        let shaders = self.shaders.get_or_insert_with(|| shader::Pipeline::new(&self.device, format));
        shaders.upload(&self.device, &self.queue, [width as f32, height as f32], &list.shaders);
        let particles = self.particles.get_or_insert_with(|| particles::Pipeline::new(&self.device, format));
        particles.upload(&self.device, &self.queue, [width as f32, height as f32], &list.emitters);

        let text = self.text.get_or_insert_with(|| {
            let cache = glyphon::Cache::new(&self.device);
            // Web: sRGB-encoded colours in a linear target, as the rects.
            let mut atlas = glyphon::TextAtlas::with_color_mode(
                &self.device,
                &self.queue,
                &cache,
                format,
                glyphon::ColorMode::Web,
            );
            let renderer = glyphon::TextRenderer::new(
                &mut atlas,
                &self.device,
                wgpu::MultisampleState::default(),
                None,
            );
            let raised = glyphon::TextRenderer::new(
                &mut atlas,
                &self.device,
                wgpu::MultisampleState::default(),
                None,
            );
            Text { atlas, renderer, raised, swash: glyphon::SwashCache::new(), cache }
        });
        let viewport = target
            .viewport
            .get_or_insert_with(|| glyphon::Viewport::new(&self.device, &text.cache));
        viewport.update(&self.queue, glyphon::Resolution { width, height });
        let base = list.base.unwrap_or_else(|| list.mark());
        let mut fonts = self.fonts.lock().unwrap();
        for (renderer, texts) in [(&mut text.renderer, &list.texts[..base.texts]), (&mut text.raised, &list.texts[base.texts..])] {
            if texts.is_empty() {
                continue;
            }
            let areas = texts.iter().map(|t| glyphon::TextArea {
                buffer: &t.buffer,
                left: t.origin[0],
                top: t.origin[1],
                scale: t.scale,
                bounds: glyphon::TextBounds {
                    left: t.clip[0].max(-1.0e9) as i32,
                    top: t.clip[1].max(-1.0e9) as i32,
                    right: t.clip[2].min(1.0e9) as i32,
                    bottom: t.clip[3].min(1.0e9) as i32,
                },
                default_color: glyphon::Color::rgba(
                    (t.color[0] * 255.0).round() as u8,
                    (t.color[1] * 255.0).round() as u8,
                    (t.color[2] * 255.0).round() as u8,
                    (t.color[3] * 255.0).round() as u8,
                ),
                custom_glyphs: &[],
            });
            if let Err(e) = renderer.prepare(
                &self.device,
                &self.queue,
                &mut fonts,
                &mut text.atlas,
                viewport,
                areas,
                &mut text.swash,
            ) {
                log::warn!("text: {e}");
            }
        }
        drop(fonts);

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("surface"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            // The base layer, then what is raised over it, each whole: the
            // raised one's shapes cut what lies under them first.
            let ends = list.mark();
            for (from, to, raised) in [(Layer::default(), base, false), (base, ends, true)] {
                rects.draw(&mut pass, from.rects as u32..to.rects as u32);
                images.draw(&mut pass, from.images..to.images);
                shaders.draw(&mut pass, from.shaders..to.shaders);
                particles.draw(&mut pass, from.emitters..to.emitters);
                if to.texts > from.texts {
                    let renderer = if raised { &text.raised } else { &text.renderer };
                    if let Err(e) = renderer.render(&text.atlas, viewport, &mut pass) {
                        log::warn!("text: {e}");
                    }
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        text.atlas.trim();
    }
}
