//! `path` nodes as meshes for the GPU (docs/svg-plan.md, phase 1): the
//! outline read from SVG path data or a polygon, a stroke turned into the
//! outline it covers (joins, caps, dashes) by tiny-skia's stroker, and both
//! tessellated by the renderer. Meshes are kept by what shapes them — the
//! outline, the stroke and the scale — so a colour, an alpha or a move
//! costs nothing here.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use genoma_render::path::{
    self as gpu, Mesh,
    path::{Path as Outline, math::point},
};
use resvg::tiny_skia::{self, LineCap, LineJoin, PathBuilder, Stroke, StrokeDash};

/// What shapes a path's meshes; colours are not part of it.
pub struct Shape<'a> {
    pub outline: Option<&'a str>,
    pub points: &'a [f32],
    pub even_odd: bool,
    /// Width, logical units; none or 0 is no stroke.
    pub stroke: Option<f32>,
    pub join: &'a str,
    pub cap: &'a str,
    /// Dashes and gaps in turn, logical units; none is a solid line.
    pub dash: Vec<f32>,
    pub dash_offset: f32,
    pub miter_limit: f32,
    /// The stroke's part drawn, 0…1 of each outline's length.
    pub trim: [f32; 2],
}

/// The outline in logical units: SVG path data (arcs, relative and short
/// forms included), else a closed polygon.
pub fn outline(outline: Option<&str>, points: &[f32]) -> Option<tiny_skia::Path> {
    let mut b = PathBuilder::new();
    match outline {
        Some(d) => {
            for segment in svgtypes::SimplifyingPathParser::from(d) {
                match segment.ok()? {
                    svgtypes::SimplePathSegment::MoveTo { x, y } => b.move_to(x as f32, y as f32),
                    svgtypes::SimplePathSegment::LineTo { x, y } => b.line_to(x as f32, y as f32),
                    svgtypes::SimplePathSegment::Quadratic { x1, y1, x, y } => b.quad_to(x1 as f32, y1 as f32, x as f32, y as f32),
                    svgtypes::SimplePathSegment::CurveTo { x1, y1, x2, y2, x, y } => {
                        b.cubic_to(x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32)
                    }
                    svgtypes::SimplePathSegment::ClosePath => b.close(),
                }
            }
        }
        None if points.len() >= 4 => {
            b.move_to(points[0], points[1]);
            for p in points[2..].chunks_exact(2) {
                b.line_to(p[0], p[1]);
            }
            b.close();
        }
        None => return None,
    }
    b.finish()
}

/// `path`, logical units, as the renderer's outline in physical pixels.
fn physical(path: &tiny_skia::Path, scale: f32) -> Outline {
    let mut b = Outline::builder();
    let p = |q: tiny_skia::Point| point(q.x * scale, q.y * scale);
    let mut open = false;
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(a) => {
                if open {
                    b.end(false);
                }
                b.begin(p(a));
                open = true;
            }
            tiny_skia::PathSegment::LineTo(a) => {
                b.line_to(p(a));
            }
            tiny_skia::PathSegment::QuadTo(c, a) => {
                b.quadratic_bezier_to(p(c), p(a));
            }
            tiny_skia::PathSegment::CubicTo(c1, c2, a) => {
                b.cubic_bezier_to(p(c1), p(c2), p(a));
            }
            tiny_skia::PathSegment::Close => {
                if open {
                    b.end(true);
                    open = false;
                }
            }
        }
    }
    if open {
        b.end(false);
    }
    b.build()
}

/// How far a curve may stray from its true line, physical pixels.
const TOLERANCE: f32 = 0.1;

/// The interior's mesh.
pub fn fill(path: &tiny_skia::Path, even_odd: bool, scale: f32) -> Mesh {
    let rule = if even_odd { gpu::path::FillRule::EvenOdd } else { gpu::path::FillRule::NonZero };
    gpu::fill(&physical(path, scale), rule, TOLERANCE)
}

/// The contours of `path`, each a path of its own.
fn contours(path: &tiny_skia::Path) -> Vec<tiny_skia::Path> {
    let mut out = Vec::new();
    let mut b = PathBuilder::new();
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(p) => {
                if let Some(done) = std::mem::take(&mut b).finish() {
                    out.push(done);
                }
                b.move_to(p.x, p.y);
            }
            tiny_skia::PathSegment::LineTo(p) => b.line_to(p.x, p.y),
            tiny_skia::PathSegment::QuadTo(c, p) => b.quad_to(c.x, c.y, p.x, p.y),
            tiny_skia::PathSegment::CubicTo(c1, c2, p) => b.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y),
            tiny_skia::PathSegment::Close => b.close(),
        }
    }
    if let Some(done) = b.finish() {
        out.push(done);
    }
    out
}

/// A contour's length, its curves followed in short chords.
fn length(contour: &tiny_skia::Path) -> f32 {
    let mut total = 0.0;
    let (mut at, mut start) = (tiny_skia::Point::zero(), tiny_skia::Point::zero());
    let chord = |a: tiny_skia::Point, b: tiny_skia::Point| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    for segment in contour.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(p) => (at, start) = (p, p),
            tiny_skia::PathSegment::LineTo(p) => {
                total += chord(at, p);
                at = p;
            }
            tiny_skia::PathSegment::QuadTo(c, p) => {
                let mut prev = at;
                for i in 1..=64 {
                    let t = i as f32 / 64.0;
                    let u = 1.0 - t;
                    let q = tiny_skia::Point::from_xy(u * u * at.x + 2.0 * u * t * c.x + t * t * p.x, u * u * at.y + 2.0 * u * t * c.y + t * t * p.y);
                    total += chord(prev, q);
                    prev = q;
                }
                at = p;
            }
            tiny_skia::PathSegment::CubicTo(c1, c2, p) => {
                let mut prev = at;
                for i in 1..=64 {
                    let t = i as f32 / 64.0;
                    let u = 1.0 - t;
                    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let q = tiny_skia::Point::from_xy(w0 * at.x + w1 * c1.x + w2 * c2.x + w3 * p.x, w0 * at.y + w1 * c1.y + w2 * c2.y + w3 * p.y);
                    total += chord(prev, q);
                    prev = q;
                }
                at = p;
            }
            tiny_skia::PathSegment::Close => {
                total += chord(at, start);
                at = start;
            }
        }
    }
    total
}

/// Each contour cut to the part from `trim[0]` to `trim[1]` of its length:
/// a dash as long as that part, starting where it starts.
fn trimmed(path: &tiny_skia::Path, trim: [f32; 2], scale: f32) -> Option<tiny_skia::Path> {
    if trim[0] <= 0.0 && trim[1] >= 1.0 {
        return Some(path.clone());
    }
    if trim[1] <= trim[0] {
        return None;
    }
    let mut b = PathBuilder::new();
    for contour in contours(path) {
        let total = length(&contour);
        if total <= 0.0 {
            continue;
        }
        let off = total * 2.0 + 1.0;
        // To the end, past any shortfall of the measure.
        let on = if trim[1] >= 1.0 { off } else { (trim[1] - trim[0]) * total };
        let Some(pattern) = StrokeDash::new(vec![on, off], on + off - trim[0] * total) else { continue };
        if let Some(part) = contour.dash(&pattern, scale) {
            b.push_path(&part);
        }
    }
    b.finish()
}

/// How a stroke is drawn.
pub struct Style<'a> {
    pub width: f32,
    pub join: &'a str,
    pub cap: &'a str,
    pub dash: &'a [f32],
    pub dash_offset: f32,
    pub miter_limit: f32,
    pub trim: [f32; 2],
}

/// The stroke's mesh, and the alpha it is drawn with: a line thinner than a
/// physical pixel is drawn a pixel wide and that much fainter, as a
/// hairline is.
pub fn stroke(path: &tiny_skia::Path, style: &Style, scale: f32) -> Option<(Mesh, f32)> {
    let physical_width = style.width * scale;
    let (drawn, alpha) = if physical_width < 1.0 { (1.0 / scale, physical_width.max(0.0)) } else { (style.width, 1.0) };
    let cut = trimmed(path, style.trim, scale)?;
    let dashed;
    // An odd count repeats, as SVG reads it.
    let pattern: Vec<f32> = if style.dash.len() % 2 == 1 { style.dash.repeat(2) } else { style.dash.to_vec() };
    let path = if pattern.iter().any(|d| *d > 0.0) {
        let pattern = StrokeDash::new(pattern, style.dash_offset)?;
        dashed = cut.dash(&pattern, scale)?;
        &dashed
    } else {
        &cut
    };
    let (join, cap) = (style.join, style.cap);
    let stroke = Stroke {
        width: drawn,
        miter_limit: style.miter_limit.max(1.0),
        line_cap: match cap {
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => LineCap::Butt,
        },
        line_join: match join {
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => LineJoin::Miter,
        },
        dash: None,
    };
    let outline = path.stroke(&stroke, scale)?;
    Some((gpu::fill(&physical(&outline, scale), gpu::path::FillRule::NonZero, TOLERANCE), alpha))
}

/// An exact gradient as `fill_paint` or `stroke_paint` gives it (kind,
/// spread, the matrix from logical units, then the kind's numbers), for a
/// mesh in physical pixels.
pub fn gradient(paint: &[f32], stops: &[(f32, [f32; 4])], scale: f32) -> Option<genoma_render::Gradient> {
    use genoma_render::{GradientKind, Spread};
    if paint.len() < 8 || stops.is_empty() {
        return None;
    }
    let kind = match paint[0] as i32 {
        1 => GradientKind::Linear,
        2 => GradientKind::Radial,
        3 => GradientKind::Conic,
        _ => return None,
    };
    let spread = match paint[1] as i32 {
        1 => Spread::Repeat,
        2 => Spread::Reflect,
        _ => Spread::Pad,
    };
    let s = scale.max(1e-6);
    let m = &paint[2..8];
    let mut params = [0.0; 6];
    for (i, v) in paint.iter().skip(8).take(6).enumerate() {
        params[i] = *v;
    }
    Some(genoma_render::Gradient {
        kind,
        spread,
        matrix: [m[0] / s, m[1] / s, m[2] / s, m[3] / s, m[4], m[5]],
        params,
        stops: stops.iter().take(genoma_render::path::STOPS).copied().collect(),
    })
}

/// A capsule's fill gradient (`fill` to `fill_bottom`, or `fill_stops`; at
/// `fill_angle`, as `fill_shape`, or `fill_across`) over a box of `size`
/// physical pixels, as the capsule draws it.
pub fn box_gradient(
    fill: [f32; 4],
    bottom: [f32; 4],
    stops: &[(f32, [f32; 4])],
    angle: Option<f32>,
    shape: u8,
    across: bool,
    size: [f32; 2],
) -> Option<genoma_render::Gradient> {
    use genoma_render::{GradientKind, Spread};
    if bottom[3] <= 0.0 && stops.is_empty() {
        return None;
    }
    let stops: Vec<(f32, [f32; 4])> = if stops.is_empty() { vec![(0.0, fill), (1.0, bottom)] } else { stops.to_vec() };
    let (c, half) = ([size[0] * 0.5, size[1] * 0.5], [(size[0] * 0.5).max(0.5), (size[1] * 0.5).max(0.5)]);
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let (kind, matrix, params) = match shape {
        1 => (GradientKind::Radial, [1.0 / half[0], 0.0, 0.0, 1.0 / half[1], -c[0] / half[0], -c[1] / half[1]], [0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
        2 => (GradientKind::Conic, identity, [c[0], c[1], angle.unwrap_or(0.0), 0.0, 0.0, 0.0]),
        _ => {
            let degrees = angle.unwrap_or(if across { 90.0 } else { 180.0 });
            let (sin, cos) = degrees.to_radians().sin_cos();
            let dir = [sin, -cos];
            let reach = (half[0] * dir[0]).abs() + (half[1] * dir[1]).abs();
            (
                GradientKind::Linear,
                identity,
                [c[0] - dir[0] * reach, c[1] - dir[1] * reach, c[0] + dir[0] * reach, c[1] + dir[1] * reach, 0.0, 0.0],
            )
        }
    };
    Some(genoma_render::Gradient { kind, spread: Spread::Pad, matrix, params, stops: stops.into_iter().take(genoma_render::path::STOPS).collect() })
}

/// An outline as cubic curves: where it starts, each curve's two control
/// points and end, and whether it closes.
#[derive(Clone, Debug, PartialEq)]
struct Contour {
    start: [f32; 2],
    curves: Vec<[f32; 6]>,
    closed: bool,
}

fn cubics(d: &str) -> Vec<Contour> {
    let mut out: Vec<Contour> = Vec::new();
    let mut at = [0.0f32; 2];
    let line = |a: [f32; 2], b: [f32; 2]| [a[0] + (b[0] - a[0]) / 3.0, a[1] + (b[1] - a[1]) / 3.0, a[0] + (b[0] - a[0]) * 2.0 / 3.0, a[1] + (b[1] - a[1]) * 2.0 / 3.0, b[0], b[1]];
    let current = |out: &mut Vec<Contour>, at: [f32; 2]| {
        if out.last().is_none_or(|c| c.closed) {
            out.push(Contour { start: at, curves: Vec::new(), closed: false });
        }
    };
    for segment in svgtypes::SimplifyingPathParser::from(d).flatten() {
        match segment {
            svgtypes::SimplePathSegment::MoveTo { x, y } => {
                at = [x as f32, y as f32];
                out.push(Contour { start: at, curves: Vec::new(), closed: false });
            }
            svgtypes::SimplePathSegment::LineTo { x, y } => {
                current(&mut out, at);
                let to = [x as f32, y as f32];
                out.last_mut().expect("made").curves.push(line(at, to));
                at = to;
            }
            svgtypes::SimplePathSegment::Quadratic { x1, y1, x, y } => {
                current(&mut out, at);
                let (q, to) = ([x1 as f32, y1 as f32], [x as f32, y as f32]);
                out.last_mut().expect("made").curves.push([
                    at[0] + (q[0] - at[0]) * 2.0 / 3.0,
                    at[1] + (q[1] - at[1]) * 2.0 / 3.0,
                    to[0] + (q[0] - to[0]) * 2.0 / 3.0,
                    to[1] + (q[1] - to[1]) * 2.0 / 3.0,
                    to[0],
                    to[1],
                ]);
                at = to;
            }
            svgtypes::SimplePathSegment::CurveTo { x1, y1, x2, y2, x, y } => {
                current(&mut out, at);
                out.last_mut().expect("made").curves.push([x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32]);
                at = [x as f32, y as f32];
            }
            svgtypes::SimplePathSegment::ClosePath => {
                if let Some(c) = out.last_mut() {
                    if at != c.start {
                        c.curves.push(line(at, c.start));
                    }
                    c.closed = true;
                    at = c.start;
                }
            }
        }
    }
    out.retain(|c| !c.curves.is_empty());
    out
}

/// The curve `i` of `c` cut in two at its middle.
fn split(c: &mut Contour, i: usize) {
    let p0 = if i == 0 { c.start } else { [c.curves[i - 1][4], c.curves[i - 1][5]] };
    let [x1, y1, x2, y2, x3, y3] = c.curves[i];
    let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let (p1, p2, p3) = ([x1, y1], [x2, y2], [x3, y3]);
    let (a, b, cc) = (mid(p0, p1), mid(p1, p2), mid(p2, p3));
    let (d, e) = (mid(a, b), mid(b, cc));
    let m = mid(d, e);
    c.curves[i] = [a[0], a[1], d[0], d[1], m[0], m[1]];
    c.curves.insert(i + 1, [e[0], e[1], cc[0], cc[1], p3[0], p3[1]]);
}

/// `c` divided, its longest curves first, until it has `count`.
fn divided(mut c: Contour, count: usize) -> Contour {
    while c.curves.len() < count {
        let mut longest = (0, -1.0f32);
        let mut from = c.start;
        for (i, k) in c.curves.iter().enumerate() {
            let chord = ((k[4] - from[0]).powi(2) + (k[5] - from[1]).powi(2)).sqrt();
            if chord > longest.1 {
                longest = (i, chord);
            }
            from = [k[4], k[5]];
        }
        split(&mut c, longest.0);
    }
    c
}

/// A contour standing for one the other outline lacks: a point where that
/// one starts, as many curves long.
fn point_like(other: &Contour) -> Contour {
    let p = other.start;
    Contour { start: p, curves: vec![[p[0], p[1], p[0], p[1], p[0], p[1]]; other.curves.len()], closed: other.closed }
}

/// Two outlines as read, and the pair matched up.
type Matched = Option<(String, String, Vec<Contour>, Vec<Contour>)>;

thread_local! {
    /// The last pair: a morph reads the same pair every frame of its way.
    static MATCHED: std::cell::RefCell<Matched> = const { std::cell::RefCell::new(None) };
}

/// The outline `t` of the way from `a` to `b`, as path data.
pub fn morph(a: &str, b: &str, t: f32) -> String {
    let (from, to) = MATCHED.with(|m| {
        let mut m = m.borrow_mut();
        if let Some((ma, mb, f, g)) = m.as_ref()
            && ma == a
            && mb == b
        {
            return (f.clone(), g.clone());
        }
        let (mut f, mut g) = (cubics(a), cubics(b));
        while f.len() < g.len() {
            f.push(point_like(&g[f.len()]));
        }
        while g.len() < f.len() {
            g.push(point_like(&f[g.len()]));
        }
        let (f, g): (Vec<Contour>, Vec<Contour>) = f
            .into_iter()
            .zip(g)
            .map(|(x, y)| {
                let n = x.curves.len().max(y.curves.len());
                (divided(x, n), divided(y, n))
            })
            .unzip();
        *m = Some((a.to_owned(), b.to_owned(), f.clone(), g.clone()));
        (f, g)
    });
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    let mut d = String::new();
    for (x, y) in from.iter().zip(&to) {
        use std::fmt::Write;
        let _ = write!(d, "M{:.3} {:.3}", lerp(x.start[0], y.start[0]), lerp(x.start[1], y.start[1]));
        for (p, q) in x.curves.iter().zip(&y.curves) {
            let v: Vec<f32> = (0..6).map(|i| lerp(p[i], q[i])).collect();
            let _ = write!(d, "C{:.3} {:.3} {:.3} {:.3} {:.3} {:.3}", v[0], v[1], v[2], v[3], v[4], v[5]);
        }
        if if t < 0.5 { x.closed } else { y.closed } {
            d.push('Z');
        }
    }
    d
}

/// Meshes kept by what shapes them.
#[derive(Default)]
pub struct Meshes {
    kept: HashMap<u64, Option<(Arc<Mesh>, f32)>>,
}

/// Past this many the cache starts again: a path whose outline moves is a
/// new mesh every frame.
const KEPT: usize = 512;

/// Which of a path's meshes.
#[derive(Hash, Clone, Copy)]
pub enum Part {
    Fill,
    Stroke,
}

impl Meshes {
    /// The key and the mesh of `part`, and its alpha (below 1 for a
    /// hairline); none when there is nothing to draw.
    pub fn get(&mut self, shape: &Shape, part: Part, scale: f32) -> Option<(u64, Arc<Mesh>, f32)> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        part.hash(&mut h);
        shape.outline.hash(&mut h);
        shape.points.iter().map(|v| v.to_bits()).collect::<Vec<_>>().hash(&mut h);
        scale.to_bits().hash(&mut h);
        match part {
            Part::Fill => shape.even_odd.hash(&mut h),
            Part::Stroke => {
                shape.stroke.map(f32::to_bits).hash(&mut h);
                (shape.join, shape.cap, shape.dash.iter().map(|v| v.to_bits()).collect::<Vec<_>>()).hash(&mut h);
                (shape.dash_offset.to_bits(), shape.miter_limit.to_bits(), shape.trim.map(f32::to_bits)).hash(&mut h);
            }
        }
        let key = h.finish();
        if let Some(found) = self.kept.get(&key) {
            return found.clone().map(|(m, a)| (key, m, a));
        }
        if self.kept.len() >= KEPT {
            self.kept.clear();
        }
        let made = outline(shape.outline, shape.points).and_then(|path| match part {
            Part::Fill => Some((fill(&path, shape.even_odd, scale), 1.0)),
            Part::Stroke => shape.stroke.filter(|w| *w > 0.0).and_then(|width| {
                let style = Style {
                    width,
                    join: shape.join,
                    cap: shape.cap,
                    dash: &shape.dash,
                    dash_offset: shape.dash_offset,
                    miter_limit: shape.miter_limit,
                    trim: shape.trim,
                };
                stroke(&path, &style, scale)
            }),
        });
        let made = made.filter(|(m, _)| !m.indices.is_empty()).map(|(m, a)| (Arc::new(m), a));
        self.kept.insert(key, made.clone());
        made.map(|(m, a)| (key, m, a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_data_and_polygons_are_read() {
        let arc = outline(Some("M 0 10 A 10 10 0 0 1 20 10 Z"), &[]).unwrap();
        let b = arc.bounds();
        assert!((b.left() - 0.0).abs() < 1e-3 && (b.right() - 20.0).abs() < 1e-3 && (b.top() - 0.0).abs() < 1e-3, "{b:?}");
        let relative = outline(Some("m5 5 h10 v10 h-10 z"), &[]).unwrap().bounds();
        assert_eq!((relative.left(), relative.top(), relative.right(), relative.bottom()), (5.0, 5.0, 15.0, 15.0));
        let polygon = outline(None, &[0.0, 0.0, 8.0, 0.0, 4.0, 6.0]).unwrap().bounds();
        assert_eq!((polygon.right(), polygon.bottom()), (8.0, 6.0));
        assert!(outline(Some("M 0 0 L"), &[]).is_none() || outline(Some("nonsense"), &[]).is_none());
    }

    #[test]
    fn meshes_follow_the_scale_not_the_colour() {
        let mut meshes = Meshes::default();
        let shape = Shape {
            outline: Some("M0 0 H10 V10 H0 Z"),
            points: &[],
            even_odd: false,
            stroke: Some(2.0),
            join: "miter",
            cap: "butt",
            dash: Vec::new(),
            dash_offset: 0.0,
            miter_limit: 4.0,
            trim: [0.0, 1.0],
        };
        let (k1, m1, _) = meshes.get(&shape, Part::Fill, 1.0).unwrap();
        let (k2, _, _) = meshes.get(&shape, Part::Fill, 1.0).unwrap();
        assert_eq!(k1, k2);
        let (k3, m3, _) = meshes.get(&shape, Part::Fill, 2.0).unwrap();
        assert_ne!(k1, k3);
        assert_eq!(m1.bounds, [-0.5, -0.5, 10.5, 10.5]);
        assert_eq!(m3.bounds, [-0.5, -0.5, 20.5, 20.5], "physical pixels at twice the scale");
        let (_, stroke, alpha) = meshes.get(&shape, Part::Stroke, 1.0).unwrap();
        assert_eq!(alpha, 1.0);
        assert_eq!(stroke.bounds, [-1.5, -1.5, 11.5, 11.5], "a 2-unit stroke centred on the outline, plus the fringe");
    }

    #[test]
    fn a_hairline_is_a_pixel_wide_and_fainter() {
        let path = outline(Some("M0 5 H20"), &[]).unwrap();
        let (mesh, alpha) = stroke(&path, &style(0.25, [0.0; 2], [0.0, 1.0]), 1.0).unwrap();
        assert_eq!(alpha, 0.25);
        assert!((mesh.bounds[3] - mesh.bounds[1] - 2.0).abs() < 1e-3, "one pixel plus the fringe: {:?}", mesh.bounds);
    }

    #[test]
    fn dashes_break_the_line() {
        let path = outline(Some("M0 0 H40"), &[]).unwrap();
        let (solid, _) = stroke(&path, &style(2.0, [0.0; 2], [0.0, 1.0]), 1.0).unwrap();
        let (dashed, _) = stroke(&path, &style(2.0, [5.0, 5.0], [0.0, 1.0]), 1.0).unwrap();
        assert!(dashed.indices.len() > solid.indices.len() * 3, "four dashes, each with its own fringe");
    }

    fn style(width: f32, dash: [f32; 2], trim: [f32; 2]) -> Style<'static> {
        let dash: &'static [f32] = if dash[0] > 0.0 { Box::leak(Box::new(dash)) } else { &[] };
        Style { width, join: "miter", cap: "butt", dash, dash_offset: 0.0, miter_limit: 4.0, trim }
    }

    #[test]
    fn a_trim_draws_part_of_each_outline() {
        let path = outline(Some("M0 0 H100 M0 20 H50"), &[]).unwrap();
        let (mesh, _) = stroke(&path, &style(2.0, [0.0; 2], [0.25, 0.75]), 1.0).unwrap();
        // The first line from 25 to 75, the second from 12.5 to 37.5.
        assert!((mesh.bounds[0] - 12.0).abs() < 0.1, "{:?}", mesh.bounds);
        assert!((mesh.bounds[2] - 75.5).abs() < 0.1, "{:?}", mesh.bounds);
        let (whole, _) = stroke(&path, &style(2.0, [0.0; 2], [0.0, 1.0]), 1.0).unwrap();
        assert!((whole.bounds[2] - 100.5).abs() < 0.1);
        assert!(stroke(&path, &style(2.0, [0.0; 2], [0.5, 0.5]), 1.0).is_none(), "nothing left");
        let curve = outline(Some("M0 0 C 30 40 70 40 100 0"), &[]).unwrap();
        let (all, _) = stroke(&curve, &style(2.0, [0.0; 2], [0.0, 1.0]), 1.0).unwrap();
        let (end, _) = stroke(&curve, &style(2.0, [0.0; 2], [0.001, 1.0]), 1.0).unwrap();
        assert!((end.bounds[2] - all.bounds[2]).abs() < 0.05, "a curve trimmed to 1 reaches its end: {:?} {:?}", end.bounds, all.bounds);
    }

    #[test]
    fn gradients_from_box_props() {
        let g = box_gradient([1.0; 4], [0.0, 0.0, 0.0, 1.0], &[], None, 0, false, [100.0, 50.0]).unwrap();
        let near = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);
        assert!(near(&g.params[..4], &[50.0, 0.0, 50.0, 50.0]), "top to bottom by default: {:?}", g.params);
        let across = box_gradient([1.0; 4], [0.0, 0.0, 0.0, 1.0], &[], None, 0, true, [100.0, 50.0]).unwrap();
        assert!((across.params[0] - 0.0).abs() < 1e-4 && (across.params[2] - 100.0).abs() < 1e-4);
        assert!(box_gradient([1.0; 4], [0.0; 4], &[], Some(45.0), 0, false, [10.0, 10.0]).is_none(), "no bottom, no stops: plain");
        let exact = gradient(&[2.0, 2.0, 1.0, 0.0, 0.0, 1.0, 5.0, 6.0, 10.0, 10.0, 8.0, 10.0, 10.0, 0.0], &[(0.0, [1.0; 4])], 2.0).unwrap();
        assert_eq!(exact.matrix, [0.5, 0.0, 0.0, 0.5, 5.0, 6.0], "logical units in, physical pixels at scale 2");
        assert_eq!(exact.spread, genoma_render::Spread::Reflect);
    }

    #[test]
    fn morphs_between_outlines() {
        let a = "M0 0 L10 0 L10 10 Z";
        let b = "M0 0 L20 0 L20 20 Z";
        let same = |x: &[Contour], y: &[Contour]| {
            x.len() == y.len()
                && x.iter().zip(y).all(|(p, q)| p.curves.len() == q.curves.len() && p.curves.iter().flatten().zip(q.curves.iter().flatten()).all(|(u, v)| (u - v).abs() < 1e-2))
        };
        assert!(same(&cubics(&morph(a, b, 0.0)), &cubics(a)));
        assert!(same(&cubics(&morph(a, b, 1.0)), &cubics(b)));
        let half = cubics(&morph(a, b, 0.5));
        assert_eq!([half[0].curves[0][4], half[0].curves[0][5]], [15.0, 0.0]);
        // A square into a triangle: divided until they match.
        let square = "M0 0 H10 V10 H0 Z";
        let m = cubics(&morph(square, a, 0.3));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].curves.len(), 4);
        // One contour into two: the missing one grows from a point.
        let two = cubics(&morph(a, "M0 0 L10 0 L10 10 Z M20 20 L30 20 L30 30 Z", 0.0));
        assert_eq!(two.len(), 2);
        assert!(two[1].curves.iter().all(|c| c[4] == 20.0 && c[5] == 20.0));
    }
}
