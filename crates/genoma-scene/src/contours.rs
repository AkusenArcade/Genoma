//! The outlines in a picture, as lines: what `g.contours` gives. The
//! picture is read small (its long side `size` pixels), smoothed, and its
//! edges found as Canny finds them (gradient, thinned to its ridge, kept
//! where strong or joined to the strong); the edge pixels are then walked
//! into chains, simplified, rounded, and the longest kept. Run once per
//! picture, off the frame: a wallpaper's lines for something to travel
//! along.

use std::path::Path;

/// What a picture's outlines come to: its own size in pixels, and each
/// line as points from 0 to 1 of that size, longest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Contours {
    pub width: u32,
    pub height: u32,
    pub lines: Vec<Vec<[f32; 2]>>,
}

/// How a picture is read.
#[derive(Debug, Clone, Copy)]
pub struct Ask {
    /// The long side it is read at, pixels: smaller is coarser and faster.
    pub size: u32,
    /// How many lines are kept, at most.
    pub most: usize,
    /// The shortest line kept, as a share of the picture's diagonal.
    pub shortest: f32,
}

impl Default for Ask {
    fn default() -> Self {
        Ask { size: 480, most: 32, shortest: 0.08 }
    }
}

/// A picture's outlines, or `None` for one that cannot be read.
pub fn contours(source: &str, ask: Ask) -> Option<Contours> {
    let image = image::ImageReader::open(Path::new(source)).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return None;
    }
    let small = image.thumbnail(ask.size.max(16), ask.size.max(16)).to_luma32f();
    let (w, h) = (small.width() as usize, small.height() as usize);
    let lines = lines_in(small.as_raw(), w, h, ask);
    Some(Contours { width, height, lines })
}

/// The lines in a grey picture `w` × `h` (values 0 to 1), as points from 0
/// to 1 of its size.
pub fn lines_in(grey: &[f32], w: usize, h: usize, ask: Ask) -> Vec<Vec<[f32; 2]>> {
    if w < 3 || h < 3 || grey.len() < w * h {
        return Vec::new();
    }
    let smooth = blur(grey, w, h);
    let (magnitude, direction) = gradient(&smooth, w, h);
    let thin = suppress(&magnitude, &direction, w, h);
    let edges = hysteresis(&thin, w, h);
    let diagonal = ((w * w + h * h) as f32).sqrt();
    let mut chains: Vec<(f32, Vec<[f32; 2]>)> = walk(&edges, w, h)
        .into_iter()
        .map(|chain| round(&simplify(&chain, 1.0)))
        .map(|chain| (length(&chain), chain))
        .filter(|(len, chain)| chain.len() >= 2 && *len >= ask.shortest * diagonal)
        .collect();
    chains.sort_by(|a, b| b.0.total_cmp(&a.0));
    chains.truncate(ask.most);
    chains
        .into_iter()
        .map(|(_, chain)| chain.into_iter().map(|[x, y]| [x / w as f32, y / h as f32]).collect())
        .collect()
}

/// A small Gaussian (1 4 6 4 1, both ways): grain is not an outline.
fn blur(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let at = |v: isize, n: usize| v.clamp(0, n as isize - 1) as usize;
    let mut across = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            across[y * w + x] = (0..5).map(|k| K[k] * src[y * w + at(x as isize + k as isize - 2, w)]).sum();
        }
    }
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = (0..5).map(|k| K[k] * across[at(y as isize + k as isize - 2, h) * w + x]).sum();
        }
    }
    out
}

/// Sobel: how steep the picture is at each pixel, and which of four ways
/// (0 across, 1 rising, 2 down, 3 falling) it is steepest.
fn gradient(src: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<u8>) {
    let mut magnitude = vec![0.0; w * h];
    let mut direction = vec![0u8; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let p = |dx: isize, dy: isize| src[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
            let gx = p(1, -1) + 2.0 * p(1, 0) + p(1, 1) - p(-1, -1) - 2.0 * p(-1, 0) - p(-1, 1);
            let gy = p(-1, 1) + 2.0 * p(0, 1) + p(1, 1) - p(-1, -1) - 2.0 * p(0, -1) - p(1, -1);
            magnitude[y * w + x] = (gx * gx + gy * gy).sqrt();
            let angle = gy.atan2(gx).to_degrees().rem_euclid(180.0);
            direction[y * w + x] = match angle {
                a if !(22.5..157.5).contains(&a) => 0,
                a if a < 67.5 => 1,
                a if a < 112.5 => 2,
                _ => 3,
            };
        }
    }
    (magnitude, direction)
}

/// Only the ridge of each edge: a pixel no steeper than a neighbour across
/// the edge goes.
fn suppress(magnitude: &[f32], direction: &[u8], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let m = magnitude[i];
            if m <= 0.0 {
                continue;
            }
            let (a, b) = match direction[i] {
                0 => (i - 1, i + 1),
                1 => (i - w - 1, i + w + 1),
                2 => (i - w, i + w),
                _ => (i - w + 1, i + w - 1),
            };
            if m >= magnitude[a] && m >= magnitude[b] {
                out[i] = m;
            }
        }
    }
    out
}

/// Strong edges, and the weaker ones joined to them. "Strong" is relative
/// to the picture: the steepest tenth of its ridges, so a soft painting and
/// a hard photograph both give lines.
fn hysteresis(thin: &[f32], w: usize, h: usize) -> Vec<bool> {
    let mut ridges: Vec<f32> = thin.iter().copied().filter(|m| *m > 0.0).collect();
    if ridges.is_empty() {
        return vec![false; w * h];
    }
    ridges.sort_by(f32::total_cmp);
    let high = ridges[(ridges.len() as f32 * 0.9) as usize].max(1e-4);
    let low = high * 0.45;
    let mut keep = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| thin[i] >= high).collect();
    for &i in &stack {
        keep[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (dx, dy) in NEIGHBOURS {
            let (nx, ny) = (x as isize + dx, y as isize + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if !keep[j] && thin[j] >= low {
                keep[j] = true;
                stack.push(j);
            }
        }
    }
    keep
}

const NEIGHBOURS: [(isize, isize); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];

/// The edge pixels walked into chains: from each one not yet taken, as far
/// as it goes one way, then the other, preferring to go on straight.
fn walk(edges: &[bool], w: usize, h: usize) -> Vec<Vec<[f32; 2]>> {
    let mut taken = vec![false; w * h];
    let mut chains = Vec::new();
    let next = |taken: &[bool], i: usize, heading: Option<(isize, isize)>| -> Option<(usize, (isize, isize))> {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        let mut best: Option<(usize, (isize, isize), isize)> = None;
        for (dx, dy) in NEIGHBOURS {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if !edges[j] || taken[j] {
                continue;
            }
            // Straight on scores highest, a sharp turn lowest.
            let score = heading.map_or(0, |(hx, hy)| hx * dx + hy * dy);
            if best.is_none_or(|b| score > b.2) {
                best = Some((j, (dx, dy), score));
            }
        }
        best.map(|(j, d, _)| (j, d))
    };
    for start in 0..w * h {
        if !edges[start] || taken[start] {
            continue;
        }
        taken[start] = true;
        let mut halves: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
        for half in &mut halves {
            let (mut i, mut heading) = (start, None);
            while let Some((j, d)) = next(&taken, i, heading) {
                taken[j] = true;
                half.push(j);
                i = j;
                heading = Some(d);
            }
        }
        let [forward, back] = halves;
        let chain: Vec<[f32; 2]> = back
            .iter()
            .rev()
            .chain(std::iter::once(&start))
            .chain(forward.iter())
            .map(|&i| [(i % w) as f32 + 0.5, (i / w) as f32 + 0.5])
            .collect();
        chains.push(chain);
    }
    chains
}

/// Ramer–Douglas–Peucker: the points a line needs to stay within
/// `epsilon` pixels of itself.
fn simplify(points: &[[f32; 2]], epsilon: f32) -> Vec<[f32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let distance = |p: [f32; 2]| {
        if len < 1e-6 {
            ((p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)).sqrt()
        } else {
            ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / len
        }
    };
    let (far, worst) = points[1..points.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, &p)| (i + 1, distance(p)))
        .fold((0, 0.0), |acc, (i, d)| if d > acc.1 { (i, d) } else { acc });
    if worst <= epsilon {
        return vec![a, b];
    }
    let mut left = simplify(&points[..=far], epsilon);
    let right = simplify(&points[far..], epsilon);
    left.pop();
    left.extend(right);
    left
}

/// Chaikin, once: the corners of the pixel walk rounded off, ends kept.
fn round(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        out.push([0.75 * a[0] + 0.25 * b[0], 0.75 * a[1] + 0.25 * b[1]]);
        out.push([0.25 * a[0] + 0.75 * b[0], 0.25 * a[1] + 0.75 * b[1]]);
    }
    out.push(points[points.len() - 1]);
    out
}

fn length(points: &[[f32; 2]]) -> f32 {
    points.windows(2).map(|p| ((p[1][0] - p[0][0]).powi(2) + (p[1][1] - p[0][1]).powi(2)).sqrt()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A light disc on a dark ground gives one line, round it.
    #[test]
    fn a_disc_gives_its_outline() {
        let (w, h) = (96, 64);
        let mut grey = vec![0.1; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 48.0, y as f32 - 32.0);
                if (dx * dx + dy * dy).sqrt() < 20.0 {
                    grey[y * w + x] = 0.9;
                }
            }
        }
        let lines = lines_in(&grey, w, h, Ask { size: 96, most: 8, shortest: 0.2 });
        assert!(!lines.is_empty(), "no line found");
        let longest = &lines[0];
        // Every point near the circle of radius 20 about the middle.
        for [x, y] in longest {
            let r = ((x * w as f32 - 48.0).powi(2) + (y * h as f32 - 32.0).powi(2)).sqrt();
            assert!((r - 20.0).abs() < 3.0, "a point {r} from the middle");
        }
        let len = length(&longest.iter().map(|[x, y]| [x * w as f32, y * h as f32]).collect::<Vec<_>>());
        assert!(len > 0.6 * std::f32::consts::TAU * 20.0, "the line runs {len}, most of the circle expected");
    }

    /// A flat picture has no outline.
    #[test]
    fn a_flat_picture_has_none() {
        let grey = vec![0.5; 64 * 64];
        assert!(lines_in(&grey, 64, 64, Ask::default()).is_empty());
    }

    #[test]
    fn simplify_keeps_a_corner() {
        let line: Vec<[f32; 2]> = (0..10).map(|i| [i as f32, 0.0]).chain((1..10).map(|i| [9.0, i as f32])).collect();
        let s = simplify(&line, 0.5);
        assert_eq!(s, vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0]]);
    }
}
