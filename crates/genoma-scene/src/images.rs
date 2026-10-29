//! Images from files (PNG, JPEG, WebP, SVG) for `g.image`: a wallpaper, a
//! cover, a thumbnail. Each is decoded and fitted to the exact physical size
//! it is shown at, with its corners rounded, on a thread of its own: a 4K
//! wallpaper takes a tenth of a second to decode, and the frame that asked
//! for it must not wait. Until it is ready the node draws nothing, and the
//! scene looks again on a short beat.
//!
//! A few threads take the requests in turn, not one each: a grid of fifty
//! photographs decoded at once is fifty full-size frames in memory together
//! (a 4K one is 33 MB), and the machine taken from whatever else it does.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

/// How an image fills its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Fit {
    /// The whole box, the image cropped about its centre: a wallpaper.
    #[default]
    Cover,
    /// The whole image, inside the box: a picture to be seen entire.
    Contain,
    /// Stretched to the box.
    Fill,
}

enum Entry {
    Pending,
    Ready(Arc<Vec<u8>>),
    Failed,
}

/// A request's result, handed back by the thread that decoded it.
type Done = Arc<Mutex<Vec<(u64, Option<Arc<Vec<u8>>>)>>>;

struct Request {
    key: u64,
    source: String,
    width: u32,
    height: u32,
    fit: Fit,
    radius: f32,
}

/// The requests waiting, newest last, and how many threads take them.
#[derive(Default)]
struct Queue {
    waiting: Mutex<(std::collections::VecDeque<Request>, usize)>,
}

const WORKERS: usize = 3;

#[derive(Default)]
pub struct Images {
    cache: HashMap<u64, (Entry, u64)>,
    done: Done,
    queue: Arc<Queue>,
    /// Counts the frames drawn, to let go of what no frame has asked for in
    /// a while.
    frame: u64,
}

/// How long the scene waits before looking again while an image decodes.
pub const BEAT: Duration = Duration::from_millis(16);

impl Images {
    /// The pixels of `source` fitted to `width` × `height` physical pixels
    /// with corners of `radius`, premultiplied RGBA, or `None` while they
    /// are being made (or when they cannot be).
    pub fn get(&mut self, source: &str, width: u32, height: u32, fit: Fit, radius: f32) -> Option<(u64, Arc<Vec<u8>>)> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("image", source, width, height, fit, (radius * 4.0).round() as i32).hash(&mut h);
        let key = h.finish();
        let frame = self.frame;
        if let Some((entry, used)) = self.cache.get_mut(&key) {
            *used = frame;
            return match entry {
                Entry::Ready(pixels) => Some((key, pixels.clone())),
                _ => None,
            };
        }
        self.cache.insert(key, (Entry::Pending, frame));
        let request = Request { key, source: source.to_owned(), width, height, fit, radius };
        let mut waiting = self.queue.waiting.lock().unwrap();
        waiting.0.push_back(request);
        // A thread more while there are fewer than WORKERS; each takes
        // requests until none is left, then ends.
        if waiting.1 < WORKERS {
            waiting.1 += 1;
            let (queue, done) = (self.queue.clone(), self.done.clone());
            let spawned = std::thread::Builder::new().name("genoma-image".into()).spawn(move || loop {
                let next = {
                    let mut waiting = queue.waiting.lock().unwrap();
                    match waiting.0.pop_front() {
                        Some(r) => r,
                        None => {
                            waiting.1 -= 1;
                            return;
                        }
                    }
                };
                let pixels = decode(&next.source, next.width, next.height, next.fit, next.radius).map(Arc::new);
                if pixels.is_none() {
                    log::warn!("g.image {}: could not be read", next.source);
                }
                done.lock().unwrap().push((next.key, pixels));
            });
            if spawned.is_err() {
                waiting.1 -= 1;
            }
        }
        None
    }

    /// Takes in what the threads finished: true when something new can be
    /// drawn.
    pub fn collect(&mut self) -> bool {
        let finished: Vec<_> = std::mem::take(&mut *self.done.lock().unwrap());
        let any = !finished.is_empty();
        for (key, pixels) in finished {
            if let Some((entry, _)) = self.cache.get_mut(&key) {
                *entry = match pixels {
                    Some(p) => Entry::Ready(p),
                    None => Entry::Failed,
                };
            }
        }
        any
    }

    /// Whether something is still being decoded.
    pub fn pending(&self) -> bool {
        self.cache.values().any(|(e, _)| matches!(e, Entry::Pending))
    }

    /// After a frame: what no frame has asked for in a while is let go (a
    /// wallpaper is 8 MB at 1080p), and what failed is asked again later.
    pub fn sweep(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        self.cache.retain(|_, (entry, used)| match entry {
            Entry::Pending => true,
            _ => frame - *used < 240,
        });
    }
}

fn decode(source: &str, width: u32, height: u32, fit: Fit, radius: f32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let path = Path::new(source);
    let image = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        svg(path, width, height)?
    } else {
        image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.into_rgba8()
    };
    let mut out = fitted(&image, width, height, fit);
    round(&mut out, width, height, radius);
    premultiply(&mut out);
    Some(out.into_raw())
}

/// An SVG drawn at the size it is shown, so it stays sharp.
fn svg(path: &Path, width: u32, height: u32) -> Option<image::RgbaImage> {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = (width as f32 / size.width()).max(height as f32 / size.height());
    let w = (size.width() * scale).ceil() as u32;
    let h = (size.height() * scale).ceil() as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia's pixels are premultiplied; the fit works on straight ones.
    let mut raw = pixmap.take();
    for px in raw.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(w.max(1), h.max(1), raw)
}

fn fitted(image: &image::RgbaImage, width: u32, height: u32, fit: Fit) -> image::RgbaImage {
    use image::imageops::{self, FilterType};
    let (iw, ih) = (image.width() as f32, image.height() as f32);
    match fit {
        Fit::Fill => imageops::resize(image, width, height, FilterType::Triangle),
        Fit::Cover => {
            // The part of the image with the box's shape, about its centre.
            let scale = (width as f32 / iw).max(height as f32 / ih);
            let cw = (width as f32 / scale).round().clamp(1.0, iw) as u32;
            let ch = (height as f32 / scale).round().clamp(1.0, ih) as u32;
            let x = ((iw as u32).saturating_sub(cw)) / 2;
            let y = ((ih as u32).saturating_sub(ch)) / 2;
            let crop = imageops::crop_imm(image, x, y, cw, ch).to_image();
            imageops::resize(&crop, width, height, FilterType::Triangle)
        }
        Fit::Contain => {
            let scale = (width as f32 / iw).min(height as f32 / ih);
            let w = (iw * scale).round().max(1.0) as u32;
            let h = (ih * scale).round().max(1.0) as u32;
            let inner = imageops::resize(image, w, h, FilterType::Triangle);
            let mut out = image::RgbaImage::new(width, height);
            imageops::overlay(&mut out, &inner, ((width - w.min(width)) / 2) as i64, ((height - h.min(height)) / 2) as i64);
            out
        }
    }
}

/// The corners cut to a radius, antialiased across one pixel.
fn round(image: &mut image::RgbaImage, width: u32, height: u32, radius: f32) {
    let r = radius.min(width as f32 / 2.0).min(height as f32 / 2.0);
    if r <= 0.0 {
        return;
    }
    let ri = r.ceil() as u32;
    for y in 0..height {
        let dy = if y < ri { r - (y as f32 + 0.5) } else if y >= height - ri { (y as f32 + 0.5) - (height as f32 - r) } else { continue };
        for x in (0..ri).chain(width.saturating_sub(ri)..width) {
            let dx = if x < ri { r - (x as f32 + 0.5) } else { (x as f32 + 0.5) - (width as f32 - r) };
            if dx <= 0.0 || dy <= 0.0 {
                continue;
            }
            let coverage = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let px = image.get_pixel_mut(x, y);
            px[3] = (px[3] as f32 * coverage).round() as u8;
        }
    }
}

fn premultiply(image: &mut image::RgbaImage) {
    for px in image.pixels_mut() {
        let a = px[3] as u32;
        if a < 255 {
            for c in 0..3 {
                px[c] = ((px[c] as u32 * a + 127) / 255) as u8;
            }
        }
    }
}
