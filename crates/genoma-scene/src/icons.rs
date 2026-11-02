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
    pub(crate) cache: HashMap<u64, Option<Arc<Vec<u8>>>>,
}

fn hex(c: [f32; 4]) -> String {
    let [r, g, b, _] = bytes(c);
    format!("rgb({r},{g},{b})")
}

fn bytes(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The `viewBox` (x, y, width, height), else the width and height, else 24.
fn view_box(svg: &str) -> [f32; 4] {
    let attr = |name: &str| -> Option<&str> {
        let start = svg.find(&format!("{name}=\""))? + name.len() + 2;
        svg[start..].split('"').next()
    };
    if let Some(v) = attr("viewBox") {
        let n: Vec<f32> = v.split([' ', ',']).filter_map(|t| t.parse().ok()).collect();
        if n.len() == 4 {
            return [n[0], n[1], n[2], n[3]];
        }
    }
    let side = |name| attr(name).and_then(|v| v.trim_end_matches("px").parse().ok()).unwrap_or(24.0);
    [0.0, 0.0, side("width"), side("height")]
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
        (source, bytes(colour), gradient.map(|(a, b)| (bytes(a), bytes(b))), pixels).hash(&mut h);
        let key = h.finish();
        if let Some(found) = self.cache.get(&key) {
            return found.clone().map(|p| (key, p));
        }
        // A name is one of the shell's own glyphs, painted in its role; a path
        // is an application's icon, drawn in its own colours.
        let recolour = !Path::new(source).is_absolute();
        let pixmap = self.find(source).and_then(|path| render(&path, recolour.then_some(colour), gradient, pixels));
        let pixels = pixmap.map(|p| Arc::new(p.take()));
        self.cache.insert(key, pixels.clone());
        pixels.map(|p| (key, p))
    }
}

fn render(
    path: &Path,
    colour: Option<[f32; 4]>,
    gradient: Option<([f32; 4], [f32; 4])>,
    pixels: u32,
) -> Option<tiny_skia::Pixmap> {
    let mut target = tiny_skia::Pixmap::new(pixels, pixels)?;
    if path.extension().is_some_and(|e| e == "png") {
        // An application's raster icon, fitted to the square. Halved first
        // until one step is left: a single bicubic pass from 256 to 24 reads
        // one pixel in ten and the picture comes out jagged.
        let mut image = tiny_skia::Pixmap::load_png(path).ok()?;
        while image.width().max(image.height()) >= pixels * 2 {
            let (w, h) = ((image.width() / 2).max(1), (image.height() / 2).max(1));
            let mut half = tiny_skia::Pixmap::new(w, h)?;
            half.draw_pixmap(
                0,
                0,
                image.as_ref(),
                &tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() },
                tiny_skia::Transform::from_scale(w as f32 / image.width() as f32, h as f32 / image.height() as f32),
                None,
            );
            image = half;
        }
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
    let mut body = source;
    // Two equal stops are a colour, its alpha kept: the gradient's stops
    // would carry the colour but not the muting.
    let (colour, gradient) = match gradient {
        Some((top, bottom)) if colour.is_some() && bytes(top) == bytes(bottom) => (Some(top), None),
        other => (colour, other),
    };
    if let Some(colour) = colour {
        let paint = if gradient.is_some() { "url(#genomaGradient)".to_string() } else { hex(colour) };
        body = body.replace("currentColor", &paint);
    }
    if let (Some(_), Some((top, bottom))) = (colour, gradient) {
        // 165°, from the top colour to the bottom one, as Icon.qml writes it,
        // across the whole glyph in the file's own coordinates. Measured on
        // each shape instead, a straight stroke (a minus, a bar) has a box
        // with no height, and the specification says not to draw it at all.
        let r = 165f32.to_radians();
        let (dx, dy) = (r.sin() / 2.0, -r.cos() / 2.0);
        let [x, y, w, h] = view_box(&body);
        let defs = format!(
            "<defs><linearGradient id=\"genomaGradient\" gradientUnits=\"userSpaceOnUse\" \
             x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">\
             <stop offset=\"0\" stop-color=\"{}\" stop-opacity=\"{}\"/>\
             <stop offset=\"1\" stop-color=\"{}\" stop-opacity=\"{}\"/></linearGradient></defs>",
            x + (0.5 - dx) * w,
            y + (0.5 - dy) * h,
            x + (0.5 + dx) * w,
            y + (0.5 + dy) * h,
            hex(top),
            top[3],
            hex(bottom),
            bottom[3]
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
    if let Some(colour) = colour.filter(|c| c[3] < 1.0 && gradient.is_none()) {
        for px in target.pixels_mut() {
            let c = px.demultiply();
            let a = (c.alpha() as f32 * colour[3]).round() as u8;
            *px = tiny_skia::ColorU8::from_rgba(c.red(), c.green(), c.blue(), a).premultiply();
        }
    }
    Some(target)
}
