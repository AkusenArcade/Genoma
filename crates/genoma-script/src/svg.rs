//! `g.svg`: an SVG document as a subtree of nodes (docs/svg-plan.md,
//! phase 3). usvg reads it (CSS, `use`, `symbol`, inherited attributes,
//! markers and text resolved into groups and paths); every group becomes a
//! box carrying its transform and opacity, every path a `path` node, so an
//! element with an `id` is a node the interface can address, bind and
//! animate. Nothing is rasterised: the paths are drawn on the GPU.
//!
//! `currentColor` follows the node's `color`: the document is read with a
//! colour nobody uses as its current one, and whatever comes back in that
//! colour takes `color` instead, a value or a binding. With
//! `gradient_top` and `gradient_bottom` it takes `g.icon`'s 165° gradient
//! across the whole document instead, from one to the other.

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use genoma_scene::{resvg, usvg};
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

/// How a part's paints are taken: as they are, as a clip's shape (opaque,
/// whatever its colour), or as a luminance mask's content (its brightness
/// becomes its alpha, as the mask is read by alpha).
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Paint,
    Clip,
    Luminance,
}

/// `colour` ("#rgb", "#rrggbb" or "#rrggbbaa") with its alpha multiplied by
/// `alpha`; anything else as it is.
fn faded(colour: &str, alpha: f32) -> String {
    let Some(hex) = colour.strip_prefix('#').filter(|h| h.chars().all(|c| c.is_ascii_hexdigit())) else { return colour.to_owned() };
    let full: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).chain("ff".chars()).collect(),
        6 => format!("{hex}ff"),
        8 => hex.to_owned(),
        _ => return colour.to_owned(),
    };
    let Ok(a) = u8::from_str_radix(&full[6..8], 16) else { return colour.to_owned() };
    format!("#{}{:02x}", &full[..6], (a as f32 * alpha.clamp(0.0, 1.0)).round() as u8)
}

/// The node's colour for currentColor, at `alpha`: a value faded, or a
/// binding that fades what it returns.
fn current_at(lua: &Lua, current: &LuaValue, alpha: f32) -> mlua::Result<LuaValue> {
    if alpha >= 1.0 {
        return Ok(current.clone());
    }
    Ok(match current {
        LuaValue::String(s) => LuaValue::String(lua.create_string(faded(&s.to_str()?, alpha))?),
        LuaValue::Function(f) => {
            let f = f.clone();
            LuaValue::Function(lua.create_function(move |lua, ()| {
                Ok(match f.call::<LuaValue>(())? {
                    LuaValue::String(s) => LuaValue::String(lua.create_string(faded(&s.to_str()?, alpha))?),
                    other => other,
                })
            })?)
        }
        other => other.clone(),
    })
}

/// `gradient_top` and `gradient_bottom`: what currentColor becomes, as
/// `g.icon` paints a glyph — a 165° line across the whole document, in the
/// document's own units.
struct Tint {
    top: LuaValue,
    bottom: LuaValue,
    /// x1, y1, x2, y2 in the document's space.
    line: [f32; 4],
}

impl Tint {
    fn new(top: LuaValue, bottom: LuaValue, size: [f32; 2]) -> Self {
        let r = 165f32.to_radians();
        let (dx, dy) = (r.sin() / 2.0, -r.cos() / 2.0);
        let [w, h] = size;
        Tint { top, bottom, line: [(0.5 - dx) * w, (0.5 - dy) * h, (0.5 + dx) * w, (0.5 + dy) * h] }
    }

    /// The two stops at `alpha`: a table, or a binding when either end is.
    fn stops(&self, lua: &Lua, alpha: f32) -> mlua::Result<LuaValue> {
        let top = current_at(lua, &self.top, alpha)?;
        let bottom = current_at(lua, &self.bottom, alpha)?;
        fn table(lua: &Lua, top: LuaValue, bottom: LuaValue) -> mlua::Result<Table> {
            let first = lua.create_table()?;
            first.set(1, 0.0)?;
            first.set(2, top)?;
            let last = lua.create_table()?;
            last.set(1, 1.0)?;
            last.set(2, bottom)?;
            lua.create_sequence_from([first, last])
        }
        if !top.is_function() && !bottom.is_function() {
            return Ok(LuaValue::Table(table(lua, top, bottom)?));
        }
        let read = |v: &LuaValue| -> mlua::Result<LuaValue> {
            match v {
                LuaValue::Function(f) => f.call(()),
                other => Ok(other.clone()),
            }
        };
        Ok(LuaValue::Function(lua.create_function(move |lua, ()| table(lua, read(&top)?, read(&bottom)?))?))
    }
}

fn luminance(c: usvg::Color, alpha: f32) -> String {
    let l = (0.2125 * c.red as f32 + 0.7154 * c.green as f32 + 0.0721 * c.blue as f32) / 255.0;
    hex(usvg::Color::white(), l * alpha)
}

/// What a paint sets on a path node, under `which` ("fill" or "stroke"):
/// a colour, or an exact gradient and its stops.
/// `place` takes the path's own units to the document's, where a tint's
/// line is drawn.
#[allow(clippy::too_many_arguments)]
fn paint(
    lua: &Lua,
    node: &Table,
    which: &str,
    paint: &usvg::Paint,
    opacity: f32,
    current: &LuaValue,
    tint: Option<&Tint>,
    place: usvg::Transform,
    mode: Mode,
) -> mlua::Result<()> {
    let colour_key = if which == "fill" { "fill" } else { "color" };
    if mode == Mode::Clip {
        return node.set(colour_key, "#ffffffff");
    }
    let colour = |c: usvg::Color, a: f32| if mode == Mode::Luminance { luminance(c, a) } else { hex(c, a) };
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
            stop.set(2, colour(s.color(), s.opacity().get() * opacity))?;
            stops.set(i + 1, stop)?;
        }
        node.set(format!("{which}_stops"), stops)?;
        // The paint is the gradient's; the colour must not hide it.
        node.set(colour_key, "#00000000")
    };
    let marked = |c: &usvg::Color| (c.red, c.green, c.blue) == CURRENT && mode == Mode::Paint;
    match paint {
        usvg::Paint::Color(c) if marked(c) && tint.is_some() => {
            let tint = tint.expect("checked");
            let mut numbers = vec![1.0, 0.0, place.sx, place.ky, place.kx, place.sy, place.tx, place.ty];
            numbers.extend_from_slice(&tint.line);
            numbers.extend_from_slice(&[0.0, 0.0]);
            node.set(format!("{which}_paint"), numbers)?;
            node.set(format!("{which}_stops"), tint.stops(lua, opacity)?)?;
            node.set(colour_key, "#00000000")
        }
        usvg::Paint::Color(c) if marked(c) && !current.is_nil() => {
            node.set(colour_key, current_at(lua, current, opacity)?)
        }
        usvg::Paint::Color(c) => node.set(colour_key, colour(*c, opacity)),
        usvg::Paint::LinearGradient(l) => gradient(1.0, l, [l.x1(), l.y1(), l.x2(), l.y2(), 0.0, 0.0]),
        usvg::Paint::RadialGradient(r) => gradient(2.0, r, [r.cx(), r.cy(), r.r().get(), r.fx(), r.fy(), r.fr().get()]),
        // Drawn by resvg instead (`Build::raster`), never reaching here.
        usvg::Paint::Pattern(_) => Ok(()),
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
    tint: Option<Tint>,
    parts: Option<Table>,
    warned: std::cell::Cell<bool>,
    /// Pixels a document unit for what resvg draws in Genoma's place.
    resolution: f32,
    /// Logical units a document unit, as the document is fitted.
    fit: f32,
    /// The document's, naming the pictures resvg draws.
    key: u64,
    drawn: std::cell::Cell<u32>,
}

/// A filter Genoma draws itself: one Gaussian blur of the group as it is,
/// the same both ways — its standard deviation, in the group's units.
fn plain_blur(g: &usvg::Group) -> Option<f32> {
    let [filter] = g.filters() else { return None };
    let [primitive] = filter.primitives() else { return None };
    match primitive.kind() {
        usvg::filter::Kind::GaussianBlur(b) if *b.input() == usvg::filter::Input::SourceGraphic && b.std_dev_x() == b.std_dev_y() => {
            Some(b.std_dev_x().get())
        }
        _ => None,
    }
}

/// A filter resvg draws in Genoma's place.
fn other_filter(g: &usvg::Group) -> bool {
    !g.filters().is_empty() && plain_blur(g).is_none()
}

fn is_pattern(p: &usvg::Path) -> bool {
    matches!(p.fill().map(|f| f.paint()), Some(usvg::Paint::Pattern(_))) || matches!(p.stroke().map(|s| s.paint()), Some(usvg::Paint::Pattern(_)))
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

    fn group(&self, g: &usvg::Group, mode: Mode) -> mlua::Result<Table> {
        let node = layer(self.lua, "box", g.id(), self.size)?;
        if let Some(m) = affine(g.transform()) {
            node.set("affine", m.to_vec())?;
        }
        if g.opacity().get() < 1.0 {
            node.set("opacity", g.opacity().get())?;
            // As SVG has it: its children do not show through each other.
            node.set("isolate", true)?;
        }
        if let Some(deviation) = plain_blur(g) {
            // blur_radius is the deviation in logical units: the group's
            // own scaled by everything above it and by the fit.
            let t = g.abs_transform();
            let enlarged = (t.sx * t.sy - t.kx * t.ky).abs().sqrt();
            node.set("blur_radius", deviation * enlarged * self.fit)?;
        }
        let mut i = 1;
        for child in g.children() {
            let made = match child {
                usvg::Node::Group(inner) if other_filter(inner) && mode == Mode::Paint => {
                    if !self.warned.replace(true) {
                        log::info!("g.svg: a filter other than a plain blur is drawn by resvg, its group as one picture");
                    }
                    self.raster(child, g.abs_transform())?
                }
                usvg::Node::Group(inner) => Some(self.group(inner, mode)?),
                usvg::Node::Path(p) if is_pattern(p) && mode == Mode::Paint => self.raster(child, g.abs_transform())?,
                usvg::Node::Path(p) => self.path(p, mode)?,
                usvg::Node::Text(t) => Some(self.group(t.flattened(), mode)?),
                usvg::Node::Image(_) if mode == Mode::Paint => self.raster(child, g.abs_transform())?,
                usvg::Node::Image(_) => None,
            };
            if let Some(made) = made {
                node.set(i, made)?;
                i += 1;
            }
        }
        self.with_parts(&node, g.id())?;
        // A clip and a mask each multiply what shows: with both, the clip
        // cuts an inner box and the mask the outer.
        let clip = g.clip_path().map(|c| self.clip(c)).transpose()?;
        let mask = g.mask().map(|m| self.mask(m)).transpose()?;
        let node = match (clip, mask) {
            (None, None) => node,
            (Some(cut), None) | (None, Some(cut)) => {
                node.set(node.raw_len() + 1, cut)?;
                node
            }
            (Some(cut), Some(mask)) => {
                let inner = layer(self.lua, "box", "", self.size)?;
                for pair in node.clone().sequence_values::<Table>() {
                    inner.set(inner.raw_len() + 1, pair?)?;
                }
                for k in 1..=node.raw_len() {
                    node.raw_set(k, LuaValue::Nil)?;
                }
                inner.set(inner.raw_len() + 1, cut)?;
                node.set(1, inner)?;
                node.set(2, mask)?;
                node
            }
        };
        Ok(node)
    }

    /// A clip path as a mask child: its shapes, opaque.
    fn clip(&self, c: &usvg::ClipPath) -> mlua::Result<Table> {
        let mask = layer(self.lua, "box", "", self.size)?;
        mask.set("mask", true)?;
        if let Some(m) = affine(c.transform()) {
            mask.set("affine", m.to_vec())?;
        }
        mask.set(1, self.group(c.root(), Mode::Clip)?)?;
        if c.clip_path().is_some() {
            log::info!("g.svg: a clip path's own clip path is not drawn yet");
        }
        Ok(mask)
    }

    /// A mask as a mask child: read by alpha, or by luminance turned into
    /// alpha.
    fn mask(&self, m: &usvg::Mask) -> mlua::Result<Table> {
        let mask = layer(self.lua, "box", "", self.size)?;
        mask.set("mask", true)?;
        let mode = if m.kind() == usvg::MaskType::Luminance { Mode::Luminance } else { Mode::Paint };
        mask.set(1, self.group(m.root(), mode)?)?;
        if m.mask().is_some() {
            log::info!("g.svg: a mask's own mask is not drawn yet");
        }
        Ok(mask)
    }

    /// What Genoma does not draw itself (a pattern, an embedded image),
    /// drawn by resvg into a picture at the document's resolution and laid
    /// where it was, in its place among its siblings.
    fn raster(&self, node: &usvg::Node, parent: usvg::Transform) -> mlua::Result<Option<Table>> {
        let Some(bbox) = node.abs_layer_bounding_box() else { return Ok(None) };
        // A shape's layer box leaves its stroke out: the picture takes both.
        let stroke = node.abs_stroke_bounding_box();
        let (left, top) = (bbox.x().min(stroke.x()), bbox.y().min(stroke.y()));
        let (right, bottom) = (bbox.right().max(stroke.right()), bbox.bottom().max(stroke.bottom()));
        let k = self.resolution;
        // Out to whole pixels of the document as shown: the picture lands
        // on the pixel grid and halves exactly, with no blur from either.
        let grid = self.fit.max(1e-3);
        let (x0, y0) = ((left * grid).floor() / grid, (top * grid).floor() / grid);
        let (x1, y1) = ((right * grid).ceil() / grid, (bottom * grid).ceil() / grid);
        let (w, h) = (((x1 - x0) * k).round().max(1.0) as u32, ((y1 - y0) * k).round().max(1.0) as u32);
        let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(w, h) else { return Ok(None) };
        // render_node moves the node by its absolute box's corner but draws
        // it in its own space: the ancestors' transform goes in between.
        let (bx, by) = (bbox.x(), bbox.y());
        let transform = resvg::tiny_skia::Transform::from_scale(k, k).pre_translate(-x0, -y0).pre_concat(parent).pre_translate(bx, by);
        resvg::render_node(node, transform, &mut pixmap.as_mut());
        let n = self.drawn.get();
        self.drawn.set(n + 1);
        let folder = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("genoma").join("svg");
        let file = folder.join(format!("{:016x}-{n}-{w}x{h}.png", self.key));
        if !file.exists() {
            let saved = std::fs::create_dir_all(&folder).map_err(|e| e.to_string()).and_then(|()| pixmap.save_png(&file).map_err(|e| e.to_string()));
            if let Err(e) = saved {
                log::warn!("g.svg: {}: {e}", file.display());
                return Ok(None);
            }
        }
        // In the document's own space: the ancestors' transform undone.
        let undo = layer(self.lua, "box", "", self.size)?;
        if let Some(inverse) = inverse(parent)
            && inverse != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        {
            undo.set("affine", inverse.to_vec())?;
        }
        let image = self.lua.create_table()?;
        image.set("__kind", "image")?;
        image.set("overlay", true)?;
        image.set("attach", "corner")?;
        image.set("offset_x", x0)?;
        image.set("offset_y", y0)?;
        image.set("width", x1 - x0)?;
        image.set("height", y1 - y0)?;
        image.set("src", file.to_string_lossy().as_ref())?;
        // Exactly over its box: the picture is the box's shape already.
        image.set("fit", "fill")?;
        if !node.id().is_empty() {
            image.set("id", node.id())?;
        }
        self.with_parts(&image, node.id())?;
        undo.set(1, image)?;
        log::debug!("g.svg: {} drawn by resvg, {w}×{h}", if node.id().is_empty() { "an element" } else { node.id() });
        Ok(Some(undo))
    }

    fn path(&self, p: &usvg::Path, mode: Mode) -> mlua::Result<Option<Table>> {
        if !p.is_visible() || (p.fill().is_none() && p.stroke().is_none()) {
            return Ok(None);
        }
        let node = layer(self.lua, "path", p.id(), self.size)?;
        node.set("d", data(p.data()))?;
        node.set("fill", "#00000000")?;
        if let Some(fill) = p.fill() {
            paint(self.lua, &node, "fill", fill.paint(), fill.opacity().get(), &self.current, self.tint.as_ref(), p.abs_transform(), mode)?;
            if fill.rule() == usvg::FillRule::EvenOdd {
                node.set("even_odd", true)?;
            }
        }
        // A clip is its shapes' interiors, not their strokes.
        if let Some(stroke) = p.stroke().filter(|_| mode != Mode::Clip) {
            paint(self.lua, &node, "stroke", stroke.paint(), stroke.opacity().get(), &self.current, self.tint.as_ref(), p.abs_transform(), mode)?;
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
    let (top, bottom): (LuaValue, LuaValue) = (t.get("gradient_top")?, t.get("gradient_bottom")?);
    let tinted = !top.is_nil() && !bottom.is_nil();
    // currentColor read as the colour nobody uses, to be found again.
    let marked = if current.is_nil() && !tinted {
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
    // Fitted, centred, the aspect kept, as preserveAspectRatio's default.
    let k = (width / size[0].max(1e-6)).min(height / size[1].max(1e-6));
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        marked.hash(&mut h);
        h.finish()
    };
    // What resvg draws, at twice the size the document is shown: sharp on
    // a doubled screen too.
    let build = Build {
        lua,
        size,
        current: current.clone(),
        tint: tinted.then(|| Tint::new(top, bottom, size)),
        parts: t.get("parts")?,
        warned: std::cell::Cell::new(false),
        resolution: (k * 2.0).max(1.0),
        fit: k,
        key,
        drawn: std::cell::Cell::new(0),
    };
    let canvas = build.group(tree.root(), Mode::Paint)?;
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
            && matches!(s.to_str()?.as_ref(), "src" | "source" | "parts" | "color" | "gradient_top" | "gradient_bottom" | "__kind")
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
        assert!(hand.get::<bool>("isolate").unwrap(), "group opacity, as SVG has it");
    }

    #[test]
    fn a_tint_is_the_icons_gradient_across_the_document() {
        let lua = Lua::new();
        let t: Table = lua
            .load(format!("return {{ source = [==[{DOC}]==], gradient_top = '#ffffff', gradient_bottom = function() return '#000000' end }}"))
            .eval()
            .unwrap();
        let root = svg(&lua, t, Path::new(".")).unwrap();
        assert!(root.get::<LuaValue>("gradient_top").unwrap().is_nil(), "the tint is not the root box's");
        let arm = find(&root, "arm").expect("arm");
        assert_eq!(arm.get::<String>("color").unwrap(), "#00000000", "the gradient paints the stroke");
        let paint: Vec<f32> = arm.get("stroke_paint").unwrap();
        // Linear, padded; the arm's own units to the document's (its group
        // moved it by 4, 2 in the document's 48 units, shown at 24); then
        // the 165° line across the document's 24 × 24.
        assert_eq!(&paint[..2], &[1.0, 0.0]);
        assert!((paint[6] - 2.0).abs() < 1e-4 && (paint[7] - 1.0).abs() < 1e-4, "{paint:?}");
        assert!((paint[8] - 8.894).abs() < 0.01 && (paint[11] - 23.59).abs() < 0.01, "{paint:?}");
        // A binding at either end makes the stops a binding (the group's
        // opacity stays the group's box's).
        let stops: mlua::Function = arm.get("stroke_stops").unwrap();
        let stops: Vec<Table> = stops.call(()).unwrap();
        let stops: Vec<(f32, String)> = stops.iter().map(|s| (s.get(1).unwrap(), s.get(2).unwrap())).collect();
        assert_eq!(stops, vec![(0.0, "#ffffff".to_owned()), (1.0, "#000000".to_owned())]);
        // A colour of the document's own keeps it.
        let dot = find(&root, "dot").expect("dot");
        assert!(dot.get::<LuaValue>("fill_paint").unwrap().is_nil());
    }

    #[test]
    fn current_colour_keeps_its_opacity() {
        assert_eq!(faded("#ffffff", 0.25), "#ffffff40");
        assert_eq!(faded("#abc", 1.0), "#aabbccff");
        assert_eq!(faded("#00000080", 0.5), "#00000040");
        assert_eq!(faded("red", 0.5), "red");
    }

    #[test]
    fn clips_and_masks_become_mask_children() {
        let lua = Lua::new();
        let doc = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
            <clipPath id="c"><circle cx="10" cy="10" r="5" fill="red" stroke="blue"/></clipPath>
            <mask id="m"><rect width="20" height="20" fill="#808080"/></mask>
            <g id="both" clip-path="url(#c)" mask="url(#m)"><rect id="r" width="20" height="20" fill="green"/></g>
        </svg>"##;
        let t: Table = lua.load(format!("return {{ source = [==[{doc}]==] }}")).eval().unwrap();
        let root = svg(&lua, t, Path::new("/")).unwrap();
        let both = find(&root, "both").unwrap();
        let children: Vec<Table> = both.sequence_values::<Table>().map(Result::unwrap).collect();
        assert_eq!(children.len(), 2, "the content with its clip inside, the mask outside");
        assert!(children[1].get::<bool>("mask").unwrap());
        let inner: Vec<Table> = children[0].sequence_values::<Table>().map(Result::unwrap).collect();
        assert!(inner.iter().any(|c| c.get::<Option<bool>>("mask").unwrap() == Some(true)));
        let mut shapes = Vec::new();
        fn paths(t: &Table, out: &mut Vec<Table>) {
            if t.get::<Option<String>>("__kind").unwrap().as_deref() == Some("path") {
                out.push(t.clone());
            }
            for c in t.sequence_values::<Table>() {
                paths(&c.unwrap(), out);
            }
        }
        paths(&children[0], &mut shapes);
        let clip_shape = shapes.iter().find(|p| p.get::<String>("fill").unwrap() == "#ffffffff").expect("the clip's circle, opaque");
        assert!(clip_shape.get::<Option<f32>>("stroke").unwrap().is_none(), "a clip has no stroke");
        let mut masked = Vec::new();
        paths(&children[1], &mut masked);
        assert_eq!(masked[0].get::<String>("fill").unwrap(), "#ffffff80", "grey luminance as half alpha");
    }
}
