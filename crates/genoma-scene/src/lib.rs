//! The node tree of one surface (PRD §4): layout through `taffy`, text
//! measured with `cosmic-text`, transitions from the timing set, motion
//! driven by data, and the draw list the renderer gets.
//!
//! The scene never decides to draw. It records that something changed
//! (`take_dirty`) and says, after each step, whether something is moving and
//! when it next will (`Motion`); the surface asks the compositor for a frame
//! only then.

mod icons;
mod schema;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use genoma_core::timing::Timing;
use genoma_render::{DrawList, Fonts, Rect, Text, cosmic_text as ct};
use taffy::{
    AlignItems, AvailableSpace, Dimension, Display, FlexDirection, JustifyContent,
    LengthPercentage, LengthPercentageAuto, Size, Style, TaffyTree,
};

pub use schema::{PROPS, Prop, PropInfo, Ty, luau_definitions};
pub use taffy::NodeId;

/// What a node draws. A box only arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Box,
    Capsule,
    Text,
    Icon,
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

/// The beat Bioma's processor dot keeps: it swells to 1.45 over the first
/// 9% of the interval, settles over the next 15%, and rests for the rest.
const BEAT_RISE: f32 = 0.09;
const BEAT_FALL: f32 = 0.15;
const BEAT_PEAK: f32 = 1.45;

/// The smallest step worth a frame: a quarter of a physical pixel. Motion
/// slower than one step per refresh gets a frame per step, not per refresh,
/// so a slow satellite costs in proportion to how far it actually moves.
const STEP: f32 = 0.25;

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
    level: f32,
    color: [f32; 4],
    text: String,
    font_size: f32,
    font: Option<String>,
    tabular: bool,
    max_width: Option<f32>,
    buffer: Option<Arc<ct::Buffer>>,
    /// Beats a minute, and where in the current beat the node is (0…1).
    pulse: f32,
    beat: f32,
    /// Turns a second, the angle reached (in turns) and the orbit's radius.
    orbit: f32,
    angle: f32,
    orbit_radius: f32,
    icon: Option<String>,
    gradient: (Option<[f32; 4]>, Option<[f32; 4]>),
    stroke: Option<f32>,
    hollow: bool,
    blur: bool,
    fill_bottom: [f32; 4],
    arc_rate: f32,
    offset: [f32; 2],
    /// Half the node's width as last laid out: how far the arc's end is from
    /// the centre, to pace its frames.
    arc_radius: f32,
    wave: f32,
    wave_period: f32,
    wave_time: f32,
    wave_phase: f32,
    arc: f32,
    sway: f32,
    sway_range: f32,
    sway_phase: f32,
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
            level: 1.0,
            color: [1.0; 4],
            text: String::new(),
            font_size: 14.0,
            font: None,
            tabular: false,
            max_width: None,
            buffer: None,
            pulse: 0.0,
            beat: 0.0,
            orbit: 0.0,
            angle: 0.0,
            orbit_radius: 0.0,
            icon: None,
            gradient: (None, None),
            stroke: None,
            hollow: false,
            blur: false,
            fill_bottom: [0.0; 4],
            arc_rate: 0.0,
            offset: [0.0; 2],
            arc_radius: 0.0,
            wave: 0.0,
            wave_period: 1.0,
            wave_time: 0.0,
            wave_phase: 0.0,
            arc: 1.0,
            sway: 0.0,
            sway_range: 0.0,
            sway_phase: 0.0,
            seen: Vec::new(),
        }
    }

    /// The size factor the beat gives the node now.
    fn swell(&self) -> f32 {
        if self.pulse <= 0.0 {
            return 1.0;
        }
        // OutQuad to the peak, InQuad back, as Vital.qml's beat.
        let b = self.beat;
        if b < BEAT_RISE {
            let t = b / BEAT_RISE;
            1.0 + (BEAT_PEAK - 1.0) * (1.0 - (1.0 - t) * (1.0 - t))
        } else if b < BEAT_RISE + BEAT_FALL {
            let t = (b - BEAT_RISE) / BEAT_FALL;
            BEAT_PEAK - (BEAT_PEAK - 1.0) * (t * t)
        } else {
            1.0
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

/// What the scene needs from the clock after a step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    /// Something moves now: draw again on the next frame.
    pub moving: bool,
    /// Nothing moves now, but something will start to after this long (the
    /// next beat). The surface sleeps until then.
    pub wake: Option<Duration>,
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
    /// Nodes with a pulse or an orbit, stepped on every advance.
    movers: Vec<NodeId>,
    last_step: Option<Instant>,
    dirty: bool,
    icons: icons::Icons,
    /// The blurred shapes as last drawn, logical units: x, y, width, height,
    /// radius.
    blur: Vec<[f32; 5]>,
}

impl Scene {
    pub fn new(fonts: Fonts) -> Scene {
        let mut tree = TaffyTree::new();
        let root = tree
            .new_leaf_with_context(Style::default(), Node::new(Kind::Box, Role::Group))
            .expect("root");
        Scene {
            tree,
            root,
            fonts,
            animations: Vec::new(),
            movers: Vec::new(),
            last_step: None,
            dirty: true,
            icons: icons::Icons::default(),
            blur: Vec::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Where icon names are looked up, first match wins.
    pub fn set_icon_dirs(&mut self, dirs: Vec<std::path::PathBuf>) {
        self.icons.dirs = dirs;
        self.dirty = true;
    }

    /// The shapes the compositor should blur behind, as last drawn.
    pub fn blur_shapes(&self) -> &[[f32; 5]] {
        &self.blur
    }

    pub fn create(&mut self, kind: Kind, role: Role) -> NodeId {
        let style = match kind {
            // Children side by side and centred, as a capsule holds them.
            Kind::Capsule => Style {
                align_items: AlignItems::CENTER,
                justify_content: JustifyContent::CENTER,
                flex_shrink: 0.0,
                ..Style::default()
            },
            Kind::Box => Style { align_items: AlignItems::CENTER, flex_shrink: 0.0, ..Style::default() },
            Kind::Text | Kind::Icon => Style { flex_shrink: 0.0, ..Style::default() },
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
        self.movers.retain(|&m| m != node);
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
        let animate = !first && prop.info().animatable && timing.is_some_and(|t| t.duration > 0.0);
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
            Prop::Level => Value::Number(n.level),
            Prop::Arc => Value::Number(n.arc),
            Prop::Fill => Value::Color(n.fill),
            Prop::FillBottom => Value::Color(n.fill_bottom),
            Prop::RimTop => Value::Color(n.rim_top),
            Prop::RimBottom => Value::Color(n.rim_bottom),
            Prop::Color => Value::Color(n.color),
            _ => return None,
        })
    }

    fn apply(&mut self, node: NodeId, prop: Prop, value: Value) -> Result<(), Error> {
        let bad = || Err(Error(format!("{} wants {:?}, got {value:?}", prop.info().name, prop.info().ty)));
        let mut style = self.tree.style(node).map_err(|e| Error(e.to_string()))?.clone();
        let n = self.tree.get_node_context_mut(node).expect("checked by set");
        let mut restyle = false;
        let mut reshape = false;
        let mut mover = false;
        let number = |v: &Value| if let Value::Number(x) = v { Some(*x) } else { None };
        let colour = |v: &Value| if let Value::Color(c) = v { Some(*c) } else { None };
        let text = |v: &Value| if let Value::Text(t) = v { Some(t.clone()) } else { None };
        let flag = |v: &Value| if let Value::Bool(b) = v { Some(*b) } else { None };
        macro_rules! take {
            ($f:expr) => {
                match $f(&value) {
                    Some(v) => v,
                    None => return bad(),
                }
            };
        }
        match prop {
            Prop::Width => (style.size.width, restyle) = (Dimension::length(take!(number)), true),
            Prop::Height => (style.size.height, restyle) = (Dimension::length(take!(number)), true),
            Prop::MinWidth => {
                (style.min_size.width, restyle) = (LengthPercentageAuto::length(take!(number)), true)
            }
            Prop::MaxWidth => {
                let v = take!(number);
                style.max_size.width = LengthPercentageAuto::length(v);
                n.max_width = Some(v);
                restyle = true;
                reshape = true;
            }
            Prop::Grow => (style.flex_grow, restyle) = (take!(number), true),
            Prop::Padding => {
                let l = LengthPercentage::length(take!(number));
                style.padding = taffy::Rect { left: l, right: l, top: l, bottom: l };
                restyle = true;
            }
            Prop::PaddingX => {
                let l = LengthPercentage::length(take!(number));
                (style.padding.left, style.padding.right, restyle) = (l, l, true);
            }
            Prop::PaddingY => {
                let l = LengthPercentage::length(take!(number));
                (style.padding.top, style.padding.bottom, restyle) = (l, l, true);
            }
            Prop::Gap => (style.gap, restyle) = (Size::length(take!(number)), true),
            Prop::Direction => {
                style.flex_direction =
                    if take!(text) == "column" { FlexDirection::Column } else { FlexDirection::Row };
                restyle = true;
            }
            Prop::Align => {
                style.align_items = match take!(text).as_str() {
                    "start" => AlignItems::FLEX_START,
                    "end" => AlignItems::FLEX_END,
                    "stretch" => AlignItems::STRETCH,
                    _ => AlignItems::CENTER,
                };
                restyle = true;
            }
            Prop::Justify => {
                style.justify_content = match take!(text).as_str() {
                    "start" => JustifyContent::FLEX_START,
                    "end" => JustifyContent::FLEX_END,
                    "between" => JustifyContent::SPACE_BETWEEN,
                    _ => JustifyContent::CENTER,
                };
                restyle = true;
            }
            Prop::Present => {
                n.present = take!(flag);
                style.display = if n.present { Display::Flex } else { Display::None };
                restyle = true;
            }
            Prop::Radius => n.radius = Some(take!(number)),
            Prop::Level => n.level = take!(number).clamp(0.0, 1.0),
            Prop::Fill => n.fill = take!(colour),
            Prop::FillBottom => n.fill_bottom = take!(colour),
            Prop::ArcRate => (n.arc_rate, mover) = (take!(number).max(0.0), true),
            Prop::OffsetX => n.offset[0] = take!(number),
            Prop::OffsetY => n.offset[1] = take!(number),
            Prop::PaddingLeft => (style.padding.left, restyle) = (LengthPercentage::length(take!(number)), true),
            Prop::PaddingRight => (style.padding.right, restyle) = (LengthPercentage::length(take!(number)), true),
            Prop::RimTop => n.rim_top = take!(colour),
            Prop::RimBottom => n.rim_bottom = take!(colour),
            Prop::Color => n.color = take!(colour),
            Prop::Text => {
                let t = take!(text);
                if n.text == t {
                    return Ok(());
                }
                n.text = t;
                reshape = true;
            }
            Prop::FontSize => (n.font_size, reshape) = (take!(number), true),
            Prop::Font => (n.font, reshape) = (Some(take!(text)), true),
            Prop::Tabular => (n.tabular, reshape) = (take!(flag), true),
            Prop::Pulse => (n.pulse, mover) = (take!(number).max(0.0), true),
            Prop::Orbit => (n.orbit, mover) = (take!(number), true),
            Prop::OrbitRadius => n.orbit_radius = take!(number),
            Prop::Icon => n.icon = Some(take!(text)),
            Prop::GradientTop => n.gradient.0 = Some(take!(colour)),
            Prop::GradientBottom => n.gradient.1 = Some(take!(colour)),
            Prop::Stroke => n.stroke = Some(take!(number)),
            Prop::Hollow => n.hollow = take!(flag),
            Prop::Blur => n.blur = take!(flag),
            Prop::Wave => n.wave = take!(number),
            Prop::WavePeriod => n.wave_period = take!(number).max(1.0),
            Prop::WaveTime => (n.wave_time, mover) = (take!(number).max(0.0), true),
            Prop::Arc => n.arc = take!(number),
            Prop::Angle => n.angle = take!(number).rem_euclid(1.0),
            Prop::Sway => (n.sway, mover) = (take!(number).max(0.0), true),
            Prop::SwayRange => n.sway_range = take!(number),
            Prop::Overlay => {
                style.position = if take!(flag) { taffy::Position::Absolute } else { taffy::Position::Relative };
                restyle = true;
            }
        }
        let moves = n.pulse > 0.0 || n.orbit != 0.0 || n.wave_time > 0.0 || n.sway > 0.0 || n.arc_rate > 0.0;
        if reshape {
            n.buffer = None;
            self.tree.mark_dirty(node).ok();
        }
        if restyle {
            self.tree.set_style(node, style).ok();
        }
        if mover {
            self.movers.retain(|&m| m != node);
            if moves {
                self.movers.push(node);
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Steps transitions, beats and orbits to `now`, and says what the clock
    /// owes the scene next. `scale` turns logical motion into physical pixels.
    pub fn advance(&mut self, now: Instant, scale: f32) -> Motion {
        let dt = self.last_step.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        self.last_step = Some(now);

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
        let mut motion = Motion { moving: !self.animations.is_empty(), wake: None };

        for &node in &self.movers {
            let Some(n) = self.tree.get_node_context_mut(node) else { continue };
            if !n.present {
                continue;
            }
            if n.orbit != 0.0 {
                // The phase is integrated, so a change of speed bends the
                // orbit instead of making the satellite jump.
                n.angle = (n.angle + n.orbit * dt).rem_euclid(1.0);
                let speed = n.orbit.abs() * std::f32::consts::TAU * n.orbit_radius * scale;
                let wait = Duration::from_secs_f32(STEP / speed.max(1e-3));
                motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
            }
            // A travelling wave and a sway: each paced by its own speed, a
            // frame a quarter pixel of movement.
            let mut pace = |speed: f32| {
                let wait = Duration::from_secs_f32(STEP / speed.max(1e-3));
                motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
            };
            if n.arc_rate > 0.0 && n.arc < 1.0 {
                n.arc = (n.arc + n.arc_rate * dt).min(1.0);
                // The arc's end moves along a circle as wide as the node.
                let radius = n.arc_radius;
                pace(n.arc_rate * std::f32::consts::TAU * radius * scale);
            }
            if n.wave_time > 0.0 {
                n.wave_phase = (n.wave_phase + dt / n.wave_time).rem_euclid(1.0);
                pace(n.wave_period / n.wave_time * scale);
            }
            if n.sway > 0.0 {
                n.sway_phase = (n.sway_phase + dt / n.sway).rem_euclid(1.0);
                // The peak speed of a sinusoidal sway.
                pace(std::f32::consts::PI * n.sway_range / n.sway * scale);
            }
            if n.pulse > 0.0 {
                let interval = 60.0 / n.pulse;
                n.beat = (n.beat + dt / interval).rem_euclid(1.0);
                if n.beat < BEAT_RISE + BEAT_FALL {
                    motion.moving = true;
                } else {
                    let wait = Duration::from_secs_f32((1.0 - n.beat) * interval);
                    motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
                }
            }
            self.dirty = true;
        }
        // A wait shorter than a refresh is a frame on every refresh.
        if motion.wake.is_some_and(|w| w < Duration::from_millis(4)) {
            motion.moving = true;
        }
        if motion.moving {
            motion.wake = None;
        }
        motion
    }

    /// Whether anything changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Lays the tree out and returns its size in logical units. A given
    /// width or height is the surface's (a membrane spans its edge); without
    /// one the tree takes the size of its content.
    pub fn layout(&mut self, width: Option<f32>, height: Option<f32>) -> (f32, f32) {
        let size = Size {
            width: width.map_or(Dimension::auto(), Dimension::length),
            height: height.map_or(Dimension::auto(), Dimension::length),
        };
        // Setting a style invalidates the layout cache: only when it changed.
        let mut root = self.tree.style(self.root).expect("root").clone();
        if root.size != size {
            root.size = size;
            self.tree.set_style(self.root, root).ok();
        }
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
        let space = Size {
            width: width.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
            height: height.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
        };
        self.tree.compute_layout_with_measure(self.root, space, measure).expect("layout");
        let size = self.tree.layout(self.root).expect("root layout").size;
        (size.width, size.height)
    }

    /// What the surface shows, at `scale` physical pixels per logical unit.
    pub fn draw_list(&mut self, scale: f32) -> DrawList {
        let mut list = DrawList::default();
        let mut fonts = self.fonts.lock().unwrap();
        let mut out = Out { list: &mut list, icons: &mut self.icons, blur: Vec::new() };
        collect(&mut self.tree, &mut fonts, self.root, [0.0; 4], scale, &mut out);
        self.blur = out.blur;
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
    if n.tabular {
        let mut features = ct::FontFeatures::new();
        features.enable(ct::FeatureTag::new(b"tnum"));
        attrs = attrs.font_features(features);
    }
    // A limited width ends the line in an ellipsis: never wrapped, never
    // shrunk (Bioma's window title).
    buffer.set_wrap(ct::Wrap::None);
    buffer.set_ellipsize(match n.max_width {
        Some(_) => ct::Ellipsize::End(ct::EllipsizeHeightLimit::Lines(1)),
        None => ct::Ellipsize::None,
    });
    buffer.set_size(n.max_width, None);
    buffer.set_text(&n.text, &attrs, ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let buffer = Arc::new(buffer);
    n.buffer = Some(buffer.clone());
    buffer
}

struct Out<'a> {
    list: &'a mut DrawList,
    icons: &'a mut icons::Icons,
    blur: Vec<[f32; 5]>,
}

/// Bioma's rim: 1 logical unit, rounded to the physical pixel, never under 2
/// physical pixels; or a given stroke, crisped to whole pixels.
fn rim_width(n: &Node, scale: f32) -> f32 {
    if n.rim_top[3] <= 0.0 && n.rim_bottom[3] <= 0.0 {
        return 0.0;
    }
    match n.stroke {
        Some(s) => (s * scale).round().max(1.0),
        None => scale.round().max(2.0),
    }
}

fn collect(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    scale: f32,
    out: &mut Out,
) {
    let layout = *tree.layout(node).expect("laid out");
    // An overlay is centred in its parent, whatever taffy reports for an
    // out-of-flow box.
    let overlay = tree.style(node).is_ok_and(|s| s.position == taffy::Position::Absolute);
    let mut origin = if overlay {
        [parent[0] + (parent[2] - layout.size.width) / 2.0, parent[1] + (parent[3] - layout.size.height) / 2.0]
    } else {
        [parent[0] + layout.location.x, parent[1] + layout.location.y]
    };
    let own = [origin[0], origin[1], layout.size.width, layout.size.height];
    if let Some(n) = tree.get_node_context_mut(node) {
        n.arc_radius = layout.size.width / 2.0;
    }
    let Some(n) = tree.get_node_context_mut(node) else { return };
    if !n.present {
        return;
    }
    // Motion moves and swells what a node draws, never its place in the
    // layout: nothing around a beating dot shifts.
    if n.orbit_radius > 0.0 {
        let a = n.angle * std::f32::consts::TAU;
        origin[0] += a.sin() * n.orbit_radius;
        origin[1] -= a.cos() * n.orbit_radius;
    }
    origin[0] += n.offset[0];
    origin[1] += n.offset[1];
    if n.sway > 0.0 {
        origin[0] += n.sway_range * (0.5 - 0.5 * (n.sway_phase * std::f32::consts::TAU).cos());
    }
    match n.kind {
        Kind::Capsule => {
            let swell = n.swell();
            let w = layout.size.width * swell;
            let h = layout.size.height * swell;
            let x = origin[0] - (w - layout.size.width) / 2.0;
            let y = origin[1] - (h - layout.size.height) / 2.0;
            let size = [w * scale, h * scale];
            let radius = n.radius.map_or(size[1] / 2.0, |r| r * scale).min(size[0].min(size[1]) / 2.0);
            let rim = rim_width(n, scale);
            if n.blur {
                out.blur.push([x, y, w, h, radius / scale]);
            }
            let hollow = n.hollow;
            out.list.rects.push(Rect {
                origin: [x * scale, y * scale],
                size,
                radius,
                rim,
                level: n.level,
                cut: 0.0,
                fill: n.fill,
                rim_top: n.rim_top,
                rim_bottom: n.rim_bottom,
                wave: [n.wave * scale, n.wave_period * scale, n.wave_phase, 0.0],
                arc: [0.0, n.arc, 0.0, 0.0],
                fill_to: n.fill_bottom,
            });
            // A band drawn only outside its cells: each child capsule's
            // shape is taken back out of it before the child is drawn.
            if hollow {
                for child in tree.children(node).unwrap_or_default() {
                    let Ok(l) = tree.layout(child) else { continue };
                    let l = *l;
                    let Some(c) = tree.get_node_context(child) else { continue };
                    if c.kind != Kind::Capsule || !c.present {
                        continue;
                    }
                    let cs = [l.size.width * scale, l.size.height * scale];
                    out.list.rects.push(Rect {
                        origin: [(origin[0] + l.location.x) * scale, (origin[1] + l.location.y) * scale],
                        size: cs,
                        radius: c.radius.map_or(cs[1] / 2.0, |r| r * scale).min(cs[0].min(cs[1]) / 2.0),
                        rim: 0.0,
                        level: 1.0,
                        cut: 1.0,
                        fill: [0.0, 0.0, 0.0, 1.0],
                        rim_top: [0.0; 4],
                        rim_bottom: [0.0; 4],
                        wave: [0.0; 4],
                        arc: Rect::WHOLE,
                        fill_to: [0.0; 4],
                    });
                }
            }
        }
        Kind::Icon => {
            let pixels = (layout.size.width.min(layout.size.height) * scale).round() as u32;
            if let (Some(source), true) = (n.icon.clone(), pixels > 0) {
                let gradient = match n.gradient {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                };
                if let Some((key, data)) = out.icons.raster(&source, n.color, gradient, pixels) {
                    out.list.images.push(genoma_render::Image {
                        key,
                        pixels: data,
                        width: pixels,
                        height: pixels,
                        origin: [(origin[0] * scale).round(), (origin[1] * scale).round()],
                    });
                }
            }
        }
        Kind::Text => {
            let buffer = shape(fonts, n);
            out.list.texts.push(Text {
                buffer,
                origin: [(origin[0] + layout.padding.left) * scale, (origin[1] + layout.padding.top) * scale],
                scale,
                color: n.color,
            });
        }
        Kind::Box => {}
    }
    for child in tree.children(node).unwrap_or_default() {
        collect(tree, fonts, child, [own[0], own[1], own[2], own[3]], scale, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overlay_sits_at_the_parents_alignment() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let parent = scene.create(Kind::Box, Role::None);
        scene.append(root, parent);
        for (p, v) in [(Prop::Width, 26.0), (Prop::Height, 26.0)] {
            scene.set(parent, p, Value::Number(v), None).unwrap();
        }
        scene.set(parent, Prop::Justify, Value::Text("center".into()), None).unwrap();
        let child = scene.create(Kind::Capsule, Role::None);
        scene.append(parent, child);
        for (p, v) in [(Prop::Width, 10.0), (Prop::Height, 10.0)] {
            scene.set(child, p, Value::Number(v), None).unwrap();
        }
        scene.set(child, Prop::Overlay, Value::Bool(true), None).unwrap();
        scene.set(child, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
        scene.layout(None, None);
        let l = scene.tree.layout(child).unwrap();
        assert_eq!((l.size.width, l.size.height), (10.0, 10.0));
        assert_eq!((l.location.x, l.location.y), (8.0, 8.0), "centred in 26");
        let list = scene.draw_list(1.0);
        assert_eq!(list.rects.len(), 1);
    }
}
