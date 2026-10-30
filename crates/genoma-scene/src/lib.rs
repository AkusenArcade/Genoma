//! The node tree of one surface (PRD §4): layout through `taffy`, text
//! measured with `cosmic-text`, transitions from the timing set, motion
//! driven by data, and the draw list the renderer gets.
//!
//! The scene never decides to draw. It records that something changed
//! (`take_dirty`) and says, after each step, whether something is moving and
//! when it next will (`Motion`); the surface asks the compositor for a frame
//! only then.

mod glyphs;
mod icons;
mod images;
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

pub use schema::{CURSORS, PROPS, Prop, PropInfo, Ty, luau_definitions};
pub use taffy::NodeId;

/// What a node draws. A box only arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Box,
    Capsule,
    Text,
    Icon,
    /// A picture from a file, fitted to its box (`g.image`).
    Image,
    /// A quadratic Bézier with a width, grown along its length.
    Thread,
    /// Particles born at a rate, from `from` toward a heading or to `to`.
    Emitter,
    /// A field the interface writes in WGSL, fed by numbers it binds
    /// (`g.shader`).
    Shader,
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
    Numbers(Vec<f32>),
}

/// The beat Bioma's processor dot keeps: it swells to 1.45 over the first
/// 9% of the interval, settles over the next 15%, and rests for the rest.
const BEAT_RISE: f32 = 0.09;
const BEAT_FALL: f32 = 0.15;
const BEAT_PEAK: f32 = 1.45;

/// The smallest step worth a frame: a quarter of a physical pixel. Motion
/// slower than one step per refresh gets a frame per step, not per refresh,
/// so a slow satellite costs in proportion to how far it actually moves.
/// `GENOMA_STEP` sets another, for the eye test that chooses it (PRD §11.1).
fn step() -> f32 {
    static STEP: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *STEP.get_or_init(|| {
        std::env::var("GENOMA_STEP").ok().and_then(|s| s.parse().ok()).filter(|s: &f32| *s > 0.0).unwrap_or(0.25)
    })
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
    src: Option<String>,
    fit: images::Fit,
    gradient: (Option<[f32; 4]>, Option<[f32; 4]>),
    stroke: Option<f32>,
    hollow: bool,
    blur: bool,
    fill_bottom: [f32; 4],
    arc_rate: f32,
    offset: [f32; 2],
    /// Takes pointer input: it is hit-tested and belongs to the input region.
    interactive: bool,
    opacity: f32,
    attach: u8,
    catch: bool,
    growth: f32,
    reach: f32,
    clip: bool,
    scroll_y: f32,
    dash: [f32; 2],
    italic: bool,
    wrap: bool,
    weight: Option<u16>,
    /// Letter spacing, em.
    tracking: f32,
    crisp: bool,
    /// A thread's start, control point and end, logical units in its box.
    path: [f32; 6],
    node_size: f32,
    bud: f32,
    /// A key no other node in the process has: emitters are kept on the
    /// shared GPU by it.
    uid: u64,
    emit: Emit,
    /// Depth: rotations in degrees, z toward the viewer, the eye's distance
    /// and the pivot, as given.
    turn: [f32; 3],
    perspective: f32,
    pivot: [f32; 2],
    /// Where the node was last drawn, logical units: x, y, width, height.
    bounds: [f32; 4],
    /// Half the node's width as last laid out: how far the arc's end is from
    /// the centre, to pace its frames.
    arc_radius: f32,
    wave: f32,
    wave_period: f32,
    wave_time: f32,
    level_left: bool,
    fill_across: bool,
    raised: bool,
    /// The pointer's shape over it, an index into `schema::CURSORS`; 0 is
    /// whatever the node around it asks for.
    cursor: u8,
    /// An image's decode size, logical units, when not its box's.
    decode: [Option<f32>; 2],
    /// An image's blur, done once as it is decoded, logical units.
    soften: f32,
    wave_phase: f32,
    arc: f32,
    sway: f32,
    sway_range: f32,
    sway_phase: f32,
    /// A shader node's WGSL and its key; the numbers bound to it, the ones
    /// drawn (easing toward them over `follow` seconds), and its clock.
    shader: Option<(Arc<String>, u64)>,
    values: Vec<f32>,
    shown: Vec<f32>,
    follow: f32,
    clock: bool,
    time: f32,
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
            src: None,
            fit: images::Fit::Cover,
            gradient: (None, None),
            stroke: None,
            hollow: false,
            blur: false,
            fill_bottom: [0.0; 4],
            arc_rate: 0.0,
            offset: [0.0; 2],
            interactive: false,
            opacity: 1.0,
            attach: 0,
            catch: false,
            growth: 1.0,
            reach: 1.0,
            clip: false,
            scroll_y: 0.0,
            dash: [0.0; 2],
            italic: false,
            wrap: false,
            weight: None,
            tracking: 0.0,
            crisp: false,
            path: [0.0; 6],
            node_size: 0.0,
            bud: 0.0,
            uid: NEXT_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            emit: Emit::default(),
            turn: [0.0; 3],
            perspective: 800.0,
            pivot: [0.5, 0.5],
            bounds: [0.0; 4],
            arc_radius: 0.0,
            wave: 0.0,
            wave_period: 1.0,
            wave_time: 0.0,
            level_left: false,
            fill_across: false,
            raised: false,
            cursor: 0,
            decode: [None, None],
            soften: 0.0,
            wave_phase: 0.0,
            arc: 1.0,
            sway: 0.0,
            sway_range: 0.0,
            sway_phase: 0.0,
            shader: None,
            values: Vec::new(),
            shown: Vec::new(),
            follow: 0.0,
            clock: false,
            time: 0.0,
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
    /// A spring's position and velocity, per component of the value.
    spring: Option<(Vec<f32>, Vec<f32>)>,
}

fn components(v: &Value) -> Vec<f32> {
    match v {
        Value::Number(n) => vec![*n],
        Value::Color(c) => c.to_vec(),
        _ => Vec::new(),
    }
}

fn from_components(like: &Value, x: &[f32]) -> Value {
    match like {
        Value::Number(_) => Value::Number(x[0]),
        Value::Color(_) => Value::Color([x[0].clamp(0.0, 1.0), x[1].clamp(0.0, 1.0), x[2].clamp(0.0, 1.0), x[3].clamp(0.0, 1.0)]),
        other => other.clone(),
    }
}

static NEXT_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// An emitter's settings and its ring of births.
#[derive(Clone, Debug)]
struct Emit {
    rate: f32,
    lifetime: f32,
    speed: f32,
    spread: f32,
    heading: f32,
    gravity: f32,
    size: f32,
    target: bool,
    /// A control point is set: carried particles follow the curve.
    bend: bool,
    /// When each slot's particle was born, seconds since the scene's epoch;
    /// negative is an empty slot.
    births: Vec<f32>,
    cursor: usize,
    /// Particles owed by the rate and not yet born.
    owed: f32,
    generation: u64,
}

impl Default for Emit {
    fn default() -> Self {
        Emit {
            rate: 0.0,
            lifetime: 1.0,
            speed: 40.0,
            spread: 30.0,
            heading: 0.0,
            gravity: 0.0,
            size: 3.0,
            target: false,
            bend: false,
            births: Vec::new(),
            cursor: 0,
            owed: 0.0,
            generation: 0,
        }
    }
}

impl Emit {
    /// Births owed since the last step, spread across it; whether any
    /// particle is still alive at `now`.
    fn step(&mut self, now: f32, dt: f32) -> bool {
        let wanted = ((self.rate * self.lifetime).ceil() as usize + 1).max(1);
        if self.births.len() < wanted && self.rate > 0.0 {
            self.births.resize(wanted, -1.0);
        }
        if self.rate > 0.0 && !self.births.is_empty() {
            self.owed += self.rate * dt;
            let mut born = 0;
            while self.owed >= 1.0 {
                self.owed -= 1.0;
                let at = now - self.owed / self.rate;
                let n = self.births.len();
                self.births[self.cursor % n] = at;
                self.cursor = (self.cursor + 1) % n;
                born += 1;
            }
            if born > 0 {
                self.generation += 1;
            }
        } else {
            self.owed = 0.0;
        }
        self.births.iter().any(|&b| b >= 0.0 && now - b < self.lifetime)
    }
}

/// One node as assistive technology sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Accessible {
    pub id: u64,
    pub role: Role,
    pub name: Option<String>,
    /// x, y, width, height, logical units, as last drawn.
    pub bounds: [f32; 4],
    pub children: Vec<u64>,
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
    images: images::Images,
    glyphs: glyphs::Glyphs,
    /// Particles' clock: their births and their age are seconds since this.
    epoch: Instant,
    now: f32,
    /// The blurred shapes as last drawn.
    blur: Vec<Blur>,
    /// Images whose pixels have been drawn since their `src` was set, and
    /// those that got there in the last frame, for `on_ready`.
    ready: std::collections::HashSet<NodeId>,
    newly_ready: Vec<NodeId>,
}

/// A shape the compositor blurs behind, logical units.
#[derive(Clone, Debug, PartialEq)]
pub enum Blur {
    /// x, y, width, height, radius.
    Rounded([f32; 5]),
    /// A shape under depth, as its projected outline: a convex polygon,
    /// since a flat convex shape stays convex through a perspective.
    Outline(Vec<[f32; 2]>),
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
            images: images::Images::default(),
            glyphs: glyphs::Glyphs::default(),
            epoch: Instant::now(),
            now: 0.0,
            blur: Vec::new(),
            ready: Default::default(),
            newly_ready: Vec::new(),
        }
    }

    /// How soon to look again for an image still being decoded, if one is.
    pub fn images_pending(&self) -> Option<std::time::Duration> {
        self.images.pending().then_some(images::BEAT)
    }

    /// The images drawn for the first time since their `src` was set, once:
    /// what `on_ready` is told.
    pub fn take_ready(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.newly_ready)
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Where icon names are looked up, first match wins.
    pub fn set_icon_dirs(&mut self, dirs: Vec<std::path::PathBuf>) {
        self.icons.dirs = dirs;
        self.dirty = true;
    }

    /// What assistive technology sees: every node with a role or a text, by
    /// a stable id, with its name, its bounds as last drawn (logical units)
    /// and its nearest such descendants. The first entry is the root.
    pub fn accessible(&self) -> Vec<Accessible> {
        let mut out = vec![Accessible {
            id: u64::from(self.root),
            role: Role::Group,
            name: None,
            bounds: [0.0; 4],
            children: Vec::new(),
        }];
        let mut children = Vec::new();
        self.accessible_under(self.root, &mut out, &mut children);
        out[0].children = children;
        out
    }

    fn accessible_under(&self, node: NodeId, out: &mut Vec<Accessible>, found: &mut Vec<u64>) {
        for child in self.tree.children(node).unwrap_or_default() {
            let Some(n) = self.tree.get_node_context(child) else { continue };
            if !n.present || n.opacity <= 0.0 {
                continue;
            }
            let speaks = n.role != Role::None || n.kind == Kind::Text;
            if speaks {
                let index = out.len();
                let name = n.name.clone().or_else(|| (n.kind == Kind::Text && !n.text.is_empty()).then(|| n.text.clone()));
                out.push(Accessible {
                    id: u64::from(child),
                    role: if n.role == Role::None { Role::Label } else { n.role },
                    name,
                    bounds: n.bounds,
                    children: Vec::new(),
                });
                found.push(u64::from(child));
                let mut inner = Vec::new();
                self.accessible_under(child, out, &mut inner);
                out[index].children = inner;
            } else {
                self.accessible_under(child, out, found);
            }
        }
    }

    /// The node's parent, for an event that bubbles up.
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.tree.parent(node)
    }

    /// Marks a node as taking pointer input.
    pub fn set_interactive(&mut self, node: NodeId, interactive: bool) {
        if let Some(n) = self.tree.get_node_context_mut(node) {
            n.interactive = interactive;
            self.dirty = true;
        }
    }

    /// The node that is catching the pointer and the keyboard, if any.
    pub fn catcher(&self) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.root, &mut |id, n| {
            if n.catch && found.is_none() {
                found = Some(id);
            }
        });
        found
    }

    /// Whether a transition is in flight: an expansion still closing keeps
    /// its surface until it has.
    pub fn transitioning(&self) -> bool {
        !self.animations.is_empty()
    }

    /// The pointer's shape over `node`, by its name in `schema::CURSORS`:
    /// its own, or the nearest it sits in that names one.
    pub fn cursor(&self, mut node: NodeId) -> &'static str {
        loop {
            match self.tree.get_node_context(node) {
                Some(n) if n.cursor != 0 => return schema::CURSORS[n.cursor as usize],
                _ => {}
            }
            match self.tree.parent(node) {
                Some(p) => node = p,
                None => return schema::CURSORS[0],
            }
        }
    }

    /// Whether `node` is `ancestor` or inside it.
    pub fn within(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.tree.parent(node) {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// The topmost interactive node under a point, logical units, as last
    /// drawn: the last one in drawing order wins, as it is drawn on top.
    /// What is raised is drawn over everything else, so it is hit first.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let (mut found, mut raised) = (None, None);
        self.walk_layered(self.root, false, &mut |id, n, up| {
            let [bx, by, bw, bh] = n.bounds;
            if n.interactive && x >= bx && y >= by && x < bx + bw && y < by + bh {
                if up { raised = Some(id) } else { found = Some(id) }
            }
        });
        raised.or(found)
    }

    fn walk_layered(&self, node: NodeId, up: bool, f: &mut dyn FnMut(NodeId, &Node, bool)) {
        let Some(n) = self.tree.get_node_context(node) else { return };
        if !n.present {
            return;
        }
        let up = up || n.raised;
        f(node, n, up);
        for child in self.tree.children(node).unwrap_or_default() {
            self.walk_layered(child, up, f);
        }
    }

    /// A point in the surface, logical units, from `node`'s top-left corner
    /// as last drawn.
    pub fn local(&self, node: NodeId, x: f32, y: f32) -> (f32, f32) {
        self.tree.get_node_context(node).map_or((0.0, 0.0), |n| (x - n.bounds[0], y - n.bounds[1]))
    }

    /// The rectangles that take the pointer, logical units: the input region.
    pub fn input_rects(&self) -> Vec<[f32; 4]> {
        let mut rects = Vec::new();
        self.walk(self.root, &mut |_, n| {
            if n.interactive {
                rects.push(n.bounds);
            }
        });
        rects
    }

    fn walk(&self, node: NodeId, f: &mut dyn FnMut(NodeId, &Node)) {
        let Some(n) = self.tree.get_node_context(node) else { return };
        if !n.present {
            return;
        }
        f(node, n);
        for child in self.tree.children(node).unwrap_or_default() {
            self.walk(child, f);
        }
    }

    /// The shapes the compositor should blur behind, as last drawn.
    pub fn blur_shapes(&self) -> &[Blur] {
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
            Kind::Text | Kind::Icon | Kind::Image | Kind::Thread | Kind::Emitter | Kind::Shader => Style { flex_shrink: 0.0, ..Style::default() },
        };
        self.tree.new_leaf_with_context(style, Node::new(kind, role)).expect("node")
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.tree.add_child(parent, child).expect("append");
        self.dirty = true;
    }

    /// `node` and everything under it.
    pub fn subtree(&self, node: NodeId) -> Vec<NodeId> {
        let mut nodes = vec![node];
        let mut i = 0;
        while i < nodes.len() {
            nodes.extend(self.tree.children(nodes[i]).unwrap_or_default());
            i += 1;
        }
        nodes
    }

    /// Puts `parent`'s children in this order: a keyed list's rows after it
    /// changed.
    pub fn set_children(&mut self, parent: NodeId, children: &[NodeId]) {
        if self.tree.children(parent).is_ok_and(|now| now != children) {
            self.tree.set_children(parent, children).expect("set children");
            self.dirty = true;
        }
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
        let animate = !first && prop.info().animatable && timing.is_some_and(|t| t.moves());
        if animate {
            let from = self.current(node, prop);
            if from.as_ref() == Some(&value) {
                // Already there, and staying: whatever was on its way
                // elsewhere is called off. Left running, a value sent back
                // to where it stood before it had moved went on to the
                // target it no longer had (an opacity 1, 0, 1 in one moment
                // stayed at 0).
                self.animations.retain(|a| !(a.node == node && a.prop == prop));
                return Ok(());
            }
            if let (Some(from), Some(timing)) = (from, timing) {
                // A spring already moving keeps its velocity: redirected, it
                // bends toward the new target instead of starting again.
                let carried = self
                    .animations
                    .iter()
                    .find(|a| a.node == node && a.prop == prop)
                    .and_then(|a| a.spring.as_ref().map(|(_, v)| v.clone()));
                self.animations.retain(|a| !(a.node == node && a.prop == prop));
                let spring = timing.spring.map(|_| {
                    let x = components(&from);
                    let v = carried.filter(|v| v.len() == x.len()).unwrap_or_else(|| vec![0.0; x.len()]);
                    (x, v)
                });
                self.animations.push(Animation { node, prop, from, to: value, start: Instant::now(), timing, spring });
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
            Prop::Opacity => Value::Number(n.opacity),
            Prop::Growth => Value::Number(n.growth),
            Prop::Reach => Value::Number(n.reach),
            Prop::ScrollY => Value::Number(n.scroll_y),
            Prop::FromX => Value::Number(n.path[0]),
            Prop::FromY => Value::Number(n.path[1]),
            Prop::BendX => Value::Number(n.path[2]),
            Prop::BendY => Value::Number(n.path[3]),
            Prop::ToX => Value::Number(n.path[4]),
            Prop::ToY => Value::Number(n.path[5]),
            Prop::Bud => Value::Number(n.bud),
            Prop::RotateX => Value::Number(n.turn[0]),
            Prop::RotateY => Value::Number(n.turn[1]),
            Prop::Depth => Value::Number(n.turn[2]),
            Prop::OffsetX => Value::Number(n.offset[0]),
            Prop::OffsetY => Value::Number(n.offset[1]),
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
        let numbers = |v: &Value| if let Value::Numbers(x) = v { Some(x.clone()) } else { None };
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
            Prop::Opacity => n.opacity = take!(number).clamp(0.0, 1.0),
            Prop::Attach => {
                n.attach = match take!(text).as_str() {
                    "below" => 1,
                    "above" => 2,
                    "below-center" => 3,
                    "above-center" => 4,
                    "left" => 5,
                    "right" => 6,
                    "corner" => 7,
                    _ => 0,
                }
            }
            Prop::Catch => n.catch = take!(flag),
            Prop::Growth => n.growth = take!(number).clamp(0.0, 1.0),
            Prop::Reach => n.reach = take!(number).clamp(0.0, 1.0),
            Prop::Clip => n.clip = take!(flag),
            Prop::ScrollY => n.scroll_y = take!(number),
            Prop::Dash => n.dash[0] = take!(number).max(0.0),
            Prop::DashGap => n.dash[1] = take!(number).max(0.0),
            Prop::Italic => (n.italic, reshape) = (take!(flag), true),
            Prop::Wrap => (n.wrap, reshape) = (take!(flag), true),
            Prop::Weight => (n.weight, reshape) = (Some(take!(number).clamp(100.0, 900.0) as u16), true),
            Prop::Tracking => (n.tracking, reshape) = (take!(number), true),
            Prop::Crisp => n.crisp = take!(flag),
            Prop::FromX => n.path[0] = take!(number),
            Prop::FromY => n.path[1] = take!(number),
            Prop::BendX => (n.path[2], n.emit.bend) = (take!(number), true),
            Prop::BendY => (n.path[3], n.emit.bend) = (take!(number), true),
            Prop::ToX => (n.path[4], n.emit.target) = (take!(number), true),
            Prop::ToY => (n.path[5], n.emit.target) = (take!(number), true),
            Prop::NodeSize => n.node_size = take!(number).max(0.0),
            Prop::Bud => n.bud = take!(number).max(0.0),
            Prop::Rate => (n.emit.rate, mover) = (take!(number).max(0.0), true),
            Prop::Lifetime => n.emit.lifetime = take!(number).max(0.05),
            Prop::Speed => n.emit.speed = take!(number),
            Prop::Spread => n.emit.spread = take!(number),
            Prop::Heading => n.emit.heading = take!(number),
            Prop::Gravity => n.emit.gravity = take!(number),
            Prop::ParticleSize => n.emit.size = take!(number).max(0.5),
            Prop::RotateX => n.turn[0] = take!(number),
            Prop::RotateY => n.turn[1] = take!(number),
            Prop::Depth => n.turn[2] = take!(number),
            Prop::Perspective => n.perspective = take!(number).max(1.0),
            Prop::PivotX => n.pivot[0] = take!(number),
            Prop::PivotY => n.pivot[1] = take!(number),
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
            Prop::Src if n.kind == Kind::Shader => {
                let path = take!(text);
                // Read when set: a reload sets it again, so an edited file is
                // seen with the interface that uses it.
                n.shader = match std::fs::read_to_string(&path) {
                    Ok(source) => {
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&source, &mut h);
                        Some((Arc::new(source), std::hash::Hasher::finish(&h)))
                    }
                    Err(e) => {
                        log::warn!("g.shader {path}: {e}");
                        None
                    }
                };
            }
            Prop::Src => n.src = Some(take!(text)),
            Prop::Values => {
                n.values = take!(numbers);
                n.values.truncate(128);
                if n.follow <= 0.0 || n.shown.len() != n.values.len() {
                    n.shown = n.values.clone();
                }
                mover = true;
            }
            Prop::Follow => n.follow = take!(number).max(0.0),
            Prop::Clock => (n.clock, mover) = (take!(flag), true),
            Prop::Fit => {
                n.fit = match take!(text).as_str() {
                    "contain" => images::Fit::Contain,
                    "fill" => images::Fit::Fill,
                    _ => images::Fit::Cover,
                }
            }
            Prop::GradientTop => n.gradient.0 = Some(take!(colour)),
            Prop::GradientBottom => n.gradient.1 = Some(take!(colour)),
            Prop::Stroke => n.stroke = Some(take!(number)),
            Prop::Hollow => n.hollow = take!(flag),
            Prop::Blur => n.blur = take!(flag),
            Prop::Wave => n.wave = take!(number),
            Prop::WavePeriod => n.wave_period = take!(number).max(1.0),
            Prop::WaveTime => (n.wave_time, mover) = (take!(number).max(0.0), true),
            Prop::LevelLeft => n.level_left = take!(flag),
            Prop::FillAcross => n.fill_across = take!(flag),
            Prop::Raised => n.raised = take!(flag),
            Prop::Cursor => {
                let name = take!(text);
                n.cursor = schema::CURSORS.iter().position(|c| *c == name).unwrap_or(0) as u8;
            }
            Prop::DecodeWidth => n.decode[0] = Some(take!(number)).filter(|v| *v > 0.0),
            Prop::DecodeHeight => n.decode[1] = Some(take!(number)).filter(|v| *v > 0.0),
            Prop::Soften => n.soften = take!(number).max(0.0),
            Prop::Arc => n.arc = take!(number),
            Prop::Angle => n.angle = take!(number).rem_euclid(1.0),
            Prop::Sway => (n.sway, mover) = (take!(number).max(0.0), true),
            Prop::SwayRange => n.sway_range = take!(number),
            Prop::Overlay => {
                style.position = if take!(flag) { taffy::Position::Absolute } else { taffy::Position::Relative };
                restyle = true;
            }
        }
        let moves = n.pulse > 0.0 || n.orbit != 0.0 || n.wave_time > 0.0 || n.sway > 0.0 || n.arc_rate > 0.0 || n.kind == Kind::Emitter
            || n.kind == Kind::Shader;
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
        // A new picture is not ready until it is drawn.
        if prop == Prop::Src {
            self.ready.remove(&node);
        }
        self.dirty = true;
        Ok(())
    }

    /// Steps transitions, beats and orbits to `now`, and says what the clock
    /// owes the scene next. `scale` turns logical motion into physical pixels.
    pub fn advance(&mut self, now: Instant, scale: f32) -> Motion {
        let dt = self.last_step.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        self.last_step = Some(now);
        self.now = now.saturating_duration_since(self.epoch).as_secs_f32();
        let clock = self.now;

        let animations = std::mem::take(&mut self.animations);
        let mut running = Vec::new();
        for mut a in animations {
            // A spring integrates; it is done when it is within a quarter
            // physical pixel of its target and nearly still.
            if let (Some(spring), Some((x, v))) = (a.timing.spring, a.spring.as_mut()) {
                let waiting = now.saturating_duration_since(a.start).as_secs_f64() < a.timing.delay;
                if !waiting {
                    let target = components(&a.to);
                    // After a quiet spell the last step can be seconds old; a
                    // spring moves at most a frame's worth at a time.
                    let sdt = dt.min(1.0 / 30.0);
                    let steps = ((sdt / (1.0 / 480.0)).ceil() as usize).clamp(1, 64);
                    let h = sdt / steps as f32;
                    for _ in 0..steps {
                        for i in 0..x.len() {
                            let force = -(spring.stiffness as f32) * (x[i] - target[i]) - (spring.damping as f32) * v[i];
                            v[i] += force / spring.mass as f32 * h;
                            x[i] += v[i] * h;
                        }
                    }
                    let near = match a.to {
                        Value::Color(_) => 0.002,
                        _ => step() / scale.max(0.01),
                    };
                    let settled = x.iter().zip(&target).all(|(p, t)| (p - t).abs() < near)
                        && v.iter().all(|s| s.abs() < near * 4.0);
                    let value = if settled { a.to.clone() } else { from_components(&a.to, x) };
                    let _ = self.apply(a.node, a.prop, value);
                    if !settled {
                        running.push(a);
                    }
                } else {
                    running.push(a);
                }
                continue;
            }
            // Before its delay a transition holds where it started.
            let elapsed = now.saturating_duration_since(a.start).as_secs_f64() - a.timing.delay;
            let t = (elapsed / a.timing.duration).clamp(0.0, 1.0);
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
        // An image that finished decoding is news; one still decoding is
        // looked for again on a short beat.
        if self.images.collect() {
            self.dirty = true;
            motion.moving = true;
        }
        if self.images.pending() {
            motion.wake = Some(images::BEAT);
        }

        for &node in &self.movers {
            let Some(n) = self.tree.get_node_context_mut(node) else { continue };
            if !n.present {
                continue;
            }
            if n.kind == Kind::Shader {
                // The drawn numbers follow the bound ones, at the display's
                // rate while one is still on its way and not at all after:
                // data arriving at sixty a second breathes at a hundred and
                // twenty instead of stepping. The clock runs while asked to,
                // and while the numbers move.
                let mut moving = false;
                if n.follow > 0.0 {
                    let k = 1.0 - (-dt.min(0.05) / n.follow).exp();
                    for (shown, wanted) in n.shown.iter_mut().zip(&n.values) {
                        let gap = wanted - *shown;
                        if gap.abs() > 0.002 {
                            *shown += gap * k;
                            moving = true;
                        } else {
                            *shown = *wanted;
                        }
                    }
                }
                if n.clock || moving {
                    n.time += dt.min(0.05);
                    moving = true;
                }
                if moving {
                    motion.moving = true;
                    self.dirty = true;
                }
                continue;
            }
            if n.orbit != 0.0 {
                // The phase is integrated, so a change of speed bends the
                // orbit instead of making the satellite jump.
                n.angle = (n.angle + n.orbit * dt).rem_euclid(1.0);
                let speed = n.orbit.abs() * std::f32::consts::TAU * n.orbit_radius * scale;
                let wait = Duration::from_secs_f32(step() / speed.max(1e-3));
                motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
            }
            // A travelling wave and a sway: each paced by its own speed, a
            // frame a quarter pixel of movement.
            let mut pace = |speed: f32| {
                let wait = Duration::from_secs_f32(step() / speed.max(1e-3));
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
            if n.kind == Kind::Emitter {
                // Alive while it emits, and after, until its last particle's
                // life is over: then nothing runs.
                if n.emit.step(clock, dt.min(0.25)) {
                    let speed = if n.emit.target {
                        let (dx, dy) = (n.path[4] - n.path[0], n.path[5] - n.path[1]);
                        (dx * dx + dy * dy).sqrt() / n.emit.lifetime * 1.5
                    } else {
                        n.emit.speed.abs() + n.emit.gravity.abs() * n.emit.lifetime
                    };
                    pace(speed * scale);
                }
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
        let mut out = Out { list: &mut list, transform: Rect::IDENTITY, now: self.now, parent_shape: None, shape_here: None, clip: Rect::NO_CLIP, icons: &mut self.icons, images: &mut self.images, glyphs: &mut self.glyphs, blur: Vec::new(), upper: DrawList::default(), raised: false, drawn_images: Vec::new() };
        collect(&mut self.tree, &mut fonts, self.root, [0.0; 4], 1.0, scale, &mut out);
        self.blur = out.blur;
        for id in std::mem::take(&mut out.drawn_images) {
            if self.ready.insert(id) {
                log::debug!("image {id:?} drawn for the first time");
                self.newly_ready.push(id);
            }
        }
        let upper = out.upper;
        let has_upper = !(upper.rects.is_empty() && upper.images.is_empty() && upper.shaders.is_empty()
            && upper.emitters.is_empty() && upper.texts.is_empty());
        if has_upper {
            list.base = Some(list.mark());
            list.rects.extend(upper.rects);
            list.images.extend(upper.images);
            list.shaders.extend(upper.shaders);
            list.emitters.extend(upper.emitters);
            list.texts.extend(upper.texts);
        }
        self.images.sweep();
        list
    }
}

/// How a text is set: what both drawing it and measuring it need.
pub struct TextStyle<'a> {
    pub font: Option<&'a str>,
    pub italic: bool,
    pub tabular: bool,
    pub weight: Option<u16>,
    /// em.
    pub tracking: f32,
}

impl TextStyle<'_> {
    fn attrs(&self) -> ct::Attrs<'_> {
        let mut attrs = ct::Attrs::new();
        if let Some(font) = self.font {
            attrs = attrs.family(ct::Family::Name(font));
        }
        if self.italic {
            attrs = attrs.style(ct::Style::Italic);
        }
        if let Some(w) = self.weight {
            attrs = attrs.weight(ct::Weight(w));
        }
        if self.tracking != 0.0 {
            attrs = attrs.letter_spacing(self.tracking);
        }
        if self.tabular {
            let mut features = ct::FontFeatures::new();
            features.enable(ct::FeatureTag::new(b"tnum"));
            attrs = attrs.font_features(features);
        }
        attrs
    }
}

/// How wide `text` is set in `style` at `size`, logical units: what a layout
/// that hands out room by number (Bioma's tissues) needs before drawing.
pub fn measure_text(fonts: &mut ct::FontSystem, text: &str, size: f32, style: &TextStyle<'_>) -> f32 {
    let mut buffer = ct::Buffer::new(fonts, ct::Metrics::new(size, (size * 1.2).ceil()));
    buffer.set_wrap(ct::Wrap::None);
    buffer.set_text(text, &style.attrs(), ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

fn shape(fonts: &mut ct::FontSystem, n: &mut Node) -> Arc<ct::Buffer> {
    if let Some(buffer) = &n.buffer {
        return buffer.clone();
    }
    let metrics = ct::Metrics::new(n.font_size, (n.font_size * 1.2).ceil());
    let mut buffer = ct::Buffer::new(fonts, metrics);
    let style = TextStyle {
        font: n.font.as_deref(),
        italic: n.italic,
        tabular: n.tabular,
        weight: n.weight,
        tracking: n.tracking,
    };
    let attrs = style.attrs();
    // A limited width ends the line in an ellipsis: never wrapped, never
    // shrunk (Bioma's window title). A sentence that may wrap goes on to
    // the next line instead.
    if n.wrap && n.max_width.is_some() {
        buffer.set_wrap(ct::Wrap::WordOrGlyph);
        buffer.set_ellipsize(ct::Ellipsize::None);
    } else {
        buffer.set_wrap(ct::Wrap::None);
        buffer.set_ellipsize(match n.max_width {
            Some(_) => ct::Ellipsize::End(ct::EllipsizeHeightLimit::Lines(1)),
            None => ct::Ellipsize::None,
        });
    }
    buffer.set_size(n.max_width, None);
    buffer.set_text(&n.text, &attrs, ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let buffer = Arc::new(buffer);
    n.buffer = Some(buffer.clone());
    buffer
}

struct Out<'a> {
    list: &'a mut DrawList,
    /// The depth in force, physical pixels to projected (by rows).
    transform: [[f32; 4]; 4],
    /// The particles' clock, as the last step left it.
    now: f32,
    /// The nearest capsule above, as drawn: x, y, width, height, radius,
    /// logical units. A bud joins it.
    parent_shape: Option<[f32; 5]>,
    /// The capsule just drawn, which becomes its children's parent shape.
    shape_here: Option<[f32; 5]>,
    /// The clip in force, physical pixels: x0, y0, x1, y1.
    clip: [f32; 4],
    icons: &'a mut icons::Icons,
    images: &'a mut images::Images,
    glyphs: &'a mut glyphs::Glyphs,
    blur: Vec<Blur>,
    /// What is raised, gathered apart and put after the rest at the end.
    upper: DrawList,
    raised: bool,
    /// The images drawn this frame.
    drawn_images: Vec<NodeId>,
}

fn mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut m = [[0.0; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            m[i][j] = (0..4).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

/// A turn about (px, py): rotations about x then y in degrees, z toward the
/// viewer, then the perspective of an eye `eye` pixels away, all by rows.
fn turn_about(px: f32, py: f32, turn: [f32; 3], eye: f32) -> [[f32; 4]; 4] {
    let (rx, ry) = (turn[0].to_radians(), turn[1].to_radians());
    let to = [[1.0, 0.0, 0.0, px], [0.0, 1.0, 0.0, py], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let back = [[1.0, 0.0, 0.0, -px], [0.0, 1.0, 0.0, -py], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    // Perspective: w = 1 - z / eye, so nearer (positive z) is larger.
    let persp = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, -1.0 / eye, 1.0]];
    let x = [[1.0, 0.0, 0.0, 0.0], [0.0, rx.cos(), -rx.sin(), 0.0], [0.0, rx.sin(), rx.cos(), 0.0], [0.0, 0.0, 0.0, 1.0]];
    let y = [[ry.cos(), 0.0, ry.sin(), 0.0], [0.0, 1.0, 0.0, 0.0], [-ry.sin(), 0.0, ry.cos(), 0.0], [0.0, 0.0, 0.0, 1.0]];
    let z = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, turn[2]], [0.0, 0.0, 0.0, 1.0]];
    mul(to, mul(persp, mul(z, mul(x, mul(y, back)))))
}

/// A rounded rectangle's outline, physical pixels, through a depth matrix,
/// in logical units. Each corner is a few segments: the region it becomes is
/// cut in whole logical rows, which hides the facets. None when part of it
/// is at or behind the eye.
fn projected_outline(m: [[f32; 4]; 4], [x, y, w, h]: [f32; 4], radius: f32, scale: f32) -> Option<Vec<[f32; 2]>> {
    const STEPS: usize = 6;
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let corners = [
        ([x + w - r, y + r], -90.0f32),
        ([x + w - r, y + h - r], 0.0),
        ([x + r, y + h - r], 90.0),
        ([x + r, y + r], 180.0),
    ];
    let mut outline = Vec::with_capacity(4 * (STEPS + 1));
    for ([cx, cy], start) in corners {
        for i in 0..=STEPS {
            let a = (start + 90.0 * i as f32 / STEPS as f32).to_radians();
            let p = [cx + r * a.cos(), cy + r * a.sin(), 0.0, 1.0];
            let q: [f32; 4] = std::array::from_fn(|k| (0..4).map(|j| m[k][j] * p[j]).sum());
            if q[3] <= 1e-3 {
                return None;
            }
            outline.push([q[0] / q[3] / scale, q[1] / q[3] / scale]);
        }
    }
    Some(outline)
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

/// A raised node and everything in it go to the raised layer, drawn after
/// the whole base layer; a raised node inside a raised one is already there.
fn collect(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    let raise = !out.raised && tree.get_node_context(node).is_some_and(|n| n.raised && n.present);
    if raise {
        std::mem::swap(out.list, &mut out.upper);
        out.raised = true;
    }
    collect_node(tree, fonts, node, parent, alpha, scale, out);
    if raise {
        std::mem::swap(out.list, &mut out.upper);
        out.raised = false;
    }
}

fn collect_node(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    let layout = *tree.layout(node).expect("laid out");
    // An overlay is centred in its parent, whatever taffy reports for an
    // out-of-flow box.
    let overlay = tree.style(node).is_ok_and(|s| s.position == taffy::Position::Absolute);
    let attach = tree.get_node_context(node).map_or(0, |n| n.attach);
    let mut origin = if overlay {
        match attach {
            1 => [parent[0], parent[1] + parent[3]],
            2 => [parent[0], parent[1] - layout.size.height],
            3 => [parent[0] + (parent[2] - layout.size.width) / 2.0, parent[1] + parent[3]],
            4 => [parent[0] + (parent[2] - layout.size.width) / 2.0, parent[1] - layout.size.height],
            5 => [parent[0] - layout.size.width, parent[1] + (parent[3] - layout.size.height) / 2.0],
            6 => [parent[0] + parent[2], parent[1] + (parent[3] - layout.size.height) / 2.0],
            7 => [parent[0], parent[1]],
            _ => [parent[0] + (parent[2] - layout.size.width) / 2.0, parent[1] + (parent[3] - layout.size.height) / 2.0],
        }
    } else {
        [parent[0] + layout.location.x, parent[1] + layout.location.y]
    };
    // An offset moves the node and everything in it: a panel set below its
    // thread carries its content with it.
    if let Some(n) = tree.get_node_context(node) {
        origin[0] += n.offset[0];
        origin[1] += n.offset[1];
    }
    let own = [origin[0], origin[1], layout.size.width, layout.size.height];
    // Depth: this node's turn about its pivot, through its perspective,
    // composed with what its ancestors already turned.
    let saved_transform = out.transform;
    if let Some(n) = tree.get_node_context(node) {
        if n.turn != [0.0; 3] {
            let px = (own[0] + own[2] * n.pivot[0]) * scale;
            let py = (own[1] + own[3] * n.pivot[1]) * scale;
            out.transform = mul(out.transform, turn_about(px, py, n.turn, n.perspective * scale));
        }
    }
    let declared_width = tree
        .style(node)
        .ok()
        .and_then(|st| (st.size.width.tag() == taffy::CompactLength::LENGTH_TAG).then(|| st.size.width.value()));
    // What can be hit is what can be seen: the bounds inside the clip.
    let c = [out.clip[0] / scale, out.clip[1] / scale, out.clip[2] / scale, out.clip[3] / scale];
    let x0 = own[0].max(c[0]);
    let y0 = own[1].max(c[1]);
    let x1 = (own[0] + own[2]).min(c[2]);
    let y1 = (own[1] + own[3]).min(c[3]);
    if let Some(n) = tree.get_node_context_mut(node) {
        n.arc_radius = layout.size.width / 2.0;
        n.bounds = [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)];
    }
    let Some(n) = tree.get_node_context_mut(node) else { return };
    if !n.present {
        return;
    }
    // Group opacity: everything below multiplies by it, and a subtree at
    // zero draws nothing.
    let alpha = alpha * n.opacity;
    if alpha <= 0.0 {
        return;
    }
    let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
    // Motion moves and swells what a node draws, never its place in the
    // layout: nothing around a beating dot shifts.
    if n.orbit_radius > 0.0 {
        let a = n.angle * std::f32::consts::TAU;
        origin[0] += a.sin() * n.orbit_radius;
        origin[1] -= a.cos() * n.orbit_radius;
    }
    if n.sway > 0.0 {
        origin[0] += n.sway_range * (0.5 - 0.5 * (n.sway_phase * std::f32::consts::TAU).cos());
    }
    match n.kind {
        Kind::Capsule => {
            let swell = n.swell();
            let mut w = layout.size.width * swell;
            let mut h = layout.size.height * swell;
            let mut x = origin[0] - (w - layout.size.width) / 2.0;
            let mut y = origin[1] - (h - layout.size.height) / 2.0;
            // Growth: the shape's rectangle on its way from a point (the
            // middle of the parent's edge it leaves from) to its own, the
            // edge nearest that point held. Its content is never scaled: it
            // enters once the shape is grown.
            if n.growth < 1.0 {
                let g = n.growth;
                let (full_w, full_h) = (w, h);
                w *= g;
                h *= g;
                match n.attach {
                    // Beside the parent: across from its side's middle.
                    5 | 6 => {
                        let node_y = parent[1] + parent[3] / 2.0;
                        y = node_y + (y - node_y) * g;
                        if n.attach == 5 {
                            x += full_w - w;
                        }
                    }
                    // Over the parent: up from its top's middle.
                    2 | 4 => {
                        let node_x = parent[0] + parent[2] / 2.0;
                        x = node_x + (x - node_x) * g;
                        y += full_h - h;
                    }
                    _ => {
                        let node_x = parent[0] + parent[2] / 2.0;
                        x = node_x + (x - node_x) * g;
                    }
                }
            }
            // Reach: a thread drawn from its top down to this fraction.
            if n.reach < 1.0 {
                h *= n.reach;
            }
            // A hairline sits on whole physical pixels, at least one wide, or
            // antialiasing spreads it over two half-lit ones.
            // The declared width, not taffy's: taffy rounds edges to whole
            // units, which turns 1.3 at a fractional place into 2.
            if n.crisp {
                let declared = declared_width.unwrap_or(w);
                let centre = x + w / 2.0;
                w = (declared * scale).round().max(1.0) / scale;
                x = ((centre - w / 2.0) * scale).round() / scale;
            }
            let size = [w * scale, h * scale];
            let radius = n.radius.map_or(size[1] / 2.0, |r| r * scale).min(size[0].min(size[1]) / 2.0);
            let rim = rim_width(n, scale);
            if n.blur {
                if out.transform == Rect::IDENTITY {
                    out.blur.push(Blur::Rounded([x, y, w, h, radius / scale]));
                } else if let Some(outline) = projected_outline(out.transform, [x * scale, y * scale, size[0], size[1]], radius, scale) {
                    out.blur.push(Blur::Outline(outline));
                }
            }
            let hollow = n.hollow;
            // A bud is drawn as one shape with its parent capsule, without
            // the parent's interior: the parent's centre relative to this
            // one, its half size and radius, and the union's softness.
            let (bud, bud_shape) = match (n.bud > 0.0, out.parent_shape) {
                (true, Some([px, py, pw, ph, pr])) => (
                    [
                        ((px + pw / 2.0) - (x + w / 2.0)) * scale,
                        ((py + ph / 2.0) - (y + h / 2.0)) * scale,
                        pw / 2.0 * scale,
                        ph / 2.0 * scale,
                    ],
                    [pr * scale, n.bud * scale, 0.0, 0.0],
                ),
                _ => ([0.0; 4], [0.0; 4]),
            };
            out.shape_here = Some([x, y, w, h, radius / scale]);
            // A shape grown to nothing draws nothing, bridge included: a bud
            // still inside its cell would otherwise swell the cell's edge.
            let nothing = w <= 0.01 || h <= 0.01;
            // A raised shape first erases what the base layer drew under it:
            // its glass then shows the compositor's blur of what is behind
            // the surface, not the shell's own shapes through it.
            if !nothing && n.raised {
                out.list.rects.push(Rect {
                    origin: [x * scale, y * scale],
                    size,
                    radius,
                    rim: 0.0,
                    level: 1.0,
                    cut: 1.0,
                    fill: [0.0, 0.0, 0.0, 1.0],
                    rim_top: [0.0; 4],
                    rim_bottom: [0.0; 4],
                    wave: [0.0; 4],
                    arc: Rect::WHOLE,
                    fill_to: [0.0; 4],
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud,
                    bud_shape,
                    transform: out.transform,
                });
            }
            if !nothing {
                out.list.rects.push(Rect {
                    origin: [x * scale, y * scale],
                    size,
                    radius,
                    rim,
                    level: n.level,
                    cut: 0.0,
                    fill: fade(n.fill),
                    rim_top: fade(n.rim_top),
                    rim_bottom: fade(n.rim_bottom),
                    wave: [
                        n.wave * scale,
                        n.wave_period * scale,
                        n.wave_phase,
                        (if n.level_left { 1.0 } else { 0.0 }) + if n.fill_across { 2.0 } else { 0.0 },
                    ],
                    arc: [0.0, n.arc, n.dash[0] * scale, n.dash[1] * scale],
                    fill_to: if n.fill_bottom[3] > 0.0 { fade(n.fill_bottom) } else { [0.0; 4] },
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud,
                    bud_shape,
                    transform: out.transform,
                });
            }
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
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        transform: out.transform,
                    });
                }
            }
        }
        Kind::Thread => {
            // The box the curve lives in, grown by the stroke and the nodes
            // so neither is cut by the quad.
            let pad = n.node_size.max(n.stroke.unwrap_or(1.3)) + 2.0;
            let bx = origin[0] - pad;
            let by = origin[1] - pad;
            let bw = layout.size.width + pad * 2.0;
            let bh = layout.size.height + pad * 2.0;
            let centre = [(bx + bw / 2.0) * scale, (by + bh / 2.0) * scale];
            let at = |x: f32, y: f32| [(origin[0] + x) * scale - centre[0], (origin[1] + y) * scale - centre[1]];
            let [fx, fy, cx, cy, tx, ty] = n.path;
            let (a, b, c) = (at(fx, fy), at(cx, cy), at(tx, ty));
            let stroke = n.stroke.unwrap_or(1.3);
            let width = if n.crisp { (stroke * scale).round().max(1.0) } else { stroke * scale };
            let g = n.growth.clamp(0.0, 1.0);
            out.list.rects.push(Rect {
                origin: [bx * scale, by * scale],
                size: [bw * scale, bh * scale],
                radius: 0.0,
                rim: 0.0,
                level: 1.0,
                cut: 2.0,
                fill: fade(n.fill),
                rim_top: [0.0; 4],
                rim_bottom: [0.0; 4],
                wave: [0.0; 4],
                arc: Rect::WHOLE,
                fill_to: [0.0; 4],
                clip: out.clip,
                path: [a[0], a[1], b[0], b[1]],
                path_end: [c[0], c[1], width, g],
                bud: [0.0; 4],
                bud_shape: [0.0; 4],
                transform: out.transform,
            });
            // The nodes: at the start, and at the tip as it grows.
            if n.node_size > 0.0 && g > 0.0 {
                let point = |t: f32| {
                    let u = 1.0 - t;
                    [u * u * fx + 2.0 * u * t * cx + t * t * tx, u * u * fy + 2.0 * u * t * cy + t * t * ty]
                };
                let d = n.node_size * scale;
                for p in [point(0.0), point(g)] {
                    out.list.rects.push(Rect {
                        origin: [(origin[0] + p[0]) * scale - d / 2.0, (origin[1] + p[1]) * scale - d / 2.0],
                        size: [d, d],
                        radius: d / 2.0,
                        rim: 0.0,
                        level: 1.0,
                        cut: 0.0,
                        fill: fade(n.color),
                        rim_top: [0.0; 4],
                        rim_bottom: [0.0; 4],
                        wave: [0.0; 4],
                        arc: Rect::WHOLE,
                        fill_to: [0.0; 4],
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        transform: out.transform,
                    });
                }
            }
        }
        Kind::Emitter => {
            let e = &n.emit;
            if !e.births.is_empty() {
                let to = if e.target { [n.path[4], n.path[5]] } else { [0.0, 0.0] };
                out.list.emitters.push(genoma_render::Emitter {
                    key: n.uid,
                    generation: e.generation,
                    births: Arc::new(e.births.clone()),
                    params: genoma_render::EmitterParams {
                        points: [
                            (origin[0] + n.path[0]) * scale,
                            (origin[1] + n.path[1]) * scale,
                            (origin[0] + to[0]) * scale,
                            (origin[1] + to[1]) * scale,
                        ],
                        motion: [out.now, e.lifetime, e.speed * scale, e.spread.to_radians()],
                        shape: [e.heading.to_radians(), e.gravity * scale, e.size * scale, if e.target { 1.0 } else { 0.0 }],
                        color: fade(n.fill),
                        clip: out.clip,
                        viewport: [0.0; 4],
                        bend: if e.bend {
                            [(origin[0] + n.path[2]) * scale, (origin[1] + n.path[3]) * scale, 1.0, 0.0]
                        } else {
                            [0.0; 4]
                        },
                    },
                });
            }
        }
        Kind::Shader => {
            if let Some((source, key)) = n.shader.clone() {
                out.list.shaders.push(genoma_render::Shader {
                    source,
                    key,
                    origin: [origin[0] * scale, origin[1] * scale],
                    size: [layout.size.width, layout.size.height],
                    scale,
                    time: n.time,
                    alpha,
                    clip: out.clip,
                    values: n.shown.clone(),
                });
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
                        size: [pixels as f32, pixels as f32],
                        uv: [0.0, 0.0, 1.0, 1.0],
                        radius: 0.0,
                        alpha,
                        clip: out.clip,
                        transform: out.transform,
                    });
                }
            }
        }
        Kind::Image => {
            let w = (layout.size.width * scale).round() as u32;
            let h = (layout.size.height * scale).round() as u32;
            // Decoded at its own size when it has one: a box whose width
            // moves (a carousel's step) shows the middle of one decode
            // rather than asking for a new one every frame.
            let dw = n.decode[0].map_or(w, |d| (d * scale).round() as u32).max(1);
            let dh = n.decode[1].map_or(h, |d| (d * scale).round() as u32).max(1);
            if let (Some(source), true) = (n.src.clone(), w > 0 && h > 0) {
                let radius = n.radius.unwrap_or(0.0) * scale;
                // The blur's radius in the decoded picture's own pixels.
                let soften = n.soften * scale * dw as f32 / w.max(1) as f32;
                if let Some((key, data)) = out.images.get(&source, dw, dh, n.fit, 0.0, soften) {
                    out.drawn_images.push(node);
                    let part = |shown: u32, decoded: u32| {
                        if shown >= decoded {
                            (0.0, 1.0)
                        } else {
                            let m = (decoded - shown) as f32 / 2.0 / decoded as f32;
                            (m, 1.0 - m)
                        }
                    };
                    let (u0, u1) = part(w, dw);
                    let (v0, v1) = part(h, dh);
                    out.list.images.push(genoma_render::Image {
                        key,
                        pixels: data,
                        width: dw,
                        height: dh,
                        origin: [(origin[0] * scale).round(), (origin[1] * scale).round()],
                        size: [w as f32, h as f32],
                        uv: [u0, v0, u1, v1],
                        radius,
                        alpha,
                        clip: out.clip,
                        transform: out.transform,
                    });
                }
            }
        }
        Kind::Text => {
            let buffer = shape(fonts, n);
            let at = [(origin[0] + layout.padding.left) * scale, (origin[1] + layout.padding.top) * scale];
            if out.transform == Rect::IDENTITY {
                out.list.texts.push(Text { buffer, origin: at, scale, color: fade(n.color), clip: out.clip });
            } else if let Some(r) = out.glyphs.raster(fonts, n.uid, &buffer, at, scale, n.color) {
                // Tilted: through the subtree's matrix as an image.
                out.list.images.push(genoma_render::Image {
                    key: r.key,
                    pixels: r.pixels,
                    width: r.width,
                    height: r.height,
                    origin: [at[0].floor() + r.offset[0] as f32, at[1].floor() + r.offset[1] as f32],
                    size: [r.width as f32, r.height as f32],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    radius: 0.0,
                    alpha: alpha * n.color[3],
                    clip: out.clip,
                    transform: out.transform,
                });
            }
        }
        Kind::Box => {}
    }
    // A clipping node narrows the clip for everything inside it, and a
    // scrolled one moves its children up.
    let (clip, scroll) = tree.get_node_context(node).map_or((false, 0.0), |n| (n.clip, n.scroll_y));
    let saved = out.clip;
    let saved_parent = out.parent_shape;
    if let Some(shape) = out.shape_here.take() {
        out.parent_shape = Some(shape);
    }
    if clip {
        let c = [own[0] * scale, own[1] * scale, (own[0] + own[2]) * scale, (own[1] + own[3]) * scale];
        out.clip = [saved[0].max(c[0]), saved[1].max(c[1]), saved[2].min(c[2]), saved[3].min(c[3])];
    }
    for child in tree.children(node).unwrap_or_default() {
        collect(tree, fonts, child, [own[0], own[1] - scroll, own[2], own[3]], alpha, scale, out);
    }
    out.clip = saved;
    out.parent_shape = saved_parent;
    out.transform = saved_transform;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spring_scene() -> (Scene, NodeId, Timing) {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let n = scene.create(Kind::Capsule, Role::None);
        scene.append(root, n);
        scene.set(n, Prop::OffsetX, Value::Number(0.0), None).unwrap();
        let timing = Timing {
            duration: 0.0,
            curve: genoma_core::timing::Curve::LINEAR,
            delay: 0.0,
            spring: Some(genoma_core::timing::Spring { stiffness: 300.0, damping: 26.0, mass: 1.0 }),
        };
        (scene, n, timing)
    }

    fn offset(scene: &Scene, n: NodeId) -> f32 {
        scene.tree.get_node_context(n).unwrap().offset[0]
    }

    #[test]
    fn a_spring_settles_and_stops_asking_for_frames() {
        let (mut scene, n, timing) = spring_scene();
        let t0 = Instant::now();
        scene.advance(t0, 1.0);
        scene.set(n, Prop::OffsetX, Value::Number(100.0), Some(timing)).unwrap();
        let mut frames = 0;
        let mut t = t0;
        let mut peak = 0.0f32;
        while scene.transitioning() && frames < 600 {
            t += Duration::from_millis(8);
            scene.advance(t, 1.0);
            peak = peak.max(offset(&scene, n));
            frames += 1;
        }
        assert!(!scene.transitioning(), "settled within 600 frames");
        assert_eq!(offset(&scene, n), 100.0, "exactly on the target");
        assert!(peak > 100.0 && peak < 110.0, "a little overshoot at this damping: {peak}");
        assert!(frames < 150, "settles in about a second: {frames} frames");
    }

    #[test]
    fn a_redirected_spring_keeps_its_velocity() {
        let (mut scene, n, timing) = spring_scene();
        let t0 = Instant::now();
        scene.advance(t0, 1.0);
        scene.set(n, Prop::OffsetX, Value::Number(100.0), Some(timing)).unwrap();
        let mut t = t0;
        for _ in 0..10 {
            t += Duration::from_millis(8);
            scene.advance(t, 1.0);
        }
        let before = offset(&scene, n);
        // Redirected back toward 0: still moving forward for a moment.
        scene.set(n, Prop::OffsetX, Value::Number(0.0), Some(timing)).unwrap();
        t += Duration::from_millis(8);
        scene.advance(t, 1.0);
        assert!(offset(&scene, n) > before, "carried on before turning: {} after {before}", offset(&scene, n));
    }

    #[test]
    fn a_crisp_hairline_is_one_whole_pixel() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let parent = scene.create(Kind::Capsule, Role::None);
        scene.append(root, parent);
        for (p, v) in [(Prop::Width, 101.0), (Prop::Height, 40.0), (Prop::PaddingLeft, 13.7)] {
            scene.set(parent, p, Value::Number(v), None).unwrap();
        }
        let line = scene.create(Kind::Capsule, Role::None);
        scene.append(parent, line);
        for (p, v) in [(Prop::Width, 1.3), (Prop::Height, 14.0), (Prop::Radius, 0.0)] {
            scene.set(line, p, Value::Number(v), None).unwrap();
        }
        scene.set(line, Prop::Overlay, Value::Bool(true), None).unwrap();
        scene.set(line, Prop::Attach, Value::Text("below-center".into()), None).unwrap();
        scene.set(line, Prop::Crisp, Value::Bool(true), None).unwrap();
        scene.set(line, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
        // A place with a fraction, where taffy would round the width to 2.
        scene.set(line, Prop::OffsetX, Value::Number(0.35), None).unwrap();
        scene.layout(None, None);
        let list = scene.draw_list(1.0);
        let r = list.rects.last().unwrap();
        assert_eq!(r.size[0], 1.0, "one pixel wide");
        assert_eq!(r.origin[0], r.origin[0].round(), "on a whole pixel: {:?}", r.origin);
    }

    #[test]
    fn a_growing_shape_holds_the_edge_that_faces_its_parent() {
        // Parent 100×40 at the origin; a 60×30 panel half grown on each side.
        for (attach, held) in [("below-center", "top"), ("above-center", "bottom"), ("right", "left"), ("left", "right")] {
            let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
            let mut scene = Scene::new(fonts);
            let root = scene.root();
            let parent = scene.create(Kind::Capsule, Role::None);
            scene.append(root, parent);
            for (p, v) in [(Prop::Width, 100.0), (Prop::Height, 40.0)] {
                scene.set(parent, p, Value::Number(v), None).unwrap();
            }
            let panel = scene.create(Kind::Capsule, Role::None);
            scene.append(parent, panel);
            for (p, v) in [(Prop::Width, 60.0), (Prop::Height, 30.0), (Prop::Growth, 0.5)] {
                scene.set(panel, p, Value::Number(v), None).unwrap();
            }
            scene.set(panel, Prop::Overlay, Value::Bool(true), None).unwrap();
            scene.set(panel, Prop::Attach, Value::Text(attach.into()), None).unwrap();
            scene.set(panel, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
            scene.layout(None, None);
            let list = scene.draw_list(1.0);
            let r = list.rects.last().unwrap();
            let (x0, y0, x1, y1) = (r.origin[0], r.origin[1], r.origin[0] + r.size[0], r.origin[1] + r.size[1]);
            assert_eq!(r.size, [30.0, 15.0], "{attach}: half grown");
            match held {
                // Against the parent's edge, centred across it.
                "top" => assert_eq!((y0, (x0 + x1) / 2.0), (40.0, 50.0), "{attach}"),
                "bottom" => assert_eq!((y1, (x0 + x1) / 2.0), (0.0, 50.0), "{attach}"),
                "left" => assert_eq!((x0, (y0 + y1) / 2.0), (100.0, 20.0), "{attach}"),
                _ => assert_eq!((x1, (y0 + y1) / 2.0), (0.0, 20.0), "{attach}"),
            }
        }
    }

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
