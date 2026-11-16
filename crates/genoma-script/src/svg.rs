//! `g.svg`: an SVG document as a subtree of nodes (docs/svg-plan.md,
//! phase 3). usvg reads it (CSS, `use`, `symbol`, inherited attributes,
//! markers and text resolved into groups and paths); every group becomes a
//! box carrying its transform and opacity, every path a `path` node, so an
//! element with an `id` is a node the interface can address, bind and
//! animate. Nothing is rasterised: the paths are drawn on the GPU.
//!
//! `currentColor` follows the node's `color`: the document is read with a
//! colour nobody uses as its current one, and whatever comes back in that
//! colour takes `color` instead, a value or a binding.

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use genoma_scene::usvg;
use mlua::{Lua, Table, Value as LuaValue};

/// The colour `currentColor` is read as, to be found again.
const CURRENT: (u8, u8, u8) = (0xfe, 0x01, 0xfd);

/// The system's fonts, read once and only for a document with text.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

fn hex(c: usvg::Color, alpha: f32) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c.red, c.green, c.blue, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The inverse of an SVG matrix: what takes the user space to a
/// gradient's own.
fn inverse(t: usvg::Transform) -> Option<[f32; 6]> {
    let det = t.sx * t.sy - t.kx * t.ky;
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (t.sy / det, -t.ky / det, -t.kx / det, t.sx / det);
    Some([a, b, c, d, -(a * t.tx + c * t.ty), -(b * t.tx + d * t.ty)])
}

/// What a paint sets on a path node, under `which` ("fill" or "stroke"):
/// a colour, or an exact gradient and its stops.
fn paint(lua: &Lua, node: &Table, which: &str, paint: &usvg::Paint, opacity: f32, current: &LuaValue) -> mlua::Result<()> {
    let colour_key = if which == "fill" { "fill" } else { "color" };
    let gradient = |kind: f32, base: &usvg::BaseGradient, params: [f32; 6]| -> mlua::Result<()> {
        let Some(m) = inverse(base.transform()) else { return Ok(()) };
        let spread = match base.spread_method() {
            usvg::SpreadMethod::Pad => 0.0,
            usvg::SpreadMethod::Repeat => 1.0,
            usvg::SpreadMethod::Reflect => 2.0,
        };
        let mut numbers = vec![kind, spread];
        numbers.extend_from_slice(&m);
        numbers.extend_from_slice(&params);
        node.set(format!("{which}_paint"), numbers)?;
        let stops = lua.create_table()?;
        for (i, s) in base.stops().iter().take(8).enumerate() {
            let stop = lua.create_table()?;
            stop.set(1, s.offset().get())?;
            stop.set(2, hex(s.color(), s.opacity().get() * opacity))?;
            stops.set(i + 1, stop)?;
        }
        node.set(format!("{which}_stops"), stops)?;
        // The paint is the gradient's; the colour must not hide it.
        node.set(colour_key, "#00000000")
    };
    match paint {
        usvg::Paint::Color(c) if (c.red, c.green, c.blue) == CURRENT && !current.is_nil() => node.set(colour_key, current.clone()),
        usvg::Paint::Color(c) => node.set(colour_key, hex(*c, opacity)),
        usvg::Paint::LinearGradient(l) => gradient(1.0, l, [l.x1(), l.y1(), l.x2(), l.y2(), 0.0, 0.0]),
        usvg::Paint::RadialGradient(r) => gradient(2.0, r, [r.cx(), r.cy(), r.r().get(), r.fx(), r.fy(), r.fr().get()]),
        usvg::Paint::Pattern(_) => {
            log::info!("g.svg: a pattern is not drawn yet; that part is left out");
            Ok(())
        }
    }
}

/// An SVG matrix as `affine` takes it, none when it changes nothing.
fn affine(t: usvg::Transform) -> Option<[f32; 6]> {
    (!t.is_identity()).then_some([t.sx, t.ky, t.kx, t.sy, t.tx, t.ty])
}

/// The node every child of the document sits in: at its parent's corner,
/// out of the flow, as large as the document's own space.
fn layer(lua: &Lua, kind: &str, id: &str, size: [f32; 2]) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("__kind", kind)?;
    t.set("overlay", true)?;
    t.set("attach", "corner")?;
    t.set("width", size[0])?;
    t.set("height", size[1])?;
    if !id.is_empty() {
        t.set("id", id)?;
    }
    Ok(t)
}

/// A path's data as `d` writes it.
fn data(path: &usvg::tiny_skia_path::Path) -> String {
    use std::fmt::Write;
    let mut d = String::new();
    for segment in path.segments() {
        let _ = match segment {
            usvg::tiny_skia_path::PathSegment::MoveTo(p) => write!(d, "M{} {}", p.x, p.y),
            usvg::tiny_skia_path::PathSegment::LineTo(p) => write!(d, "L{} {}", p.x, p.y),
            usvg::tiny_skia_path::PathSegment::QuadTo(c, p) => write!(d, "Q{} {} {} {}", c.x, c.y, p.x, p.y),
            usvg::tiny_skia_path::PathSegment::CubicTo(c1, c2, p) => write!(d, "C{} {} {} {} {} {}", c1.x, c1.y, c2.x, c2.y, p.x, p.y),
            usvg::tiny_skia_path::PathSegment::Close => write!(d, "Z"),
        };
    }
    d
}

struct Build<'a> {
    lua: &'a Lua,
    size: [f32; 2],
    current: LuaValue,
    parts: Option<Table>,
    warned: std::cell::Cell<bool>,
}

impl Build<'_> {
    /// The props `parts` gives the node with this id, over its own.
    fn with_parts(&self, node: &Table, id: &str) -> mlua::Result<()> {
        if id.is_empty() {
            return Ok(());
        }
        if let Some(own) = self.parts.as_ref().and_then(|p| p.get::<Option<Table>>(id).ok().flatten()) {
            for pair in own.pairs::<LuaValue, LuaValue>() {
                let (k, v) = pair?;
                node.set(k, v)?;
            }
        }
        Ok(())
    }

    fn group(&self, g: &usvg::Group) -> mlua::Result<Table> {
        let node = layer(self.lua, "box", g.id(), self.size)?;
        if let Some(m) = affine(g.transform()) {
            node.set("affine", m.to_vec())?;
        }
        if g.opacity().get() < 1.0 {
            node.set("opacity", g.opacity().get())?;
        }
        if (g.clip_path().is_some() || g.mask().is_some() || !g.filters().is_empty()) && !self.warned.replace(true) {
            log::info!("g.svg: clip paths, masks and filters are not drawn yet; their groups are drawn without them");
        }
        let mut i = 1;
        for child in g.children() {
            let made = match child {
                usvg::Node::Group(g) => Some(self.group(g)?),
                usvg::Node::Path(p) => self.path(p)?,
                usvg::Node::Text(t) => Some(self.group(t.flattened())?),
                usvg::Node::Image(_) => {
                    log::info!("g.svg: an embedded image is not drawn yet; it is left out");
                    None
                }
            };
            if let Some(made) = made {
                node.set(i, made)?;
                i += 1;
            }
        }
        self.with_parts(&node, g.id())?;
        Ok(node)
    }

    fn path(&self, p: &usvg::Path) -> mlua::Result<Option<Table>> {
        if !p.is_visible() || (p.fill().is_none() && p.stroke().is_none()) {
            return Ok(None);
        }
        let node = layer(self.lua, "path", p.id(), self.size)?;
        node.set("d", data(p.data()))?;
        node.set("fill", "#00000000")?;
        if let Some(fill) = p.fill() {
            paint(self.lua, &node, "fill", fill.paint(), fill.opacity().get(), &self.current)?;
            if fill.rule() == usvg::FillRule::EvenOdd {
                node.set("even_odd", true)?;
            }
        }
        if let Some(stroke) = p.stroke() {
            paint(self.lua, &node, "stroke", stroke.paint(), stroke.opacity().get(), &self.current)?;
            node.set("stroke", stroke.width().get())?;
            node.set(
                "cap",
                match stroke.linecap() {
                    usvg::LineCap::Round => "round",
                    usvg::LineCap::Square => "square",
                    usvg::LineCap::Butt => "butt",
                },
            )?;
            node.set(
                "join",
                match stroke.linejoin() {
                    usvg::LineJoin::Round => "round",
                    usvg::LineJoin::Bevel => "bevel",
                    usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => "miter",
                },
            )?;
            node.set("miter_limit", stroke.miterlimit().get())?;
            if let Some(dashes) = stroke.dasharray() {
                node.set("dash_array", dashes.to_vec())?;
                node.set("dash_offset", stroke.dashoffset())?;
            }
        }
        self.with_parts(&node, p.id())?;
        Ok(Some(node))
    }
}

/// `g.svg { src = "…svg" | source = "<svg…>", width, height, color, parts,
/// … }`: the document fitted into width × height (its own size by
/// default), centred, its aspect kept. Other keys are the root box's.
pub fn svg(lua: &Lua, t: Table, dir: &Path) -> mlua::Result<Table> {
    let source: String = match (t.get::<Option<String>>("source")?, t.get::<Option<String>>("src")?) {
        (Some(s), _) => s,
        (None, Some(src)) => {
            let mut path = crate::io::expand(&src);
            if path.is_relative() {
                path = dir.join(path);
            }
            std::fs::read_to_string(&path).map_err(|e| mlua::Error::runtime(format!("g.svg: {}: {e}", path.display())))?
        }
        (None, None) => return Err(mlua::Error::runtime("g.svg needs src or source")),
    };
    let current: LuaValue = t.get("color")?;
    // currentColor read as the colour nobody uses, to be found again.
    let marked = if current.is_nil() {
        source.clone()
    } else {
        match source.find("<svg") {
            Some(at) => format!("{}<svg color=\"#{:02x}{:02x}{:02x}\"{}", &source[..at], CURRENT.0, CURRENT.1, CURRENT.2, &source[at + 4..]),
            None => source.clone(),
        }
    };
    let mut options = usvg::Options::default();
    if marked.contains("<text") {
        options.fontdb = fonts();
    }
    let tree = usvg::Tree::from_str(&marked, &options).map_err(|e| mlua::Error::runtime(format!("g.svg: {e}")))?;
    let size = [tree.size().width(), tree.size().height()];
    let width: f32 = t.get::<Option<f32>>("width")?.unwrap_or(size[0]);
    let height: f32 = t.get::<Option<f32>>("height")?.unwrap_or(size[1]);
    let build = Build { lua, size, current: current.clone(), parts: t.get("parts")?, warned: std::cell::Cell::new(false) };
    let canvas = build.group(tree.root())?;
    // Fitted, centred, the aspect kept, as preserveAspectRatio's default.
    let k = (width / size[0].max(1e-6)).min(height / size[1].max(1e-6));
    let fit = [k, 0.0, 0.0, k, (width - size[0] * k) * 0.5, (height - size[1] * k) * 0.5];
    let inner = layer(lua, "box", "", size)?;
    if fit != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
        inner.set("affine", fit.to_vec())?;
    }
    inner.set(1, canvas)?;
    // The root: the caller's keys, but those that made the document.
    let root = lua.create_table()?;
    for pair in t.pairs::<LuaValue, LuaValue>() {
        let (k, v) = pair?;
        if let LuaValue::String(s) = &k
            && matches!(s.to_str()?.as_ref(), "src" | "source" | "parts" | "color" | "__kind")
        {
            continue;
        }
        root.set(k, v)?;
    }
    root.set("__kind", "box")?;
    root.set("width", width)?;
    root.set("height", height)?;
    root.set(root.raw_len() + 1, inner)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 48 48">
        <g id="hand" transform="translate(4 2)" opacity="0.5">
            <path id="arm" d="M0 0 H10" stroke="currentColor" stroke-width="2" stroke-dasharray="3 1 2"/>
        </g>
        <circle id="dot" cx="24" cy="24" r="4" fill="#ff0000" fill-opacity="0.5"/>
    </svg>"##;

    fn find(t: &Table, id: &str) -> Option<Table> {
        if t.get::<Option<String>>("id").ok().flatten().as_deref() == Some(id) {
            return Some(t.clone());
        }
        t.sequence_values::<Table>().flatten().find_map(|c| find(&c, id))
    }

    #[test]
    fn a_document_becomes_addressable_nodes() {
        let lua = Lua::new();
        let t: Table = lua.load(format!("return {{ source = [==[{DOC}]==], width = 48, color = '#00ff00', id = 'icon', parts = {{ dot = {{ opacity = 0.25 }} }} }}")).eval().unwrap();
        let root = svg(&lua, t, Path::new("/")).unwrap();
        assert_eq!(root.get::<String>("__kind").unwrap(), "box");
        assert_eq!(root.get::<String>("id").unwrap(), "icon", "the caller's keys stay on the root");
        assert_eq!((root.get::<f32>("width").unwrap(), root.get::<f32>("height").unwrap()), (48.0, 24.0));
        let hand = find(&root, "hand").unwrap();
        assert_eq!(hand.get::<Vec<f32>>("affine").unwrap(), vec![1.0, 0.0, 0.0, 1.0, 4.0, 2.0]);
        assert_eq!(hand.get::<f32>("opacity").unwrap(), 0.5);
        let arm = find(&root, "arm").unwrap();
        assert_eq!(arm.get::<String>("__kind").unwrap(), "path");
        assert_eq!(arm.get::<String>("color").unwrap(), "#00ff00", "currentColor takes the node's colour");
        assert_eq!(arm.get::<Vec<f32>>("dash_array").unwrap(), vec![3.0, 1.0, 2.0, 3.0, 1.0, 2.0], "an odd count repeated, as SVG reads it");
        let dot = find(&root, "dot").unwrap();
        assert_eq!(dot.get::<String>("fill").unwrap(), "#ff000080");
        assert_eq!(dot.get::<f32>("opacity").unwrap(), 0.25, "parts are laid over");
        assert!(dot.get::<String>("d").unwrap().starts_with('M'));
    }
}
