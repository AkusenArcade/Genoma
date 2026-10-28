//! The node tree of one surface (PRD §4): layout through `taffy`, text
//! measured with `cosmic-text`, transitions from the timing set, and the draw
//! list the renderer gets.
//!
//! The scene never decides to draw. It records that something changed
//! (`take_dirty`) and that a transition is in flight (`animating`); the
//! surface asks the compositor for a frame only then.

use std::{sync::Arc, time::Instant};

use genoma_core::timing::{Curve, Timing};
use genoma_render::{DrawList, Fonts, Rect, Text, cosmic_text as ct};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent,
    LengthPercentage, Size, Style, TaffyTree, style_helpers::TaffyMaxContent,
};

pub use taffy::NodeId;

/// What a node draws. A box only arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Box,
    Capsule,
    Text,
}

/// The node's meaning for assistive technology (PRD §12, question 5). Carried
/// from the start; the AT-SPI adapter reads it from M3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    None,
    Group,
    Label,
    Button,
    Slider,
    Timer,
    Status,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f32),
    Color([f32; 4]),
    Text(String),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    Width,
    Height,
    MinWidth,
    Padding,
    PaddingX,
    PaddingY,
    Gap,
    Direction,
    Align,
    Justify,
    Radius,
    Fill,
    RimTop,
    RimBottom,
    Text,
    FontSize,
    Font,
    Tabular,
    Color,
    Present,
}

impl Prop {
    pub fn from_name(name: &str) -> Option<Prop> {
        Some(match name {
            "width" => Prop::Width,
            "height" => Prop::Height,
            "min_width" => Prop::MinWidth,
            "padding" => Prop::Padding,
            "padding_x" => Prop::PaddingX,
            "padding_y" => Prop::PaddingY,
            "gap" => Prop::Gap,
            "direction" => Prop::Direction,
            "align" => Prop::Align,
            "justify" => Prop::Justify,
            "radius" => Prop::Radius,
            "fill" => Prop::Fill,
            "rim_top" => Prop::RimTop,
            "rim_bottom" => Prop::RimBottom,
            "text" => Prop::Text,
            "size" => Prop::FontSize,
            "font" => Prop::Font,
            "tabular" => Prop::Tabular,
            "color" => Prop::Color,
            "present" => Prop::Present,
            _ => return None,
        })
    }

    fn animatable(self) -> bool {
        matches!(
            self,
            Prop::Width
                | Prop::Height
                | Prop::Radius
                | Prop::Fill
                | Prop::RimTop
                | Prop::RimBottom
                | Prop::Color
        )
    }
}

/// Per-node state, kept as taffy's node context.
pub struct Node {
    kind: Kind,
    pub role: Role,
    pub name: Option<String>,
    present: bool,
    radius: Option<f32>,
    fill: [f32; 4],
    rim_top: [f32; 4],
    rim_bottom: [f32; 4],
    color: [f32; 4],
    text: String,
    font_size: f32,
    font: Option<String>,
    tabular: bool,
    buffer: Option<Arc<ct::Buffer>>,
    /// Props set at least once: the first value is placed, never animated.
    seen: Vec<Prop>,
}

impl Node {
    fn new(kind: Kind, role: Role) -> Node {
        Node {
            kind,
            role,
            name: None,
            present: true,
            radius: None,
            fill: [0.0; 4],
            rim_top: [0.0; 4],
            rim_bottom: [0.0; 4],
            color: [1.0; 4],
            text: String::new(),
            font_size: 14.0,
            font: None,
            tabular: false,
            buffer: None,
            seen: Vec::new(),
        }
    }
}

struct Animation {
    node: NodeId,
    prop: Prop,
    from: Value,
    to: Value,
    start: Instant,
    timing: Timing,
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub struct Scene {
    tree: TaffyTree<Node>,
    root: NodeId,
    fonts: Fonts,
    animations: Vec<Animation>,
    dirty: bool,
}

impl Scene {
    pub fn new(fonts: Fonts) -> Scene {
        let mut tree = TaffyTree::new();
        let root = tree
            .new_leaf_with_context(Style::default(), Node::new(Kind::Box, Role::Group))
            .expect("root");
        Scene { tree, root, fonts, animations: Vec::new(), dirty: true }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn create(&mut self, kind: Kind, role: Role) -> NodeId {
        let style = match kind {
            // Children side by side and centred, as a capsule holds them.
            Kind::Capsule => Style {
                align_items: AlignItems::CENTER,
                justify_content: JustifyContent::CENTER,
                ..Style::default()
            },
            _ => Style::default(),
        };
        self.tree.new_leaf_with_context(style, Node::new(kind, role)).expect("node")
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.tree.add_child(parent, child).expect("append");
        self.dirty = true;
    }

    /// Removes `node` and everything under it.
    pub fn remove(&mut self, node: NodeId) {
        for child in self.tree.children(node).unwrap_or_default() {
            self.remove(child);
        }
        self.animations.retain(|a| a.node != node);
        let _ = self.tree.remove(node);
        self.dirty = true;
    }

    /// Empties the root: an interface about to be mounted again.
    pub fn clear(&mut self) {
        for child in self.tree.children(self.root).unwrap_or_default() {
            self.remove(child);
        }
    }

    pub fn set_name(&mut self, node: NodeId, name: Option<String>) {
        if let Some(n) = self.tree.get_node_context_mut(node) {
            n.name = name;
        }
    }

    /// Sets a property. With a timing, an animatable property moves to the
    /// new value over that timing instead of jumping, except the first time
    /// it is set.
    pub fn set(&mut self, node: NodeId, prop: Prop, value: Value, timing: Option<Timing>) -> Result<(), Error> {
        let Some(n) = self.tree.get_node_context_mut(node) else {
            return Err(Error("node is gone".into()));
        };
        let first = !n.seen.contains(&prop);
        if first {
            n.seen.push(prop);
        }
        let animate = !first && prop.animatable() && timing.is_some_and(|t| t.duration > 0.0);
        if animate {
            let from = self.current(node, prop);
            if from.as_ref() == Some(&value) {
                return Ok(());
            }
            if let (Some(from), Some(timing)) = (from, timing) {
                self.animations.retain(|a| !(a.node == node && a.prop == prop));
                self.animations.push(Animation { node, prop, from, to: value, start: Instant::now(), timing });
                self.dirty = true;
                return Ok(());
            }
        }
        self.animations.retain(|a| !(a.node == node && a.prop == prop));
        self.apply(node, prop, value)
    }

    fn current(&self, node: NodeId, prop: Prop) -> Option<Value> {
        let n = self.tree.get_node_context(node)?;
        let style = self.tree.style(node).ok()?;
        let length = |d: Dimension| (d.tag() == taffy::CompactLength::LENGTH_TAG).then(|| d.value());
        Some(match prop {
            Prop::Width => Value::Number(length(style.size.width)?),
            Prop::Height => Value::Number(length(style.size.height)?),
            Prop::Radius => Value::Number(n.radius?),
            Prop::Fill => Value::Color(n.fill),
            Prop::RimTop => Value::Color(n.rim_top),
            Prop::RimBottom => Value::Color(n.rim_bottom),
            Prop::Color => Value::Color(n.color),
            _ => return None,
        })
    }

    fn apply(&mut self, node: NodeId, prop: Prop, value: Value) -> Result<(), Error> {
        let bad = |what: &str| Err(Error(format!("{prop:?} wants {what}, got {value:?}")));
        let mut style = self.tree.style(node).map_err(|e| Error(e.to_string()))?.clone();
        let mut restyle = true;
        let n = self.tree.get_node_context_mut(node).expect("checked by set");
        let mut reshape = false;
        match (prop, &value) {
            (Prop::Width, Value::Number(v)) => style.size.width = Dimension::length(*v),
            (Prop::Height, Value::Number(v)) => style.size.height = Dimension::length(*v),
            (Prop::MinWidth, Value::Number(v)) => style.min_size.width = taffy::LengthPercentageAuto::length(*v),
            (Prop::Padding, Value::Number(v)) => {
                let l = LengthPercentage::length(*v);
                style.padding = taffy::Rect { left: l, right: l, top: l, bottom: l };
            }
            (Prop::PaddingX, Value::Number(v)) => {
                style.padding.left = LengthPercentage::length(*v);
                style.padding.right = LengthPercentage::length(*v);
            }
            (Prop::PaddingY, Value::Number(v)) => {
                style.padding.top = LengthPercentage::length(*v);
                style.padding.bottom = LengthPercentage::length(*v);
            }
            (Prop::Gap, Value::Number(v)) => style.gap = Size::length(*v),
            (Prop::Direction, Value::Text(v)) => {
                style.flex_direction = match v.as_str() {
                    "row" => FlexDirection::Row,
                    "column" => FlexDirection::Column,
                    _ => return bad("\"row\" or \"column\""),
                }
            }
            (Prop::Align, Value::Text(v)) => {
                style.align_items = match v.as_str() {
                    "start" => AlignItems::FLEX_START,
                    "center" => AlignItems::CENTER,
                    "end" => AlignItems::FLEX_END,
                    "stretch" => AlignItems::STRETCH,
                    _ => return bad("start, center, end or stretch"),
                }
            }
            (Prop::Justify, Value::Text(v)) => {
                style.justify_content = match v.as_str() {
                    "start" => JustifyContent::FLEX_START,
                    "center" => JustifyContent::CENTER,
                    "end" => JustifyContent::FLEX_END,
                    "between" => JustifyContent::SPACE_BETWEEN,
                    _ => return bad("start, center, end or between"),
                }
            }
            (Prop::Present, Value::Bool(v)) => {
                n.present = *v;
                style.display = if *v { Display::Flex } else { Display::None };
            }
            (Prop::Radius, Value::Number(v)) => {
                n.radius = Some(*v);
                restyle = false;
            }
            (Prop::Fill, Value::Color(c)) => {
                n.fill = *c;
                restyle = false;
            }
            (Prop::RimTop, Value::Color(c)) => {
                n.rim_top = *c;
                restyle = false;
            }
            (Prop::RimBottom, Value::Color(c)) => {
                n.rim_bottom = *c;
                restyle = false;
            }
            (Prop::Color, Value::Color(c)) => {
                n.color = *c;
                restyle = false;
            }
            (Prop::Text, Value::Text(t)) => {
                if n.text == *t {
                    return Ok(());
                }
                n.text = t.clone();
                reshape = true;
            }
            (Prop::FontSize, Value::Number(v)) => {
                n.font_size = *v;
                reshape = true;
            }
            (Prop::Font, Value::Text(f)) => {
                n.font = Some(f.clone());
                reshape = true;
            }
            (Prop::Tabular, Value::Bool(v)) => {
                n.tabular = *v;
                reshape = true;
            }
            (Prop::Width | Prop::Height | Prop::MinWidth | Prop::Padding | Prop::PaddingX
                | Prop::PaddingY | Prop::Gap | Prop::Radius | Prop::FontSize, _) => return bad("a number"),
            (Prop::Fill | Prop::RimTop | Prop::RimBottom | Prop::Color, _) => return bad("a colour"),
            (Prop::Present | Prop::Tabular, _) => return bad("a boolean"),
            (Prop::Direction | Prop::Align | Prop::Justify | Prop::Text | Prop::Font, _) => {
                return bad("a string");
            }
        }
        if reshape {
            n.buffer = None;
            self.tree.mark_dirty(node).ok();
        }
        if restyle {
            self.tree.set_style(node, style).ok();
        }
        self.dirty = true;
        Ok(())
    }

    /// Steps every transition to `now`. Returns whether any is still moving,
    /// which is the only reason to ask for another frame.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.animations.is_empty() {
            return false;
        }
        let animations = std::mem::take(&mut self.animations);
        let mut running = Vec::new();
        for a in animations {
            let t = (now.duration_since(a.start).as_secs_f64() / a.timing.duration).clamp(0.0, 1.0);
            let y = a.timing.curve.eval(t) as f32;
            let value = match (&a.from, &a.to) {
                (Value::Number(f), Value::Number(to)) => Value::Number(f + (to - f) * y),
                (Value::Color(f), Value::Color(to)) => {
                    let mut c = [0.0; 4];
                    for i in 0..4 {
                        c[i] = (f[i] + (to[i] - f[i]) * y).clamp(0.0, 1.0);
                    }
                    Value::Color(c)
                }
                _ => a.to.clone(),
            };
            let _ = self.apply(a.node, a.prop, value);
            if t < 1.0 {
                running.push(a);
            }
        }
        self.animations = running;
        !self.animations.is_empty()
    }

    pub fn animating(&self) -> bool {
        !self.animations.is_empty()
    }

    /// Whether anything changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Lays the tree out at its own size and returns it, in logical units.
    pub fn layout(&mut self) -> (f32, f32) {
        let mut fonts = self.fonts.lock().unwrap();
        let measure = |inputs: taffy::LayoutInput, _: NodeId, node: Option<&mut Node>, style: &Style| {
            taffy::compute_leaf_layout(inputs, style, |_, _| 0.0, |known, _available| match node {
                Some(n) if n.kind == Kind::Text => {
                    let buffer = shape(&mut fonts, n);
                    let (mut w, mut h) = (0.0f32, 0.0f32);
                    for run in buffer.layout_runs() {
                        w = w.max(run.line_w);
                        h += run.line_height;
                    }
                    Size { width: known.width.unwrap_or(w.ceil()), height: known.height.unwrap_or(h.ceil()) }
                }
                _ => Size::ZERO,
            })
        };
        self.tree
            .compute_layout_with_measure(self.root, Size::MAX_CONTENT, measure)
            .expect("layout");
        let size = self.tree.layout(self.root).expect("root layout").size;
        (size.width, size.height)
    }

    /// What the surface shows, at `scale` physical pixels per logical unit.
    pub fn draw_list(&mut self, scale: f32) -> DrawList {
        let mut list = DrawList::default();
        let mut fonts = self.fonts.lock().unwrap();
        collect(&mut self.tree, &mut fonts, self.root, [0.0, 0.0], scale, &mut list);
        list
    }
}

fn shape(fonts: &mut ct::FontSystem, n: &mut Node) -> Arc<ct::Buffer> {
    if let Some(buffer) = &n.buffer {
        return buffer.clone();
    }
    let metrics = ct::Metrics::new(n.font_size, (n.font_size * 1.2).ceil());
    let mut buffer = ct::Buffer::new(fonts, metrics);
    let mut attrs = ct::Attrs::new();
    if let Some(font) = &n.font {
        attrs = attrs.family(ct::Family::Name(font));
    }
    let mut features = ct::FontFeatures::new();
    if n.tabular {
        features.enable(ct::FeatureTag::new(b"tnum"));
        attrs = attrs.font_features(features);
    }
    buffer.set_size(None, None);
    buffer.set_text(&n.text, &attrs, ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let buffer = Arc::new(buffer);
    n.buffer = Some(buffer.clone());
    buffer
}

fn collect(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 2],
    scale: f32,
    list: &mut DrawList,
) {
    let layout = *tree.layout(node).expect("laid out");
    let origin = [parent[0] + layout.location.x, parent[1] + layout.location.y];
    let Some(n) = tree.get_node_context_mut(node) else { return };
    if !n.present {
        return;
    }
    match n.kind {
        Kind::Capsule => {
            let size = [layout.size.width * scale, layout.size.height * scale];
            let radius = n.radius.map_or(size[1] / 2.0, |r| r * scale).min(size[0].min(size[1]) / 2.0);
            // Bioma's rim: 1 logical unit, rounded to the physical pixel,
            // never under 2 physical pixels.
            let rim = if n.rim_top[3] > 0.0 || n.rim_bottom[3] > 0.0 { scale.round().max(2.0) } else { 0.0 };
            list.rects.push(Rect {
                origin: [origin[0] * scale, origin[1] * scale],
                size,
                radius,
                rim,
                _pad: [0.0; 2],
                fill: n.fill,
                rim_top: n.rim_top,
                rim_bottom: n.rim_bottom,
            });
        }
        Kind::Text => {
            let buffer = shape(fonts, n);
            let content = taffy::Point { x: layout.padding.left, y: layout.padding.top };
            list.texts.push(Text {
                buffer,
                origin: [(origin[0] + content.x) * scale, (origin[1] + content.y) * scale],
                scale,
                color: n.color,
            });
        }
        Kind::Box => {}
    }
    for child in tree.children(node).unwrap_or_default() {
        collect(tree, fonts, child, origin, scale, list);
    }
}

/// A curve for a timing given as four numbers, as Bioma's tokens write them.
pub fn curve(points: [f64; 4]) -> Curve {
    Curve { x1: points[0], y1: points[1], x2: points[2], y2: points[3] }
}

