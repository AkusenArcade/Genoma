//! Icons rasterised for the renderer (Bioma's `components/Icon.qml`): an SVG
//! with `currentColor` painted in the node's colour, or the 165° gradient,
//! drawn at the exact physical size it is shown.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

use resvg::{tiny_skia, usvg};

#[derive(Default)]
pub struct Icons {
    pub dirs: Vec<PathBuf>,
    cache: HashMap<u64, Option<Arc<Vec<u8>>>>,
}

fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("rgb({},{},{})", b(c[0]), b(c[1]), b(c[2]))
}

impl Icons {
    fn find(&self, source: &str) -> Option<PathBuf> {
        let path = Path::new(source);
        if path.is_absolute() {
            return path.exists().then(|| path.to_owned());
        }
        self.dirs.iter().map(|d| d.join(format!("{source}.svg"))).find(|p| p.exists())
    }

    /// The cache key and the pixels, or `None` if the source cannot be drawn.
    pub fn raster(
        &mut self,
        source: &str,
        colour: [f32; 4],
        gradient: Option<([f32; 4], [f32; 4])>,
        pixels: u32,
    ) -> Option<(u64, Arc<Vec<u8>>)> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (source, hex(colour), gradient.map(|(a, b)| (hex(a), hex(b))), pixels).hash(&mut h);
        let key = h.finish();
        if let Some(found) = self.cache.get(&key) {
            return found.clone().map(|p| (key, p));
        }
        let pixmap = self.find(source).and_then(|path| render(&path, colour, gradient, pixels));
        let pixels = pixmap.map(|p| Arc::new(p.take()));
        self.cache.insert(key, pixels.clone());
        pixels.map(|p| (key, p))
    }
}

fn render(
    path: &Path,
    colour: [f32; 4],
    gradient: Option<([f32; 4], [f32; 4])>,
    pixels: u32,
) -> Option<tiny_skia::Pixmap> {
    let mut target = tiny_skia::Pixmap::new(pixels, pixels)?;
    if path.extension().is_some_and(|e| e == "png") {
        // An application's raster icon, fitted to the square.
        let image = tiny_skia::Pixmap::load_png(path).ok()?;
        let s = pixels as f32 / image.width().max(image.height()) as f32;
        target.draw_pixmap(
            0,
            0,
            image.as_ref(),
            &tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() },
            tiny_skia::Transform::from_scale(s, s),
            None,
        );
        return Some(target);
    }
    let source = std::fs::read_to_string(path).ok()?;
    let paint = match gradient {
        Some(_) => "url(#genomaGradient)".to_string(),
        None => hex(colour),
    };
    let mut body = source.replace("currentColor", &paint);
    if let Some((top, bottom)) = gradient {
        // 165°, from the top colour to the bottom one, as Icon.qml writes it.
        let r = 165f32.to_radians();
        let (dx, dy) = (r.sin() / 2.0, -r.cos() / 2.0);
        let defs = format!(
            "<defs><linearGradient id=\"genomaGradient\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">\
             <stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/></linearGradient></defs>",
            0.5 - dx, 0.5 - dy, 0.5 + dx, 0.5 + dy, hex(top), hex(bottom)
        );
        if let Some(end) = body.find('>') {
            body.insert_str(end + 1, &defs);
        }
    }
    let tree = usvg::Tree::from_str(&body, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let s = pixels as f32 / size.width().max(size.height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut target.as_mut());
    // The colour's own alpha, for a muted icon.
    if colour[3] < 1.0 && gradient.is_none() {
        for px in target.pixels_mut() {
            let c = px.demultiply();
            let a = (c.alpha() as f32 * colour[3]).round() as u8;
            *px = tiny_skia::ColorU8::from_rgba(c.red(), c.green(), c.blue(), a).premultiply();
        }
    }
    Some(target)
}
