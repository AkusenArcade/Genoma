//! Images from files (PNG, JPEG, WebP, GIF, SVG) for `g.image`: a
//! wallpaper, a cover, a thumbnail. An animated GIF, WebP or PNG is decoded
//! into all its frames, each fitted as a still is, and each with its time;
//! the scene picks the frame for the moment and sleeps until the next. Each is decoded and fitted to the exact physical size
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
    /// An animation: each frame's pixels and how long it is shown, seconds.
    /// Kept whole here (frames are not let go after upload).
    Frames(Arc<Vec<(Arc<Vec<u8>>, f32)>>),
    /// On the GPU, its pixels let go here.
    Uploaded,
    Failed,
}

/// Pixels this large are let go once the GPU holds them: a wallpaper's
/// 20 MB, not an icon's few kilobytes.
const LET_GO: usize = 1 << 20;

fn nothing() -> Arc<Vec<u8>> {
    static EMPTY: std::sync::LazyLock<Arc<Vec<u8>>> = std::sync::LazyLock::new(|| Arc::new(Vec::new()));
    EMPTY.clone()
}

/// What a decode gives: one picture, or an animation's frames.
enum Decoded {
    Still(Vec<u8>),
    Frames(Vec<(Arc<Vec<u8>>, f32)>),
}

/// The most frames kept of one animation, and the most bytes: past either,
/// it plays the frames it has.
const MOST_FRAMES: usize = 600;
const MOST_FRAME_BYTES: usize = 256 << 20;

/// A request's result, handed back by the thread that decoded it.
type Done = Arc<Mutex<Vec<(u64, Option<Decoded>)>>>;

struct Request {
    key: u64,
    source: String,
    width: u32,
    height: u32,
    fit: Fit,
    radius: f32,
    soften: f32,
    /// Only this part of the fitted picture is kept: x, y, w, h.
    crop: Option<[u32; 4]>,
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
    /// with corners of `radius`, blurred by `soften` of those pixels,
    /// premultiplied RGBA, or `None` while they are being made (or when they
    /// cannot be).
    /// The pixels of `source` fitted to `width` × `height`, keeping only
    /// `crop` (x, y, w, h of the fitted picture) when one is given. For an
    /// animation, the frame shown `at` seconds into it (looping), with the
    /// seconds left until the next; for a still, no seconds.
    #[allow(clippy::too_many_arguments)]
    pub fn get_part(
        &mut self,
        source: &str,
        width: u32,
        height: u32,
        fit: Fit,
        radius: f32,
        soften: f32,
        crop: Option<[u32; 4]>,
        at: f32,
    ) -> Option<(u64, Arc<Vec<u8>>, Option<f32>)> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("image", source, width, height, fit, (radius * 4.0).round() as i32, (soften * 4.0).round() as i32, crop).hash(&mut h);
        let key = h.finish();
        let frame = self.frame;
        if let Some((entry, used)) = self.cache.get_mut(&key) {
            *used = frame;
            match entry {
                Entry::Ready(pixels) => {
                    if pixels.len() >= LET_GO && genoma_render::resident(key) {
                        *entry = Entry::Uploaded;
                        return Some((key, nothing(), None));
                    }
                    return Some((key, pixels.clone(), None));
                }
                Entry::Frames(frames) => {
                    let (i, left) = frame_at(frames, at);
                    let frame_key = key ^ (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    return Some((frame_key, frames[i].0.clone(), Some(left)));
                }
                Entry::Uploaded if genoma_render::resident(key) => return Some((key, nothing(), None)),
                // Dropped from the GPU since: decoded again.
                Entry::Uploaded => {
                    self.cache.remove(&key);
                }
                _ => return None,
            }
        }
        self.cache.insert(key, (Entry::Pending, frame));
        let request = Request { key, source: source.to_owned(), width, height, fit, radius, soften, crop };
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
                let pixels = decode(&next.source, next.width, next.height, next.fit, next.radius, next.soften, next.crop);
                log::debug!("g.image {} decoded at {}x{}", next.source, next.width, next.height);
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
                    Some(Decoded::Still(p)) => Entry::Ready(Arc::new(p)),
                    Some(Decoded::Frames(f)) => Entry::Frames(Arc::new(f)),
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

/// The frame shown `at` seconds into an animation that loops, and the
/// seconds left before the next.
fn frame_at(frames: &[(Arc<Vec<u8>>, f32)], at: f32) -> (usize, f32) {
    let total: f32 = frames.iter().map(|f| f.1).sum();
    if frames.len() < 2 || total <= 0.0 {
        return (0, f32::INFINITY);
    }
    let mut t = at.rem_euclid(total);
    for (i, (_, d)) in frames.iter().enumerate() {
        if t < *d {
            return (i, d - t);
        }
        t -= d;
    }
    (frames.len() - 1, frames[frames.len() - 1].1)
}

/// An animated GIF, WebP or PNG's frames, whole-canvas and with their
/// times; `None` for a file that is not one (or holds one frame).
fn animation(path: &Path) -> Option<Vec<(image::RgbaImage, f32)>> {
    use image::AnimationDecoder;
    let reader = || -> Option<std::io::BufReader<std::fs::File>> { Some(std::io::BufReader::new(std::fs::File::open(path).ok()?)) };
    let format = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.format()?;
    let frames = match format {
        image::ImageFormat::Gif => image::codecs::gif::GifDecoder::new(reader()?).ok()?.into_frames(),
        image::ImageFormat::WebP => {
            let d = image::codecs::webp::WebPDecoder::new(reader()?).ok()?;
            if !d.has_animation() {
                return None;
            }
            d.into_frames()
        }
        image::ImageFormat::Png => {
            let d = image::codecs::png::PngDecoder::new(reader()?).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            d.apng().ok()?.into_frames()
        }
        _ => return None,
    };
    let mut out = Vec::new();
    for frame in frames.take(MOST_FRAMES) {
        let frame = frame.ok()?;
        let (n, d) = frame.delay().numer_denom_ms();
        let ms = n as f32 / d.max(1) as f32;
        // As browsers do: a delay of 10 ms or less is a tenth of a second.
        let seconds = if ms <= 10.0 { 0.1 } else { ms / 1000.0 };
        out.push((frame.into_buffer(), seconds));
    }
    (out.len() > 1).then_some(out)
}

/// One picture (a still, or a frame) made what the node shows: fitted,
/// softened, rounded, cropped and premultiplied.
#[allow(clippy::too_many_arguments)]
fn finish(image: &image::RgbaImage, width: u32, height: u32, fit: Fit, radius: f32, soften: f32, crop: Option<[u32; 4]>) -> Vec<u8> {
    let mut out = fitted(image, width, height, fit);
    // Once, here, off the frame: a backdrop costs its decode and nothing
    // after.
    if soften >= 0.5 {
        out = image::imageops::fast_blur(&out, soften);
    }
    round(&mut out, width, height, radius);
    if let Some([x, y, w, h]) = crop {
        out = image::imageops::crop_imm(&out, x, y, w, h).to_image();
    }
    premultiply(&mut out);
    out.into_raw()
}

fn decode(source: &str, width: u32, height: u32, fit: Fit, radius: f32, soften: f32, crop: Option<[u32; 4]>) -> Option<Decoded> {
    if width == 0 || height == 0 {
        return None;
    }
    let path = Path::new(source);
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        return Some(Decoded::Still(finish(&svg(path, width, height)?, width, height, fit, radius, soften, crop)));
    }
    if let Some(frames) = animation(path) {
        let mut out = Vec::with_capacity(frames.len());
        let mut bytes = 0;
        for (image, seconds) in frames {
            let pixels = finish(&image, width, height, fit, radius, soften, crop);
            bytes += pixels.len();
            out.push((Arc::new(pixels), seconds));
            if bytes > MOST_FRAME_BYTES {
                log::warn!("g.image {source}: {} frames kept, the rest would pass {} MB", out.len(), MOST_FRAME_BYTES >> 20);
                break;
            }
        }
        return Some(Decoded::Frames(out));
    }
    let image = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.into_rgba8();
    Some(Decoded::Still(finish(&image, width, height, fit, radius, soften, crop)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_animation_loops_through_its_frames_in_their_times() {
        let f = |d| (nothing(), d);
        let frames = [f(0.1), f(0.2), f(0.1)];
        let near = |(i, left): (usize, f32), (j, l): (usize, f32)| i == j && (left - l).abs() < 1e-5;
        assert!(near(frame_at(&frames, 0.0), (0, 0.1)));
        assert!(near(frame_at(&frames, 0.15), (1, 0.15)));
        assert!(near(frame_at(&frames, 0.35), (2, 0.05)));
        assert!(near(frame_at(&frames, 0.45), (0, 0.05)), "loops");
        assert_eq!(frame_at(&frames[..1], 3.0).0, 0);
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
