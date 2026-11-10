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
    collections::HashMap,
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex, mpsc},
    thread,
};

pub use glyphon::cosmic_text;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

mod blur;
mod gpu;
mod image;
mod particles;
mod rect;
pub mod shader;

pub use particles::{Emitter, EmitterParams};
pub use rect::Rect;
pub use shader::Shader;
pub use image::resident;

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
#[derive(Clone)]
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
#[derive(Clone)]
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
    /// Saturation, contrast, brightness (1 each changes nothing), then a
    /// tint's red, green, blue and strength (0 none): see [`Image::NO_FILTER`].
    pub filter: [f32; 8],
    /// Another texture (a group's) laid over the same rectangle whose alpha
    /// this one's is multiplied by: a mask. `None` draws it whole.
    pub mask: Option<u64>,
}

impl Image {
    /// The picture as it is.
    pub const NO_FILTER: [f32; 8] = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
}

/// What one surface shows, in layers drawn one after the other. Inside a
/// layer every rect is drawn, then every image, every shader node, the
/// particles and every text: one draw call per kind. A shape that comes
/// later in the tree than something of a later kind it overlaps starts a
/// new layer, so it lies over it (a scrim over a picture); shapes that do
/// not overlap stay batched whatever their order. What is raised over all
/// of it (a list a dropdown opened) is a layer of its own, last.
/// The outline of a text or a path as a distance field, for a shape to be
/// combined with (`combine_with` naming it): one byte a pixel, read as
/// coverage is, 0.5 on the edge, 1 inside and 0 outside at the spread the
/// shape's entry gives, physical pixels.
#[derive(Clone)]
pub struct Field {
    /// What the pixels show: the same key is the same field.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u8>>,
}

#[derive(Clone, Default)]
pub struct DrawList {
    /// The distance fields its rects' shapes read, by place (a shape's entry
    /// names its field by index here).
    pub fields: Vec<Field>,
    /// Subtrees drawn blurred (see [`Group`]), inner ones first: each is
    /// drawn into a texture of its own and shows in this list as an image
    /// of its key.
    pub groups: Vec<Group>,
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
    /// Where each item can leave paint, physical pixels, x0, y0, x1, y1, by
    /// kind as `order`'s ranks: what a frame that redraws only what changed
    /// has to clear and draw again (see [`DrawList::damage`]).
    paint: [Vec<[f32; 4]>; 5],
}

/// A subtree drawn blurred: its own list drawn into a texture over
/// `bounds`, blurred over `sigma`, and shown where it was as the image whose
/// key is `key`. The key is the list's fingerprint with the blur's: the same
/// key is the same picture, so one at rest is never drawn or blurred again.
#[derive(Clone)]
pub struct Group {
    pub key: u64,
    pub list: DrawList,
    /// x0, y0, x1, y1, physical pixels, whole: what the texture covers.
    pub bounds: [f32; 4],
    /// The Gaussian's standard deviation, physical pixels.
    pub sigma: f32,
}

impl Group {
    /// Its list as its texture takes it: the bounds' corner at the
    /// texture's.
    pub(crate) fn list_for_texture(&self, x0: f32, y0: f32) -> DrawList {
        self.list.clone().shifted(x0, y0)
    }
}

/// What changed between two frames of one surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Damage {
    /// Nothing at all.
    None,
    /// Only inside this rectangle, whole physical pixels: x, y, width, height.
    Within([u32; 4]),
    /// Too much, or the layers themselves: the whole surface.
    Whole,
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

/// Whether two rectangles (x0, y0, x1, y1) share any area.
fn meets(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// Whether two items of the same kind draw the same pixels.
fn same_rect(a: &Rect, b: &Rect) -> bool {
    bytemuck::bytes_of(a) == bytemuck::bytes_of(b)
}

fn same_image(a: &Image, b: &Image) -> bool {
    a.key == b.key
        && a.width == b.width
        && a.height == b.height
        && a.origin == b.origin
        && a.size == b.size
        && a.uv == b.uv
        && a.radius == b.radius
        && a.alpha == b.alpha
        && a.clip == b.clip
        && a.fade == b.fade
        && a.transform == b.transform
        && a.filter == b.filter
        && a.mask == b.mask
}

fn same_shader(a: &Shader, b: &Shader) -> bool {
    a.key == b.key
        && a.origin == b.origin
        && a.size == b.size
        && a.scale == b.scale
        && a.time == b.time
        && a.alpha == b.alpha
        && a.clip == b.clip
        && a.fade == b.fade
        && a.values == b.values
        && a.transform == b.transform
        && a.backdrop == b.backdrop
}

fn same_emitter(a: &Emitter, b: &Emitter) -> bool {
    a.key == b.key && a.generation == b.generation && bytemuck::bytes_of(&a.params) == bytemuck::bytes_of(&b.params)
}

fn same_text(a: &Text, b: &Text) -> bool {
    Arc::ptr_eq(&a.buffer, &b.buffer) && a.origin == b.origin && a.scale == b.scale && a.color == b.color && a.clip == b.clip
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

    /// A distance field for a shape to read: its place in the list, the
    /// same field once however many shapes read it.
    pub fn push_field(&mut self, field: Field) -> usize {
        match self.fields.iter().position(|f| f.key == field.key) {
            Some(i) => i,
            None => {
                self.fields.push(field);
                self.fields.len() - 1
            }
        }
    }

    pub fn push_rect(&mut self, r: Rect) {
        let tilted = r.transform != Rect::IDENTITY;
        self.order(0, bounds(r.origin, r.size, r.clip, tilted));
        // The quad the GPU covers: a pixel past every side for the
        // antialiased edge, and a bud's parent box (rect.rs, `vs`).
        let (mut lo, mut hi) = (r.origin, [r.origin[0] + r.size[0], r.origin[1] + r.size[1]]);
        if r.bud_shape[1] > 0.0 {
            // A negative radius: no shape in the bud's place, only more.
            if r.bud_shape[0] >= 0.0 {
                let centre = [r.origin[0] + r.size[0] * 0.5 + r.bud[0], r.origin[1] + r.size[1] * 0.5 + r.bud[1]];
                lo = [lo[0].min(centre[0] - r.bud[2]), lo[1].min(centre[1] - r.bud[3])];
                hi = [hi[0].max(centre[0] + r.bud[2]), hi[1].max(centre[1] + r.bud[3])];
            }
            // And the more shapes it is combined with.
            for i in 0..4 {
                let (b, present) = (r.shapes[i * 2], r.shapes[i * 2 + 1][1] > 0.5);
                if present {
                    let c = [r.origin[0] + r.size[0] * 0.5 + b[0], r.origin[1] + r.size[1] * 0.5 + b[1]];
                    lo = [lo[0].min(c[0] - b[2]), lo[1].min(c[1] - b[3])];
                    hi = [hi[0].max(c[0] + b[2]), hi[1].max(c[1] + b[3])];
                }
            }
        }
        let quad = [lo[0] - 1.0, lo[1] - 1.0];
        let size = [hi[0] - lo[0] + 2.0, hi[1] - lo[1] + 2.0];
        self.paint[0].push(bounds(quad, size, r.clip, tilted));
        self.rects.push(r);
    }

    pub fn push_image(&mut self, i: Image) {
        let tilted = i.transform != Rect::IDENTITY;
        let painted = bounds(i.origin, i.size, i.clip, tilted);
        self.order(1, painted);
        self.paint[1].push(bounds([i.origin[0] - 1.0, i.origin[1] - 1.0], [i.size[0] + 2.0, i.size[1] + 2.0], i.clip, tilted));
        self.images.push(i);
    }

    pub fn push_shader(&mut self, s: Shader) {
        let size = [s.size[0] * s.scale, s.size[1] * s.scale];
        let tilted = s.transform != Rect::IDENTITY;
        let painted = bounds(s.origin, size, s.clip, tilted);
        self.order(2, painted);
        self.paint[2].push(bounds([s.origin[0] - 1.0, s.origin[1] - 1.0], [size[0] + 2.0, size[1] + 2.0], s.clip, tilted));
        self.shaders.push(s);
    }

    /// Particles go anywhere their emitter throws them: they count as
    /// covering all of their clip.
    pub fn push_emitter(&mut self, e: Emitter) {
        let painted = bounds([0.0; 2], [0.0; 2], e.params.clip, true);
        self.order(3, painted);
        self.paint[3].push(painted);
        self.emitters.push(e);
    }

    /// `size` is the laid-out text's, physical pixels.
    pub fn push_text(&mut self, t: Text, size: [f32; 2]) {
        // Glyphs reach past the line box: italics, accents, descenders.
        let slack = 4.0 * t.scale;
        let origin = [t.origin[0] - slack, t.origin[1] - slack];
        let size = [size[0] + slack * 2.0, size[1] + slack * 2.0];
        let painted = bounds(origin, size, t.clip, false);
        self.order(4, painted);
        self.paint[4].push(painted);
        self.texts.push(t);
    }

    /// What has to be drawn again to turn `before` into this list, on a
    /// surface `viewport` physical pixels large: every item that changed,
    /// where it was and where it is. Different layers, or a different
    /// count of anything, is the whole surface; so is a change over more
    /// than half of it, which a whole frame draws as cheaply.
    pub fn damage(&self, before: &DrawList, viewport: [u32; 2]) -> Damage {
        let counts = |l: &DrawList| [l.rects.len(), l.images.len(), l.shaders.len(), l.emitters.len(), l.texts.len()];
        if counts(self) != counts(before) || self.breaks.len() != before.breaks.len() {
            return Damage::Whole;
        }
        let same_breaks = self.breaks.iter().zip(&before.breaks).all(|(a, b)| {
            a.rects == b.rects && a.images == b.images && a.shaders == b.shaders && a.emitters == b.emitters && a.texts == b.texts
        });
        if !same_breaks {
            return Damage::Whole;
        }
        let mut area: Option<[f32; 4]> = None;
        let mut add = |b: [f32; 4]| {
            if b[2] <= b[0] || b[3] <= b[1] {
                return;
            }
            area = Some(match area {
                None => b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        };
        let mut changed = |kind: usize, i: usize| {
            add(self.paint[kind][i]);
            add(before.paint[kind][i]);
        };
        for i in 0..self.rects.len() {
            if !same_rect(&self.rects[i], &before.rects[i]) {
                changed(0, i);
            }
        }
        for i in 0..self.images.len() {
            if !same_image(&self.images[i], &before.images[i]) {
                changed(1, i);
            }
        }
        for i in 0..self.shaders.len() {
            if !same_shader(&self.shaders[i], &before.shaders[i]) {
                changed(2, i);
            }
        }
        for i in 0..self.emitters.len() {
            if !same_emitter(&self.emitters[i], &before.emitters[i]) {
                changed(3, i);
            }
        }
        for i in 0..self.texts.len() {
            if !same_text(&self.texts[i], &before.texts[i]) {
                changed(4, i);
            }
        }
        let Some(a) = area else { return Damage::None };
        // Out to whole pixels, inside the surface.
        let (w, h) = (viewport[0] as f32, viewport[1] as f32);
        let x0 = a[0].floor().clamp(0.0, w);
        let y0 = a[1].floor().clamp(0.0, h);
        let x1 = a[2].ceil().clamp(0.0, w);
        let y1 = a[3].ceil().clamp(0.0, h);
        if x1 <= x0 || y1 <= y0 {
            return Damage::None;
        }
        if (x1 - x0) * (y1 - y0) > 0.5 * w * h {
            return Damage::Whole;
        }
        Damage::Within([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
    }

    /// Only what can leave paint inside `area` (x, y, width, height), in the
    /// same layers and order: what a frame drawn again within it needs.
    pub fn within(&self, area: [u32; 4]) -> DrawList {
        let r = [area[0] as f32, area[1] as f32, (area[0] + area[2]) as f32, (area[1] + area[3]) as f32];
        // The fields whole: the rects kept name them by place.
        let mut out = DrawList { fields: self.fields.clone(), ..DrawList::default() };
        for (from, to) in self.layers() {
            for i in from.rects..to.rects {
                if meets(&self.paint[0][i], &r) {
                    out.rects.push(self.rects[i]);
                    out.paint[0].push(self.paint[0][i]);
                }
            }
            for i in from.images..to.images {
                if meets(&self.paint[1][i], &r) {
                    out.images.push(self.images[i].clone());
                    out.paint[1].push(self.paint[1][i]);
                }
            }
            for i in from.shaders..to.shaders {
                if meets(&self.paint[2][i], &r) {
                    out.shaders.push(self.shaders[i].clone());
                    out.paint[2].push(self.paint[2][i]);
                }
            }
            for i in from.emitters..to.emitters {
                if meets(&self.paint[3][i], &r) {
                    out.emitters.push(self.emitters[i].clone());
                    out.paint[3].push(self.paint[3][i]);
                }
            }
            for i in from.texts..to.texts {
                if meets(&self.paint[4][i], &r) {
                    out.texts.push(self.texts[i].clone());
                    out.paint[4].push(self.paint[4][i]);
                }
            }
            let mark = out.mark();
            if mark.total() > out.breaks.last().map_or(0, |b| b.total()) {
                out.breaks.push(mark);
            }
        }
        // The last layer ends at the list's end, not at a break.
        if out.breaks.last().is_some_and(|b| b.total() == out.mark().total()) {
            out.breaks.pop();
        }
        out
    }

    /// Where anything in it leaves paint, physical pixels, x0, y0, x1, y1;
    /// none when it is empty.
    pub fn paint_bounds(&self) -> Option<[f32; 4]> {
        self.paint.iter().flatten().fold(None, |acc: Option<[f32; 4]>, b| {
            if b[2] <= b[0] || b[3] <= b[1] {
                return acc;
            }
            Some(match acc {
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
                None => *b,
            })
        })
    }

    /// A number that is the same for two lists that draw the same pixels:
    /// what a blurred group is cached by.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for b in &self.breaks {
            (b.rects, b.images, b.shaders, b.emitters, b.texts).hash(&mut h);
        }
        for r in &self.rects {
            bytemuck::bytes_of(r).hash(&mut h);
        }
        for f in &self.fields {
            f.key.hash(&mut h);
        }
        for i in &self.images {
            i.key.hash(&mut h);
            for v in i.origin.iter().chain(&i.size).chain(&i.uv).chain(&i.clip).chain(&i.fade).chain(i.transform.iter().flatten()) {
                v.to_bits().hash(&mut h);
            }
            (i.width, i.height, i.radius.to_bits(), i.alpha.to_bits()).hash(&mut h);
            i.filter.map(f32::to_bits).hash(&mut h);
            i.mask.hash(&mut h);
        }
        for sh in &self.shaders {
            sh.key.hash(&mut h);
            for v in sh.origin.iter().chain(&sh.size).chain(&sh.clip).chain(&sh.fade).chain(&sh.values).chain(&sh.pointer).chain(sh.transform.iter().flatten()) {
                v.to_bits().hash(&mut h);
            }
            (sh.scale.to_bits(), sh.time.to_bits(), sh.alpha.to_bits()).hash(&mut h);
            sh.backdrop.map(|(k, b)| (k, b.map(f32::to_bits))).hash(&mut h);
        }
        for e in &self.emitters {
            (e.key, e.generation).hash(&mut h);
            bytemuck::bytes_of(&e.params).hash(&mut h);
        }
        for t in &self.texts {
            (Arc::as_ptr(&t.buffer) as usize).hash(&mut h);
            for v in t.origin.iter().chain(&t.color).chain(&t.clip) {
                v.to_bits().hash(&mut h);
            }
            t.scale.to_bits().hash(&mut h);
        }
        for g in &self.groups {
            g.key.hash(&mut h);
        }
        h.finish()
    }

    /// The same list moved by `-dx, -dy`: drawn into a texture whose corner
    /// is that point of the surface. The move is composed after each item's
    /// own matrix, so what depth turned stays turned; clips are measured
    /// before the matrix and are moved with it.
    pub fn shifted(mut self, dx: f32, dy: f32) -> DrawList {
        // Row vectors: p · M · T, T's last row the move.
        let after = |m: &mut [[f32; 4]; 4]| {
            for row in m.iter_mut() {
                row[0] -= dx * row[3];
                row[1] -= dy * row[3];
            }
        };
        let clip = |c: &mut [f32; 4]| {
            c[0] -= dx;
            c[1] -= dy;
            c[2] -= dx;
            c[3] -= dy;
        };
        for r in &mut self.rects {
            r.origin[0] -= dx;
            r.origin[1] -= dy;
            clip(&mut r.clip);
            r.fade[0] -= dy;
        }
        for i in &mut self.images {
            i.origin[0] -= dx;
            i.origin[1] -= dy;
            clip(&mut i.clip);
            i.fade[0] -= dy;
        }
        for sh in &mut self.shaders {
            sh.origin[0] -= dx;
            sh.origin[1] -= dy;
            clip(&mut sh.clip);
            sh.fade[0] -= dy;
        }
        for e in &mut self.emitters {
            e.params.points[0] -= dx;
            e.params.points[1] -= dy;
            e.params.points[2] -= dx;
            e.params.points[3] -= dy;
            e.params.bend[0] -= dx;
            e.params.bend[1] -= dy;
            clip(&mut e.params.clip);
        }
        for t in &mut self.texts {
            t.origin[0] -= dx;
            t.origin[1] -= dy;
            clip(&mut t.clip);
        }
        for p in self.paint.iter_mut().flatten() {
            clip(p);
        }
        // The matrices turn about surface points: moved before and after,
        // they turn the same way about the moved ones.
        let before = |m: &mut [[f32; 4]; 4]| {
            // p · T⁻¹ · M · T with T⁻¹ adding the move back: row 3 of M gains
            // dx · row 0 + dy · row 1.
            let (r0, r1) = (m[0], m[1]);
            for (c, v) in m[3].iter_mut().enumerate() {
                *v += dx * r0[c] + dy * r1[c];
            }
        };
        for r in &mut self.rects {
            if r.transform != Rect::IDENTITY {
                before(&mut r.transform);
                after(&mut r.transform);
            }
        }
        for i in &mut self.images {
            if i.transform != Rect::IDENTITY {
                before(&mut i.transform);
                after(&mut i.transform);
            }
        }
        for sh in &mut self.shaders {
            if sh.transform != Rect::IDENTITY {
                before(&mut sh.transform);
                after(&mut sh.transform);
            }
        }
        for e in &mut self.emitters {
            if e.params.transform != Rect::IDENTITY {
                before(&mut e.params.transform);
                after(&mut e.params.transform);
            }
        }
        self
    }

    /// `other`'s layers after all of these.
    pub fn append(&mut self, other: DrawList) {
        self.groups.extend(other.groups);
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
        // Its fields join these: a shape that names one by place names it
        // by its place here.
        let places: Vec<usize> = other.fields.into_iter().map(|f| self.push_field(f)).collect();
        self.rects.extend(other.rects.into_iter().map(|mut r| {
            for i in 0..4 {
                let entry = &mut r.shapes[i * 2 + 1];
                if entry[1] > 1.5 {
                    entry[2] = places.get(entry[2] as usize).copied().unwrap_or(0) as f32;
                }
            }
            r
        }));
        self.images.extend(other.images);
        self.shaders.extend(other.shaders);
        self.emitters.extend(other.emitters);
        self.texts.extend(other.texts);
        for (mine, theirs) in self.paint.iter_mut().zip(other.paint) {
            mine.extend(theirs);
        }
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
    Draw { id: SurfaceId, width: u32, height: u32, list: Box<DrawList> },
    /// With a sender, answered once the swapchain is gone.
    Destroy { id: SurfaceId, done: Option<std::sync::mpsc::Sender<()>> },
    /// Drawn into a texture of its own and read back: straight RGBA.
    Snapshot { width: u32, height: u32, list: Box<DrawList>, reply: mpsc::Sender<Result<Vec<u8>, String>> },
}

/// One surface's last frames as the render thread measured them, for the
/// profiling overlay.
#[derive(Clone, Debug, Default)]
pub struct SurfaceStats {
    pub frames: u64,
    /// Frames drawn in the last second.
    pub fps: f32,
    /// The last frame's drawing, and its wait for a buffer, milliseconds.
    pub render_ms: f32,
    pub wait_ms: f32,
    /// What the last frame drew again, physical pixels (x, y, width,
    /// height); the whole surface for a whole frame, none for nothing.
    pub damage: Option<[u32; 4]>,
    /// Drawn into a canvas of its own, only what changes.
    pub canvas: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub surfaces: HashMap<SurfaceId, SurfaceStats>,
    /// Textures kept (pictures, blurred subtrees) and the bytes of those,
    /// the distance fields' atlas and the canvases together.
    pub textures: usize,
    pub texture_bytes: u64,
}

pub(crate) type SharedStats = Arc<Mutex<Stats>>;

/// The main thread's handle on the render thread.
pub struct Renderer {
    tx: mpsc::Sender<Message>,
    info: wgpu::AdapterInfo,
    stats: SharedStats,
}

impl Renderer {
    /// Starts the render thread and opens the device there, on the adapter at
    /// `pci_bus_id` when one matches (see [`gpu::Gpu::new`]).
    pub fn spawn(pci_bus_id: Option<String>, fonts: Fonts) -> Result<Renderer, Error> {
        let (tx, rx) = mpsc::channel::<Message>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stats: SharedStats = Arc::default();
        let shared = stats.clone();
        thread::Builder::new()
            .name("genoma-render".into())
            .spawn(move || {
                let mut gpu = match gpu::Gpu::new(pci_bus_id.as_deref(), fonts, shared) {
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
                            gpu.draw(id, width, height, *list)
                        }
                        Message::Snapshot { width, height, list, reply } => {
                            let _ = reply.send(gpu.snapshot(width, height, &list));
                        }
                        Message::Destroy { id, done } => {
                            gpu.destroy_target(id);
                            if let Some(done) = done {
                                let _ = done.send(());
                            }
                        }
                    }
                }
            })
            .map_err(|_| Error::Thread)?;
        let info = ready_rx.recv().map_err(|_| Error::Thread)??;
        Ok(Renderer { tx, info, stats })
    }

    /// What the render thread last measured, for the profiling overlay.
    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
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
        let _ = self.tx.send(Message::Draw { id, width, height, list: Box::new(list) });
    }

    /// `list` drawn at this size into a texture rather than a surface, and
    /// read back: RGBA, straight alpha, a row after another. What
    /// `genoma snapshot` writes as a picture; it waits for the GPU.
    pub fn snapshot(&self, width: u32, height: u32, list: DrawList) -> Result<Vec<u8>, String> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Message::Snapshot { width, height, list: Box::new(list), reply }).map_err(|_| "the render thread is gone".to_owned())?;
        answer.recv().map_err(|_| "the render thread is gone".to_owned())?
    }

    pub fn destroy_target(&self, id: SurfaceId) {
        let _ = self.tx.send(Message::Destroy { id, done: None });
    }

    /// Destroys the target and waits until the renderer has let go of the
    /// surface, every frame sent before included: the surface itself can
    /// then be destroyed. A surface destroyed while the renderer still
    /// presents to it is a protocol error, and the end of the connection.
    pub fn destroy_target_now(&self, id: SurfaceId) {
        let (done, wait) = std::sync::mpsc::channel();
        if self.tx.send(Message::Destroy { id, done: Some(done) }).is_ok() {
            let _ = wait.recv_timeout(std::time::Duration::from_secs(2));
        }
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
    /// The built-in pipelines' WGSL, checked as wgpu would at start: a
    /// mistake here otherwise shows first as a panic on the render thread.
    #[test]
    fn every_built_in_pipeline_compiles() {
        use wgpu::naga;
        for (name, source) in [
            ("rect", crate::rect::SHADER),
            ("image", crate::image::SHADER),
            ("particles", crate::particles::SHADER),
            ("blur", crate::blur::SOURCE),
        ] {
            let module = naga::front::wgsl::parse_str(source).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

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
            filter: Image::NO_FILTER,
            mask: None,
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

    fn scene(moved: f32) -> DrawList {
        let mut list = DrawList::default();
        list.push_rect(rect(0.0));
        list.push_image(image(200.0));
        list.push_rect(rect(400.0 + moved));
        list
    }

    #[test]
    fn the_same_frame_is_no_damage() {
        assert_eq!(scene(0.0).damage(&scene(0.0), [1000, 100]), Damage::None);
    }

    #[test]
    fn a_moved_shape_damages_where_it_was_and_is() {
        // At 400 and then 405, ten wide, a pixel past each side.
        assert_eq!(scene(5.0).damage(&scene(0.0), [1000, 100]), Damage::Within([399, 0, 17, 11]));
    }

    #[test]
    fn a_different_count_is_the_whole_surface() {
        let mut more = scene(0.0);
        more.push_rect(rect(600.0));
        assert_eq!(more.damage(&scene(0.0), [1000, 100]), Damage::Whole);
    }

    #[test]
    fn a_change_over_most_of_it_is_the_whole_surface() {
        let (mut a, mut b) = (DrawList::default(), DrawList::default());
        a.push_rect(rect(0.0));
        b.push_rect(rect(4.0));
        assert_eq!(b.damage(&a, [20, 12]), Damage::Whole);
    }

    #[test]
    fn within_keeps_only_what_paints_there() {
        let list = scene(0.0);
        let part = list.within([395, 0, 20, 10]);
        assert_eq!((part.rects.len(), part.images.len()), (1, 0));
        assert_eq!(part.rects[0].origin, [400.0, 0.0]);
        let part = list.within([0, 0, 1000, 10]);
        assert_eq!((part.rects.len(), part.images.len()), (2, 1));
    }

    #[test]
    fn within_keeps_the_layers_order() {
        // A shape over a picture: two layers, and both kept where they meet.
        let mut list = DrawList::default();
        list.push_image(image(0.0));
        list.push_rect(rect(5.0));
        let part = list.within([0, 0, 20, 10]);
        let layers: Vec<_> = part.layers().collect();
        assert_eq!(layers.len(), 2);
        assert_eq!((layers[0].1.images, layers[1].0.rects, layers[1].1.rects), (1, 0, 1));
    }
}
