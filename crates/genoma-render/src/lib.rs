//! Genoma's renderer.
//!
//! One `wgpu` device for the whole process, on the compositor's GPU, and every
//! surface drawn through it (PRD §5.1). Shapes are distance fields evaluated in
//! the fragment shader: the CPU sends a rectangle's parameters, never its
//! outline (PRD §5.2).
//!
//! The device lives on a render thread of its own (PRD §4). The main thread
//! builds a [`DrawList`] for a surface and hands it over; the render thread
//! draws it and presents. Nothing flows back but the adapter's description.

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex, mpsc},
    thread,
};

pub use glyphon::cosmic_text;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

mod gpu;
mod rect;

pub use rect::Rect;

/// Fonts, shared by the main thread (which shapes text to lay it out) and the
/// render thread (which rasterises the glyphs). Shaping and drawing never run
/// for long, so they rarely wait on each other.
pub type Fonts = Arc<Mutex<cosmic_text::FontSystem>>;

#[derive(Debug)]
pub enum Error {
    NoAdapter,
    Device(wgpu::RequestDeviceError),
    Surface(wgpu::CreateSurfaceError),
    NoAlpha,
    Thread,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoAdapter => write!(f, "no Vulkan adapter"),
            Error::Device(e) => write!(f, "device: {e}"),
            Error::Surface(e) => write!(f, "surface: {e}"),
            Error::NoAlpha => write!(f, "the surface offers no premultiplied alpha"),
            Error::Thread => write!(f, "the render thread is gone"),
        }
    }
}

impl std::error::Error for Error {}

/// One run of shaped text, already laid out by the main thread.
pub struct Text {
    pub buffer: Arc<cosmic_text::Buffer>,
    /// Top-left corner, physical pixels.
    pub origin: [f32; 2],
    /// Logical to physical: the buffer is shaped in logical units and drawn
    /// at this scale, so glyphs are rasterised at the size they are seen.
    pub scale: f32,
    /// Straight sRGB-encoded RGBA, as everything else.
    pub color: [f32; 4],
}

/// What one surface shows, in drawing order: every rect, then every text.
#[derive(Default)]
pub struct DrawList {
    pub rects: Vec<Rect>,
    pub texts: Vec<Text>,
}

/// A surface the render thread knows, named by the main thread.
pub type SurfaceId = u32;

/// A pointer the main thread vouches for: a `wl_display` or `wl_surface`
/// that outlives the target made from it.
struct Raw(NonNull<c_void>);
// Safety: libwayland objects may be used from any thread; the caller of
// `Renderer::create_target` guarantees the lifetime.
unsafe impl Send for Raw {}

enum Message {
    Create { id: SurfaceId, display: Raw, surface: Raw },
    Draw { id: SurfaceId, width: u32, height: u32, list: DrawList },
    Destroy { id: SurfaceId },
}

/// The main thread's handle on the render thread.
pub struct Renderer {
    tx: mpsc::Sender<Message>,
    info: wgpu::AdapterInfo,
}

impl Renderer {
    /// Starts the render thread and opens the device there, on the adapter at
    /// `pci_bus_id` when one matches (see [`gpu::Gpu::new`]).
    pub fn spawn(pci_bus_id: Option<String>, fonts: Fonts) -> Result<Renderer, Error> {
        let (tx, rx) = mpsc::channel::<Message>();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("genoma-render".into())
            .spawn(move || {
                let mut gpu = match gpu::Gpu::new(pci_bus_id.as_deref(), fonts) {
                    Ok(gpu) => {
                        let _ = ready_tx.send(Ok(gpu.adapter_info()));
                        gpu
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Blocks between messages: no message, no work.
                for message in rx {
                    match message {
                        Message::Create { id, display, surface } => {
                            if let Err(e) = unsafe { gpu.create_target(id, display.0, surface.0) } {
                                log::error!("surface {id}: {e}");
                            }
                        }
                        Message::Draw { id, width, height, list } => {
                            gpu.draw(id, width, height, &list)
                        }
                        Message::Destroy { id } => gpu.destroy_target(id),
                    }
                }
            })
            .map_err(|_| Error::Thread)?;
        let info = ready_rx.recv().map_err(|_| Error::Thread)??;
        Ok(Renderer { tx, info })
    }

    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.info
    }

    /// Makes a swapchain for a Wayland surface.
    ///
    /// # Safety
    /// `display` and `surface` must be a live `wl_display` and `wl_surface`
    /// that outlive the target, which lasts until [`Renderer::destroy_target`].
    pub unsafe fn create_target(&self, id: SurfaceId, display: NonNull<c_void>, surface: NonNull<c_void>) {
        let _ = self.tx.send(Message::Create { id, display: Raw(display), surface: Raw(surface) });
    }

    /// Draws `list` on the surface at `width`×`height` physical pixels.
    /// Whatever the surface's state must say with this frame (a frame
    /// callback, a viewport) has to be requested before this call.
    pub fn draw(&self, id: SurfaceId, width: u32, height: u32, list: DrawList) {
        let _ = self.tx.send(Message::Draw { id, width, height, list });
    }

    pub fn destroy_target(&self, id: SurfaceId) {
        let _ = self.tx.send(Message::Destroy { id });
    }
}

pub(crate) fn raw_handles(
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
) -> wgpu::SurfaceTargetUnsafe {
    wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display))),
        raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
    }
}
