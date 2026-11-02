//! Text under depth (PRD §12, question 2): glyphon draws text flat, so text
//! in a tilted subtree is rasterised once on the CPU, at the physical size it
//! has when flat, and drawn as an image through the subtree's matrix. At rest
//! the matrix is the identity again and the text goes back to glyphon, so a
//! foreshortened raster is only ever seen in motion.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use genoma_render::cosmic_text as ct;

#[derive(Clone)]
pub struct Raster {
    /// The renderer's texture key: what the pixels show, not which node.
    pub key: u64,
    /// Premultiplied, sRGB-encoded RGBA, as icons are.
    pub pixels: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// The raster's top-left corner from the text's origin rounded down,
    /// physical pixels.
    pub offset: [i32; 2],
}

#[derive(Default)]
pub struct Glyphs {
    swash: Option<ct::SwashCache>,
    /// Each text node's last raster: one per node, so a text that changes
    /// while tilted does not pile up rasters.
    last: HashMap<u64, Raster>,
    /// Each text node's last shadow.
    shadows: HashMap<u64, Raster>,
}

impl Glyphs {
    /// `uid` is the node; `origin` is where glyphon would draw it, physical
    /// pixels, so the raster has the same subpixel placement and the switch
    /// back to glyphon at rest does not move a glyph; `colour` is straight
    /// RGB, its alpha left to the image so a fade does not rasterise again.
    /// `lit` is a gradient from `colour` at the top of the text's box to this
    /// colour at its bottom, and the box's height, physical pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn raster(
        &mut self,
        fonts: &mut ct::FontSystem,
        uid: u64,
        buffer: &ct::Buffer,
        origin: [f32; 2],
        scale: f32,
        colour: [f32; 4],
        lit: Option<([f32; 4], f32)>,
    ) -> Option<Raster> {
        let rgb = colour.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let lit = lit.map(|(c, h)| (c.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8), h.max(1.0)));
        let mut placed = Vec::new();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "text".hash(&mut h);
        (rgb[0], rgb[1], rgb[2]).hash(&mut h);
        if let Some((c, height)) = lit {
            (c[0], c[1], c[2], height.to_bits()).hash(&mut h);
        }
        for run in buffer.layout_runs() {
            let baseline = (run.line_y * scale).round() as i32;
            for glyph in run.glyphs {
                let p = glyph.physical((origin[0] - origin[0].floor(), origin[1] - origin[1].floor()), scale);
                (p.cache_key, p.x, p.y + baseline).hash(&mut h);
                placed.push((p.cache_key, p.x, p.y + baseline));
            }
        }
        let key = h.finish();
        if let Some(r) = self.last.get(&uid).filter(|r| r.key == key) {
            return Some(r.clone());
        }

        let swash = self.swash.get_or_insert_with(ct::SwashCache::new);
        let images: Vec<_> = placed
            .iter()
            .filter_map(|&(cache_key, x, y)| {
                let image = swash.get_image_uncached(fonts, cache_key)?;
                let p = image.placement;
                (p.width > 0 && p.height > 0).then(|| (x + p.left, y - p.top, image))
            })
            .collect();
        let x0 = images.iter().map(|(x, _, _)| *x).min()?;
        let y0 = images.iter().map(|(_, y, _)| *y).min()?;
        let x1 = images.iter().map(|(x, _, i)| x + i.placement.width as i32).max()?;
        let y1 = images.iter().map(|(_, y, i)| y + i.placement.height as i32).max()?;
        let (width, height) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        // The colour of each row: the text's, or its light at that height in
        // the box (which starts at the origin, the raster's rows from y0).
        let top = origin[1] - origin[1].floor();
        let row_colour = |y: i32| -> [u32; 3] {
            match lit {
                None => [rgb[0] as u32, rgb[1] as u32, rgb[2] as u32],
                Some((bottom, height)) => {
                    let t = ((y as f32 + 0.5 - top) / height).clamp(0.0, 1.0);
                    std::array::from_fn(|k| (rgb[k] as f32 + (bottom[k] as f32 - rgb[k] as f32) * t).round() as u32)
                }
            }
        };
        for (gx, gy, image) in &images {
            let (w, hh) = (image.placement.width as i32, image.placement.height as i32);
            for row in 0..hh {
                let rgb = row_colour(gy + row);
                for col in 0..w {
                    let i = (row * w + col) as usize;
                    // A mask's coverage takes the text's colour; a colour glyph
                    // (an emoji) keeps its own, premultiplied here.
                    let texel = match image.content {
                        ct::SwashContent::Color => {
                            let c = &image.data[i * 4..i * 4 + 4];
                            let a = c[3] as u32;
                            [c[0] as u32 * a / 255, c[1] as u32 * a / 255, c[2] as u32 * a / 255, a]
                        }
                        ct::SwashContent::Mask | ct::SwashContent::SubpixelMask => {
                            let a = match image.content {
                                ct::SwashContent::Mask => image.data[i],
                                _ => image.data[i * 4 + 1],
                            } as u32;
                            [rgb[0] * a / 255, rgb[1] * a / 255, rgb[2] * a / 255, a]
                        }
                    };
                    let at = (((gy - y0 + row) * width as i32 + gx - x0 + col) * 4) as usize;
                    // Glyphs may overlap: over, premultiplied.
                    let below = &mut pixels[at..at + 4];
                    let keep = 255 - texel[3];
                    for k in 0..4 {
                        below[k] = (texel[k] + below[k] as u32 * keep / 255).min(255) as u8;
                    }
                }
            }
        }
        let raster = Raster { key, pixels: Arc::new(pixels), width, height, offset: [x0, y0] };
        self.last.insert(uid, raster.clone());
        Some(raster)
    }

    /// The text's glyphs blurred, in `colour`: a shadow drawn under it.
    /// Rasterised on whole pixels and kept by what it shows, not where, so
    /// a text that moves does not blur again; the offset is from the
    /// text's origin rounded down. `blur` is the radius, physical pixels.
    pub fn shadow(
        &mut self,
        fonts: &mut ct::FontSystem,
        uid: u64,
        buffer: &ct::Buffer,
        scale: f32,
        colour: [f32; 4],
        blur: f32,
    ) -> Option<Raster> {
        let rgb = colour.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let mut placed = Vec::new();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "shadow".hash(&mut h);
        (rgb[0], rgb[1], rgb[2], blur.to_bits()).hash(&mut h);
        for run in buffer.layout_runs() {
            let baseline = (run.line_y * scale).round() as i32;
            for glyph in run.glyphs {
                let p = glyph.physical((0.0, 0.0), scale);
                (p.cache_key, p.x, p.y + baseline).hash(&mut h);
                placed.push((p.cache_key, p.x, p.y + baseline));
            }
        }
        let key = h.finish();
        if let Some(r) = self.shadows.get(&uid).filter(|r| r.key == key) {
            return Some(r.clone());
        }
        let swash = self.swash.get_or_insert_with(ct::SwashCache::new);
        let images: Vec<_> = placed
            .iter()
            .filter_map(|&(cache_key, x, y)| {
                let image = swash.get_image_uncached(fonts, cache_key)?;
                let p = image.placement;
                (p.width > 0 && p.height > 0).then(|| (x + p.left, y - p.top, image))
            })
            .collect();
        // Three box blurs make a gaussian of sigma `blur / 2`; the raster is
        // padded by three sigmas so the shadow is never cut.
        let sigma = (blur / 2.0).max(0.5);
        let box_r = (((12.0 * sigma * sigma / 3.0 + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as i32;
        let pad = box_r * 3 + 1;
        let x0 = images.iter().map(|(x, _, _)| *x).min()? - pad;
        let y0 = images.iter().map(|(_, y, _)| *y).min()? - pad;
        let x1 = images.iter().map(|(x, _, i)| x + i.placement.width as i32).max()? + pad;
        let y1 = images.iter().map(|(_, y, i)| y + i.placement.height as i32).max()? + pad;
        let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut cover = vec![0f32; width * height];
        for (gx, gy, image) in &images {
            let (w, hh) = (image.placement.width as i32, image.placement.height as i32);
            for row in 0..hh {
                for col in 0..w {
                    let i = (row * w + col) as usize;
                    let a = match image.content {
                        ct::SwashContent::Color => image.data[i * 4 + 3],
                        ct::SwashContent::Mask => image.data[i],
                        ct::SwashContent::SubpixelMask => image.data[i * 4 + 1],
                    } as f32
                        / 255.0;
                    let at = ((gy - y0 + row) as usize) * width + (gx - x0 + col) as usize;
                    cover[at] = (cover[at] + a).min(1.0);
                }
            }
        }
        let mut scratch = vec![0f32; width * height];
        for _ in 0..3 {
            box_blur(&cover, &mut scratch, width, height, box_r, true);
            box_blur(&scratch, &mut cover, width, height, box_r, false);
        }
        let alpha = colour[3].clamp(0.0, 1.0);
        let mut pixels = vec![0u8; width * height * 4];
        for (i, c) in cover.iter().enumerate() {
            let a = (c * alpha).clamp(0.0, 1.0);
            pixels[i * 4] = (rgb[0] as f32 * a).round() as u8;
            pixels[i * 4 + 1] = (rgb[1] as f32 * a).round() as u8;
            pixels[i * 4 + 2] = (rgb[2] as f32 * a).round() as u8;
            pixels[i * 4 + 3] = (a * 255.0).round() as u8;
        }
        let raster = Raster { key, pixels: Arc::new(pixels), width: width as u32, height: height as u32, offset: [x0, y0] };
        self.shadows.insert(uid, raster.clone());
        Some(raster)
    }
}

/// One pass of a box blur of radius `r`, along rows or columns, by a
/// running sum: the cost does not grow with the radius.
fn box_blur(from: &[f32], to: &mut [f32], width: usize, height: usize, r: i32, rows: bool) {
    let (lines, length, step, stride) = if rows { (height, width, 1, width) } else { (width, height, width, 1) };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let base = line * stride;
        let at = |i: i32| -> f32 { if i < 0 || i >= length as i32 { 0.0 } else { from[base + i as usize * step] } };
        let mut sum: f32 = (-r..=r).map(at).sum();
        for i in 0..length as i32 {
            to[base + i as usize * step] = sum * norm;
            sum += at(i + r + 1) - at(i - r);
        }
    }
}
