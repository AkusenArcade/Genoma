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
}

impl Glyphs {
    /// `uid` is the node; `origin` is where glyphon would draw it, physical
    /// pixels, so the raster has the same subpixel placement and the switch
    /// back to glyphon at rest does not move a glyph; `colour` is straight
    /// RGB, its alpha left to the image so a fade does not rasterise again.
    pub fn raster(
        &mut self,
        fonts: &mut ct::FontSystem,
        uid: u64,
        buffer: &ct::Buffer,
        origin: [f32; 2],
        scale: f32,
        colour: [f32; 4],
    ) -> Option<Raster> {
        let rgb = colour.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let mut placed = Vec::new();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "text".hash(&mut h);
        (rgb[0], rgb[1], rgb[2]).hash(&mut h);
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
        for (gx, gy, image) in &images {
            let (w, hh) = (image.placement.width as i32, image.placement.height as i32);
            for row in 0..hh {
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
                            [rgb[0] as u32 * a / 255, rgb[1] as u32 * a / 255, rgb[2] as u32 * a / 255, a]
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
}
