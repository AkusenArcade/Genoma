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
mod image;
mod particles;
mod rect;
mod shader;

pub use particles::{Emitter, EmitterParams};
pub use rect::Rect;
pub use shader::Shader;

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
    /// Drawn only inside this rectangle, physical pixels: x0, y0, x1, y1.
    pub clip: [f32; 4],
}

/// A rasterised image, premultiplied RGBA at its physical size. The render
/// thread keeps one texture per `key`; the pixels travel with every list but
/// are uploaded only the first time a key is seen.
pub struct Image {
    pub key: u64,
    pub pixels: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// Top-left corner, physical pixels, on whole pixels.
    pub origin: [f32; 2],
    /// The size it is drawn at, physical pixels: the texture's, or smaller
    /// for a part of it (`uv`).
    pub size: [f32; 2],
    /// The part of the texture drawn, 0…1: u0, v0, u1, v1.
    pub uv: [f32; 4],
    /// Corners rounded as it is drawn, physical pixels: the texture keeps
    /// square ones, so one decode serves every width it is shown at.
    pub radius: f32,
    /// Multiplies the image's own alpha: a subtree fading in.
    pub alpha: f32,
    /// Drawn only inside this rectangle, physical pixels: x0, y0, x1, y1.
    pub clip: [f32; 4],
    /// Faded toward a clip's foot, as a rect's (see [`Rect::fade`]).
    pub fade: [f32; 4],
    /// Depth, as a rect's (see [`Rect::transform`]).
    pub transform: [[f32; 4]; 4],
}

/// What one surface shows, in layers drawn one after the other. Inside a
/// layer every rect is drawn, then every image, every shader node, the
/// particles and every text: one draw call per kind. A shape that comes
/// later in the tree than something of a later kind it overlaps starts a
/// new layer, so it lies over it (a scrim over a picture); shapes that do
/// not overlap stay batched whatever their order. What is raised over all
/// of it (a list a dropdown opened) is a layer of its own, last.
#[derive(Default)]
pub struct DrawList {
    /// Where each layer but the last ends.
    pub breaks: Vec<Layer>,
    pub rects: Vec<Rect>,
    pub images: Vec<Image>,
    pub shaders: Vec<Shader>,
    pub texts: Vec<Text>,
    pub emitters: Vec<Emitter>,
    /// What the current layer holds that is drawn over its rects: the kind's
    /// rank and its bounds, physical pixels.
    over: Vec<(u8, [f32; 4])>,
}

/// A surface the render thread knows, named by the main thread.
pub type SurfaceId = u32;

/// How many of each kind there are up to a layer's end.
#[derive(Clone, Copy, Debug, Default)]
pub struct Layer {
    pub rects: usize,
    pub images: usize,
    pub shaders: usize,
    pub emitters: usize,
    pub texts: usize,
}

impl Layer {
    fn total(&self) -> usize {
        self.rects + self.images + self.shaders + self.emitters + self.texts
    }
}

/// What an item covers, clipped: everything its clip allows when it is
/// tilted (`anywhere`), as its own corners no longer say where it lands.
fn bounds(origin: [f32; 2], size: [f32; 2], clip: [f32; 4], anywhere: bool) -> [f32; 4] {
    if anywhere {
        return clip;
    }
    [
        origin[0].max(clip[0]),
        origin[1].max(clip[1]),
        (origin[0] + size[0]).min(clip[2]),
        (origin[1] + size[1]).min(clip[3]),
    ]
}

impl DrawList {
    /// The counts now: what is pushed after this is in the next layer.
    pub fn mark(&self) -> Layer {
        Layer {
            rects: self.rects.len(),
            images: self.images.len(),
            shaders: self.shaders.len(),
            emitters: self.emitters.len(),
            texts: self.texts.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mark().total() == 0
    }

    /// Ends the current layer: what is pushed after this is drawn over all
    /// of it.
    pub fn split(&mut self) {
        let mark = self.mark();
        if self.breaks.last().is_none_or(|b| b.total() < mark.total()) && mark.total() > 0 {
            self.breaks.push(mark);
        }
        self.over.clear();
    }

    /// Where an item of kind `rank` (rects 0, images 1, shaders 2, particles
    /// 3, text 4) covering `bounds` goes: a new layer when the current one
    /// already holds something of a later kind under it.
    fn order(&mut self, rank: u8, bounds: [f32; 4]) {
        let under = |b: &[f32; 4]| b[0] < bounds[2] && bounds[0] < b[2] && b[1] < bounds[3] && bounds[1] < b[3];
        if self.over.iter().any(|(r, b)| *r > rank && under(b)) {
            self.split();
        }
        if rank > 0 {
            self.over.push((rank, bounds));
        }
    }

    pub fn push_rect(&mut self, r: Rect) {
        self.order(0, bounds(r.origin, r.size, r.clip, r.transform != Rect::IDENTITY));
        self.rects.push(r);
    }

    pub fn push_image(&mut self, i: Image) {
        self.order(1, bounds(i.origin, i.size, i.clip, i.transform != Rect::IDENTITY));
        self.images.push(i);
    }

    pub fn push_shader(&mut self, s: Shader) {
        let size = [s.size[0] * s.scale, s.size[1] * s.scale];
        self.order(2, bounds(s.origin, size, s.clip, false));
        self.shaders.push(s);
    }

    /// Particles go anywhere their emitter throws them: they count as
    /// covering all of their clip.
    pub fn push_emitter(&mut self, e: Emitter) {
        self.order(3, bounds([0.0; 2], [0.0; 2], e.params.clip, true));
        self.emitters.push(e);
    }

    /// `size` is the laid-out text's, physical pixels.
    pub fn push_text(&mut self, t: Text, size: [f32; 2]) {
        // Glyphs reach past the line box: italics, accents, descenders.
        let slack = 4.0 * t.scale;
        let origin = [t.origin[0] - slack, t.origin[1] - slack];
        let size = [size[0] + slack * 2.0, size[1] + slack * 2.0];
        self.order(4, bounds(origin, size, t.clip, false));
        self.texts.push(t);
    }

    /// `other`'s layers after all of these.
    pub fn append(&mut self, other: DrawList) {
        self.split();
        let base = self.mark();
        for b in other.breaks {
            self.breaks.push(Layer {
                rects: base.rects + b.rects,
                images: base.images + b.images,
                shaders: base.shaders + b.shaders,
                emitters: base.emitters + b.emitters,
                texts: base.texts + b.texts,
            });
        }
        self.rects.extend(other.rects);
        self.images.extend(other.images);
        self.shaders.extend(other.shaders);
        self.emitters.extend(other.emitters);
        self.texts.extend(other.texts);
        self.over = other.over;
    }

    /// Each layer's start and end, in drawing order.
    pub fn layers(&self) -> impl Iterator<Item = (Layer, Layer)> + '_ {
        let ends = self.breaks.iter().copied().chain(std::iter::once(self.mark()));
        let starts = std::iter::once(Layer::default()).chain(self.breaks.iter().copied());
        starts.zip(ends)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32) -> Rect {
        Rect { origin: [x, 0.0], size: [10.0, 10.0], clip: Rect::NO_CLIP, transform: Rect::IDENTITY, ..bytemuck::Zeroable::zeroed() }
    }

    fn image(x: f32) -> Image {
        Image {
            key: 0,
            pixels: Arc::new(Vec::new()),
            width: 1,
            height: 1,
            origin: [x, 0.0],
            size: [10.0, 10.0],
            uv: [0.0, 0.0, 1.0, 1.0],
            radius: 0.0,
            alpha: 1.0,
            clip: Rect::NO_CLIP,
            fade: [0.0; 4],
            transform: Rect::IDENTITY,
        }
    }

    #[test]
    fn a_shape_over_a_picture_starts_a_layer() {
        let mut list = DrawList::default();
        list.push_image(image(0.0));
        list.push_rect(rect(5.0));
        assert_eq!(list.layers().count(), 2);
    }

    #[test]
    fn shapes_beside_each_other_stay_batched() {
        let mut list = DrawList::default();
        for i in 0..4 {
            list.push_rect(rect(i as f32 * 20.0));
            list.push_image(image(i as f32 * 20.0));
        }
        assert_eq!(list.layers().count(), 1);
    }

    #[test]
    fn what_is_raised_comes_last() {
        let mut list = DrawList::default();
        list.push_rect(rect(0.0));
        let mut upper = DrawList::default();
        upper.push_rect(rect(100.0));
        list.append(upper);
        let layers: Vec<_> = list.layers().collect();
        assert_eq!(layers.len(), 2);
        assert_eq!((layers[1].0.rects, layers[1].1.rects), (1, 2));
    }
}
