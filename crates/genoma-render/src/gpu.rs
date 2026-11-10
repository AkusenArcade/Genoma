//! The device and the targets, owned by the render thread.

use std::{collections::HashMap, ffi::c_void, ptr::NonNull};

use crate::{Damage, DrawList, Error, Fonts, Layer, SurfaceId, blur, image, particles, raw_handles, rect, shader};

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
    clear: Option<Clear>,
    blur: Option<blur::Pipeline>,
    text: Option<Text>,
    targets: HashMap<SurfaceId, Target>,
    /// The last frame's wait for its buffer, and each surface's minute of
    /// frames, render time and waiting, for the debug log.
    waited: std::time::Duration,
    tallies: HashMap<SurfaceId, (u64, std::time::Duration, std::time::Duration, std::time::Instant)>,
    /// What the overlay reads (`Renderer::stats`), and each surface's
    /// recent frames, for its rate.
    stats: crate::SharedStats,
    recent: HashMap<SurfaceId, std::collections::VecDeque<std::time::Instant>>,
    /// The last frame's damage, as `draw_inner` worked it out.
    damaged: crate::Damage,
}

/// Glyphon's pieces, made once the surface format is known.
struct Text {
    atlas: glyphon::TextAtlas,
    /// One per layer that has text, kept between frames: each draws only
    /// its own layer's.
    renderers: Vec<glyphon::TextRenderer>,
    swash: glyphon::SwashCache,
    cache: glyphon::Cache,
}

struct Target {
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    size: Option<(u32, u32)>,
    viewport: Option<glyphon::Viewport>,
    /// Whether the swapchain's images can be copied into, which a canvas
    /// needs.
    copyable: bool,
    canvas: Option<Canvas>,
    /// The last frame's list, what the next is compared with.
    last: Option<DrawList>,
    /// Frames drawn in a row, and when the last was.
    streak: u32,
    drawn: Option<std::time::Instant>,
}

impl Gpu {
    /// Opens the device on the adapter at `pci_bus_id` (`0000:03:00.0`), the
    /// compositor's main device when the caller knows it. Without a match it
    /// takes Vulkan's first hardware adapter and says so: drawing on another
    /// GPU than the compositor's copies every frame between the two.
    pub(crate) fn new(pci_bus_id: Option<&str>, fonts: Fonts, stats: crate::SharedStats) -> Result<Gpu, Error> {
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
            clear: None,
            blur: None,
            text: None,
            targets: HashMap::new(),
            waited: std::time::Duration::ZERO,
            tallies: HashMap::new(),
            stats,
            recent: HashMap::new(),
            damaged: crate::Damage::Whole,
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
        // Mailbox: the frame callback paces the frames (one per callback),
        // and the compositor takes the latest whose drawing is done. Fifo
        // made Mesa hold each frame until the GPU had finished it, so with a
        // game filling the GPU an opening queued its frames one behind the
        // other: the compositor's blur at once, the capsules 5 s later, at
        // 0.3 s with Mailbox on the same load (2026-11-02).
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            log::warn!("surface {id}: no mailbox presentation; frames wait in fifo behind the GPU");
            wgpu::PresentMode::Fifo
        };
        let copyable = caps.usages.contains(wgpu::TextureUsages::COPY_DST);
        self.targets.insert(
            id,
            Target { surface, format, present_mode, size: None, viewport: None, copyable, canvas: None, last: None, streak: 0, drawn: None },
        );
        Ok(())
    }

    pub(crate) fn destroy_target(&mut self, id: SurfaceId) {
        self.targets.remove(&id);
        self.recent.remove(&id);
        self.stats.lock().unwrap().surfaces.remove(&id);
        if let Some(images) = &mut self.images {
            images.forget(id);
        }
    }

    pub(crate) fn draw(&mut self, id: SurfaceId, width: u32, height: u32, list: DrawList) {
        let started = std::time::Instant::now();
        let counts = (list.rects.len(), list.images.len(), list.texts.len());
        self.draw_inner(id, width, height, list);
        let took = started.elapsed();
        log::trace!("render {} us", took.as_micros());
        let tally = self.tallies.entry(id).or_insert((0, Default::default(), Default::default(), std::time::Instant::now()));
        tally.0 += 1;
        tally.1 += took;
        tally.2 += self.waited;
        if tally.3.elapsed() >= std::time::Duration::from_secs(60) {
            let (n, total, waiting, _) = *tally;
            log::debug!(
                "surface {id}: {n} renders, {:.2} ms each, of which {:.2} ms waiting for a buffer",
                total.as_secs_f64() * 1000.0 / n.max(1) as f64,
                waiting.as_secs_f64() * 1000.0 / n.max(1) as f64
            );
            *tally = (0, Default::default(), Default::default(), std::time::Instant::now());
        }
        self.record(id, took, width, height);
        if took > std::time::Duration::from_millis(20) {
            log::warn!(
                "surface {id}: rendered in {} ms ({} rects, {} images, {} texts)",
                took.as_millis(),
                counts.0,
                counts.1,
                counts.2
            );
        }
    }

    /// The frame just drawn, into what the overlay reads.
    fn record(&mut self, id: SurfaceId, took: std::time::Duration, width: u32, height: u32) {
        let now = std::time::Instant::now();
        let recent = self.recent.entry(id).or_default();
        recent.push_back(now);
        while recent.front().is_some_and(|t| now.duration_since(*t) > std::time::Duration::from_secs(1)) {
            recent.pop_front();
        }
        let fps = recent.len() as f32;
        let canvas = self.targets.get(&id).is_some_and(|t| t.canvas.is_some());
        let (textures, mut bytes) = self.images.as_ref().map_or((0, 0), |i| i.memory());
        bytes += self.rects.as_ref().map_or(0, |r| r.atlas_bytes());
        bytes += self.targets.values().filter_map(|t| t.canvas.as_ref().and(t.size)).map(|(w, h)| u64::from(w) * u64::from(h) * 4).sum::<u64>();
        let mut stats = self.stats.lock().unwrap();
        let s = stats.surfaces.entry(id).or_default();
        s.frames += 1;
        s.fps = fps;
        s.render_ms = (took - self.waited.min(took)).as_secs_f32() * 1000.0;
        s.wait_ms = self.waited.as_secs_f32() * 1000.0;
        s.damage = match self.damaged {
            crate::Damage::None => None,
            crate::Damage::Within(r) => Some(r),
            crate::Damage::Whole => Some([0, 0, width, height]),
        };
        s.canvas = canvas;
        stats.textures = textures;
        stats.texture_bytes = bytes;
    }

    fn draw_inner(&mut self, id: SurfaceId, width: u32, height: u32, list: DrawList) {
        let now = std::time::Instant::now();
        // A canvas is kept only while its surface keeps moving: one at rest
        // gives its memory back.
        for t in self.targets.values_mut() {
            if t.canvas.is_some() && t.drawn.is_some_and(|d| now.duration_since(d) > CANVAS_IDLE) {
                t.canvas = None;
            }
        }
        // Blurred subtrees first, each drawn into a texture of its own and
        // blurred, unless one of the same key is kept: at rest, none is.
        if !list.groups.is_empty()
            && let Some(format) = self.targets.get(&id).map(|t| t.format)
        {
            self.draw_groups(id, format, &list);
        }
        let Some(target) = self.targets.get_mut(&id) else {
            log::warn!("draw for unknown surface {id}");
            return;
        };
        if target.size != Some((width, height)) {
            let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
            if target.copyable {
                usage |= wgpu::TextureUsages::COPY_DST;
            }
            target.surface.configure(
                &self.device,
                &wgpu::SurfaceConfiguration {
                    usage,
                    format: target.format,
                    width,
                    height,
                    present_mode: target.present_mode,
                    desired_maximum_frame_latency: 1,
                    alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                    view_formats: vec![],
                    color_space: wgpu::SurfaceColorSpace::Auto,
                },
            );
            target.size = Some((width, height));
            target.canvas = None;
            target.last = None;
        }

        // Frames in a row: a surface that animates is drawn into a canvas of
        // its own, and only what changed is drawn again (`Damage`). A frame
        // now and then is drawn whole, straight into the swapchain, as
        // before: the canvas costs a surface's worth of memory.
        target.streak = if target.drawn.is_some_and(|d| now.duration_since(d) < CANVAS_BEAT) { target.streak + 1 } else { 0 };
        target.drawn = Some(now);
        let damage = match &target.last {
            Some(before) => list.damage(before, [width, height]),
            None => Damage::Whole,
        };
        self.damaged = damage;
        let use_canvas = target.copyable && (target.canvas.is_some() || target.streak >= CANVAS_STREAK);
        if !use_canvas {
            target.canvas = None;
        } else if target.canvas.is_none() {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: target.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            log::debug!("surface {id}: drawn into a canvas, only what changes");
            target.canvas = Some(Canvas { texture, view, valid: false });
        }

        // How long the buffer took to come back: a frame the eye catches late
        // because the GPU or the compositor held it, said apart from one that
        // took long to draw.
        let asked = std::time::Instant::now();
        let acquired = target.surface.get_current_texture();
        let waited = asked.elapsed();
        self.waited = waited;
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
        let frame_view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let format = target.format;
        let mut viewport = target
            .viewport
            .take()
            .unwrap_or_else(|| {
                let cache = &self.text.get_or_insert_with(|| Text::new(&self.device, &self.queue, format)).cache;
                glyphon::Viewport::new(&self.device, cache)
            });
        viewport.update(&self.queue, glyphon::Resolution { width, height });

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let target = self.targets.get_mut(&id).expect("target");
        match &mut target.canvas {
            Some(canvas) => {
                let region = if canvas.valid { damage } else { Damage::Whole };
                log::trace!("surface {id}: damage {region:?}");
                let view = canvas.view.clone();
                match region {
                    Damage::Whole => self.render(&mut encoder, &view, format, id, [width, height], &list, &mut viewport, None),
                    Damage::Within(area) => {
                        let part = list.within(area);
                        self.render(&mut encoder, &view, format, id, [width, height], &part, &mut viewport, Some(area));
                        if let Some(images) = &mut self.images {
                            images.showing(id, list.images.iter().map(|i| i.key));
                        }
                        if let Some(shaders) = &mut self.shaders {
                            shaders.touch(list.shaders.iter().map(|s| s.key));
                        }
                    }
                    Damage::None => {}
                }
                let target = self.targets.get_mut(&id).expect("target");
                let canvas = target.canvas.as_mut().expect("canvas");
                canvas.valid = true;
                encoder.copy_texture_to_texture(
                    canvas.texture.as_image_copy(),
                    frame.texture.as_image_copy(),
                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                );
            }
            None => self.render(&mut encoder, &frame_view, format, id, [width, height], &list, &mut viewport, None),
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        if let Some(text) = &mut self.text {
            text.atlas.trim();
        }
        let target = self.targets.get_mut(&id).expect("target");
        target.viewport = Some(viewport);
        target.last = Some(list);
    }

    /// Each blurred subtree of `list` not already kept: drawn into a texture
    /// its bounds' size, blurred, and kept by its key for the image that
    /// shows it. Each is submitted on its own: the pipelines hold one
    /// frame's data, and the surface's frame comes after.
    /// `list` drawn into a texture of `width` × `height` and read back as
    /// straight-alpha RGBA. The pipelines are made for the format the first
    /// drawing used: a snapshot shares an engine only with others.
    pub(crate) fn snapshot(&mut self, width: u32, height: u32, list: &DrawList) -> Result<Vec<u8>, String> {
        const SNAPSHOT: SurfaceId = SurfaceId::MAX;
        // Not sRGB, as the surfaces are not: colours arrive encoded.
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (width, height) = (width.max(1), height.max(1));
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("snapshot"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw_groups(SNAPSHOT, format, list);
        let cache = &self.text.get_or_insert_with(|| Text::new(&self.device, &self.queue, format)).cache;
        let mut viewport = glyphon::Viewport::new(&self.device, cache);
        viewport.update(&self.queue, glyphon::Resolution { width, height });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.render(&mut encoder, &view, format, SNAPSHOT, [width, height], list, &mut viewport, None);
        // Rows padded to the copy's alignment, unpadded after.
        let row = width * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("snapshot"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let (done, waited) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = done.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        waited.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let mapped = buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity((row * height) as usize);
        for y in 0..height {
            let start = (y * padded) as usize;
            for p in mapped[start..start + row as usize].chunks_exact(4) {
                // Drawn premultiplied: straight for a picture file.
                let a = p[3];
                let un = |c: u8| if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 };
                out.extend_from_slice(&[un(p[0]), un(p[1]), un(p[2]), a]);
            }
        }
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    fn draw_groups(&mut self, id: SurfaceId, format: wgpu::TextureFormat, list: &DrawList) {
        for group in &list.groups {
            if self.images.as_ref().is_some_and(|i| i.has(group.key)) || self.shaders.as_ref().is_some_and(|s| s.has_backdrop(group.key)) {
                continue;
            }
            let [x0, y0, x1, y1] = group.bounds;
            let size = [((x1 - x0).max(1.0)) as u32, ((y1 - y0).max(1.0)) as u32];
            let sheet = blur::Sheet::new(&self.device, format, size, "blurred subtree");
            let moved = group.list_for_texture(x0, y0);
            let cache = &self.text.get_or_insert_with(|| Text::new(&self.device, &self.queue, format)).cache;
            let mut viewport = glyphon::Viewport::new(&self.device, cache);
            viewport.update(&self.queue, glyphon::Resolution { width: size[0], height: size[1] });
            let mut encoder = self.device.create_command_encoder(&Default::default());
            self.render(&mut encoder, &sheet.view, format, id, size, &moved, &mut viewport, None);
            self.queue.submit([encoder.finish()]);
            // Unblurred, what is under a node for a shader to read.
            let done = if group.sigma > 0.0 {
                let blur = self.blur.get_or_insert_with(|| blur::Pipeline::new(&self.device, format));
                let mut encoder = self.device.create_command_encoder(&Default::default());
                let blurred = blur.blur(&self.device, &mut encoder, sheet, group.sigma);
                self.queue.submit([encoder.finish()]);
                blurred
            } else {
                sheet
            };
            if let Some(images) = &mut self.images {
                images.adopt(&self.device, group.key, &done.view, u64::from(size[0]) * u64::from(size[1]) * 4);
            }
            if let Some(shaders) = &mut self.shaders {
                shaders.adopt_backdrop(&self.device, group.key, &done.view);
            }
            log::debug!("surface {id}: drew a group, {}×{} px, blurred over {:.1} px", size[0], size[1], group.sigma);
        }
    }

    /// Draws `list` into `view`: the whole of it cleared first, or only
    /// `within` (x, y, width, height), cleared and drawn over again while
    /// the rest keeps what the view held.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        id: SurfaceId,
        size: [u32; 2],
        list: &DrawList,
        viewport: &mut glyphon::Viewport,
        within: Option<[u32; 4]>,
    ) {
        let [width, height] = size;
        let rects = self.rects.get_or_insert_with(|| rect::Pipeline::new(&self.device, format));
        rects.upload(&self.device, &self.queue, [width as f32, height as f32], &list.rects, &list.fields);
        let images = self.images.get_or_insert_with(|| image::Pipeline::new(&self.device, format));
        images.upload(id, &self.device, &self.queue, [width as f32, height as f32], &list.images);
        let shaders = self.shaders.get_or_insert_with(|| shader::Pipeline::new(&self.device, format));
        shaders.upload(&self.device, &self.queue, [width as f32, height as f32], &list.shaders);
        let particles = self.particles.get_or_insert_with(|| particles::Pipeline::new(&self.device, format));
        particles.upload(&self.device, &self.queue, [width as f32, height as f32], &list.emitters);
        let clear = self.clear.get_or_insert_with(|| Clear::new(&self.device, format));

        let text = self.text.get_or_insert_with(|| Text::new(&self.device, &self.queue, format));
        let layers: Vec<(Layer, Layer)> = list.layers().collect();
        let mut fonts = self.fonts.lock().unwrap();
        let mut used = 0;
        for (from, to) in &layers {
            let texts = &list.texts[from.texts..to.texts];
            if texts.is_empty() {
                continue;
            }
            if text.renderers.len() <= used {
                text.renderers.push(glyphon::TextRenderer::new(
                    &mut text.atlas,
                    &self.device,
                    wgpu::MultisampleState::default(),
                    None,
                ));
            }
            let renderer = &mut text.renderers[used];
            used += 1;
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

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("surface"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if within.is_some() { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) },
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        if let Some([x, y, w, h]) = within {
            // Only the changed rectangle is touched: emptied, then drawn
            // again by everything that leaves paint in it.
            pass.set_scissor_rect(x, y, w, h);
            clear.draw(&mut pass);
        }
        // Layer by layer, each whole: a later layer's shapes lie over
        // everything an earlier one drew, and a raised one's cut what
        // lies under them first.
        let mut used = 0;
        for (from, to) in &layers {
            rects.draw(&mut pass, from.rects as u32..to.rects as u32);
            images.draw(&mut pass, from.images..to.images);
            shaders.draw(&mut pass, from.shaders..to.shaders);
            particles.draw(&mut pass, from.emitters..to.emitters);
            if to.texts > from.texts {
                let renderer = &text.renderers[used];
                used += 1;
                if let Err(e) = renderer.render(&text.atlas, viewport, &mut pass) {
                    log::warn!("text: {e}");
                }
            }
        }
    }
}

/// A surface drawn into a texture of its own, kept between frames: only
/// what changes is drawn again, and the whole is copied to the swapchain.
struct Canvas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Whether it holds the last frame whole.
    valid: bool,
}

/// Frames this close together are in a row.
const CANVAS_BEAT: std::time::Duration = std::time::Duration::from_millis(50);
/// After this many in a row, the surface gets a canvas.
const CANVAS_STREAK: u32 = 20;
/// A canvas not drawn into for this long is let go.
const CANVAS_IDLE: std::time::Duration = std::time::Duration::from_secs(2);

impl Text {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Text {
        let cache = glyphon::Cache::new(device);
        // Web: sRGB-encoded colours in a linear target, as the rects.
        let atlas = glyphon::TextAtlas::with_color_mode(device, queue, &cache, format, glyphon::ColorMode::Web);
        Text { atlas, renderers: Vec::new(), swash: glyphon::SwashCache::new(), cache }
    }
}

/// Empties the scissored part of a pass: transparent, written over what is
/// there rather than blended with it.
struct Clear {
    pipeline: wgpu::RenderPipeline,
}

impl Clear {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Clear {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clear"),
            source: wgpu::ShaderSource::Wgsl(
                "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
                    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
                }
                @fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }"
                    .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clear"),
            layout: None,
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
        Clear { pipeline }
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.draw(0..3, 0..1);
    }
}
