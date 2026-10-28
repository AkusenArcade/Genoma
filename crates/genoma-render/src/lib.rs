//! Genoma's renderer.
//!
//! One `wgpu` device for the whole process, on the compositor's GPU, and every
//! surface drawn through it (PRD §5.1). Shapes are distance fields evaluated in
//! the fragment shader: the CPU sends a rectangle's parameters, never its
//! outline (PRD §5.2).

use std::{ffi::c_void, ptr::NonNull};

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

mod rect;

pub use rect::Rect;

#[derive(Debug)]
pub enum Error {
    NoAdapter,
    Device(wgpu::RequestDeviceError),
    Surface(wgpu::CreateSurfaceError),
    NoAlpha,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoAdapter => write!(f, "no Vulkan adapter"),
            Error::Device(e) => write!(f, "device: {e}"),
            Error::Surface(e) => write!(f, "surface: {e}"),
            Error::NoAlpha => write!(f, "the surface offers no premultiplied alpha"),
        }
    }
}

impl std::error::Error for Error {}

/// The process's one GPU.
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    rects: Option<rect::Pipeline>,
}

impl Gpu {
    /// Opens the device on the adapter at `pci_bus_id` (`0000:03:00.0`), the
    /// compositor's main device when the caller knows it. Without a match it
    /// takes Vulkan's first hardware adapter and says so: drawing on another
    /// GPU than the compositor's copies every frame between the two.
    pub fn new(pci_bus_id: Option<&str>) -> Result<Gpu, Error> {
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

        Ok(Gpu { instance, adapter, device, queue, rects: None })
    }

    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    /// A target for one Wayland surface.
    ///
    /// # Safety
    /// `display` and `surface` must be a live `wl_display` and `wl_surface`
    /// that outlive the returned target.
    pub unsafe fn target(
        &self,
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
    ) -> Result<Target, Error> {
        let surface = unsafe {
            self.instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    display,
                ))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
            })
        }
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
        Ok(Target { surface, format, size: None })
    }

    /// Draws `rects` over a transparent surface. Coordinates are physical
    /// pixels of the target.
    pub fn draw(&mut self, target: &mut Target, width: u32, height: u32, rects: &[Rect]) {
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

        let frame = match target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => {
                log::warn!("no frame to draw into: {other:?}");
                target.size = None;
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let pipeline = self
            .rects
            .get_or_insert_with(|| rect::Pipeline::new(&self.device, target.format));
        pipeline.upload(&self.device, &self.queue, [width as f32, height as f32], rects);

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
            pipeline.draw(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

/// One Wayland surface's swapchain.
pub struct Target {
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    size: Option<(u32, u32)>,
}
