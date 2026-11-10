//! Distance fields of texts and paths, for a capsule to be combined with
//! (`combine_with` naming one): a drop that takes a label into itself. The
//! outline comes from the raster the node already has (a text's glyphs on
//! the CPU, a path's picture); its exact Euclidean distance transform
//! (Felzenszwalb and Huttenlocher) is taken inside and out, one byte a
//! pixel, and kept while the outline stays the same.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::Arc,
};

use genoma_render::Field;

/// A field where it was drawn: its corner, physical pixels, and how far
/// past the outline its distances reach.
#[derive(Clone)]
pub(crate) struct Placed {
    pub field: Field,
    pub corner: [f32; 2],
    pub spread: f32,
}

/// The fields of one scene: made once per outline, kept while asked for.
#[derive(Default)]
pub(crate) struct Fields {
    made: HashMap<u64, Field>,
    used: HashSet<u64>,
    /// By the id of the text or path: this frame's, and the last frame's
    /// for a capsule that comes earlier in the tree than what it names.
    pub here: HashMap<String, Placed>,
    pub last: HashMap<String, Placed>,
    /// The ids some `combine_with` names.
    pub wanted: Vec<String>,
}

impl Fields {
    /// A new frame: what was placed last is kept to be read a frame late.
    pub fn begin(&mut self, wanted: Vec<String>) {
        self.last = std::mem::take(&mut self.here);
        self.used.clear();
        self.wanted = wanted;
    }

    /// The frame is drawn: a field nobody read goes.
    pub fn end(&mut self) {
        let used = &self.used;
        self.made.retain(|k, _| used.contains(k));
    }

    pub fn wants(&self, id: Option<&str>) -> bool {
        id.is_some_and(|id| self.wanted.iter().any(|w| w == id))
    }

    /// The field of a raster (premultiplied RGBA, its alpha the outline),
    /// `key` naming what it shows, its corner at `corner`, physical pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn place(&mut self, id: &str, key: u64, rgba: &[u8], width: u32, height: u32, corner: [f32; 2], spread: f32) {
        let margin = spread.ceil() as u32;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("distance", key, spread.to_bits()).hash(&mut h);
        let field_key = h.finish();
        self.used.insert(field_key);
        let field = self
            .made
            .entry(field_key)
            .or_insert_with(|| {
                let alpha: Vec<f32> = rgba.chunks_exact(4).map(|p| p[3] as f32 / 255.0).collect();
                let (data, w, h) = encode(&alpha, width as usize, height as usize, margin as usize, spread);
                Field { key: field_key, width: w as u32, height: h as u32, data: Arc::new(data) }
            })
            .clone();
        let corner = [corner[0] - margin as f32, corner[1] - margin as f32];
        self.here.insert(id.to_owned(), Placed { field, corner, spread });
    }

    /// What a capsule names: this frame's, else the last one's.
    pub fn named(&mut self, id: &str) -> Option<Placed> {
        let found = self.here.get(id).or_else(|| self.last.get(id)).cloned()?;
        self.used.insert(found.field.key);
        Some(found)
    }
}

/// The signed distance, physical pixels, negative inside, of each pixel of
/// a coverage raster padded by `margin` on every side, encoded as one byte
/// read as coverage is: 0.5 on the edge, 1 at `spread` inside, 0 at `spread`
/// outside.
pub(crate) fn encode(alpha: &[f32], width: usize, height: usize, margin: usize, spread: f32) -> (Vec<u8>, usize, usize) {
    let (w, h) = (width + 2 * margin, height + 2 * margin);
    let mut cover = vec![0.0_f32; w * h];
    for y in 0..height {
        for x in 0..width {
            cover[(y + margin) * w + x + margin] = alpha[y * width + x];
        }
    }
    let distance = signed(&cover, w, h);
    let data = distance.iter().map(|d| ((0.5 - d / (2.0 * spread)).clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    (data, w, h)
}

/// The signed distance of each pixel to the outline of `cover` (0…1), half
/// covered being the edge: the distance to the nearest pixel across, less
/// half a pixel, and on the edge's own pixels what their coverage says.
pub(crate) fn signed(cover: &[f32], w: usize, h: usize) -> Vec<f32> {
    const FAR: f32 = 1.0e20;
    let inside: Vec<bool> = cover.iter().map(|a| *a >= 0.5).collect();
    let to_inside = transform(&inside.iter().map(|i| if *i { 0.0 } else { FAR }).collect::<Vec<_>>(), w, h);
    let to_outside = transform(&inside.iter().map(|i| if *i { FAR } else { 0.0 }).collect::<Vec<_>>(), w, h);
    (0..w * h)
        .map(|i| {
            let a = cover[i];
            if a > 0.0 && a < 1.0 {
                0.5 - a
            } else if inside[i] {
                -(to_outside[i].sqrt() - 0.5)
            } else {
                to_inside[i].sqrt() - 0.5
            }
        })
        .collect()
}

/// The squared Euclidean distance transform of a grid of 0 (on a feature)
/// and a large number (off), by rows then columns.
fn transform(grid: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = grid.to_vec();
    let mut line = vec![0.0; w.max(h)];
    let mut result = vec![0.0; w.max(h)];
    for y in 0..h {
        line[..w].copy_from_slice(&out[y * w..(y + 1) * w]);
        one_dimension(&line[..w], &mut result[..w]);
        out[y * w..(y + 1) * w].copy_from_slice(&result[..w]);
    }
    for x in 0..w {
        for y in 0..h {
            line[y] = out[y * w + x];
        }
        one_dimension(&line[..h], &mut result[..h]);
        for y in 0..h {
            out[y * w + x] = result[y];
        }
    }
    out
}

/// One line of the transform: the lower envelope of the parabolas rooted at
/// each sample.
fn one_dimension(f: &[f32], d: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0.0_f32; n + 1];
    let mut k = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
                break;
            }
        }
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        *out = dq * dq + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disc_is_its_distance_from_the_centre_less_its_radius() {
        let (w, h, r) = (41, 41, 10.0_f32);
        let cover: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 - 20.0, (i / w) as f32 - 20.0);
                if (x * x + y * y).sqrt() <= r { 1.0 } else { 0.0 }
            })
            .collect();
        let d = signed(&cover, w, h);
        let at = |x: usize, y: usize| d[y * w + x];
        assert!(at(20, 20) < -9.0, "deep inside: {}", at(20, 20));
        assert!((at(20 + 15, 20) - 5.0).abs() < 1.0, "five past the edge: {}", at(35, 20));
        assert!((at(20 + 10, 20 + 10) - (200.0_f32.sqrt() - 10.0)).abs() < 1.0, "along the diagonal too");
    }

    #[test]
    fn the_encoding_puts_the_edge_at_a_half_and_pads_with_far() {
        // One opaque pixel in a 3×3, padded by 4.
        let mut alpha = vec![0.0; 9];
        alpha[4] = 1.0;
        let (data, w, h) = encode(&alpha, 3, 3, 4, 8.0);
        assert_eq!((w, h), (11, 11));
        let centre = data[5 * w + 5];
        assert!(centre > 100 && centre < 140, "about the edge: {centre}");
        assert!(data[0] < 40, "the padded corner is well outside: {}", data[0]);
    }

    #[test]
    fn a_field_is_made_once_and_dropped_when_nobody_reads_it() {
        let mut fields = Fields::default();
        fields.begin(vec!["label".into()]);
        assert!(fields.wants(Some("label")) && !fields.wants(Some("other")) && !fields.wants(None));
        let rgba = vec![255u8; 4 * 4 * 2];
        fields.place("label", 7, &rgba, 4, 2, [10.0, 20.0], 6.0);
        let placed = fields.named("label").expect("placed");
        assert_eq!(placed.corner, [4.0, 14.0], "the corner less the margin");
        assert_eq!((placed.field.width, placed.field.height), (16, 14));
        fields.end();
        // Next frame: read a frame late, then forgotten when nobody asks.
        fields.begin(vec!["label".into()]);
        assert!(fields.named("label").is_some(), "the last frame's");
        fields.end();
        fields.begin(Vec::new());
        fields.end();
        assert!(fields.made.is_empty());
    }
}
