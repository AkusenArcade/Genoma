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
    /// Dash and gap, logical units; a dash of 0 is a solid line.
    pub dash: [f32; 2],
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

/// The stroke's mesh at `width` logical units, and the alpha it is drawn
/// with: a line thinner than a physical pixel is drawn a pixel wide and
/// that much fainter, as a hairline is.
pub fn stroke(path: &tiny_skia::Path, width: f32, join: &str, cap: &str, dash: [f32; 2], scale: f32) -> Option<(Mesh, f32)> {
    let physical_width = width * scale;
    let (drawn, alpha) = if physical_width < 1.0 { (1.0 / scale, physical_width.max(0.0)) } else { (width, 1.0) };
    let dashed;
    let path = if dash[0] > 0.0 {
        let pattern = StrokeDash::new(vec![dash[0], dash[1].max(0.0)], 0.0)?;
        dashed = path.dash(&pattern, scale)?;
        &dashed
    } else {
        path
    };
    let stroke = Stroke {
        width: drawn,
        miter_limit: 4.0,
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
                (shape.join, shape.cap, shape.dash.map(f32::to_bits)).hash(&mut h);
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
            Part::Stroke => shape.stroke.filter(|w| *w > 0.0).and_then(|w| stroke(&path, w, shape.join, shape.cap, shape.dash, scale)),
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
        let shape = Shape { outline: Some("M0 0 H10 V10 H0 Z"), points: &[], even_odd: false, stroke: Some(2.0), join: "miter", cap: "butt", dash: [0.0; 2] };
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
        let (mesh, alpha) = stroke(&path, 0.25, "miter", "butt", [0.0; 2], 1.0).unwrap();
        assert_eq!(alpha, 0.25);
        assert!((mesh.bounds[3] - mesh.bounds[1] - 2.0).abs() < 1e-3, "one pixel plus the fringe: {:?}", mesh.bounds);
    }

    #[test]
    fn dashes_break_the_line() {
        let path = outline(Some("M0 0 H40"), &[]).unwrap();
        let (solid, _) = stroke(&path, 2.0, "miter", "butt", [0.0; 2], 1.0).unwrap();
        let (dashed, _) = stroke(&path, 2.0, "miter", "butt", [5.0, 5.0], 1.0).unwrap();
        assert!(dashed.indices.len() > solid.indices.len() * 3, "four dashes, each with its own fringe");
    }
}
