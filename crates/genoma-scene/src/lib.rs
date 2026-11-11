//! The node tree of one surface (PRD §4): layout through `taffy`, text
//! measured with `cosmic-text`, transitions from the timing set, motion
//! driven by data, and the draw list the renderer gets.
//!
//! The scene never decides to draw. It records that something changed
//! (`take_dirty`) and says, after each step, whether something is moving and
//! when it next will (`Motion`); the surface asks the compositor for a frame
//! only then.

mod distance;
pub mod field;
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
    /// A line of text to type into (`g.field`): caret, selection, undo,
    /// the clipboard and input methods.
    Field,
    /// A shape of any outline (`g.path`): SVG path data or a polygon,
    /// filled, stroked, dashed.
    Path,
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
    Runs(Vec<TextRun>),
    Stops(Vec<(f32, [f32; 4])>),
}

/// A stretch of a text with a style of its own: characters `from` to `to`,
/// counted from 1, both included. What it leaves unset is the text's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextRun {
    pub from: usize,
    pub to: usize,
    pub weight: Option<u16>,
    pub italic: Option<bool>,
    pub font: Option<String>,
    /// Its own size, on the text's lines: the line height stays the text's.
    pub size: Option<f32>,
    pub color: Option<[f32; 4]>,
    pub strike: bool,
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
    /// What the interface calls the node (`id = "clock"`): where a popup
    /// is anchored.
    pub id: Option<String>,
    present: bool,
    radius: Option<f32>,
    fill: [f32; 4],
    rim_top: [f32; 4],
    rim_bottom: [f32; 4],
    level: f32,
    color: [f32; 4],
    color_bottom: [f32; 4],
    shadow: [f32; 4],
    shadow_blur: f32,
    shadow_y: f32,
    /// A capsule's shadow: x offset and spread (logical).
    shadow_x: f32,
    shadow_spread: f32,
    /// A capsule's inner shadow: colour, then blur, x, y, spread.
    inner_shadow: [f32; 4],
    inner: [f32; 4],
    /// A capsule's outline (`outline`, not the path's): colour, then width
    /// and offset.
    ring: [f32; 4],
    ring_size: [f32; 2],
    text: String,
    font_size: f32,
    font: Option<String>,
    tabular: bool,
    /// Characters counted from 1 drawn in `mark_color`, and the fade the
    /// buffer was shaped at: a marked glyph carries its own colour, which
    /// the text's alpha does not reach, so the shape follows the fade.
    marks: Vec<f32>,
    runs: Vec<TextRun>,
    mark_color: [f32; 4],
    marks_alpha: f32,
    /// 0 start, 1 centre, 2 end: where a line sits within `max_width`.
    text_align: u8,
    ellipsize: u8,
    max_width: Option<f32>,
    buffer: Option<Arc<ct::Buffer>>,
    /// A field's editing state; `None` on every other node.
    edit: Option<Box<field::Editor>>,
    placeholder: String,
    placeholder_color: [f32; 4],
    selection_color: [f32; 4],
    caret_color: [f32; 4],
    focused: bool,
    secret: bool,
    /// Where a field's caret was last drawn, logical units in the surface:
    /// where an input method puts its candidates.
    caret_rect: Option<[f32; 4]>,
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
    fill_stops: Vec<(f32, [f32; 4])>,
    arc_rate: f32,
    offset: [f32; 2],
    /// Takes pointer input: it is hit-tested and belongs to the input region.
    interactive: bool,
    /// The frame it was last drawn in with something to see, and where it
    /// was then, unclipped: what the keyboard's focus may reach, and where
    /// a well scrolls to bring it into view.
    drawn_frame: u64,
    /// The frame that last laid it out (its `bounds` are that frame's): a
    /// node inside something absent keeps an old one.
    laid_frame: u64,
    extent: [f32; 4],
    opacity: f32,
    attach: u8,
    catch: bool,
    /// The compositor's shortcuts stand aside while this node is there.
    inhibit_shortcuts: bool,
    /// The session stays awake while this node is seen.
    inhibit_idle: bool,
    /// An animated image: whether it plays, how far into it, and the
    /// seconds its frame has left as last drawn.
    playing: bool,
    play_time: f32,
    frame_left: f32,
    /// A nine-slice's edges in the picture's own pixels: top, right,
    /// bottom, left; `None` is an ordinary image.
    slice: Option<[f32; 4]>,
    /// Drawn as its parent's mask rather than as itself.
    mask: bool,
    /// A shader drawn as its parent's post-processing (`pass`).
    pass: bool,
    /// Drawn into a picture of its own and kept (`cache`).
    cache: bool,
    /// How it meets what is under: `genoma_render::Image::blend`.
    blend: u8,
    /// An image's colours: saturation, contrast, brightness, then a tint
    /// (`genoma_render::Image::filter`).
    filter: [f32; 8],
    growth: f32,
    reach: f32,
    clip: bool,
    fade_bottom: f32,
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
    halo: f32,
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
    /// The fill gradient turned (CSS degrees), and its shape: 0 linear,
    /// 1 radial, 2 conic. `None` with shape 0 is the vertical gradient.
    fill_angle: Option<f32>,
    fill_shape: u8,
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
    /// Why its shader is not drawn: the file, and what WGSL said is wrong.
    shader_error: Option<String>,
    values: Vec<f32>,
    shown: Vec<f32>,
    follow: f32,
    clock: bool,
    time: f32,
    /// What it simulates (`compute`): the WGSL and its key; whether that
    /// file's own `shade` can draw the node, and why not; why the
    /// simulation itself is not run.
    compute: Option<(Arc<String>, u64)>,
    compute_shades: Result<(), String>,
    compute_error: Option<String>,
    /// Its grid (cells across and down; unset, a cell every two logical
    /// units), its agents, how large Genoma draws them (logical units),
    /// steps a second, the steps taken and the part of one owed.
    grid: Option<[u32; 2]>,
    agents: u32,
    agent_size: f32,
    steps_rate: f32,
    steps_taken: u64,
    steps_owed: f32,
    /// Props set at least once: the first value is placed, never animated.
    seen: Vec<Prop>,
    /// A path's outline: SVG path data, or a polygon's corners; how its
    /// corners and ends are drawn, its fill rule, and how far it reaches
    /// right and down (its size when none is given).
    outline: Option<String>,
    points: Vec<f32>,
    join: &'static str,
    cap: &'static str,
    even_odd: bool,
    reach_of_path: [f32; 2],
    /// Drawn as one shape with the capsule of this id: union (0), subtract
    /// (1) or intersect (2), this soft.
    combine_with: Option<String>,
    combine: u8,
    combine_soft: f32,
    /// Drawn blurred, this spread, logical units; 0 is not blurred.
    blur_radius: f32,
    /// What is under it shown through it blurred this much, logical units.
    backdrop_blur: f32,
    /// A shader that reads what is under it.
    reads_backdrop: bool,
    /// The keyboard walks inside it, the focused node ringed in this.
    navigate: bool,
    focus_color: [f32; 4],
    /// In a window, a press on it moves the window (a title bar).
    move_window: bool,
    /// A `g.list`: the keyboard's focus on one of its rows scrolls it.
    rows_list: bool,
    /// It has an `on_drop`: what is dragged over it may be dropped there.
    drop_target: bool,
    /// What dragging it gives another program: paths, one a line, and text.
    drag_files: Option<String>,
    drag_text: Option<String>,
}

impl Node {
    fn new(kind: Kind, role: Role) -> Node {
        Node {
            kind,
            role,
            name: None,
            id: None,
            present: true,
            radius: None,
            fill: [0.0; 4],
            rim_top: [0.0; 4],
            rim_bottom: [0.0; 4],
            level: 1.0,
            color: [1.0; 4],
            color_bottom: [0.0; 4],
            shadow: [0.0; 4],
            shadow_blur: 0.0,
            shadow_y: 0.0,
            shadow_x: 0.0,
            shadow_spread: 0.0,
            inner_shadow: [0.0; 4],
            inner: [0.0; 4],
            ring: [0.0; 4],
            ring_size: [1.0, 0.0],
            text: String::new(),
            font_size: 14.0,
            font: None,
            tabular: false,
            marks: Vec::new(),
            runs: Vec::new(),
            mark_color: [1.0; 4],
            marks_alpha: 1.0,
            text_align: 0,
            ellipsize: 0,
            max_width: None,
            buffer: None,
            edit: (kind == Kind::Field).then(Box::default),
            placeholder: String::new(),
            placeholder_color: [0.0; 4],
            selection_color: [0.0; 4],
            caret_color: [0.0; 4],
            focused: false,
            secret: false,
            caret_rect: None,
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
            fill_stops: Vec::new(),
            arc_rate: 0.0,
            offset: [0.0; 2],
            // A field takes the pointer, to place its caret.
            interactive: kind == Kind::Field,
            drawn_frame: 0,
            laid_frame: 0,
            extent: [0.0; 4],
            opacity: 1.0,
            attach: 0,
            catch: false,
            inhibit_shortcuts: false,
            inhibit_idle: false,
            playing: true,
            play_time: 0.0,
            frame_left: f32::INFINITY,
            slice: None,
            mask: false,
            pass: false,
            cache: false,
            blend: 0,
            filter: genoma_render::Image::NO_FILTER,
            growth: 1.0,
            reach: 1.0,
            clip: false,
            fade_bottom: 0.0,
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
            halo: 0.0,
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
            fill_angle: None,
            fill_shape: 0,
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
            shader_error: None,
            values: Vec::new(),
            shown: Vec::new(),
            follow: 0.0,
            clock: false,
            time: 0.0,
            compute: None,
            compute_shades: Ok(()),
            compute_error: None,
            grid: None,
            agents: 0,
            agent_size: 0.0,
            steps_rate: 60.0,
            steps_taken: 0,
            steps_owed: 0.0,
            seen: Vec::new(),
            outline: None,
            points: Vec::new(),
            join: "miter",
            cap: "butt",
            even_odd: false,
            reach_of_path: [0.0; 2],
            combine_with: None,
            combine: 0,
            combine_soft: 0.0,
            blur_radius: 0.0,
            backdrop_blur: 0.0,
            reads_backdrop: false,
            navigate: false,
            focus_color: [0.478, 0.635, 0.969, 1.0],
            move_window: false,
            rows_list: false,
            drop_target: false,
            drag_files: None,
            drag_text: None,
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

/// Whether moving `prop` from `a` to `b` changes nothing anyone can see: a
/// quarter of a physical pixel for a length, under 1/500 for anything else.
fn unseen(prop: Prop, a: &Value, b: &Value, scale: f32) -> bool {
    let lengths = [
        Prop::Width, Prop::Height, Prop::Radius, Prop::OffsetX, Prop::OffsetY, Prop::ScrollY,
        Prop::FromX, Prop::FromY, Prop::ToX, Prop::ToY, Prop::BendX, Prop::BendY,
    ];
    let near = if lengths.contains(&prop) { step() / scale.max(0.01) } else { 0.002 };
    let (a, b) = (components(a), components(b));
    a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| (x - y).abs() < near)
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
    /// The colour and the diameter at the end of a life, when they change
    /// along it.
    color_end: Option<[f32; 4]>,
    size_end: Option<f32>,
    /// Seconds of its path drawn behind each particle; 0 none.
    trail: f32,
    /// Born where its parent emitter's particles die, `burst` at each,
    /// rather than at its own rate.
    sub: bool,
    burst: f32,
    /// How long a slot is kept after its particle's life: the longest life
    /// of the bursts its sub-emitters make there.
    linger: f32,
    /// The ids of the nodes its particles are pulled toward (`pointer`, the
    /// pointer), how hard (logical units a second squared; negative pushes)
    /// and how far the pull reaches (logical units).
    attract: Option<String>,
    attraction: f32,
    reach: f32,
    /// The ids of the nodes its particles bounce off, and how much of their
    /// speed they keep, 0…1.
    collide: Option<String>,
    bounce: f32,
    /// How fast a particle drawn as a picture (`icon`, `src`) turns at
    /// most, degrees a second.
    spin: f32,
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
            color_end: None,
            size_end: None,
            trail: 0.0,
            sub: false,
            burst: 8.0,
            linger: 0.0,
            attract: None,
            attraction: 200.0,
            reach: 120.0,
            collide: None,
            bounce: 0.5,
            spin: 0.0,
            births: Vec::new(),
            cursor: 0,
            owed: 0.0,
            generation: 0,
        }
    }
}

impl Emit {
    /// Births owed since the last step, spread across it; whether it still
    /// emits or any particle is alive at `now`.
    fn step(&mut self, now: f32, dt: f32) -> bool {
        // A sub-emitter's particles are its parent's to time.
        if self.sub {
            return false;
        }
        // A slot is not reused while what its sub-emitters burst there lives.
        let kept = self.lifetime + self.linger;
        let wanted = ((self.rate * kept).ceil() as usize + 1).max(1);
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
        // Alive while it emits, even before its first particle is owed: a
        // first step too short for one asked for no frame and the emitter
        // never started.
        self.rate > 0.0 || self.births.iter().any(|&b| b >= 0.0 && now - b < kept)
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

/// One node as an inspector shows it (`Scene::inspect`).
#[derive(Clone, Debug)]
pub struct Inspected {
    pub node: u64,
    pub parent: Option<u64>,
    pub depth: usize,
    pub kind: String,
    pub role: String,
    pub name: Option<String>,
    pub id: Option<String>,
    pub text: Option<String>,
    /// Where it was last drawn, logical units.
    pub bounds: [f32; 4],
    /// Shown: present, and every ancestor too.
    pub present: bool,
    /// Name and value of each property that reads back, zero ones left out.
    pub props: Vec<(&'static str, String)>,
}

pub struct Scene {
    tree: TaffyTree<Node>,
    root: NodeId,
    fonts: Fonts,
    animations: Vec<Animation>,
    /// Nodes with a pulse or an orbit, stepped on every advance.
    movers: Vec<NodeId>,
    last_step: Option<Instant>,
    /// The surface's scale at the last frame: what a quarter pixel is.
    scale: f32,
    dirty: bool,
    icons: icons::Icons,
    images: images::Images,
    glyphs: glyphs::Glyphs,
    /// The outlines of texts and paths that capsules are combined with.
    distance: distance::Fields,
    /// Particles' clock: their births and their age are seconds since this.
    epoch: Instant,
    now: f32,
    /// The blurred shapes as last drawn.
    blur: Vec<Blur>,
    /// Images whose pixels have been drawn since their `src` was set, and
    /// those that got there in the last frame, for `on_ready`.
    ready: std::collections::HashSet<NodeId>,
    newly_ready: Vec<NodeId>,
    /// The animated images drawn last frame.
    animated: Vec<NodeId>,
    /// Nodes whose laid-out size is reported, with the size last told
    /// (none before the first layout), and the changes since the last take,
    /// for `on_size`.
    sized: std::collections::HashMap<NodeId, Option<[f32; 2]>>,
    newly_sized: Vec<(NodeId, f32, f32)>,
    /// The node an inspector points at, outlined over everything.
    highlight: Option<NodeId>,
    /// The node the keyboard is on inside a `navigate` node, ringed.
    focus_ring: Option<NodeId>,
    /// Where the pointer is on the surface (logical units), whether a button
    /// is held; none while it is elsewhere. What shaders read as
    /// `shader.pointer`.
    pointer: Option<(f32, f32, bool)>,
    /// Counts the frames drawn, for `drawn_frame`.
    frame: u64,
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
        // Taffy rounds every edge to a whole logical unit, which on a
        // fractional screen is no pixel at all: at 1.5 an odd unit is half a
        // physical pixel. The layout stays exact; `place` lands it on the
        // screen's pixels when it is drawn.
        tree.disable_rounding();
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
            scale: 1.0,
            dirty: true,
            icons: icons::Icons::default(),
            images: images::Images::default(),
            glyphs: glyphs::Glyphs::default(),
            distance: distance::Fields::default(),
            epoch: Instant::now(),
            now: 0.0,
            blur: Vec::new(),
            ready: Default::default(),
            newly_ready: Vec::new(),
            animated: Vec::new(),
            sized: Default::default(),
            newly_sized: Vec::new(),
            highlight: None,
            focus_ring: None,
            pointer: None,
            frame: 0,
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

    /// Reports `node`'s laid-out size from the next layout on, and again
    /// whenever it changes: content whose height only the layout knows (a
    /// page of wrapped text in a scrolling well).
    pub fn watch_size(&mut self, node: NodeId) {
        self.sized.entry(node).or_insert(None);
        self.dirty = true;
    }

    /// The watched nodes whose size changed in the layouts since the last
    /// take, with their new width and height: what `on_size` is told.
    pub fn take_sized(&mut self) -> Vec<(NodeId, f32, f32)> {
        std::mem::take(&mut self.newly_sized)
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// `node`'s padding and border, top and bottom together, as last laid
    /// out: what its own height does not leave its children.
    pub fn inset_y(&self, node: NodeId) -> f32 {
        self.tree.layout(node).map_or(0.0, |l| l.padding.top + l.padding.bottom + l.border.top + l.border.bottom)
    }

    /// The space between `node`'s children, logical units.
    pub fn gap(&self, node: NodeId) -> f32 {
        let Ok(style) = self.tree.style(node) else { return 0.0 };
        let gap = style.gap.height.into_raw();
        if gap.tag() == taffy::CompactLength::LENGTH_TAG { gap.value() } else { 0.0 }
    }

    /// Where icon names are looked up, first match wins.
    pub fn set_icon_dirs(&mut self, dirs: Vec<std::path::PathBuf>) {
        // A name not found in the old places may be in the new ones: what
        // was rasterised, or found missing, is asked again.
        if self.icons.dirs != dirs {
            self.icons.dirs = dirs;
            self.icons.cache.clear();
        }
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

    /// The node's parent, for an event that bubbles up; none for a node
    /// no longer in the tree: the pointer may still name one that a
    /// change of the list it was in took away.
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.tree.get_node_context(node)?;
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

    /// Whether a node asks the compositor's shortcuts to stand aside.
    pub fn inhibits_shortcuts(&self) -> bool {
        let mut found = false;
        self.walk(self.root, &mut |_, n| found |= n.inhibit_shortcuts);
        found
    }

    /// Whether a node asking the session to stay awake is seen now: present,
    /// laid out in the last frame, with a size.
    pub fn inhibits_idle(&self) -> bool {
        let mut found = false;
        self.walk(self.root, &mut |node, n| found |= n.inhibit_idle && self.seen(node));
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
                Some(n) if n.kind == Kind::Field => return "text",
                _ => {}
            }
            match self.parent(node) {
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
            match self.parent(node) {
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

    /// What the keyboard's focus can reach inside `within`, in the order the
    /// tree has them: buttons and sliders that take the pointer and were
    /// seen in the last frame (not faded to nothing, not inside something
    /// absent). A row scrolled out of its well still counts.
    /// `within` itself is left out: it is the cell that opened, whose own
    /// press would close what the keyboard is walking through.
    pub fn focusables(&self, within: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for child in self.tree.children(within).unwrap_or_default() {
            self.collect_focusables(child, &mut out);
        }
        out
    }

    fn collect_focusables(&self, node: NodeId, out: &mut Vec<NodeId>) {
        let Some(n) = self.tree.get_node_context(node) else { return };
        if !n.present {
            return;
        }
        if n.interactive && matches!(n.role, Role::Button | Role::Slider) && n.drawn_frame == self.frame {
            out.push(node);
        }
        for child in self.tree.children(node).unwrap_or_default() {
            self.collect_focusables(child, out);
        }
    }

    /// Where a node was last drawn, unclipped, logical units.
    pub fn extent(&self, node: NodeId) -> Option<[f32; 4]> {
        self.tree.get_node_context(node).map(|n| n.extent)
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
            Kind::Text | Kind::Icon | Kind::Image | Kind::Thread | Kind::Emitter | Kind::Shader | Kind::Field | Kind::Path => {
                Style { flex_shrink: 0.0, ..Style::default() }
            }
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
        self.sized.remove(&node);
        self.newly_sized.retain(|&(n, ..)| n != node);
        let _ = self.tree.remove(node);
        self.dirty = true;
    }

    /// Empties the root: an interface about to be mounted again.
    pub fn clear(&mut self) {
        for child in self.tree.children(self.root).unwrap_or_default() {
            self.remove(child);
        }
    }

    // ---- Fields ---------------------------------------------------------

    /// The field that has the keys (`focused`), if one is shown.
    pub fn focused_field(&self) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.root, &mut |id, n| {
            if found.is_none() && n.focused && n.edit.is_some() {
                found = Some(id);
            }
        });
        found
    }

    pub fn is_field(&self, node: NodeId) -> bool {
        self.tree.get_node_context(node).is_some_and(|n| n.edit.is_some())
    }

    pub fn field_text(&self, node: NodeId) -> Option<String> {
        self.tree.get_node_context(node)?.edit.as_ref().map(|e| e.text().to_owned())
    }

    /// The field's text, caret and selection's other end (bytes), as an
    /// input method is told them.
    pub fn field_surrounding(&self, node: NodeId) -> Option<(String, usize, usize)> {
        let e = self.tree.get_node_context(node)?.edit.as_ref()?;
        let (caret, anchor) = match e.selection() {
            Some((a, b)) if e.caret() == a => (a, b),
            Some((a, b)) => (b, a),
            None => (e.caret(), e.caret()),
        };
        Some((e.text().to_owned(), caret, anchor))
    }

    /// Whether the field is a password's.
    pub fn field_is_secret(&self, node: NodeId) -> bool {
        self.tree.get_node_context(node).is_some_and(|n| n.secret)
    }

    /// Where the field's caret was last drawn, logical units in the surface.
    pub fn field_caret_rect(&self, node: NodeId) -> Option<[f32; 4]> {
        self.tree.get_node_context(node)?.caret_rect
    }

    fn edited(&mut self, node: NodeId, reshape: bool) {
        if reshape {
            if let Some(n) = self.tree.get_node_context_mut(node) {
                n.buffer = None;
            }
            self.tree.mark_dirty(node).ok();
        }
        self.dirty = true;
    }

    /// A key for the field: what it did.
    pub fn field_key(&mut self, node: NodeId, name: &str, typed: Option<&str>, ctrl: bool, shift: bool) -> Option<field::Outcome> {
        let n = self.tree.get_node_context_mut(node)?;
        let secret = n.secret;
        let out = n.edit.as_mut()?.key(name, typed, ctrl, shift, secret);
        if out.used {
            self.edited(node, true);
        }
        Some(out)
    }

    /// Text inserted at the caret (pasted, or committed by an input
    /// method): whether the field's text changed.
    pub fn field_insert(&mut self, node: NodeId, text: &str) -> bool {
        let Some(edit) = self.tree.get_node_context_mut(node).and_then(|n| n.edit.as_mut()) else { return false };
        let before = edit.text().to_owned();
        edit.insert(text);
        let changed = edit.text() != before;
        self.edited(node, true);
        changed
    }

    /// What an input method is composing, or `None` when it stopped.
    pub fn field_preedit(&mut self, node: NodeId, preedit: Option<(String, Option<usize>)>) {
        let Some(edit) = self.tree.get_node_context_mut(node).and_then(|n| n.edit.as_mut()) else { return };
        if edit.preedit() == preedit.as_ref() {
            return;
        }
        edit.set_preedit(preedit);
        self.edited(node, true);
    }

    pub fn field_delete_surrounding(&mut self, node: NodeId, before: usize, after: usize) -> bool {
        let Some(edit) = self.tree.get_node_context_mut(node).and_then(|n| n.edit.as_mut()) else { return false };
        let changed = edit.delete_surrounding(before, after);
        self.edited(node, true);
        changed
    }

    /// The byte under a point of the field, local to it, logical units.
    fn field_byte(&mut self, node: NodeId, x: f32) -> Option<usize> {
        let pad = self.tree.layout(node).map_or(0.0, |l| l.padding.left);
        let mut fonts = self.fonts.lock().unwrap();
        let n = self.tree.get_node_context_mut(node)?;
        let buffer = shape(&mut fonts, n);
        let edit = n.edit.as_ref()?;
        if edit.display(n.secret).text.is_empty() {
            return Some(0);
        }
        let at = byte_at(&buffer, x - pad + edit.scroll);
        Some(edit.from_display(at, n.secret))
    }

    /// A press on the field, local to it: the caret goes there (a second
    /// press selects the word, a third the line; Shift extends).
    pub fn field_press(&mut self, node: NodeId, x: f32, shift: bool) {
        let Some(at) = self.field_byte(node, x) else { return };
        if let Some(edit) = self.tree.get_node_context_mut(node).and_then(|n| n.edit.as_mut()) {
            edit.press(at, shift, std::time::Instant::now());
        }
        self.edited(node, false);
    }

    /// The pointer dragged over the field after a press, local to it.
    pub fn field_drag(&mut self, node: NodeId, x: f32) {
        let Some(at) = self.field_byte(node, x) else { return };
        if let Some(edit) = self.tree.get_node_context_mut(node).and_then(|n| n.edit.as_mut()) {
            edit.drag(at);
        }
        self.edited(node, false);
    }

    pub fn set_id(&mut self, node: NodeId, id: Option<String>) {
        if let Some(n) = self.tree.get_node_context_mut(node) {
            n.id = id;
        }
    }

    /// The node the interface called `id`, if this scene holds one, shown.
    /// Every node, absent ones too, in tree order: what an inspector shows.
    /// Properties are those that read back, left out at zero.
    pub fn inspect(&self) -> Vec<Inspected> {
        let mut out = Vec::new();
        self.inspect_under(self.root, None, 0, true, &mut out);
        out
    }

    fn inspect_under(&self, node: NodeId, parent: Option<NodeId>, depth: usize, shown: bool, out: &mut Vec<Inspected>) {
        let Some(n) = self.tree.get_node_context(node) else { return };
        let present = shown && n.present;
        // What a new node of its kind reads back is left out.
        let fresh = Node::new(n.kind, n.role);
        let fresh_style = Style::default();
        let style = self.tree.style(node).ok();
        let mut props = Vec::new();
        for info in PROPS {
            let now = style.and_then(|st| value_of(n, st, info.prop));
            if now.is_some() && now == value_of(&fresh, &fresh_style, info.prop) {
                continue;
            }
            let value = match now {
                Some(Value::Number(v)) if v != 0.0 => format!("{v}"),
                Some(Value::Color(c)) if c[3] > 0.0 => {
                    let [r, g, b, a] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
                }
                _ => continue,
            };
            props.push((info.name, value));
        }
        out.push(Inspected {
            node: u64::from(node),
            parent: parent.map(u64::from),
            depth,
            kind: format!("{:?}", n.kind).to_lowercase(),
            role: format!("{:?}", n.role).to_lowercase(),
            name: n.name.clone(),
            id: n.id.clone(),
            text: (!n.text.is_empty()).then(|| n.text.clone()),
            bounds: n.extent,
            present,
            props,
        });
        for child in self.tree.children(node).unwrap_or_default() {
            self.inspect_under(child, Some(node), depth + 1, present, out);
        }
    }

    /// The first `navigate` node shown, in tree order.
    pub fn first_navigate(&self) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.root, &mut |node, n| {
            if found.is_none() && n.navigate {
                found = Some(node);
            }
        });
        found
    }

    /// The nearest `navigate` node at or above `node`.
    pub fn navigate_scope(&self, node: NodeId) -> Option<NodeId> {
        let mut at = Some(node);
        while let Some(id) = at {
            if self.tree.get_node_context(id).is_some_and(|n| n.navigate) {
                return Some(id);
            }
            at = self.parent(id);
        }
        None
    }

    /// Marks `node` as one things may be dropped on (it has an `on_drop`).
    pub fn set_drop_target(&mut self, node: NodeId, on: bool) {
        if let Some(n) = self.tree.get_node_context_mut(node) {
            n.drop_target = on;
        }
    }

    /// The topmost drop target under a point, logical units, as last drawn.
    pub fn drop_target_at(&self, x: f32, y: f32) -> Option<NodeId> {
        let (mut found, mut raised) = (None, None);
        self.walk_layered(self.root, false, &mut |id, n, up| {
            let [bx, by, bw, bh] = n.bounds;
            if n.drop_target && x >= bx && y >= by && x < bx + bw && y < by + bh {
                if up { raised = Some(id) } else { found = Some(id) }
            }
        });
        raised.or(found)
    }

    /// What dragging `node` gives, from it or the nearest node around it
    /// that says: the files (paths) and the text.
    pub fn drag_data(&self, node: NodeId) -> Option<(Vec<String>, Option<String>)> {
        let mut at = Some(node);
        while let Some(id) = at {
            let n = self.tree.get_node_context(id)?;
            if n.drag_files.is_some() || n.drag_text.is_some() {
                let files = n.drag_files.as_deref().map(|f| f.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
                return Some((files, n.drag_text.clone()));
            }
            at = self.parent(id);
        }
        None
    }

    /// Marks `node` as a `g.list`, which scrolls to the row the keyboard's
    /// focus is on (its `on_wheel` takes logical units).
    pub fn mark_list(&mut self, node: NodeId) {
        if let Some(n) = self.tree.get_node_context_mut(node) {
            n.rows_list = true;
        }
    }

    /// The nearest `g.list` around `node`, not `node` itself.
    pub fn list_around(&self, node: NodeId) -> Option<NodeId> {
        let mut at = self.parent(node);
        while let Some(id) = at {
            if self.tree.get_node_context(id).is_some_and(|n| n.rows_list) {
                return Some(id);
            }
            at = self.parent(id);
        }
        None
    }

    /// Whether a press on `node` moves its window (`move_window`).
    pub fn moves_window(&self, node: NodeId) -> bool {
        self.tree.get_node_context(node).is_some_and(|n| n.move_window)
    }

    /// Whether `node` is a slider: the arrows along it are its own.
    pub fn is_slider(&self, node: NodeId) -> bool {
        self.tree.get_node_context(node).is_some_and(|n| n.role == Role::Slider)
    }

    /// Where the pointer is now. A shader that reads `shader.pointer` is
    /// drawn again; nothing else is.
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32, bool)>) {
        if pointer == self.pointer {
            return;
        }
        self.pointer = pointer;
        let mut reads = false;
        self.walk(self.root, &mut |_, n| {
            reads |= n.shader.as_ref().is_some_and(|(source, _)| source.contains("shader.pointer"))
                || n.compute.as_ref().is_some_and(|(source, _)| source.contains("shader.pointer"));
        });
        if reads {
            self.dirty = true;
        }
    }

    /// Whether a button is held, as the pointer last said.
    pub fn pointer_held(&self) -> bool {
        self.pointer.is_some_and(|p| p.2)
    }

    /// Rings `node` as the keyboard's, or nothing.
    pub fn set_focus_ring(&mut self, node: Option<NodeId>) {
        if node != self.focus_ring {
            self.focus_ring = node;
            self.dirty = true;
        }
    }

    /// Outlines `node` over everything this surface draws, or nothing.
    pub fn set_highlight(&mut self, node: Option<u64>) {
        let node = node.map(NodeId::from).filter(|n| self.tree.get_node_context(*n).is_some());
        if node != self.highlight {
            self.highlight = node;
            self.dirty = true;
        }
    }

    /// Every id some `combine_with` names.
    fn combine_names(&self) -> Vec<String> {
        let mut wanted: Vec<String> = Vec::new();
        self.walk(self.root, &mut |_, n| {
            for id in n.combine_with.as_deref().unwrap_or("").split_whitespace() {
                if !wanted.iter().any(|w| w == id) {
                    wanted.push(id.to_owned());
                }
            }
        });
        wanted
    }

    /// The capsules named by a `combine_with`, by id: where each was last
    /// drawn and its radius, logical units. A text or a path named is its
    /// outline's distance field instead (see `distance`).
    fn combined_shapes(&self) -> std::collections::HashMap<String, [f32; 5]> {
        let mut wanted: Vec<String> = Vec::new();
        self.walk(self.root, &mut |_, n| {
            // And the nodes an emitter's particles are pulled toward or
            // bounce off.
            let named = [n.combine_with.as_deref(), n.emit.attract.as_deref(), n.emit.collide.as_deref()];
            for id in named.into_iter().flatten().flat_map(str::split_whitespace) {
                if !wanted.iter().any(|w| w == id) {
                    wanted.push(id.to_owned());
                }
            }
        });
        let mut shapes = std::collections::HashMap::new();
        if wanted.is_empty() {
            return shapes;
        }
        self.walk(self.root, &mut |_, n| {
            if let Some(id) = &n.id
                && wanted.contains(id)
                && !shapes.contains_key(id)
                && !matches!(n.kind, Kind::Text | Kind::Path)
            {
                let [x, y, w, h] = n.extent;
                // A capsule is round at the ends unless told otherwise; any
                // other node is square unless given a radius.
                let round = if n.kind == Kind::Capsule { h / 2.0 } else { 0.0 };
                let radius = n.radius.unwrap_or(round).min(w.min(h) / 2.0);
                shapes.insert(id.clone(), [x, y, w, h, radius]);
            }
        });
        shapes
    }

    pub fn find(&self, id: &str) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.root, &mut |node, n| {
            if found.is_none() && n.id.as_deref() == Some(id) {
                found = Some(node);
            }
        });
        found
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
            // Closer than the eye can tell, it jumps: a label that follows
            // a drifting cell a hundredth of a pixel a step would otherwise
            // be one transition after another, and every refresh a frame.
            if from.as_ref().is_some_and(|f| f != &value && unseen(prop, f, &value, self.scale)) {
                self.animations.retain(|a| !(a.node == node && a.prop == prop));
                return self.apply(node, prop, value);
            }
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

    /// Where `prop` stands on `node` now, for the properties a value can be
    /// read back from: numbers, colours, sizes.
    pub fn value(&self, node: NodeId, prop: Prop) -> Option<Value> {
        self.current(node, prop)
    }

    fn current(&self, node: NodeId, prop: Prop) -> Option<Value> {
        let n = self.tree.get_node_context(node)?;
        let style = self.tree.style(node).ok()?;
        value_of(n, style, prop)
    }
}

/// What `prop` reads back as on a node with that style.
fn value_of(n: &Node, style: &Style, prop: Prop) -> Option<Value> {
    {
        let length = |d: Dimension| (d.tag() == taffy::CompactLength::LENGTH_TAG).then(|| d.value());
        Some(match prop {
            Prop::Width => Value::Number(length(style.size.width)?),
            Prop::Height => Value::Number(length(style.size.height)?),
            Prop::Radius => Value::Number(n.radius?),
            Prop::Level => Value::Number(n.level),
            Prop::Arc => Value::Number(n.arc),
            Prop::FillAngle => Value::Number(n.fill_angle?),
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
            Prop::CombineSoft => Value::Number(n.combine_soft),
            Prop::BlurRadius => Value::Number(n.blur_radius),
            Prop::Saturation => Value::Number(n.filter[0]),
            Prop::Contrast => Value::Number(n.filter[1]),
            Prop::Brightness => Value::Number(n.filter[2]),
            Prop::Tint => Value::Color([n.filter[4], n.filter[5], n.filter[6], n.filter[7]]),
            Prop::BackdropBlur => Value::Number(n.backdrop_blur),
            Prop::Halo => Value::Number(n.halo),
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
            Prop::ColorBottom => Value::Color(n.color_bottom),
            Prop::Shadow => Value::Color(n.shadow),
            Prop::ShadowBlur => Value::Number(n.shadow_blur),
            Prop::ShadowY => Value::Number(n.shadow_y),
            Prop::ShadowX => Value::Number(n.shadow_x),
            Prop::ShadowSpread => Value::Number(n.shadow_spread),
            Prop::InnerShadow => Value::Color(n.inner_shadow),
            Prop::InnerBlur => Value::Number(n.inner[0]),
            Prop::InnerX => Value::Number(n.inner[1]),
            Prop::InnerY => Value::Number(n.inner[2]),
            Prop::InnerSpread => Value::Number(n.inner[3]),
            Prop::Outline => Value::Color(n.ring),
            Prop::OutlineWidth => Value::Number(n.ring_size[0]),
            Prop::OutlineOffset => Value::Number(n.ring_size[1]),
            _ => return None,
        })
    }
}

impl Scene {
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
        let runs = |v: &Value| if let Value::Runs(x) = v { Some(x.clone()) } else { None };
        let stops = |v: &Value| if let Value::Stops(x) = v { Some(x.clone()) } else { None };
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
            Prop::FillStops => n.fill_stops = take!(stops),
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
            Prop::InhibitShortcuts => n.inhibit_shortcuts = take!(flag),
            Prop::InhibitIdle => n.inhibit_idle = take!(flag),
            Prop::Playing => n.playing = take!(flag),
            Prop::Mask => n.mask = take!(flag),
            Prop::Cache => n.cache = take!(flag),
            Prop::Pass => {
                // Out of the flow, as an overlay; without a size of its own it
                // covers its parent (`place`).
                n.pass = take!(flag);
                style.position = if n.pass { taffy::Position::Absolute } else { taffy::Position::Relative };
                restyle = true;
            }
            Prop::Blend => {
                n.blend = match take!(text).as_str() {
                    "multiply" => 1,
                    "screen" => 2,
                    "add" => 3,
                    "overlay" => genoma_render::Image::OVERLAY,
                    _ => 0,
                };
            }
            Prop::Slice => {
                let v = take!(numbers);
                n.slice = match v.as_slice() {
                    [] => None,
                    [a] => Some([*a; 4]),
                    [t, r, b, l, ..] => Some([*t, *r, *b, *l]),
                    [v, h] | [v, h, _] => Some([*v, *h, *v, *h]),
                }
                .map(|s| s.map(|x| x.max(0.0)));
            }
            Prop::Saturation => n.filter[0] = take!(number).max(0.0),
            Prop::Contrast => n.filter[1] = take!(number).max(0.0),
            Prop::Brightness => n.filter[2] = take!(number).max(0.0),
            Prop::Tint => {
                let c = take!(colour);
                n.filter[4..8].copy_from_slice(&c);
            }
            Prop::Growth => n.growth = take!(number).clamp(0.0, 1.0),
            Prop::Reach => n.reach = take!(number).clamp(0.0, 1.0),
            Prop::Clip => n.clip = take!(flag),
            Prop::FadeBottom => n.fade_bottom = take!(number),
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
            Prop::Halo => n.halo = take!(number).max(0.0),
            Prop::Rate => (n.emit.rate, mover) = (take!(number).max(0.0), true),
            Prop::Lifetime => n.emit.lifetime = take!(number).max(0.05),
            Prop::Speed => n.emit.speed = take!(number),
            Prop::Spread => n.emit.spread = take!(number),
            Prop::Heading => n.emit.heading = take!(number),
            Prop::Gravity => n.emit.gravity = take!(number),
            Prop::ParticleSize => n.emit.size = take!(number).max(0.5),
            Prop::ColorEnd => n.emit.color_end = Some(take!(colour)),
            Prop::SizeEnd => n.emit.size_end = Some(take!(number).max(0.0)),
            Prop::Trail => n.emit.trail = take!(number).max(0.0),
            Prop::Sub => n.emit.sub = take!(flag),
            Prop::Burst => n.emit.burst = take!(number).max(0.0).round(),
            Prop::Attract => n.emit.attract = Some(take!(text)).filter(|t| !t.trim().is_empty()),
            Prop::Attraction => n.emit.attraction = take!(number),
            Prop::AttractionReach => n.emit.reach = take!(number).max(1.0),
            Prop::Collide => n.emit.collide = Some(take!(text)).filter(|t| !t.trim().is_empty()),
            Prop::Bounce => n.emit.bounce = take!(number).clamp(0.0, 1.0),
            Prop::Spin => n.emit.spin = take!(number),
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
            Prop::ColorBottom => n.color_bottom = take!(colour),
            Prop::Shadow => n.shadow = take!(colour),
            Prop::ShadowBlur => n.shadow_blur = take!(number),
            Prop::ShadowY => n.shadow_y = take!(number),
            Prop::ShadowX => n.shadow_x = take!(number),
            Prop::ShadowSpread => n.shadow_spread = take!(number),
            Prop::InnerShadow => n.inner_shadow = take!(colour),
            Prop::InnerBlur => n.inner[0] = take!(number).max(0.0),
            Prop::InnerX => n.inner[1] = take!(number),
            Prop::InnerY => n.inner[2] = take!(number),
            Prop::InnerSpread => n.inner[3] = take!(number),
            Prop::Outline => n.ring = take!(colour),
            Prop::OutlineWidth => n.ring_size[0] = take!(number).max(0.0),
            Prop::OutlineOffset => n.ring_size[1] = take!(number),
            Prop::Text => {
                let t = take!(text);
                // A field follows its binding when it says something other
                // than what the field holds: cleared after a submit, say.
                if let Some(edit) = n.edit.as_mut() {
                    if edit.text() != t {
                        edit.set_text(&t);
                        reshape = true;
                    }
                    n.text = t;
                } else {
                    if n.text == t {
                        return Ok(());
                    }
                    n.text = t;
                    reshape = true;
                }
            }
            Prop::Placeholder => (n.placeholder, reshape) = (take!(text), true),
            Prop::PlaceholderColor => n.placeholder_color = take!(colour),
            Prop::SelectionColor => n.selection_color = take!(colour),
            Prop::CaretColor => n.caret_color = take!(colour),
            Prop::Focused => n.focused = take!(flag),
            Prop::Secret => (n.secret, reshape) = (take!(flag), true),
            Prop::FontSize => (n.font_size, reshape) = (take!(number), true),
            Prop::Font => (n.font, reshape) = (Some(take!(text)), true),
            Prop::Tabular => (n.tabular, reshape) = (take!(flag), true),
            Prop::Marks => {
                let marks = take!(numbers);
                if n.marks == marks {
                    return Ok(());
                }
                (n.marks, reshape) = (marks, true);
            }
            Prop::MarkColor => (n.mark_color, reshape) = (take!(colour), true),
            Prop::Runs => {
                let runs = take!(runs);
                if n.runs == runs {
                    return Ok(());
                }
                (n.runs, reshape) = (runs, true);
            }
            Prop::Ellipsize => {
                n.ellipsize = match take!(text).as_str() {
                    "middle" => 1,
                    "start" => 2,
                    _ => 0,
                };
                reshape = true;
            }
            Prop::TextAlign => {
                n.text_align = match take!(text).as_str() {
                    "center" => 1,
                    "end" => 2,
                    _ => 0,
                };
                reshape = true;
            }
            Prop::Pulse => (n.pulse, mover) = (take!(number).max(0.0), true),
            Prop::Orbit => (n.orbit, mover) = (take!(number), true),
            Prop::OrbitRadius => n.orbit_radius = take!(number),
            Prop::Icon => n.icon = Some(take!(text)),
            Prop::Src if n.kind == Kind::Shader => {
                let path = take!(text);
                // Read when set: a reload sets it again, so an edited file is
                // seen with the interface that uses it. `genoma:name` is one
                // of the effects Genoma ships.
                let read = match path.strip_prefix("genoma:") {
                    Some(name) => genoma_render::shader::built_in(name)
                        .map(str::to_owned)
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such built-in effect")),
                    None => std::fs::read_to_string(&path),
                };
                // Checked as the device will compile it, before it goes there:
                // a mistake is drawn over the node, at its own line, and the
                // node waits for the file to be saved right.
                n.shader_error = None;
                let file = std::path::Path::new(&path).file_name().map_or(path.clone(), |f| f.to_string_lossy().into_owned());
                n.shader = match read {
                    Ok(source) => {
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&source, &mut h);
                        let key = std::hash::Hasher::finish(&h);
                        match checked(key, &source) {
                            Ok(()) => Some((Arc::new(source), key)),
                            Err(e) => {
                                log::warn!("g.shader {path}: {e}");
                                n.shader_error = Some(format!("{file}: {e}"));
                                None
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("g.shader {path}: {e}");
                        n.shader_error = Some(format!("{file}: {e}"));
                        None
                    }
                };
                // One that reads the pointer takes it, or the surface would
                // let it through to what is under.
                if n.shader.as_ref().is_some_and(|(source, _)| source.contains("shader.pointer")) {
                    n.interactive = true;
                }
            }
            Prop::Compute => {
                let path = take!(text);
                // As `src`: read when set, `genoma:name` one Genoma ships,
                // checked before it goes to the device.
                let read = match path.strip_prefix("genoma:") {
                    Some(name) => genoma_render::sim::built_in(name)
                        .map(str::to_owned)
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such built-in simulation")),
                    None => std::fs::read_to_string(&path),
                };
                let file = std::path::Path::new(&path).file_name().map_or(path.clone(), |f| f.to_string_lossy().into_owned());
                n.compute_error = None;
                n.compute = match read {
                    Ok(source) => {
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&("compute", &source), &mut h);
                        let key = std::hash::Hasher::finish(&h);
                        match genoma_render::sim::check(&source) {
                            Ok(()) => {
                                // Its own `shade`, for a node with no `src`.
                                n.compute_shades = checked(key, &source).map_err(|e| format!("{file}: {e}"));
                                Some((Arc::new(source), key))
                            }
                            Err(e) => {
                                log::warn!("g.shader compute {path}: {e}");
                                n.compute_error = Some(format!("{file}: {e}"));
                                None
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("g.shader compute {path}: {e}");
                        n.compute_error = Some(format!("{file}: {e}"));
                        None
                    }
                };
                // Started afresh with another file.
                n.steps_taken = 0;
                n.steps_owed = 0.0;
                if n.compute.as_ref().is_some_and(|(source, _)| source.contains("shader.pointer")) {
                    n.interactive = true;
                }
                mover = true;
            }
            Prop::Grid => {
                let v = take!(numbers);
                n.grid = match v.as_slice() {
                    [w, h, ..] => Some([w.clamp(1.0, 2048.0) as u32, h.clamp(1.0, 2048.0) as u32]),
                    [w] => Some([w.clamp(1.0, 2048.0) as u32; 2]),
                    [] => None,
                };
            }
            Prop::Agents => n.agents = take!(number).clamp(0.0, 65536.0) as u32,
            Prop::AgentSize => n.agent_size = take!(number).max(0.0),
            Prop::Steps => n.steps_rate = take!(number).clamp(0.0, 960.0),
            Prop::Src => {
                let src = take!(text);
                // Another picture: an animation starts from its first frame.
                if n.src.as_deref() != Some(src.as_str()) {
                    n.play_time = 0.0;
                }
                n.src = Some(src);
            }
            Prop::Values => {
                n.values = take!(numbers);
                n.values.truncate(1024);
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
            Prop::D => {
                let d = take!(text);
                n.reach_of_path = path_reach(&d);
                n.outline = Some(d);
                restyle = true;
            }
            Prop::Points => {
                n.points = take!(numbers);
                n.reach_of_path = n.points.chunks_exact(2).fold([0.0f32; 2], |r, p| [r[0].max(p[0]), r[1].max(p[1])]);
                restyle = true;
            }
            Prop::Join => {
                n.join = match take!(text).as_str() {
                    "round" => "round",
                    "bevel" => "bevel",
                    _ => "miter",
                }
            }
            Prop::Cap => {
                n.cap = match take!(text).as_str() {
                    "round" => "round",
                    "square" => "square",
                    _ => "butt",
                }
            }
            Prop::EvenOdd => n.even_odd = take!(flag),
            Prop::CombineWith => n.combine_with = Some(take!(text)).filter(|id| !id.is_empty()),
            Prop::Combine => {
                n.combine = match take!(text).as_str() {
                    "subtract" => 1,
                    "intersect" => 2,
                    _ => 0,
                }
            }
            Prop::CombineSoft => n.combine_soft = take!(number).max(0.0),
            Prop::Navigate => n.navigate = take!(flag),
            Prop::DragFiles => {
                n.drag_files = Some(take!(text)).filter(|t| !t.is_empty());
                n.interactive |= n.drag_files.is_some();
            }
            Prop::DragText => {
                n.drag_text = Some(take!(text)).filter(|t| !t.is_empty());
                n.interactive |= n.drag_text.is_some();
            }
            Prop::MoveWindow => {
                n.move_window = take!(flag);
                // It takes the press even with no handler of its own.
                if n.move_window {
                    n.interactive = true;
                }
            }
            Prop::BlurRadius => n.blur_radius = take!(number).max(0.0),
            Prop::BackdropBlur => n.backdrop_blur = take!(number).max(0.0),
            Prop::Backdrop => n.reads_backdrop = take!(flag),
            Prop::FocusColor => n.focus_color = take!(colour),
            Prop::Hollow => n.hollow = take!(flag),
            Prop::Blur => n.blur = take!(flag),
            Prop::Wave => n.wave = take!(number),
            Prop::WavePeriod => n.wave_period = take!(number).max(1.0),
            Prop::WaveTime => (n.wave_time, mover) = (take!(number).max(0.0), true),
            Prop::LevelLeft => n.level_left = take!(flag),
            Prop::FillAcross => n.fill_across = take!(flag),
            Prop::FillAngle => n.fill_angle = Some(take!(number)),
            Prop::FillShape => {
                n.fill_shape = match take!(text).as_str() {
                    "radial" => 1,
                    "conic" => 2,
                    _ => 0,
                };
            }
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
        self.scale = scale;
        self.now = now.saturating_duration_since(self.epoch).as_secs_f32();
        let clock = self.now;

        let animations = std::mem::take(&mut self.animations);
        let mut running = Vec::new();
        // What is not seen keeps no surface drawing: a transition on a node
        // in nothing visible (a closed cell's panel, inside something of no
        // size) waits, its clock running, and goes on where it should be
        // once it is seen again. One on a node growing from nothing counts,
        // as its parent is seen.
        let mut waiting_unseen = 0;
        for mut a in animations {
            if !self.seen(a.node) && !self.parent(a.node).is_some_and(|p| self.seen(p)) {
                waiting_unseen += 1;
                running.push(a);
                continue;
            }
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
            // Ended where it was going, exactly: a fade that stopped at
            // 0.99999994 was a fade to 1 started again by every new 1.
            let value = if t >= 1.0 { a.to.clone() } else { match (&a.from, &a.to) {
                (Value::Number(f), Value::Number(to)) => Value::Number(f + (to - f) * y),
                (Value::Color(f), Value::Color(to)) => {
                    let mut c = [0.0; 4];
                    for i in 0..4 {
                        c[i] = (f[i] + (to[i] - f[i]) * y).clamp(0.0, 1.0);
                    }
                    Value::Color(c)
                }
                _ => a.to.clone(),
            } };
            let _ = self.apply(a.node, a.prop, value);
            if t < 1.0 {
                running.push(a);
            }
        }
        self.animations = running;
        if log::log_enabled!(log::Level::Trace) && !self.animations.is_empty() {
            let props: Vec<String> = self.animations.iter().take(6).map(|a| format!("{:?}:{:?}->{:?}", a.prop, a.from, a.to)).collect();
            log::trace!("moving: {} animations {}", self.animations.len(), props.join(" "));
        }
        let mut motion = Motion { moving: self.animations.len() > waiting_unseen, wake: None };
        // An image that finished decoding is news; one still decoding is
        // looked for again on a short beat.
        if self.images.collect() {
            self.dirty = true;
            motion.moving = true;
        }
        if self.images.pending() {
            motion.wake = Some(images::BEAT);
        }

        // Animated images: each one's time runs while it plays and is seen,
        // and the surface is drawn again when its frame changes, not before.
        for node in self.animated.clone() {
            if !self.seen(node) {
                continue;
            }
            let Some(n) = self.tree.get_node_context_mut(node) else { continue };
            if !n.playing || !n.frame_left.is_finite() {
                continue;
            }
            n.play_time += dt;
            n.frame_left -= dt;
            let wait = if n.frame_left <= 0.0 {
                self.dirty = true;
                Duration::ZERO
            } else {
                Duration::from_secs_f32(n.frame_left)
            };
            motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
        }
        let seen: Vec<bool> = self.movers.iter().map(|&m| self.seen(m)).collect();
        // An emitter keeps its slots, and its frames, while the bursts its
        // sub-emitters make where its particles die still live; and keeps
        // the pace of the fastest of them. The longest life, the top speed.
        let lingers: Vec<(f32, f32)> = self
            .movers
            .iter()
            .map(|&m| {
                let mut linger = (0.0f32, 0.0f32);
                if self.tree.get_node_context(m).is_some_and(|n| n.kind == Kind::Emitter) {
                    for c in self.tree.children(m).unwrap_or_default() {
                        if let Some(c) = self.tree.get_node_context(c).filter(|c| c.kind == Kind::Emitter && c.emit.sub && c.present) {
                            linger.0 = linger.0.max(c.emit.lifetime);
                            linger.1 = linger.1.max(c.emit.speed.abs() + c.emit.gravity.abs() * c.emit.lifetime);
                        }
                    }
                }
                linger
            })
            .collect();
        for (k, &node) in self.movers.iter().enumerate() {
            let Some(n) = self.tree.get_node_context_mut(node) else { continue };
            // Unseen, it neither moves nor asks for frames: its phase picks
            // up where it was.
            if !n.present || !seen[k] {
                continue;
            }
            if n.kind == Kind::Shader {
                // The drawn numbers follow the bound ones while one is still
                // on its way, and not at all after: data arriving at sixty a
                // second breathes instead of stepping. They are paced as
                // everything else that travels, a frame a quarter pixel of
                // movement, its numbers taken as logical units (the shapes a
                // shader is given are): lobes that turn slowly ask for few
                // frames, fast ones for every refresh. The clock, when asked
                // for, runs at the display's rate.
                let mut moving = false;
                let mut fastest = 0.0f32;
                if n.follow > 0.0 {
                    let k = 1.0 - (-dt.min(0.1) / n.follow).exp();
                    for (shown, wanted) in n.shown.iter_mut().zip(&n.values) {
                        let gap = wanted - *shown;
                        if gap.abs() > 0.002 {
                            *shown += gap * k;
                            moving = true;
                            // How fast it closes now, units a second.
                            fastest = fastest.max((wanted - *shown).abs() / n.follow);
                        } else {
                            *shown = *wanted;
                        }
                    }
                }
                if n.clock || moving {
                    // A paced shader's frames are further apart than a
                    // clock's; after a quiet spell neither leaps.
                    n.time += dt.min(if n.clock { 0.05 } else { 0.1 });
                    self.dirty = true;
                }
                // Its simulation steps while its clock runs, at its rate.
                if n.clock && n.compute.is_some() {
                    n.steps_owed += n.steps_rate * dt.min(0.05);
                    let whole = n.steps_owed.floor();
                    n.steps_owed -= whole;
                    n.steps_taken += whole as u64;
                }
                if n.clock {
                    motion.moving = true;
                } else if moving {
                    let wait = Duration::from_secs_f32((step() / (fastest * scale).max(1e-3)).min(0.1));
                    motion.wake = Some(motion.wake.map_or(wait, |w| w.min(wait)));
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
                n.emit.linger = lingers[k].0;
                if n.emit.step(clock, dt.min(0.25)) {
                    let speed = if n.emit.target {
                        let (dx, dy) = (n.path[4] - n.path[0], n.path[5] - n.path[1]);
                        (dx * dx + dy * dy).sqrt() / n.emit.lifetime * 1.5
                    } else {
                        let pull = if n.emit.attract.is_some() { n.emit.attraction.abs() } else { 0.0 };
                        n.emit.speed.abs() + (n.emit.gravity.abs() + pull) * n.emit.lifetime
                    };
                    pace(speed.max(lingers[k].1) * scale);
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

    /// Whether `node` was laid out in the last frame and showed some of
    /// itself: present, inside its clips, of some size.
    fn seen(&self, node: NodeId) -> bool {
        self.tree.get_node_context(node).is_some_and(|n| n.present && n.laid_frame == self.frame && n.bounds[2] > 0.0 && n.bounds[3] > 0.0)
    }

    /// What is moving now and why, for the inspector: each transition on
    /// its way (node, property) and each node that moves of itself (a
    /// shader's clock, an orbit, a filling arc, a wave, a sway, particles,
    /// a pulse). A surface that keeps drawing at rest says why here.
    pub fn moving_report(&self) -> Vec<String> {
        // A transition on something unseen waits and asks for nothing: said
        // after the ones that move.
        let mut out: Vec<String> = Vec::new();
        let mut waiting = 0;
        for a in &self.animations {
            if self.seen(a.node) || self.parent(a.node).is_some_and(|p| self.seen(p)) {
                out.push(format!("{} transition {:?}", u64::from(a.node), a.prop));
            } else {
                waiting += 1;
            }
        }
        if waiting > 0 {
            out.push(format!("{waiting} transitions waiting, unseen"));
        }
        for &node in &self.movers {
            let Some(n) = self.tree.get_node_context(node) else { continue };
            if !n.present || !self.seen(node) {
                continue;
            }
            let id = u64::from(node);
            let mut why = Vec::new();
            if n.kind == Kind::Shader && n.clock {
                why.push("shader clock".to_owned());
            }
            if n.orbit != 0.0 {
                why.push(format!("orbit {}", n.orbit));
            }
            if n.arc_rate > 0.0 && n.arc < 1.0 {
                why.push("arc filling".to_owned());
            }
            if n.wave_time > 0.0 {
                why.push("wave".to_owned());
            }
            if n.sway > 0.0 {
                why.push("sway".to_owned());
            }
            if n.kind == Kind::Emitter {
                why.push("particles".to_owned());
            }
            if n.pulse > 0.0 {
                why.push(format!("pulse {}", n.pulse));
            }
            if !why.is_empty() {
                out.push(format!("{id} {:?} {}", n.kind, why.join(", ")));
            }
        }
        out
    }

    /// Whether anything changed since the last call.
    /// To be drawn again, though nothing in it changed: something drawn
    /// over it from outside (the profiling overlay) did.
    pub fn touch(&mut self) {
        self.dirty = true;
    }

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
                    // Aligned lines are placed within the whole width, so the
                    // node takes it.
                    if n.text_align != 0 {
                        w = n.max_width.unwrap_or(w);
                    }
                    Size { width: known.width.unwrap_or(w.ceil()), height: known.height.unwrap_or(h.ceil()) }
                }
                // A field is a line high, and as wide as what it shows and
                // its caret unless given a width.
                // A path reaches as far right and down as its outline, from
                // its own corner, unless given a size.
                Some(n) if n.kind == Kind::Path => Size {
                    width: known.width.unwrap_or(n.reach_of_path[0].ceil()),
                    height: known.height.unwrap_or(n.reach_of_path[1].ceil()),
                },
                Some(n) if n.kind == Kind::Field => {
                    let buffer = shape(&mut fonts, n);
                    let w = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max) + 1.0;
                    let h = (n.font_size * 1.2).ceil();
                    Size { width: known.width.unwrap_or(w.ceil()), height: known.height.unwrap_or(h) }
                }
                _ => Size::ZERO,
            })
        };
        let space = Size {
            width: width.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
            height: height.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
        };
        self.tree.compute_layout_with_measure(self.root, space, measure).expect("layout");
        for (&node, told) in self.sized.iter_mut() {
            let Ok(l) = self.tree.layout(node) else { continue };
            let now = [l.size.width, l.size.height];
            if *told != Some(now) {
                *told = Some(now);
                self.newly_sized.retain(|&(n, ..)| n != node);
                self.newly_sized.push((node, now[0], now[1]));
            }
        }
        let size = self.tree.layout(self.root).expect("root layout").size;
        (size.width, size.height)
    }

    /// What the surface shows, at `scale` physical pixels per logical unit.
    pub fn draw_list(&mut self, scale: f32) -> DrawList {
        let mut list = DrawList::default();
        let mut fonts = self.fonts.lock().unwrap();
        let viewport = self.tree.layout(self.root).map_or([0.0; 2], |l| [l.size.width * scale, l.size.height * scale]);
        self.frame += 1;
        let others = self.combined_shapes();
        self.distance.begin(self.combine_names());
        let ring_colour = self.focus_ring.and_then(|n| self.navigate_scope(n)).and_then(|s| self.tree.get_node_context(s)).map(|s| s.focus_color);
        let mut out = Out { frame: self.frame, under: None, pointer: self.pointer, others, list: &mut list, fade: [0.0; 2], transform: Rect::IDENTITY, now: self.now, parent_shape: None, shape_here: None, clip: Rect::NO_CLIP, icons: &mut self.icons, images: &mut self.images, glyphs: &mut self.glyphs, distance: &mut self.distance, blur: Vec::new(), upper: DrawList::default(), raised: false, drawn_images: Vec::new(), animated: Vec::new(), mask: None, passes: None, emitted: std::collections::HashMap::new(), viewport };
        collect(&mut self.tree, &mut fonts, self.root, [0.0; 4], [0.0; 4], 1.0, scale, &mut out);
        // The keyboard's ring: round the focused node, a little outside it,
        // in its walk's colour.
        if let Some(node) = self.focus_ring
            && let (Some(n), Some(colour)) = (self.tree.get_node_context(node), ring_colour)
            && n.present
        {
            {
                let [x, y, w, h] = n.extent;
                let gap = 3.0;
                let radius = n.radius.unwrap_or(h / 2.0).min(w.min(h) / 2.0) + gap;
                let mut ring = flat([(x - gap) * scale, (y - gap) * scale], [(w + 2.0 * gap) * scale, (h + 2.0 * gap) * scale], radius * scale, [0.0; 4], &out);
                ring.rim = (2.0 * scale).round().max(1.0);
                ring.rim_top = colour;
                ring.rim_bottom = colour;
                out.list.push_rect(ring);
            }
        }
        // An inspector's mark: the node's box, outlined in magenta, a
        // translucent wash inside.
        if let Some([x, y, w, h]) = self.highlight.and_then(|n| self.tree.get_node_context(n)).map(|n| n.extent) {
            let mut mark = flat([x * scale, y * scale], [w.max(1.0) * scale, h.max(1.0) * scale], 0.0, [1.0, 0.0, 1.0, 0.12], &out);
            mark.rim = (2.0 * scale).round().max(1.0);
            mark.rim_top = [1.0, 0.0, 1.0, 1.0];
            mark.rim_bottom = [1.0, 0.0, 1.0, 1.0];
            out.list.push_rect(mark);
        }
        self.blur = out.blur;
        self.animated = std::mem::take(&mut out.animated);
        for id in std::mem::take(&mut out.drawn_images) {
            if self.ready.insert(id) {
                log::debug!("image {id:?} drawn for the first time");
                self.newly_ready.push(id);
            }
        }
        let upper = out.upper;
        self.distance.end();
        if !upper.is_empty() {
            list.append(upper);
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

/// The weight a text is set at: the one asked for when its family has a
/// face of it, or a variable face spanning it; else the family's nearest,
/// as CSS chooses. cosmic-text keeps a family only at the exact weight and
/// otherwise falls to another family: Orbitron packaged as 300, 500, 700
/// and 900 lost every text at the default 400. Remembered by family,
/// weight, style and stretch while the font database stays the same size.
fn settled<'a>(db: &ct::fontdb::Database, mut attrs: ct::Attrs<'a>) -> ct::Attrs<'a> {
    use std::{collections::HashMap, sync::Mutex};
    type Known = (usize, HashMap<(String, u16, u8, u16), u16>);
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let family = match attrs.family {
        ct::Family::Name(name) => name.to_owned(),
        other => format!("{other:?}"),
    };
    let style = match attrs.style {
        ct::Style::Normal => 0,
        ct::Style::Italic => 1,
        ct::Style::Oblique => 2,
    };
    let key = (family, attrs.weight.0, style, attrs.stretch.to_number());
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let known = known.get_or_insert_with(|| (db.len(), HashMap::new()));
    if known.0 != db.len() {
        *known = (db.len(), HashMap::new());
    }
    let weight = *known.1.entry(key).or_insert_with(|| {
        let asked = attrs.weight;
        let query = ct::fontdb::Query { families: &[attrs.family], weight: asked, stretch: attrs.stretch, style: attrs.style };
        let Some(face) = db.query(&query).and_then(|id| db.face(id)) else { return asked.0 };
        if face.weight == asked {
            return asked.0;
        }
        let spans = db.with_face_data(face.id, |data, index| {
            use ct::skrifa::MetadataProvider;
            let font = ct::skrifa::FontRef::from_index(data, index).ok()?;
            let axis = font.axes().get_by_tag(ct::skrifa::Tag::new(b"wght"))?;
            Some((axis.min_value()..=axis.max_value()).contains(&(asked.0 as f32)))
        });
        if spans == Some(Some(true)) { asked.0 } else { face.weight.0 }
    });
    attrs.weight = ct::Weight(weight);
    attrs
}

impl TextStyle<'_> {
    fn attrs(&self, db: &ct::fontdb::Database) -> ct::Attrs<'_> {
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
        settled(db, attrs)
    }
}

/// How wide `text` is set in `style` at `size`, logical units: what a layout
/// that hands out room by number (Bioma's tissues) needs before drawing.
pub fn measure_text(fonts: &mut ct::FontSystem, text: &str, size: f32, style: &TextStyle<'_>) -> f32 {
    let mut buffer = ct::Buffer::new(fonts, ct::Metrics::new(size, (size * 1.2).ceil()));
    buffer.set_wrap(ct::Wrap::None);
    buffer.set_text(text, &style.attrs(fonts.db()), ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

/// How tall `text` is set in `style` at `size` and wrapped within `width`,
/// logical units, as a text node with `wrap` and that `max_width` draws it:
/// what a shape that grows to hold a sentence needs before drawing it.
pub fn measure_wrapped(
    fonts: &mut ct::FontSystem,
    text: &str,
    size: f32,
    style: &TextStyle<'_>,
    width: f32,
    runs: &[TextRun],
) -> f32 {
    let line = (size * 1.2).ceil();
    let mut buffer = ct::Buffer::new(fonts, ct::Metrics::new(size, line));
    buffer.set_wrap(ct::Wrap::WordOrGlyph);
    buffer.set_size(Some(width.max(1.0)), None);
    let attrs = style.attrs(fonts.db());
    if runs.is_empty() {
        buffer.set_text(text, &attrs, ct::Shaping::Advanced, None);
    } else {
        buffer.set_rich_text(spans(fonts.db(), text, &attrs, line, runs, &[], [0.0; 4], 1.0), &attrs, ct::Shaping::Advanced, None);
    }
    buffer.shape_until_scroll(fonts, false);
    buffer.layout_runs().count().max(1) as f32 * line
}

/// A text cut where its attributes change: each character takes the text's,
/// then every run over it, then a mark's colour (`marks`, counted from 1).
/// A colour of its own is drawn into the glyphs, so it fades by `alpha`;
/// a size of its own keeps the text's `line` height.
#[allow(clippy::too_many_arguments)]
fn spans<'a>(
    db: &ct::fontdb::Database,
    text: &'a str,
    attrs: &ct::Attrs<'a>,
    line: f32,
    runs: &'a [TextRun],
    marks: &[f32],
    mark_color: [f32; 4],
    alpha: f32,
) -> Vec<(&'a str, ct::Attrs<'a>)> {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let colour = |c: [f32; 4]| ct::Color::rgba(byte(c[0]), byte(c[1]), byte(c[2]), byte(c[3] * alpha));
    let marked: std::collections::HashSet<usize> = marks.iter().map(|m| *m as usize).collect();
    let at = |i: usize| {
        let mut a = attrs.clone();
        let mut face = false;
        for r in runs.iter().filter(|r| r.from <= i && i <= r.to) {
            face |= r.weight.is_some() || r.italic.is_some() || r.font.is_some();
            if let Some(w) = r.weight {
                a = a.weight(ct::Weight(w));
            }
            if let Some(it) = r.italic {
                a = a.style(if it { ct::Style::Italic } else { ct::Style::Normal });
            }
            if let Some(f) = &r.font {
                a = a.family(ct::Family::Name(f));
            }
            if let Some(size) = r.size {
                a = a.metrics(ct::Metrics::new(size, line));
            }
            if let Some(c) = r.color {
                a = a.color(colour(c));
            }
            if r.strike {
                a = a.strikethrough();
            }
        }
        if marked.contains(&i) {
            a = a.color(colour(mark_color));
        }
        // The text's own attributes are settled already.
        if face { settled(db, a) } else { a }
    };
    let mut out: Vec<(&str, ct::Attrs)> = Vec::new();
    let (mut start, mut was): (usize, Option<ct::Attrs>) = (0, None);
    for (i, (byte_at, _)) in text.char_indices().enumerate() {
        let is = at(i + 1);
        match was.take() {
            Some(w) if w != is => {
                out.push((&text[start..byte_at], w));
                start = byte_at;
                was = Some(is);
            }
            Some(w) => was = Some(w),
            None => was = Some(is),
        }
    }
    if let Some(w) = was {
        out.push((&text[start..], w));
    }
    out
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
    let attrs = style.attrs(fonts.db());
    // A field is one line, never cut: it scrolls under its caret instead.
    if let Some(edit) = &n.edit {
        let shown = edit.display(n.secret).text;
        let text = if shown.is_empty() { n.placeholder.as_str() } else { shown.as_str() };
        buffer.set_wrap(ct::Wrap::None);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, ct::Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let buffer = Arc::new(buffer);
        n.buffer = Some(buffer.clone());
        return buffer;
    }
    // A limited width ends the line in an ellipsis: never wrapped, never
    // shrunk (Bioma's window title). A sentence that may wrap goes on to
    // the next line instead.
    if n.wrap && n.max_width.is_some() {
        buffer.set_wrap(ct::Wrap::WordOrGlyph);
        buffer.set_ellipsize(ct::Ellipsize::None);
    } else {
        buffer.set_wrap(ct::Wrap::None);
        let one = ct::EllipsizeHeightLimit::Lines(1);
        buffer.set_ellipsize(match (n.max_width, n.ellipsize) {
            (None, _) => ct::Ellipsize::None,
            (Some(_), 1) => ct::Ellipsize::Middle(one),
            (Some(_), 2) => ct::Ellipsize::Start(one),
            (Some(_), _) => ct::Ellipsize::End(one),
        });
    }
    buffer.set_size(n.max_width, None);
    let align = match n.text_align {
        1 => Some(ct::Align::Center),
        2 => Some(ct::Align::End),
        _ => None,
    };
    if n.marks.is_empty() && n.runs.is_empty() {
        buffer.set_text(&n.text, &attrs, ct::Shaping::Advanced, align);
    } else {
        let spans = spans(fonts.db(), &n.text, &attrs, metrics.line_height, &n.runs, &n.marks, n.mark_color, n.marks_alpha);
        buffer.set_rich_text(spans, &attrs, ct::Shaping::Advanced, align);
    }
    buffer.shape_until_scroll(fonts, false);
    let buffer = Arc::new(buffer);
    n.buffer = Some(buffer.clone());
    buffer
}

/// Where the caret stands before byte `at` of a one-line buffer, logical
/// units from the line's start.
fn caret_x(buffer: &ct::Buffer, at: usize) -> f32 {
    let Some(run) = buffer.layout_runs().next() else { return 0.0 };
    let mut end = 0.0f32;
    for g in run.glyphs.iter() {
        if at <= g.start {
            return g.x;
        }
        // Inside a cluster (a ligature): a share of its width.
        if at < g.end {
            return g.x + g.w * (at - g.start) as f32 / (g.end - g.start).max(1) as f32;
        }
        end = end.max(g.x + g.w);
    }
    end
}

/// The byte of a one-line buffer nearest to `x`, logical units from the
/// line's start.
fn byte_at(buffer: &ct::Buffer, x: f32) -> usize {
    let Some(run) = buffer.layout_runs().next() else { return 0 };
    for g in run.glyphs.iter() {
        if x < g.x + g.w / 2.0 {
            return g.start;
        }
    }
    run.glyphs.iter().map(|g| g.end).max().unwrap_or(0)
}

/// A plain rectangle: no rim, no gradient, no shape.
fn flat(origin: [f32; 2], size: [f32; 2], radius: f32, fill: [f32; 4], out: &Out) -> Rect {
    Rect {
        origin,
        size,
        radius,
        rim: 0.0,
        level: 1.0,
        cut: 0.0,
        fill,
        rim_top: [0.0; 4],
        rim_bottom: [0.0; 4],
        wave: [0.0; 4],
        arc: Rect::WHOLE,
        fill_to: [0.0; 4],
        stops: Rect::NO_STOPS,
        shapes: Rect::NO_SHAPES,
        clip: out.clip,
        path: [0.0; 4],
        path_end: [0.0; 4],
        bud: [0.0; 4],
        bud_shape: [0.0; 4],
        fade: out.fade_of(),
        transform: out.transform,
    }
}

/// A field: what is selected, the text (or its placeholder) scrolled to
/// keep the caret in sight, what an input method composes underlined, and
/// the caret while it has the keys. A still hairline: a caret blinking at
/// a fixed rate is motion with nothing behind it.
fn draw_field(fonts: &mut ct::FontSystem, n: &mut Node, origin: [f32; 2], layout: &taffy::Layout, scale: f32, alpha: f32, out: &mut Out) {
    let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
    let buffer = shape(fonts, n);
    let Some(display) = n.edit.as_ref().map(|e| e.display(n.secret)) else { return };
    let empty = display.text.is_empty();
    let left0 = origin[0] + layout.padding.left;
    let room = (layout.size.width - layout.padding.left - layout.padding.right).max(1.0);
    let height = layout.size.height - layout.padding.top - layout.padding.bottom;
    let line = (n.font_size * 1.2).ceil();
    // A line high, centred in what the field is given.
    let top = origin[1] + layout.padding.top + ((height - line) / 2.0).max(0.0);
    let x_at = |i: usize| if empty { 0.0 } else { caret_x(&buffer, i) };
    let caret = x_at(display.caret);
    let width = if empty { 0.0 } else { buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max) };
    let Some(edit) = n.edit.as_mut() else { return };
    // The line scrolls under the caret, and never leaves room empty past
    // its end while some of it is out of sight before.
    let mut scroll = edit.scroll;
    if caret - scroll > room - 1.0 {
        scroll = caret - room + 1.0;
    }
    if caret < scroll {
        scroll = caret;
    }
    if width - scroll < room - 1.0 {
        scroll = width - room + 1.0;
    }
    edit.scroll = scroll.max(0.0);
    let left = left0 - edit.scroll;
    let saved = out.clip;
    out.clip = [
        saved[0].max(left0 * scale),
        saved[1].max(origin[1] * scale),
        saved[2].min((left0 + room) * scale),
        saved[3].min((origin[1] + layout.size.height) * scale),
    ];
    let px = |v: f32| (v * scale).round();
    if let Some((a, b)) = display.selection {
        let (x0, x1) = (x_at(a), x_at(b));
        let colour = if n.selection_color[3] > 0.0 { n.selection_color } else { [n.color[0], n.color[1], n.color[2], n.color[3] * 0.25] };
        let rect = flat([px(left + x0), px(top)], [px(left + x1) - px(left + x0), px(line)], 2.0 * scale, fade(colour), out);
        out.list.push_rect(rect);
    }
    let colour = match (empty, n.placeholder_color[3] > 0.0) {
        (true, true) => n.placeholder_color,
        (true, false) => [n.color[0], n.color[1], n.color[2], n.color[3] * 0.45],
        _ => n.color,
    };
    if !empty || !n.placeholder.is_empty() {
        let shown = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        out.list.push_text(Text { buffer: buffer.clone(), origin: [px(left0 - if empty { 0.0 } else { edit.scroll }), px(top)], scale, color: fade(colour), clip: out.clip }, [shown * scale, line * scale]);
    }
    let hair = scale.round().max(1.0);
    if let Some((a, b)) = display.composing {
        let (x0, x1) = (x_at(a), x_at(b));
        let rect = flat([px(left + x0), px(top + line) - hair], [px(left + x1) - px(left + x0), hair], 0.0, fade(n.color), out);
        out.list.push_rect(rect);
    }
    n.caret_rect = Some([left + caret, top, 1.0, line]);
    if n.focused {
        let colour = if n.caret_color[3] > 0.0 { n.caret_color } else { n.color };
        let rect = flat([px(left + caret), px(top)], [hair, px(line)], 0.0, fade(colour), out);
        out.list.push_rect(rect);
    }
    out.clip = saved;
}

/// The part of an image on the surface, in its decoded pixels (x, y, w,
/// h), when it reaches well past the surface and is drawn flat, its decode
/// in proportion to its box; `None` to keep it whole.
fn visible_part(at: [f32; 2], size: [u32; 2], decoded: [u32; 2], viewport: [f32; 2], flat: bool) -> Option<[u32; 4]> {
    let (w, h) = (size[0] as f32, size[1] as f32);
    if !flat || w <= 0.0 || h <= 0.0 || viewport[0] <= 0.0 || viewport[1] <= 0.0 {
        return None;
    }
    let (sx, sy) = (decoded[0] as f32 / w, decoded[1] as f32 / h);
    if (sx - sy).abs() > 0.01 {
        return None;
    }
    let x0 = at[0].max(0.0);
    let y0 = at[1].max(0.0);
    let x1 = (at[0] + w).min(viewport[0]);
    let y1 = (at[1] + h).min(viewport[1]);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Worth it only when a good part is off the surface.
    if (x1 - x0) * (y1 - y0) > 0.9 * w * h {
        return None;
    }
    let cx = (((x0 - at[0]) * sx).floor() as u32).min(decoded[0] - 1);
    let cy = (((y0 - at[1]) * sy).floor() as u32).min(decoded[1] - 1);
    let cw = ((((x1 - at[0]) * sx).ceil() as u32).min(decoded[0]) - cx).max(1);
    let ch = ((((y1 - at[1]) * sy).ceil() as u32).min(decoded[1]) - cy).max(1);
    Some([cx, cy, cw, ch])
}

struct Out<'a> {
    frame: u64,
    /// What is under the shader node being drawn, kept for it: the node,
    /// the drawn group's key, and where its texture lies from the node's
    /// corner, logical units.
    under: Option<(NodeId, u64, [f32; 4])>,
    /// The pointer on the surface, logical units, and a button held.
    pointer: Option<(f32, f32, bool)>,
    /// The shapes a `combine_with` names, by id: where each was drawn and
    /// its radius, logical units.
    others: std::collections::HashMap<String, [f32; 5]>,
    list: &'a mut DrawList,
    /// The fade toward the foot of the nearest clip that fades, physical
    /// pixels: where nothing is left and how far above it it begins.
    fade: [f32; 2],
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
    distance: &'a mut distance::Fields,
    raised: bool,
    /// The images drawn this frame.
    drawn_images: Vec<NodeId>,
    /// The animated ones among them.
    animated: Vec<NodeId>,
    /// While a masked node is gathered: where its `mask` children draw.
    mask: Option<DrawList>,
    /// While a node with pass children is gathered: the node, and its
    /// passes as they are met, in the picture's place.
    passes: Option<(NodeId, Vec<genoma_render::Shader>)>,
    /// The emitters drawn so far, for their sub-emitters: their motion,
    /// their births and the births' generation.
    emitted: std::collections::HashMap<NodeId, Emitted>,
    /// The surface, physical pixels: an image reaching past it is decoded
    /// only for what of it is on it.
    viewport: [f32; 2],
}

/// An emitter as drawn, for its sub-emitters: its key, its motion, its
/// births and their generation.
type Emitted = (u64, genoma_render::EmitterParams, Arc<Vec<f32>>, u64);

impl Out<'_> {
    fn fade_of(&self) -> [f32; 4] {
        [self.fade[0], self.fade[1], 0.0, 0.0]
    }
}

/// The fill gradient's shape in the fourth slot of `fade`, which the GPU
/// reads as `kind × 1000 + degrees`: 1 linear at an angle, 2 radial, 3
/// conic. 0 leaves the gradient vertical (or across, with `fill_across`).
fn with_fill_shape(mut fade: [f32; 4], n: &Node) -> [f32; 4] {
    fade[3] = fill_shape_code(n.fill_shape, n.fill_angle);
    fade
}

fn fill_shape_code(shape: u8, angle: Option<f32>) -> f32 {
    if shape == 0 && angle.is_none() {
        return 0.0;
    }
    let degrees = angle.unwrap_or(if shape == 0 { 180.0 } else { 0.0 }).rem_euclid(360.0);
    (shape as f32 + 1.0) * 1000.0 + degrees
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

/// Where a node is drawn: its rectangle in logical units with every edge on
/// a whole physical pixel, and the exact one taffy laid out.
///
/// It is placed against its parent's drawn rectangle and keeps the relation
/// it has to the exact one: centred stays centred, flush stays flush, and
/// anything else is rounded where it falls. Rounding each edge on its own
/// does not keep a centre — an odd gap leaves the half pixel on one side,
/// always the same one — so a centred node takes the parity of the room
/// around it, a pixel larger or smaller than it asked for.
fn place(tree: &TaffyTree<Node>, node: NodeId, parent: [f32; 4], parent_exact: [f32; 4], scale: f32) -> ([f32; 4], [f32; 4]) {
    let layout = *tree.layout(node).expect("laid out");
    let mut size = [layout.size.width, layout.size.height];
    // A pass without a width or height of its own covers its parent along
    // that axis, which taffy cannot say: an absolute box's insets are
    // measured from the root.
    if tree.get_node_context(node).is_some_and(|n| n.pass)
        && let Ok(style) = tree.style(node)
    {
        if style.size.width.is_auto() {
            size[0] = parent_exact[2];
        }
        if style.size.height.is_auto() {
            size[1] = parent_exact[3];
        }
    }
    // An overlay is centred in its parent, whatever taffy reports for an
    // out-of-flow box.
    let overlay = tree.style(node).is_ok_and(|s| s.position == taffy::Position::Absolute);
    let attach = tree.get_node_context(node).map_or(0, |n| n.attach);
    let p = parent_exact;
    let at = if overlay {
        match attach {
            1 => [p[0], p[1] + p[3]],
            2 => [p[0], p[1] - size[1]],
            3 => [p[0] + (p[2] - size[0]) / 2.0, p[1] + p[3]],
            4 => [p[0] + (p[2] - size[0]) / 2.0, p[1] - size[1]],
            5 => [p[0] - size[0], p[1] + (p[3] - size[1]) / 2.0],
            6 => [p[0] + p[2], p[1] + (p[3] - size[1]) / 2.0],
            7 => [p[0], p[1]],
            _ => [p[0] + (p[2] - size[0]) / 2.0, p[1] + (p[3] - size[1]) / 2.0],
        }
    } else {
        [p[0] + layout.location.x, p[1] + layout.location.y]
    };
    // An offset moves the node and everything in it: a panel set below its
    // thread carries its content with it. It counts in the relation, so a
    // shape set in the middle by hand is centred as surely as one laid out
    // there.
    let offset = tree.get_node_context(node).map_or([0.0; 2], |n| n.offset);
    let at = [at[0] + offset[0], at[1] + offset[1]];
    let x = on_pixels(parent[0], parent[2], at[0] - p[0], p[2], size[0], scale);
    let y = on_pixels(parent[1], parent[3], at[1] - p[1], p[3], size[1], scale);
    ([x.0, y.0, x.1, y.1], [at[0], at[1], size[0], size[1]])
}

/// One axis of `place`: a span of `len` at `at` within an exact parent span
/// of `room`, drawn inside the parent's span that starts at `start` and is
/// `drawn` long, all in logical units. Returns the drawn start and length.
fn on_pixels(start: f32, drawn: f32, at: f32, room: f32, len: f32, scale: f32) -> (f32, f32) {
    const NEAR: f32 = 0.01;
    let s = (start * scale).round();
    let e = ((start + drawn) * scale).round();
    let mut n = (len * scale).round().max(0.0);
    let from = if len > 0.0 && room > 0.0 && (at - (room - len) / 2.0).abs() < NEAR {
        if (e - s - n).rem_euclid(2.0) != 0.0 {
            n += if len * scale > n || n < 1.0 { 1.0 } else { -1.0 };
        }
        s + (e - s - n) / 2.0
    } else if at.abs() < NEAR {
        s
    } else if (at + len - room).abs() < NEAR {
        e - n
    } else if (at - room).abs() < NEAR {
        e
    } else if (at + len).abs() < NEAR {
        s - n
    } else {
        ((start + at) * scale).round()
    };
    (from / scale, n / scale)
}

/// A raised node and everything in it go to the raised layer, drawn after
/// the whole base layer; a raised node inside a raised one is already there.
#[allow(clippy::too_many_arguments)]
fn collect(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    parent_exact: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    // A pass of the node being gathered: kept for its picture, not drawn
    // here. What else it draws (its WGSL's mistake) is shown as it is.
    if let Some((owner, _)) = &out.passes
        && tree.parent(node) == Some(*owner)
        && tree.get_node_context(node).is_some_and(|n| n.pass && n.kind == Kind::Shader)
    {
        let mut own = DrawList::default();
        std::mem::swap(out.list, &mut own);
        collect_node(tree, fonts, node, parent, parent_exact, 1.0, scale, out);
        std::mem::swap(out.list, &mut own);
        match own.shaders.first() {
            Some(shader) => out.passes.as_mut().expect("checked above").1.push(shader.clone()),
            None if !own.is_empty() => out.list.append(own),
            None => {}
        }
        return;
    }
    let raise = !out.raised && tree.get_node_context(node).is_some_and(|n| n.raised && n.present);
    if raise {
        std::mem::swap(out.list, &mut out.upper);
        out.raised = true;
    }
    // A shader that reads what is under it: drawn as a group, unblurred,
    // for its g_backdrop.
    if tree.get_node_context(node).is_some_and(|n| n.present && n.reads_backdrop && n.kind == Kind::Shader) {
        let ([x, y, w, h], _) = place(tree, node, parent, parent_exact, scale);
        out.under = under_for_shader(node, [x, y, w, h], scale, out);
    }
    // Frosted: what is drawn so far under its shape, blurred, laid first.
    if let Some(n) = tree.get_node_context(node).filter(|n| n.present && n.backdrop_blur > 0.0) {
        let ([x, y, w, h], _) = place(tree, node, parent, parent_exact, scale);
        let radius = if n.kind == Kind::Capsule { n.radius.unwrap_or(h / 2.0).min(w.min(h) / 2.0) } else { n.radius.unwrap_or(0.0) };
        backdrop([x * scale, y * scale, w * scale, h * scale], radius * scale, n.backdrop_blur * scale, out);
    }
    let present = tree.get_node_context(node).is_some_and(|n| n.present);
    let is_mask = present && out.mask.is_some() && tree.get_node_context(node).is_some_and(|n| n.mask);
    let blend = if present && !is_mask { tree.get_node_context(node).map_or(0, |n| n.blend) } else { 0 };
    if blend != 0 {
        // Blended: the node and its subtree (masked, blurred) drawn into a
        // list of their own, laid as one image meeting what is under it.
        let mut own = DrawList::default();
        std::mem::swap(out.list, &mut own);
        masked_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out, false, true);
        std::mem::swap(out.list, &mut own);
        blended(own, blend, out);
    } else {
        masked_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out, is_mask, present);
    }
    if raise {
        std::mem::swap(out.list, &mut out.upper);
        out.raised = false;
    }
}

/// A node as its mask children leave it: drawn into its parent's mask when
/// it is one (`is_mask`), or masked by its own mask children, or as it is.
#[allow(clippy::too_many_arguments)]
fn masked_or_not(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    parent_exact: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
    is_mask: bool,
    present: bool,
) {
    let masked_here = present
        && tree.children(node).unwrap_or_default().iter().any(|&c| tree.get_node_context(c).is_some_and(|n| n.present && n.mask));
    if is_mask {
        // A mask: drawn into the masked node's mask, not shown itself.
        let mut mask = out.mask.take().unwrap_or_default();
        std::mem::swap(out.list, &mut mask);
        blurred_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out);
        std::mem::swap(out.list, &mut mask);
        out.mask = Some(mask);
    } else if masked_here {
        // Masked: the node and its other children drawn into a list of
        // their own, its mask children into another, and the first shown
        // where the second is opaque.
        let mut own = DrawList::default();
        std::mem::swap(out.list, &mut own);
        let saved = out.mask.replace(DrawList::default());
        blurred_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out);
        let mask = std::mem::replace(&mut out.mask, saved).unwrap_or_default();
        std::mem::swap(out.list, &mut own);
        masked(own, mask, out);
    } else {
        blurred_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out);
    }
}

/// `own` laid as one image meeting what is under it as `blend` asks
/// (`genoma_render::Image::blend`); for overlay, what `out` holds under it
/// so far is drawn into a second texture over the same rectangle.
fn blended(mut own: DrawList, blend: u8, out: &mut Out) {
    let Some([x0, y0, x1, y1]) = own.paint_bounds() else { return };
    let bounds = [x0.max(0.0).floor(), y0.max(0.0).floor(), x1.min(out.viewport[0]).ceil(), y1.min(out.viewport[1]).ceil()];
    if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
        return;
    }
    out.list.groups.extend(std::mem::take(&mut own.groups));
    let size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    let key = |list: &DrawList, what: u8| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (list.fingerprint(), bounds.map(f32::to_bits), what).hash(&mut h);
        h.finish() | (1 << 63)
    };
    let own_key = key(&own, 3 + blend);
    // Overlaid on nothing, it is itself.
    let (blend, under) = if blend == genoma_render::Image::OVERLAY {
        let under = out.list.within([bounds[0] as u32, bounds[1] as u32, size[0] as u32, size[1] as u32]);
        if under.is_empty() {
            (0, None)
        } else {
            let under_key = key(&under, 20);
            out.list.groups.push(genoma_render::Group { passes: Vec::new(), key: under_key, list: under, bounds, sigma: 0.0 });
            (blend, Some(under_key))
        }
    } else {
        (blend, None)
    };
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key: own_key, list: own, bounds, sigma: 0.0 });
    out.list.push_image(genoma_render::Image {
        key: own_key,
        pixels: Arc::new(Vec::new()),
        width: size[0] as u32,
        height: size[1] as u32,
        origin: [bounds[0], bounds[1]],
        size,
        uv: [0.0, 0.0, 1.0, 1.0],
        radius: 0.0,
        alpha: 1.0,
        clip: out.clip,
        fade: out.fade_of(),
        transform: Rect::IDENTITY,
        filter: genoma_render::Image::NO_FILTER,
        mask: under,
        blend,
    });
}

/// What `out` holds under a shader node at `rect` (logical units), and a
/// margin round it a bend can reach, as an unblurred group: the key and
/// where the texture lies from the node's corner.
fn under_for_shader(node: NodeId, rect: [f32; 4], scale: f32, out: &mut Out) -> Option<(NodeId, u64, [f32; 4])> {
    // How far beyond its edges a shader may look, logical units.
    const REACH: f32 = 24.0;
    let area = [
        ((rect[0] - REACH) * scale).max(0.0).floor(),
        ((rect[1] - REACH) * scale).max(0.0).floor(),
        ((rect[0] + rect[2] + REACH) * scale).min(out.viewport[0]).ceil(),
        ((rect[1] + rect[3] + REACH) * scale).min(out.viewport[1]).ceil(),
    ];
    let size = [area[2] - area[0], area[3] - area[1]];
    if size[0] < 1.0 || size[1] < 1.0 {
        return None;
    }
    let under = out.list.within([area[0] as u32, area[1] as u32, size[0] as u32, size[1] as u32]);
    if under.is_empty() {
        return None;
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("under", under.fingerprint(), area.map(f32::to_bits)).hash(&mut h);
        h.finish() | (1 << 63)
    };
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key, list: under, bounds: area, sigma: 0.0 });
    Some((node, key, [area[0] / scale - rect[0], area[1] / scale - rect[1], size[0] / scale, size[1] / scale]))
}

/// What `out` holds under `rect` (x, y, width, height, physical pixels),
/// blurred over `sigma` and laid inside the rectangle, its corners rounded
/// by `radius`: frosted glass. What is under is taken within three spreads
/// of it, so only a change there blurs again.
fn backdrop(rect: [f32; 4], radius: f32, sigma: f32, out: &mut Out) {
    let margin = (sigma * 3.0).ceil();
    let area = [
        (rect[0] - margin).max(0.0).floor(),
        (rect[1] - margin).max(0.0).floor(),
        (rect[0] + rect[2] + margin).min(out.viewport[0]).ceil(),
        (rect[1] + rect[3] + margin).min(out.viewport[1]).ceil(),
    ];
    let size = [area[2] - area[0], area[3] - area[1]];
    if size[0] < 1.0 || size[1] < 1.0 || rect[2] <= 0.0 || rect[3] <= 0.0 {
        return;
    }
    let under = out.list.within([area[0] as u32, area[1] as u32, size[0] as u32, size[1] as u32]);
    if under.is_empty() {
        return;
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("backdrop", under.fingerprint(), sigma.to_bits(), area.map(f32::to_bits)).hash(&mut h);
        h.finish() | (1 << 63)
    };
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key, list: under, bounds: area, sigma });
    out.list.push_image(genoma_render::Image {
        key,
        pixels: Arc::new(Vec::new()),
        width: size[0] as u32,
        height: size[1] as u32,
        origin: [rect[0], rect[1]],
        size: [rect[2], rect[3]],
        uv: [
            (rect[0] - area[0]) / size[0],
            (rect[1] - area[1]) / size[1],
            (rect[0] + rect[2] - area[0]) / size[0],
            (rect[1] + rect[3] - area[1]) / size[1],
        ],
        radius,
        alpha: 1.0,
        clip: out.clip,
        fade: out.fade_of(),
        transform: Rect::IDENTITY,
        filter: genoma_render::Image::NO_FILTER,
        mask: None,
        blend: 0,
    });
}

/// A node and its subtree, blurred as a group when it asks to be.
#[allow(clippy::too_many_arguments)]
fn blurred_or_not(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    parent_exact: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    // Blurred: the node and its subtree drawn into a list of their own,
    // shown as the image the renderer makes of it.
    let radius = tree.get_node_context(node).filter(|n| n.present).map_or(0.0, |n| n.blur_radius);
    if radius > 0.0 {
        let mut own = DrawList::default();
        std::mem::swap(out.list, &mut own);
        passed_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out);
        std::mem::swap(out.list, &mut own);
        blurred(own, radius * scale, out);
    } else {
        passed_or_not(tree, fonts, node, parent, parent_exact, alpha, scale, out);
    }
}

/// A node and its subtree, drawn into a picture when it has passes to go
/// through or asks to be kept (`cache`), or as they are.
#[allow(clippy::too_many_arguments)]
fn passed_or_not(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    parent_exact: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    let grouped = tree.get_node_context(node).filter(|n| n.present).is_some_and(|n| {
        n.cache
            || tree
                .children(node)
                .unwrap_or_default()
                .iter()
                .any(|&c| tree.get_node_context(c).is_some_and(|c| c.present && c.pass && c.kind == Kind::Shader))
    });
    if !grouped {
        collect_node(tree, fonts, node, parent, parent_exact, alpha, scale, out);
        return;
    }
    let mut own = DrawList::default();
    std::mem::swap(out.list, &mut own);
    let saved = out.passes.replace((node, Vec::new()));
    collect_node(tree, fonts, node, parent, parent_exact, alpha, scale, out);
    let passes = std::mem::replace(&mut out.passes, saved).map(|(_, p)| p).unwrap_or_default();
    std::mem::swap(out.list, &mut own);
    passed(own, passes, scale, out);
}

/// `own`, a subtree's list, drawn into a picture and put through `passes`
/// (shader nodes as drawn on the surface) in turn. The picture covers what
/// `own` paints and every pass's box, within the surface; it is keyed by
/// what it holds and by each pass's source, numbers, clock and pointer, so
/// one at rest is drawn once and kept.
fn passed(mut own: DrawList, mut passes: Vec<genoma_render::Shader>, scale: f32, out: &mut Out) {
    let mut area = own.paint_bounds();
    for p in &passes {
        let b = [p.origin[0], p.origin[1], p.origin[0] + p.size[0] * p.scale, p.origin[1] + p.size[1] * p.scale];
        area = Some(match area {
            Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            None => b,
        });
    }
    let Some([x0, y0, x1, y1]) = area else { return };
    let bounds = [x0.max(0.0).floor(), y0.max(0.0).floor(), x1.min(out.viewport[0]).ceil(), y1.min(out.viewport[1]).ceil()];
    if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
        return;
    }
    // Groups inside it are drawn first, which its picture then shows.
    out.list.groups.extend(std::mem::take(&mut own.groups));
    let size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    for p in &mut passes {
        let corner = p.origin;
        // In the picture, from its corner; the depth, clip and fade in
        // force are the picture's, applied as it is shown.
        p.origin = [corner[0] - bounds[0], corner[1] - bounds[1]];
        p.clip = [0.0, 0.0, size[0], size[1]];
        p.fade = [0.0; 4];
        p.transform = Rect::IDENTITY;
        p.alpha = 1.0;
        p.backdrop = Some((0, [(bounds[0] - corner[0]) / scale, (bounds[1] - corner[1]) / scale, size[0] / scale, size[1] / scale]));
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("passed", own.fingerprint(), bounds.map(f32::to_bits)).hash(&mut h);
        for p in &passes {
            p.key.hash(&mut h);
            for v in p.origin.iter().chain(&p.size).chain(&p.values) {
                v.to_bits().hash(&mut h);
            }
            // The pointer only for a pass that reads it: elsewhere it moves
            // over the picture without drawing it again.
            if p.source.contains("shader.pointer") {
                p.pointer.map(f32::to_bits).hash(&mut h);
            }
            (p.scale.to_bits(), p.time.to_bits()).hash(&mut h);
        }
        h.finish() | (1 << 63)
    };
    out.list.groups.push(genoma_render::Group { passes, key, list: own, bounds, sigma: 0.0 });
    out.list.push_image(genoma_render::Image {
        key,
        pixels: Arc::new(Vec::new()),
        width: size[0] as u32,
        height: size[1] as u32,
        origin: [bounds[0], bounds[1]],
        size,
        uv: [0.0, 0.0, 1.0, 1.0],
        radius: 0.0,
        alpha: 1.0,
        clip: out.clip,
        fade: out.fade_of(),
        transform: Rect::IDENTITY,
        filter: genoma_render::Image::NO_FILTER,
        mask: None,
        blend: 0,
    });
}

/// `own` shown only where `mask` is opaque: each drawn into a texture over
/// the same rectangle (what `own` paints, within the surface), the first
/// laid as an image whose alpha the second's multiplies. Kept by what each
/// draws, so a mask at rest is drawn once.
fn masked(mut own: DrawList, mut mask: DrawList, out: &mut Out) {
    let Some([x0, y0, x1, y1]) = own.paint_bounds() else { return };
    // A mask that draws nothing shows nothing.
    if mask.paint_bounds().is_none() {
        return;
    }
    let bounds = [x0.max(0.0).floor(), y0.max(0.0).floor(), x1.min(out.viewport[0]).ceil(), y1.min(out.viewport[1]).ceil()];
    if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
        return;
    }
    // Groups inside either are drawn first, which these then show.
    out.list.groups.extend(std::mem::take(&mut own.groups));
    out.list.groups.extend(std::mem::take(&mut mask.groups));
    let key = |list: &DrawList, what: u8| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (list.fingerprint(), bounds.map(f32::to_bits), what).hash(&mut h);
        h.finish() | (1 << 63)
    };
    let (content_key, mask_key) = (key(&own, 1), key(&mask, 2));
    let size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key: content_key, list: own, bounds, sigma: 0.0 });
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key: mask_key, list: mask, bounds, sigma: 0.0 });
    out.list.push_image(genoma_render::Image {
        key: content_key,
        pixels: Arc::new(Vec::new()),
        width: size[0] as u32,
        height: size[1] as u32,
        origin: [bounds[0], bounds[1]],
        size,
        uv: [0.0, 0.0, 1.0, 1.0],
        radius: 0.0,
        alpha: 1.0,
        clip: out.clip,
        fade: out.fade_of(),
        transform: Rect::IDENTITY,
        filter: genoma_render::Image::NO_FILTER,
        mask: Some(mask_key),
        blend: 0,
    });
}

/// `own`, a subtree's list, as a blurred group: its paint's bounds grown by
/// three spreads (where the Gaussian has faded out), within the surface,
/// and keyed by what it draws and how blurred, so one at rest is kept.
fn blurred(mut own: DrawList, sigma: f32, out: &mut Out) {
    let Some([x0, y0, x1, y1]) = own.paint_bounds() else { return };
    let margin = (sigma * 3.0).ceil();
    let bounds = [
        (x0 - margin).max(0.0).floor(),
        (y0 - margin).max(0.0).floor(),
        (x1 + margin).min(out.viewport[0]).ceil(),
        (y1 + margin).min(out.viewport[1]).ceil(),
    ];
    if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
        return;
    }
    // Inner blurred groups are drawn before this one, which shows them.
    let inner = std::mem::take(&mut own.groups);
    out.list.groups.extend(inner);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (own.fingerprint(), sigma.to_bits(), bounds.map(f32::to_bits)).hash(&mut h);
        h.finish() | (1 << 63)
    };
    let size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    out.list.groups.push(genoma_render::Group { passes: Vec::new(), key, list: own, bounds, sigma });
    out.list.push_image(genoma_render::Image {
        key,
        pixels: Arc::new(Vec::new()),
        width: size[0] as u32,
        height: size[1] as u32,
        origin: [bounds[0], bounds[1]],
        size,
        uv: [0.0, 0.0, 1.0, 1.0],
        radius: 0.0,
        alpha: 1.0,
        clip: out.clip,
        fade: out.fade_of(),
        transform: Rect::IDENTITY,
        filter: genoma_render::Image::NO_FILTER,
        mask: None,
        blend: 0,
    });
}

#[allow(clippy::too_many_arguments)]
fn collect_node(
    tree: &mut TaffyTree<Node>,
    fonts: &mut ct::FontSystem,
    node: NodeId,
    parent: [f32; 4],
    parent_exact: [f32; 4],
    alpha: f32,
    scale: f32,
    out: &mut Out,
) {
    let (placed, exact) = place(tree, node, parent, parent_exact, scale);
    let mut layout = *tree.layout(node).expect("laid out");
    layout.size.width = placed[2];
    layout.size.height = placed[3];
    let mut origin = [placed[0], placed[1]];
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
        n.extent = own;
        n.laid_frame = out.frame;
    }
    // A sub-emitter reads its parent's particles.
    let up = tree.parent(node);
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
    n.drawn_frame = out.frame;
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
            // The shapes it names, in order: a capsule first in the bud's
            // place, the rest (four at most) beside it, met the same way. A
            // text or a path is its outline's distance field.
            enum Named {
                Capsule([f32; 5]),
                Outline(distance::Placed),
            }
            let named: Vec<Named> = n
                .combine_with
                .as_deref()
                .map(|ids| {
                    ids.split_whitespace()
                        .filter_map(|id| match out.distance.named(id) {
                            Some(p) => Some(Named::Outline(p)),
                            None => out.others.get(id).copied().map(Named::Capsule),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let other = match named.first() {
                Some(Named::Capsule(c)) => Some(*c),
                _ => None,
            };
            let mut shapes = Rect::NO_SHAPES;
            let rest = &named[usize::from(other.is_some())..];
            for (i, shape) in rest.iter().take(4).enumerate() {
                match shape {
                    Named::Capsule([px, py, pw, ph, pr]) => {
                        shapes[i * 2] = [
                            ((px + pw / 2.0) - (x + w / 2.0)) * scale,
                            ((py + ph / 2.0) - (y + h / 2.0)) * scale,
                            pw / 2.0 * scale,
                            ph / 2.0 * scale,
                        ];
                        shapes[i * 2 + 1] = [pr * scale, 1.0, 0.0, 0.0];
                    }
                    Named::Outline(p) => {
                        let (fw, fh) = (p.field.width as f32, p.field.height as f32);
                        shapes[i * 2] = [
                            p.corner[0] + fw / 2.0 - (x + w / 2.0) * scale,
                            p.corner[1] + fh / 2.0 - (y + h / 2.0) * scale,
                            fw / 2.0,
                            fh / 2.0,
                        ];
                        let place = out.list.push_field(p.field.clone());
                        shapes[i * 2 + 1] = [p.spread, 2.0, place as f32, 0.0];
                    }
                }
            }
            let operation = if n.combine == 0 { 3.0 } else { n.combine as f32 };
            let (bud, bud_shape) = match (other, n.bud > 0.0, out.parent_shape) {
                // Only outlines: nothing in the bud's place (a negative
                // radius), the operation and its softness all the same.
                (None, _, _) if !named.is_empty() => ([0.0; 4], [-1.0, (n.combine_soft * scale).max(0.5), 0.0, operation]),
                // Combined with a named shape: its box as a bud's parent's,
                // and what to do with it. A sharp edge is half a pixel soft.
                (Some([px, py, pw, ph, pr]), _, _) => (
                    [
                        ((px + pw / 2.0) - (x + w / 2.0)) * scale,
                        ((py + ph / 2.0) - (y + h / 2.0)) * scale,
                        pw / 2.0 * scale,
                        ph / 2.0 * scale,
                    ],
                    // Joined whole (3), unlike a bud, whose parent's inside
                    // is the parent's to draw.
                    [pr * scale, (n.combine_soft * scale).max(0.5), 0.0, if n.combine == 0 { 3.0 } else { n.combine as f32 }],
                ),
                (None, true, Some([px, py, pw, ph, pr])) => (
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
                out.list.push_rect(Rect {
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
                    stops: Rect::NO_STOPS,
                    shapes: Rect::NO_SHAPES,
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud,
                    bud_shape,
                    fade: out.fade_of(),
                    transform: out.transform,
                });
            }
            // A shadow or a glow outside the shape, under it.
            if !nothing && n.halo <= 0.0 && n.shadow[3] > 0.0 {
                let blur = n.shadow_blur.max(0.0) * scale;
                let spread = n.shadow_spread * scale;
                let off = [n.shadow_x * scale, n.shadow_y * scale];
                let grow = spread + blur + 1.0;
                out.list.push_rect(Rect {
                    origin: [x * scale + off[0] - grow, y * scale + off[1] - grow],
                    size: [size[0] + grow * 2.0, size[1] + grow * 2.0],
                    radius,
                    rim: blur,
                    level: 1.0,
                    cut: 5.0,
                    fill: fade(n.shadow),
                    rim_top: [0.0; 4],
                    rim_bottom: [0.0; 4],
                    wave: [off[0], off[1], spread, 0.0],
                    arc: Rect::WHOLE,
                    fill_to: [0.0; 4],
                    stops: Rect::NO_STOPS,
                    shapes: Rect::NO_SHAPES,
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud: [0.0; 4],
                    bud_shape: [0.0; 4],
                    fade: out.fade_of(),
                    transform: out.transform,
                });
            }
            // A halo: the shape's own outline, blurred outward in its fill,
            // and nothing else of it.
            if !nothing && n.halo > 0.0 {
                // Out to its spread (a quarter) and its blur past it.
                let reach = n.halo * scale;
                let extent = reach * 1.25;
                out.list.push_rect(Rect {
                    origin: [x * scale - extent, y * scale - extent],
                    size: [size[0] + extent * 2.0, size[1] + extent * 2.0],
                    radius,
                    rim: reach,
                    level: 1.0,
                    cut: 3.0,
                    fill: fade(n.fill),
                    rim_top: [0.0; 4],
                    rim_bottom: [0.0; 4],
                    wave: [0.0; 4],
                    arc: Rect::WHOLE,
                    fill_to: [0.0; 4],
                    stops: Rect::NO_STOPS,
                    shapes: Rect::NO_SHAPES,
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud: [0.0; 4],
                    bud_shape: [0.0; 4],
                    fade: out.fade_of(),
                    transform: out.transform,
                });
            } else if !nothing {
                out.list.push_rect(Rect {
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
                    stops: Rect::stops(&n.fill_stops.iter().map(|(at, c)| (*at, fade(*c))).collect::<Vec<_>>()),
                    shapes,
                    clip: out.clip,
                    path: [0.0; 4],
                    path_end: [0.0; 4],
                    bud,
                    bud_shape,
                    fade: with_fill_shape(out.fade_of(), n),
                    transform: out.transform,
                });
                // An inner shadow over the fill, under what the capsule holds.
                if n.inner_shadow[3] > 0.0 {
                    out.list.push_rect(Rect {
                        origin: [x * scale, y * scale],
                        size,
                        radius,
                        rim: n.inner[0] * scale,
                        level: 1.0,
                        cut: 4.0,
                        fill: fade(n.inner_shadow),
                        rim_top: [0.0; 4],
                        rim_bottom: [0.0; 4],
                        wave: [n.inner[1] * scale, n.inner[2] * scale, n.inner[3] * scale, 0.0],
                        arc: Rect::WHOLE,
                        fill_to: [0.0; 4],
                        stops: Rect::NO_STOPS,
                        shapes: Rect::NO_SHAPES,
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        fade: out.fade_of(),
                        transform: out.transform,
                    });
                }
                // An outline apart from the rim: a ring its width wide, its
                // offset outside the edge.
                if n.ring[3] > 0.0 && n.ring_size[0] > 0.0 {
                    let width = n.ring_size[0] * scale;
                    let grow = n.ring_size[1] * scale + width;
                    out.list.push_rect(Rect {
                        origin: [x * scale - grow, y * scale - grow],
                        size: [size[0] + grow * 2.0, size[1] + grow * 2.0],
                        radius: (radius + grow).max(0.0),
                        rim: width,
                        level: 1.0,
                        cut: 0.0,
                        fill: [0.0; 4],
                        rim_top: fade(n.ring),
                        rim_bottom: fade(n.ring),
                        wave: [0.0; 4],
                        arc: Rect::WHOLE,
                        fill_to: [0.0; 4],
                        stops: Rect::NO_STOPS,
                        shapes: Rect::NO_SHAPES,
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        fade: out.fade_of(),
                        transform: out.transform,
                    });
                }
            }
            // A band drawn only outside its cells: each child capsule's
            // shape is taken back out of it before the child is drawn.
            if hollow {
                for child in tree.children(node).unwrap_or_default() {
                    let Some(c) = tree.get_node_context(child) else { continue };
                    if c.kind != Kind::Capsule || !c.present {
                        continue;
                    }
                    let (l, _) = place(tree, child, own, exact, scale);
                    let Some(c) = tree.get_node_context(child) else { continue };
                    let cs = [l[2] * scale, l[3] * scale];
                    out.list.push_rect(Rect {
                        origin: [l[0] * scale, l[1] * scale],
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
                        stops: Rect::NO_STOPS,
                        shapes: Rect::NO_SHAPES,
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        fade: out.fade_of(),
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
            out.list.push_rect(Rect {
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
                stops: Rect::NO_STOPS,
                shapes: Rect::NO_SHAPES,
                clip: out.clip,
                path: [a[0], a[1], b[0], b[1]],
                path_end: [c[0], c[1], width, g],
                bud: [0.0; 4],
                bud_shape: [0.0; 4],
                fade: out.fade_of(),
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
                    out.list.push_rect(Rect {
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
                        stops: Rect::NO_STOPS,
                        shapes: Rect::NO_SHAPES,
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        fade: out.fade_of(),
                        transform: out.transform,
                    });
                }
            }
        }
        Kind::Emitter => {
            let e = &n.emit;
            let to = if e.target { [n.path[4], n.path[5]] } else { [0.0, 0.0] };
            let mut params = genoma_render::EmitterParams {
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
                color_end: e.color_end.map_or([0.0, 0.0, 0.0, -1.0], fade),
                life: [e.size_end.unwrap_or(e.size) * scale, e.trail, 0.0, e.bounce],
                parent: [[0.0; 4]; 4],
                sim: [0.0; 4],
                forces: [[0.0; 4]; genoma_render::particles::FORCES],
                walls: [[0.0; 4]; genoma_render::particles::WALLS * 2],
                look: [0.0, e.spin.to_radians(), 0.0, 0.0],
                transform: out.transform,
            };
            // Each particle drawn as a picture, tinted by its colour: an
            // icon drawn white, or a picture, at the largest it is drawn.
            let pixels = (e.size.max(e.size_end.unwrap_or(0.0)) * 1.3 * scale).ceil().clamp(1.0, 256.0) as u32;
            let sprite = if let Some(icon) = &n.icon {
                out.icons.raster(icon, [1.0; 4], None, pixels).map(|(key, pixels_)| (key, pixels_, pixels, pixels))
            } else if let Some(src) = &n.src {
                out.images.get_part(src, pixels, pixels, images::Fit::Contain, 0.0, 0.0, None, 0.0).map(|(key, data, _)| (key, data, pixels, pixels))
            } else {
                None
            };
            let sprite = sprite.map(|(key, pixels, width, height)| genoma_render::particles::Sprite { key, pixels, width, height });
            if sprite.is_some() {
                params.look[0] = 1.0;
            }
            // Pulled or bounced, its particles are stepped rather than worked
            // out from their age: ballistic ones only, a carried one keeps to
            // its path.
            if !e.target {
                let mut forces = 0;
                for id in e.attract.as_deref().unwrap_or("").split_whitespace().take(genoma_render::particles::FORCES) {
                    let at = if id == "pointer" {
                        out.pointer.map(|(x, y, _)| [x, y])
                    } else {
                        out.others.get(id).map(|[x, y, w, h, _]| [x + w / 2.0, y + h / 2.0])
                    };
                    if let Some([x, y]) = at {
                        params.forces[forces] = [x * scale, y * scale, e.attraction * scale, e.reach * scale];
                        forces += 1;
                    }
                }
                let mut walls = 0;
                for id in e.collide.as_deref().unwrap_or("").split_whitespace().take(genoma_render::particles::WALLS) {
                    if let Some(&[x, y, w, h, r]) = out.others.get(id) {
                        params.walls[walls * 2] = [x * scale, y * scale, (x + w) * scale, (y + h) * scale];
                        params.walls[walls * 2 + 1] = [r * scale, 0.0, 0.0, 0.0];
                        walls += 1;
                    }
                }
                // Named, even when none is found yet: the same particles
                // whether a node is there or not.
                if e.attract.is_some() || e.collide.is_some() {
                    params.sim = [1.0, 0.0, forces as f32, walls as f32];
                }
            }
            if e.sub {
                // Bursts where the parent's particles die: its births, and
                // its motion to find where.
                let parent = up.and_then(|u| out.emitted.get(&u));
                if let Some((uid, p, births, generation)) = parent.filter(|_| e.burst > 0.0) {
                    params.life[2] = e.burst;
                    params.parent = [p.points, p.motion, p.shape, p.bend];
                    // Ballistic from where each dies, whatever it was; read
                    // from the parent's steps when it is stepped.
                    params.shape[3] = 0.0;
                    params.sim[1] = p.sim[0];
                    let parent = Some(*uid);
                    out.list.push_emitter(genoma_render::Emitter { key: n.uid, generation: *generation, births: births.clone(), params, parent, sprite });
                }
            } else if !e.births.is_empty() {
                let births = Arc::new(e.births.clone());
                out.emitted.insert(node, (n.uid, params, births.clone(), e.generation));
                out.list.push_emitter(genoma_render::Emitter { key: n.uid, generation: e.generation, births, params, parent: None, sprite });
            }
        }
        Kind::Shader => {
            // Shaded by its `src`, or by its simulation's own `shade`.
            let shaded = n.shader.clone().or_else(|| n.compute.clone().filter(|_| n.compute_shades.is_ok()));
            let said = n.shader_error.clone().or_else(|| n.compute_error.clone()).or_else(|| match (&n.shader, &n.compute_shades) {
                (None, Err(e)) if n.compute.is_some() => Some(e.clone()),
                _ => None,
            });
            if let Some(said) = &said {
                shader_error(fonts, said, [origin[0] * scale, origin[1] * scale], [layout.size.width * scale, layout.size.height * scale], scale, out);
            }
            let sim = n.compute.as_ref().map(|(source, source_key)| {
                let grid = n.grid.unwrap_or([
                    (layout.size.width / 2.0).round().max(1.0) as u32,
                    (layout.size.height / 2.0).round().max(1.0) as u32,
                ]);
                genoma_render::sim::Sim {
                    key: n.uid,
                    source: source.clone(),
                    source_key: *source_key,
                    grid: if source.contains("fn step(") { grid } else { [0, 0] },
                    agents: if source.contains("fn agent(") { n.agents } else { 0 },
                    steps: n.steps_taken,
                    dt: 1.0 / n.steps_rate.max(1.0),
                    agent_size: n.agent_size * scale,
                    agent_color: fade(n.fill),
                }
            });
            if let Some((source, key)) = shaded {
                out.list.push_shader(genoma_render::Shader {
                    source,
                    key,
                    origin: [origin[0] * scale, origin[1] * scale],
                    size: [layout.size.width, layout.size.height],
                    scale,
                    time: n.time,
                    alpha,
                    clip: out.clip,
                    fade: out.fade_of(),
                    values: n.shown.clone(),
                    transform: out.transform,
                    pointer: match out.pointer {
                        Some((x, y, held)) => [x - origin[0], y - origin[1], 1.0, if held { 1.0 } else { 0.0 }],
                        None => [-1.0e6, -1.0e6, 0.0, 0.0],
                    },
                    backdrop: out.under.filter(|(n, ..)| *n == node).map(|(_, key, at)| (key, at)),
                    sim,
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
                    out.list.push_image(genoma_render::Image {
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
                        fade: out.fade_of(),
                        transform: out.transform,
                        filter: genoma_render::Image::NO_FILTER,
                        mask: None,
                        blend: 0,
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
            if let (Some(source), Some([st, sr, sb, sl]), true) = (n.src.clone(), n.slice, w > 0 && h > 0) {
                // A nine-slice: the picture at its own size (a pixel of it a
                // logical unit), drawn as nine parts of one texture.
                if let Some((iw, ih)) = out.images.size_of(&source) {
                    let (tw, th) = (((iw as f32) * scale).round().max(1.0) as u32, ((ih as f32) * scale).round().max(1.0) as u32);
                    if let Some((key, data, left)) = out.images.get_part(&source, tw, th, images::Fit::Fill, 0.0, 0.0, None, n.play_time) {
                        out.drawn_images.push(node);
                        n.frame_left = left.unwrap_or(f32::INFINITY);
                        if left.is_some() {
                            out.animated.push(node);
                        }
                        let (bw, bh) = (w as f32, h as f32);
                        // Edges kept within the picture and the box: a box
                        // smaller than two corners shrinks them alike.
                        let fit = |a: f32, b: f32, room: f32| {
                            let (a, b) = (a.max(0.0), b.max(0.0));
                            if a + b > room && a + b > 0.0 { (a * room / (a + b), b * room / (a + b)) } else { (a, b) }
                        };
                        let (sl, sr) = fit(sl.min(iw as f32), sr.min(iw as f32), iw as f32);
                        let (st, sb) = fit(st.min(ih as f32), sb.min(ih as f32), ih as f32);
                        let (dl, dr) = fit(sl * scale, sr * scale, bw);
                        let (dt, db) = fit(st * scale, sb * scale, bh);
                        let xs = [0.0, sl / iw as f32, 1.0 - sr / iw as f32, 1.0];
                        let ys = [0.0, st / ih as f32, 1.0 - sb / ih as f32, 1.0];
                        let xd = [0.0, dl, bw - dr, bw];
                        let yd = [0.0, dt, bh - db, bh];
                        let at = [(origin[0] * scale).round(), (origin[1] * scale).round()];
                        for row in 0..3 {
                            for col in 0..3 {
                                let (dw, dh) = (xd[col + 1] - xd[col], yd[row + 1] - yd[row]);
                                if dw <= 0.0 || dh <= 0.0 || xs[col + 1] <= xs[col] || ys[row + 1] <= ys[row] {
                                    continue;
                                }
                                out.list.push_image(genoma_render::Image {
                                    key,
                                    pixels: data.clone(),
                                    width: tw,
                                    height: th,
                                    origin: [at[0] + xd[col], at[1] + yd[row]],
                                    size: [dw, dh],
                                    uv: [xs[col], ys[row], xs[col + 1], ys[row + 1]],
                                    radius: 0.0,
                                    alpha,
                                    clip: out.clip,
                                    fade: out.fade_of(),
                                    transform: out.transform,
                                    filter: n.filter,
                                    mask: None,
                                    blend: 0,
                                });
                            }
                        }
                    }
                }
            } else if let (Some(source), true) = (n.src.clone(), w > 0 && h > 0) {
                let radius = n.radius.unwrap_or(0.0) * scale;
                // The blur's radius in the decoded picture's own pixels.
                let soften = n.soften * scale * dw as f32 / w.max(1) as f32;
                let at = [(origin[0] * scale).round(), (origin[1] * scale).round()];
                // Reaching past the surface, as a wallpaper spanned over two
                // monitors does on each: only the part on it is kept, in the
                // decoded picture's own pixels. A spanned 3440 × 2520 was
                // kept whole on both screens.
                let crop = visible_part(at, [w, h], [dw, dh], out.viewport, out.transform == Rect::IDENTITY);
                if let Some((key, data, left)) = out.images.get_part(&source, dw, dh, n.fit, 0.0, soften, crop, n.play_time) {
                    out.drawn_images.push(node);
                    n.frame_left = left.unwrap_or(f32::INFINITY);
                    if left.is_some() {
                        out.animated.push(node);
                    }
                    if let Some([cx, cy, cw, ch]) = crop {
                        let (sx, sy) = (w as f32 / dw as f32, h as f32 / dh as f32);
                        out.list.push_image(genoma_render::Image {
                            key,
                            pixels: data,
                            width: cw,
                            height: ch,
                            origin: [at[0] + cx as f32 * sx, at[1] + cy as f32 * sy],
                            size: [cw as f32 * sx, ch as f32 * sy],
                            uv: [0.0, 0.0, 1.0, 1.0],
                            radius,
                            alpha,
                            clip: out.clip,
                            fade: out.fade_of(),
                            transform: out.transform,
                            filter: n.filter,
                            mask: None,
                            blend: 0,
                        });
                    } else {
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
                    out.list.push_image(genoma_render::Image {
                        key,
                        pixels: data,
                        width: dw,
                        height: dh,
                        origin: at,
                        size: [w as f32, h as f32],
                        uv: [u0, v0, u1, v1],
                        radius,
                        alpha,
                        clip: out.clip,
                        fade: out.fade_of(),
                        transform: out.transform,
                        filter: n.filter,
                        mask: None,
                        blend: 0,
                    });
                    }
                }
            }
        }
        Kind::Text => {
            if !n.marks.is_empty() || n.runs.iter().any(|r| r.color.is_some()) {
                let a = (alpha * 32.0).round() / 32.0;
                if n.marks_alpha != a {
                    n.marks_alpha = a;
                    n.buffer = None;
                }
            }
            let buffer = shape(fonts, n);
            // On a whole pixel, as the box it is centred in is.
            let at = [
                ((origin[0] + layout.padding.left) * scale).round(),
                ((origin[1] + layout.padding.top) * scale).round(),
            ];
            // Lit by a gradient: rasterised with it, as tilted text is.
            let lit = n.color_bottom[3] > 0.0;
            let size = [
                (layout.size.width - layout.padding.left - layout.padding.right) * scale,
                (layout.size.height - layout.padding.top - layout.padding.bottom) * scale,
            ];
            // Named by a capsule's combine_with: its outline as a distance
            // field, from the raster tilted text uses (kept while it stays).
            if out.distance.wants(n.id.as_deref())
                && let Some(r) = out.glyphs.raster(fonts, n.uid, &buffer, at, scale, n.color, lit.then_some((n.color_bottom, size[1])))
            {
                let id = n.id.clone().unwrap_or_default();
                let corner = [at[0].floor() + r.offset[0] as f32, at[1].floor() + r.offset[1] as f32];
                out.distance.place(&id, r.key, &r.pixels, r.width, r.height, corner, OUTLINE_SPREAD * scale);
            }
            // The shadow first, under the glyphs.
            if n.shadow[3] > 0.0
                && let Some(r) = out.glyphs.shadow(fonts, n.uid, &buffer, scale, n.shadow, n.shadow_blur * scale)
            {
                out.list.push_image(genoma_render::Image {
                    key: r.key,
                    pixels: r.pixels,
                    width: r.width,
                    height: r.height,
                    origin: [at[0].round() + r.offset[0] as f32, (at[1] + n.shadow_y * scale).round() + r.offset[1] as f32],
                    size: [r.width as f32, r.height as f32],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    radius: 0.0,
                    alpha,
                    clip: out.clip,
                    fade: out.fade_of(),
                    transform: out.transform,
                    filter: genoma_render::Image::NO_FILTER,
                    mask: None,
                    blend: 0,
                });
            }
            // Reaching into a clip's fade: glyphon cannot fade it, an image can.
            let fading = out.fade[1] > 0.0 && at[1] + size[1] + 4.0 * scale > out.fade[0] - out.fade[1];
            if out.transform == Rect::IDENTITY && !lit && !fading {
                // Struck through: glyphon draws no decoration, so each line
                // goes over the text as a shape of its own.
                let mut strikes = Vec::new();
                if n.runs.iter().any(|r| r.strike) {
                    for run in buffer.layout_runs() {
                        for span in run.decorations.iter().filter(|s| s.data.text_decoration.strikethrough) {
                            let glyphs = &run.glyphs[span.glyph_range.clone()];
                            let x0 = glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
                            let x1 = glyphs.iter().map(|g| g.x + g.w).fold(f32::NEG_INFINITY, f32::max);
                            if x1 <= x0 {
                                continue;
                            }
                            let m = &span.data.strikethrough_metrics;
                            let thick = (m.thickness * span.font_size * scale).max(1.0).round();
                            let y = (run.line_y - m.offset * span.font_size) * scale;
                            let c = span.color_opt.map_or(n.color, |c| {
                                [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
                            });
                            strikes.push(([at[0] + x0 * scale, (at[1] + y).round()], [(x1 - x0) * scale, thick], c));
                        }
                    }
                }
                out.list.push_text(Text { buffer, origin: at, scale, color: fade(n.color), clip: out.clip }, size);
                for (origin, size, c) in strikes {
                    out.list.push_rect(Rect {
                        origin,
                        size,
                        radius: 0.0,
                        rim: 0.0,
                        level: 1.0,
                        cut: 0.0,
                        fill: fade(c),
                        rim_top: [0.0; 4],
                        rim_bottom: [0.0; 4],
                        wave: [0.0; 4],
                        arc: Rect::WHOLE,
                        fill_to: [0.0; 4],
                        stops: Rect::NO_STOPS,
                        shapes: Rect::NO_SHAPES,
                        clip: out.clip,
                        path: [0.0; 4],
                        path_end: [0.0; 4],
                        bud: [0.0; 4],
                        bud_shape: [0.0; 4],
                        fade: out.fade_of(),
                        transform: out.transform,
                    });
                }
            } else if let Some(r) = out.glyphs.raster(fonts, n.uid, &buffer, at, scale, n.color, lit.then_some((n.color_bottom, size[1]))) {
                // Tilted, lit or fading: as an image, through the subtree's
                // matrix.
                out.list.push_image(genoma_render::Image {
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
                    fade: out.fade_of(),
                    transform: out.transform,
                    filter: genoma_render::Image::NO_FILTER,
                    mask: None,
                    blend: 0,
                });
            }
        }
        Kind::Field => draw_field(fonts, n, origin, &layout, scale, alpha, out),
        Kind::Path => {
            let w = (layout.size.width * scale).round() as u32;
            let h = (layout.size.height * scale).round() as u32;
            if w > 0
                && h > 0
                && let Some(svg) = path_svg(n, [layout.size.width, layout.size.height], [w, h])
                && let Some((key, data)) = out.icons.vector(&svg, w, h)
            {
                if out.distance.wants(n.id.as_deref()) {
                    let id = n.id.clone().unwrap_or_default();
                    let corner = [(origin[0] * scale).round(), (origin[1] * scale).round()];
                    out.distance.place(&id, key, &data, w, h, corner, OUTLINE_SPREAD * scale);
                }
                out.list.push_image(genoma_render::Image {
                    key,
                    pixels: data,
                    width: w,
                    height: h,
                    origin: [(origin[0] * scale).round(), (origin[1] * scale).round()],
                    size: [w as f32, h as f32],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    radius: 0.0,
                    alpha,
                    clip: out.clip,
                    fade: out.fade_of(),
                    transform: out.transform,
                    filter: genoma_render::Image::NO_FILTER,
                    mask: None,
                    blend: 0,
                });
            }
        }
        Kind::Box => {}
    }
    // A clipping node narrows the clip for everything inside it, and a
    // scrolled one moves its children up.
    let (clip, scroll, fade) = tree.get_node_context(node).map_or((false, 0.0, 0.0), |n| (n.clip, n.scroll_y, n.fade_bottom));
    let saved = out.clip;
    let saved_fade = out.fade;
    let saved_parent = out.parent_shape;
    if let Some(shape) = out.shape_here.take() {
        out.parent_shape = Some(shape);
    }
    if clip {
        let c = [own[0] * scale, own[1] * scale, (own[0] + own[2]) * scale, (own[1] + own[3]) * scale];
        out.clip = [saved[0].max(c[0]), saved[1].max(c[1]), saved[2].min(c[2]), saved[3].min(c[3])];
        if fade > 0.0 {
            out.fade = [c[3], fade * scale];
        }
    }
    let scrolled = (scroll * scale).round() / scale;
    for child in tree.children(node).unwrap_or_default() {
        collect(
            tree,
            fonts,
            child,
            [own[0], own[1] - scrolled, own[2], own[3]],
            [exact[0], exact[1] - scroll, exact[2], exact[3]],
            alpha,
            scale,
            out,
        );
    }
    out.clip = saved;
    out.fade = saved_fade;
    out.parent_shape = saved_parent;
    out.transform = saved_transform;
}

/// How far right and down SVG path data reaches from its origin, logical
/// units: the size of a path given none.
fn path_reach(d: &str) -> [f32; 2] {
    let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><path d=\"{}\"/></svg>", xml_escape(d));
    let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()) else {
        log::warn!("path: cannot read {d:?}");
        return [0.0; 2];
    };
    let b = tree.root().abs_bounding_box();
    [b.right().max(0.0), b.bottom().max(0.0)]
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A path node as an SVG document `pixels` wide and high showing `size`
/// logical units: drawn by resvg at the exact physical size.
/// A shader source checked once by its hash: a reload sets every `src`
/// again, and checking the library each time would be paid at each save.
fn checked(key: u64, source: &str) -> Result<(), String> {
    thread_local! {
        static CHECKED: std::cell::RefCell<std::collections::HashMap<u64, Result<(), String>>> = Default::default();
    }
    CHECKED.with(|c| c.borrow_mut().entry(key).or_insert_with(|| genoma_render::shader::check(source)).clone())
}

/// What is wrong with a node's shader, over the node: a red plate with the
/// file and the error, wrapped to the node's width.
fn shader_error(fonts: &mut ct::FontSystem, said: &str, origin: [f32; 2], size: [f32; 2], scale: f32, out: &mut Out) {
    let width = size[0].max(160.0 * scale);
    let mut buffer = ct::Buffer::new(fonts, ct::Metrics::new(11.0, 14.0));
    buffer.set_size(Some(width / scale - 12.0), None);
    buffer.set_text(said, &ct::Attrs::new().family(ct::Family::Monospace), ct::Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let lines = buffer.layout_runs().count().max(1) as f32;
    let height = (lines * 14.0 + 10.0) * scale;
    out.list.push_rect(Rect {
        origin,
        size: [width, height.max(size[1])],
        radius: 4.0 * scale,
        rim: 0.0,
        level: 1.0,
        cut: 0.0,
        fill: [0.55, 0.08, 0.12, 0.92],
        rim_top: [0.0; 4],
        rim_bottom: [0.0; 4],
        wave: [0.0; 4],
        arc: Rect::WHOLE,
        fill_to: [0.0; 4],
        stops: Rect::NO_STOPS,
        shapes: Rect::NO_SHAPES,
        clip: out.clip,
        path: [0.0; 4],
        path_end: [0.0; 4],
        bud: [0.0; 4],
        bud_shape: [0.0; 4],
        fade: out.fade_of(),
        transform: out.transform,
    });
    out.list.push_text(
        Text { buffer: Arc::new(buffer), origin: [origin[0] + 6.0 * scale, origin[1] + 5.0 * scale], scale, color: [1.0, 0.95, 0.95, 1.0], clip: out.clip },
        [width, height],
    );
}

/// How far past a text's or a path's outline its distance field reaches,
/// logical units: the softest union it is met with blends within this.
const OUTLINE_SPREAD: f32 = 24.0;

fn path_svg(n: &Node, size: [f32; 2], pixels: [u32; 2]) -> Option<String> {
    let paint = |c: [f32; 4]| {
        let [r, g, b, _] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        (format!("rgb({r},{g},{b})"), c[3].clamp(0.0, 1.0))
    };
    let (fill, fill_alpha) = paint(n.fill);
    let fill = if fill_alpha > 0.0 {
        format!(" fill=\"{fill}\" fill-opacity=\"{fill_alpha}\" fill-rule=\"{}\"", if n.even_odd { "evenodd" } else { "nonzero" })
    } else {
        " fill=\"none\"".to_owned()
    };
    let stroke = match n.stroke {
        Some(width) if width > 0.0 && n.color[3] > 0.0 => {
            let (colour, alpha) = paint(n.color);
            let mut s = format!(
                " stroke=\"{colour}\" stroke-opacity=\"{alpha}\" stroke-width=\"{width}\" stroke-linejoin=\"{}\" stroke-linecap=\"{}\"",
                n.join, n.cap
            );
            if n.dash[0] > 0.0 {
                s.push_str(&format!(" stroke-dasharray=\"{} {}\"", n.dash[0], n.dash[1].max(0.0)));
            }
            s
        }
        _ => String::new(),
    };
    let shape = match (&n.outline, n.points.len() >= 4) {
        (Some(d), _) => format!("<path d=\"{}\"{fill}{stroke}/>", xml_escape(d)),
        (None, true) => {
            let points: Vec<String> = n.points.chunks_exact(2).map(|p| format!("{},{}", p[0], p[1])).collect();
            format!("<polygon points=\"{}\"{fill}{stroke}/>", points.join(" "))
        }
        _ => return None,
    };
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">{shape}</svg>",
        pixels[0], pixels[1], size[0], size[1]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_without_shape_or_angle_stays_vertical() {
        assert_eq!(fill_shape_code(0, None), 0.0);
        // A turned linear fill, the angle kept within a turn.
        assert_eq!(fill_shape_code(0, Some(90.0)), 1090.0);
        assert_eq!(fill_shape_code(0, Some(-90.0)), 1270.0);
        // Radial needs no angle; conic starts at twelve without one.
        assert_eq!(fill_shape_code(1, None), 2000.0);
        assert_eq!(fill_shape_code(2, Some(45.0)), 3045.0);
    }

    #[test]
    fn a_name_cut_in_the_middle_keeps_both_ends() {
        let mut fonts = ct::FontSystem::new();
        let mut n = Node::new(Kind::Text, Role::None);
        n.text = "a-very-long-file-name-that-will-never-fit-in-the-row.pdf".into();
        n.font_size = 14.0;
        n.max_width = Some(120.0);
        n.ellipsize = 1;
        let buffer = shape(&mut fonts, &mut n);
        let run = buffer.layout_runs().next().expect("one line");
        let glyphs: Vec<_> = run.glyphs.iter().collect();
        assert!(glyphs.iter().any(|g| g.start == 0), "the start is kept");
        assert!(glyphs.iter().any(|g| g.end == n.text.len()), "the extension is kept");
        assert!(run.line_w <= 121.0, "it fits: {}", run.line_w);
        assert!(glyphs.len() < n.text.len(), "something was cut");
    }

    /// A box of `kind` under `parent`, with these props.
    fn boxed(scene: &mut Scene, parent: NodeId, props: &[(Prop, Value)]) -> NodeId {
        let n = scene.create(Kind::Box, Role::None);
        scene.append(parent, n);
        for (prop, value) in props {
            scene.set(n, *prop, value.clone(), None).unwrap();
        }
        n
    }

    fn laid_out(scene: &mut Scene, width: f32, height: f32) {
        scene.layout(Some(width), Some(height));
        let _ = scene.draw_list(1.0);
    }

    fn empty_scene() -> Scene {
        Scene::new(Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty()))))
    }

    #[test]
    fn a_row_places_its_children_with_padding_and_gap() {
        let mut scene = empty_scene();
        let root = scene.root();
        let row = boxed(&mut scene, root, &[(Prop::Padding, Value::Number(10.0)), (Prop::Gap, Value::Number(5.0))]);
        let a = boxed(&mut scene, row, &[(Prop::Width, Value::Number(20.0)), (Prop::Height, Value::Number(20.0))]);
        let b = boxed(&mut scene, row, &[(Prop::Width, Value::Number(30.0)), (Prop::Height, Value::Number(10.0))]);
        laid_out(&mut scene, 400.0, 300.0);
        let at = scene.extent(row).unwrap();
        let rel = |n: NodeId| {
            let e = scene.extent(n).unwrap();
            [e[0] - at[0], e[1] - at[1], e[2], e[3]]
        };
        // The root stretches the row down its height; a box centres its
        // children across.
        let h = at[3];
        assert_eq!(rel(a), [10.0, (h - 20.0) / 2.0, 20.0, 20.0]);
        assert_eq!(rel(b), [35.0, (h - 10.0) / 2.0, 30.0, 10.0], "after a and the gap");
        assert_eq!(at[2], 75.0, "10 + 20 + 5 + 30 + 10: its content, gap and padding");
    }

    #[test]
    fn grow_shares_the_space_left_and_stretch_fills_across() {
        let mut scene = empty_scene();
        let root = scene.root();
        let column = boxed(&mut scene, root, &[
            (Prop::Width, Value::Number(100.0)),
            (Prop::Height, Value::Number(300.0)),
            (Prop::Direction, Value::Text("column".into())),
            (Prop::Align, Value::Text("stretch".into())),
        ]);
        let fixed = boxed(&mut scene, column, &[(Prop::Height, Value::Number(60.0))]);
        let one = boxed(&mut scene, column, &[(Prop::Grow, Value::Number(1.0))]);
        let two = boxed(&mut scene, column, &[(Prop::Grow, Value::Number(2.0))]);
        laid_out(&mut scene, 400.0, 400.0);
        let column_at = scene.extent(column).unwrap();
        let rel = |n: NodeId| {
            let e = scene.extent(n).unwrap();
            [e[0] - column_at[0], e[1] - column_at[1], e[2], e[3]]
        };
        assert_eq!(rel(fixed), [0.0, 0.0, 100.0, 60.0], "stretched across");
        assert_eq!(rel(one), [0.0, 60.0, 100.0, 80.0], "a third of the 240 left");
        assert_eq!(rel(two), [0.0, 140.0, 100.0, 160.0], "two thirds");
    }

    #[test]
    fn an_absent_node_takes_no_room_and_is_not_drawn() {
        let mut scene = empty_scene();
        let root = scene.root();
        let row = boxed(&mut scene, root, &[(Prop::Gap, Value::Number(4.0))]);
        let a = boxed(&mut scene, row, &[(Prop::Width, Value::Number(10.0)), (Prop::Height, Value::Number(10.0))]);
        let b = boxed(&mut scene, row, &[(Prop::Width, Value::Number(10.0)), (Prop::Height, Value::Number(10.0))]);
        let c = boxed(&mut scene, row, &[(Prop::Width, Value::Number(10.0)), (Prop::Height, Value::Number(10.0))]);
        laid_out(&mut scene, 200.0, 100.0);
        let c_before = scene.extent(c).unwrap()[0];
        scene.set(b, Prop::Present, Value::Bool(false), None).unwrap();
        laid_out(&mut scene, 200.0, 100.0);
        assert_eq!(scene.extent(c).unwrap()[0], c_before - 14.0, "b's width and its gap given back");
        assert_eq!(scene.extent(a).unwrap()[0] + 14.0, scene.extent(c).unwrap()[0]);
    }

    #[test]
    fn a_row_finds_the_list_around_it_and_a_window_title_moves() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let list = scene.create(Kind::Box, Role::Group);
        let row = scene.create(Kind::Box, Role::Group);
        let button = scene.create(Kind::Capsule, Role::Button);
        scene.append(root, list);
        scene.append(list, row);
        scene.append(row, button);
        assert_eq!(scene.list_around(button), None, "nothing is a list until marked");
        scene.mark_list(list);
        assert_eq!(scene.list_around(button), Some(list));
        assert_eq!(scene.list_around(list), None, "a list is not around itself");
        // A title bar takes the press without a handler of its own.
        assert!(!scene.moves_window(row));
        scene.set(row, Prop::MoveWindow, Value::Bool(true), None).unwrap();
        assert!(scene.moves_window(row));
        assert!(scene.tree.get_node_context(row).unwrap().interactive);
    }

    #[test]
    fn a_path_reaches_as_far_as_its_outline() {
        assert_eq!(path_reach("M 2 3 L 10 20 L 4 8 Z"), [10.0, 20.0]);
        assert_eq!(path_reach("not a path"), [0.0, 0.0]);
    }

    #[test]
    fn a_path_is_drawn_filled_and_stroked() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let n = scene.create(Kind::Path, Role::None);
        scene.append(root, n);
        // A triangle, its point down, filled red.
        scene.set(n, Prop::Points, Value::Numbers(vec![0.0, 0.0, 20.0, 0.0, 10.0, 20.0]), None).unwrap();
        scene.set(n, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        assert_eq!(scene.layout(None, None), (20.0, 20.0), "its size is its reach");
        let node = scene.tree.get_node_context(n).unwrap();
        let svg = path_svg(node, [20.0, 20.0], [40, 40]).unwrap();
        assert!(svg.contains("<polygon") && svg.contains("fill=\"rgb(255,0,0)\"") && !svg.contains("stroke="));
        let mut icons = icons::Icons::default();
        let (_, pixels) = icons.vector(&svg, 40, 40).expect("drawn");
        let at = |x: usize, y: usize| pixels[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4].to_vec();
        assert_eq!(at(20, 10), vec![255, 0, 0, 255], "inside, at twice the scale");
        assert_eq!(at(2, 36)[3], 0, "outside, below the left edge");
        // Stroked with nothing filled: the fill is none.
        scene.set(n, Prop::Fill, Value::Color([0.0; 4]), None).unwrap();
        scene.set(n, Prop::Stroke, Value::Number(2.0), None).unwrap();
        scene.set(n, Prop::Join, Value::Text("round".into()), None).unwrap();
        let node = scene.tree.get_node_context(n).unwrap();
        let svg = path_svg(node, [20.0, 20.0], [20, 20]).unwrap();
        assert!(svg.contains("fill=\"none\"") && svg.contains("stroke-width=\"2\"") && svg.contains("stroke-linejoin=\"round\""));
    }

    #[test]
    fn a_capsule_is_combined_with_the_one_it_names() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let disc = |scene: &mut Scene, x: f32| {
            let n = scene.create(Kind::Capsule, Role::None);
            scene.append(root, n);
            for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 40.0), (Prop::OffsetX, x)] {
                scene.set(n, p, Value::Number(v), None).unwrap();
            }
            scene.set(n, Prop::Overlay, Value::Bool(true), None).unwrap();
            scene.set(n, Prop::Attach, Value::Text("corner".into()), None).unwrap();
            scene.set(n, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
            n
        };
        let a = disc(&mut scene, 0.0);
        scene.set_id(a, Some("a".into()));
        let b = disc(&mut scene, 30.0);
        scene.set(b, Prop::CombineWith, Value::Text("a".into()), None).unwrap();
        scene.set(b, Prop::Combine, Value::Text("subtract".into()), None).unwrap();
        scene.layout(Some(100.0), Some(50.0));
        // The first frame learns where "a" is; the next combines with it.
        scene.draw_list(2.0);
        let list = scene.draw_list(2.0);
        let cut = list.rects.iter().find(|r| r.bud_shape[1] > 0.0).expect("one rect combined");
        assert_eq!(cut.bud_shape[3], 1.0, "subtract");
        assert_eq!(cut.bud[0], -60.0, "a's centre is 30 units left, at twice the scale");
        assert_eq!(cut.bud[2], 40.0, "a's half width");
        // A union is drawn whole, unlike a bud.
        scene.set(b, Prop::Combine, Value::Text("union".into()), None).unwrap();
        let list = scene.draw_list(2.0);
        assert_eq!(list.rects.iter().find(|r| r.bud_shape[1] > 0.0).unwrap().bud_shape[3], 3.0);
        // Several ids: the first in the bud's place, the next beside it; one
        // that names nothing drawn is passed over.
        let c = disc(&mut scene, 60.0);
        scene.set_id(c, Some("c".into()));
        scene.set(b, Prop::CombineWith, Value::Text("a  missing c".into()), None).unwrap();
        scene.layout(Some(100.0), Some(50.0));
        scene.draw_list(2.0);
        let list = scene.draw_list(2.0);
        let joined = list.rects.iter().find(|r| r.bud_shape[1] > 0.0).expect("combined");
        assert_eq!(joined.bud[0], -60.0, "a first");
        assert_eq!(joined.shapes[0], [60.0, 0.0, 40.0, 40.0], "c, 30 units right");
        assert_eq!(joined.shapes[1][1], 1.0, "c is there");
        assert_eq!(joined.shapes[3][1], 0.0, "nothing after it");
    }

    #[test]
    fn an_inspector_sees_every_node_and_what_is_set_on_it() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let card = scene.create(Kind::Capsule, Role::Button);
        scene.append(root, card);
        scene.set_id(card, Some("card".into()));
        scene.set(card, Prop::Width, Value::Number(40.0), None).unwrap();
        scene.set(card, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        let hidden = scene.create(Kind::Text, Role::None);
        scene.append(card, hidden);
        scene.set(hidden, Prop::Present, Value::Bool(false), None).unwrap();
        let nodes = scene.inspect();
        assert_eq!(nodes.len(), 3, "root, card, text");
        let c = &nodes[1];
        assert_eq!((c.kind.as_str(), c.role.as_str(), c.id.as_deref(), c.depth), ("capsule", "button", Some("card"), 1));
        assert!(c.props.contains(&("width", "40".into())) && c.props.contains(&("fill", "#ff0000ff".into())));
        assert!(!c.props.iter().any(|(k, _)| *k == "opacity" || *k == "level"), "what a new capsule has is left out: {:?}", c.props);
        assert!(!nodes[2].present && nodes[2].parent == Some(u64::from(card)));
        // Pointed at, it is outlined; a node that is not there is no mark.
        scene.layout(Some(100.0), Some(100.0));
        let before = scene.draw_list(1.0).rects.len();
        scene.set_highlight(Some(u64::from(card)));
        assert_eq!(scene.draw_list(1.0).rects.len(), before + 1);
        scene.set_highlight(Some(999_999));
        assert_eq!(scene.draw_list(1.0).rects.len(), before);
    }

    #[test]
    fn a_blurred_subtree_is_a_group_kept_by_what_it_draws() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let card = scene.create(Kind::Capsule, Role::None);
        scene.append(root, card);
        for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 20.0)] {
            scene.set(card, p, Value::Number(v), None).unwrap();
        }
        scene.set(card, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let plain = scene.draw_list(2.0);
        assert!(plain.groups.is_empty() && plain.rects.len() == 1, "no blur, no group: nothing more than before");
        scene.set(card, Prop::BlurRadius, Value::Number(4.0), None).unwrap();
        let first = scene.draw_list(2.0);
        assert_eq!(first.groups.len(), 1);
        assert!(first.rects.is_empty(), "the card is drawn in its group, not here");
        let g = &first.groups[0];
        assert_eq!((g.sigma, g.list.rects.len()), (8.0, 1));
        // Its paint (a pixel past the shape, for the antialiased edge) grown
        // by three spreads, within the surface.
        assert_eq!(g.bounds, [0.0, 0.0, 81.0 + 24.0, 41.0 + 24.0]);
        assert_eq!(first.images.len(), 1);
        assert_eq!(first.images[0].key, g.key);
        // At rest, the same key: the renderer keeps what it blurred.
        let again = scene.draw_list(2.0);
        assert_eq!(again.groups[0].key, g.key);
        scene.set(card, Prop::BlurRadius, Value::Number(5.0), None).unwrap();
        assert_ne!(scene.draw_list(2.0).groups[0].key, g.key, "another radius, another picture");
    }

    #[test]
    fn a_mask_child_is_drawn_into_its_parents_mask() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let card = scene.create(Kind::Capsule, Role::None);
        let hole = scene.create(Kind::Capsule, Role::None);
        scene.append(root, card);
        scene.append(card, hole);
        for node in [card, hole] {
            for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 20.0)] {
                scene.set(node, p, Value::Number(v), None).unwrap();
            }
            scene.set(node, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        }
        scene.layout(Some(200.0), Some(100.0));
        assert_eq!(scene.draw_list(1.0).rects.len(), 2, "no mask: both drawn");
        scene.set(hole, Prop::Mask, Value::Bool(true), None).unwrap();
        let list = scene.draw_list(1.0);
        assert!(list.rects.is_empty(), "the card is in its group, the mask in the other");
        assert_eq!(list.groups.len(), 2);
        assert_eq!((list.groups[0].list.rects.len(), list.groups[1].list.rects.len()), (1, 1));
        assert_eq!(list.groups[0].bounds, list.groups[1].bounds, "one rectangle for both");
        assert_eq!(list.images.len(), 1);
        assert_eq!((list.images[0].key, list.images[0].mask), (list.groups[0].key, Some(list.groups[1].key)));
        // Its mask gone, the card is drawn as before.
        scene.set(hole, Prop::Mask, Value::Bool(false), None).unwrap();
        let plain = scene.draw_list(1.0);
        assert!(plain.groups.is_empty() && plain.rects.len() == 2);
    }

    #[test]
    fn pass_children_put_their_parent_through_them_in_order() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let card = scene.create(Kind::Capsule, Role::None);
        scene.append(root, card);
        for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 20.0)] {
            scene.set(card, p, Value::Number(v), None).unwrap();
        }
        scene.set(card, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        let mut passes = Vec::new();
        for effect in ["genoma:ripple", "genoma:vignette"] {
            let pass = scene.create(Kind::Shader, Role::None);
            scene.append(card, pass);
            scene.set(pass, Prop::Src, Value::Text(effect.into()), None).unwrap();
            passes.push(pass);
        }
        scene.layout(Some(200.0), Some(100.0));
        let plain = scene.draw_list(1.0);
        assert_eq!((plain.shaders.len(), plain.groups.len()), (2, 0), "not passes: drawn over the card");
        for &pass in &passes {
            scene.set(pass, Prop::Pass, Value::Bool(true), None).unwrap();
        }
        scene.layout(Some(200.0), Some(100.0));
        let list = scene.draw_list(1.0);
        assert!(list.rects.is_empty() && list.shaders.is_empty(), "the card and its passes are in the picture");
        assert_eq!((list.groups.len(), list.images.len()), (1, 1));
        let group = &list.groups[0];
        assert_eq!(list.images[0].key, group.key);
        assert_eq!(group.list.rects.len(), 1);
        assert_eq!(group.passes.len(), 2);
        // Out of the flow and over all of the card, in the order given.
        let ripple = genoma_render::shader::built_in("ripple").unwrap();
        assert_eq!(group.passes[0].source.as_str(), ripple);
        for p in &group.passes {
            assert_eq!(p.size, [40.0, 20.0]);
            assert_eq!(p.clip, [0.0, 0.0, group.bounds[2] - group.bounds[0], group.bounds[3] - group.bounds[1]]);
            let (_, at) = p.backdrop.unwrap();
            assert_eq!([at[2], at[3]], [group.bounds[2] - group.bounds[0], group.bounds[3] - group.bounds[1]], "it reads the whole picture");
        }
        // At rest, the same picture; new numbers, another.
        assert_eq!(scene.draw_list(1.0).groups[0].key, group.key);
        scene.set(passes[1], Prop::Values, Value::Numbers(vec![0.5]), None).unwrap();
        assert_ne!(scene.draw_list(1.0).groups[0].key, group.key);
    }

    #[test]
    fn a_sub_emitter_bursts_where_its_parents_particles_die() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let rocket = scene.create(Kind::Emitter, Role::None);
        let sparks = scene.create(Kind::Emitter, Role::None);
        scene.append(root, rocket);
        scene.append(rocket, sparks);
        for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 40.0), (Prop::Rate, 10.0), (Prop::Lifetime, 1.0), (Prop::Trail, 0.2)] {
            scene.set(rocket, p, Value::Number(v), None).unwrap();
        }
        scene.set(rocket, Prop::ColorEnd, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        scene.set(sparks, Prop::Sub, Value::Bool(true), None).unwrap();
        scene.set(sparks, Prop::Burst, Value::Number(6.0), None).unwrap();
        scene.set(sparks, Prop::Lifetime, Value::Number(2.0), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let start = Instant::now();
        scene.draw_list(1.0);
        for i in 1..=10 {
            scene.advance(start + Duration::from_millis(50 * i), 1.0);
        }
        let list = scene.draw_list(1.0);
        assert_eq!(list.emitters.len(), 2, "the rocket, then its sparks");
        let (rocket, sparks) = (&list.emitters[0], &list.emitters[1]);
        assert!(Arc::ptr_eq(&rocket.births, &sparks.births), "the sparks read the rocket's births");
        assert_eq!(sparks.generation, rocket.generation);
        assert_eq!(sparks.params.life[2], 6.0);
        assert_eq!(sparks.params.parent[1], rocket.params.motion, "where a rocket dies is its motion's to say");
        assert_eq!(rocket.params.color_end, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(rocket.params.life[1], 0.2, "its trail");
        assert_eq!(sparks.params.color_end[3], -1.0, "no end colour: its fill throughout");
        // Slots are kept while the sparks born there live: a second of the
        // rocket's life and two of theirs, at ten a second.
        assert!(rocket.births.len() >= 31, "{} slots", rocket.births.len());
    }

    #[test]
    fn named_forces_and_walls_make_an_emitter_stepped() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let core = scene.create(Kind::Capsule, Role::None);
        let shelf = scene.create(Kind::Box, Role::None);
        let rain = scene.create(Kind::Emitter, Role::None);
        for n in [core, shelf, rain] {
            scene.append(root, n);
        }
        scene.set_id(core, Some("core".into()));
        scene.set_id(shelf, Some("shelf".into()));
        for (n, w, h) in [(core, 20.0, 10.0), (shelf, 60.0, 10.0), (rain, 40.0, 40.0)] {
            scene.set(n, Prop::Width, Value::Number(w), None).unwrap();
            scene.set(n, Prop::Height, Value::Number(h), None).unwrap();
        }
        scene.set(rain, Prop::Rate, Value::Number(10.0), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let start = Instant::now();
        scene.draw_list(2.0);
        scene.advance(start + Duration::from_millis(300), 2.0);
        let plain = scene.draw_list(2.0);
        assert_eq!(plain.emitters[0].params.sim[0], 0.0, "nothing named: worked out from its age");
        scene.set(rain, Prop::Attract, Value::Text("core pointer nowhere".into()), None).unwrap();
        scene.set(rain, Prop::Attraction, Value::Number(-50.0), None).unwrap();
        scene.set(rain, Prop::Collide, Value::Text("shelf".into()), None).unwrap();
        scene.draw_list(2.0);
        let list = scene.draw_list(2.0);
        let p = list.emitters[0].params;
        assert_eq!(p.sim[0], 1.0, "stepped");
        assert_eq!(p.sim[2], 1.0, "the core only: no pointer over it, no node called nowhere");
        assert_eq!(p.sim[3], 1.0);
        // The core's middle, at scale 2; its push, at scale 2 as well.
        assert_eq!([p.forces[0][0], p.forces[0][1], p.forces[0][2]], [20.0, 10.0, -100.0]);
        // The shelf's box, square: a box is not rounded unless told.
        assert_eq!(p.walls[0], [40.0, 0.0, 160.0, 20.0]);
        assert_eq!(p.walls[1][0], 0.0);
    }

    #[test]
    fn a_simulation_steps_with_its_clock_and_shades_with_its_own_file() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let water = scene.create(Kind::Shader, Role::None);
        scene.append(root, water);
        for (p, v) in [(Prop::Width, 80.0), (Prop::Height, 40.0), (Prop::Steps, 100.0)] {
            scene.set(water, p, Value::Number(v), None).unwrap();
        }
        scene.set(water, Prop::Compute, Value::Text("genoma:waves".into()), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let list = scene.draw_list(1.0);
        assert_eq!(list.shaders.len(), 1, "no src: the file's own shade draws it");
        let sim = list.shaders[0].sim.clone().expect("a simulation");
        assert_eq!((sim.grid, sim.agents, sim.steps), ([40, 20], 0, 0), "a cell every two units; no agents in a grid's file");
        assert_eq!(list.shaders[0].key, sim.source_key);
        // Stopped clock, no steps; running, a hundred a second.
        let start = Instant::now();
        scene.advance(start + Duration::from_millis(30), 1.0);
        assert_eq!(scene.draw_list(1.0).shaders[0].sim.as_ref().unwrap().steps, 0);
        scene.set(water, Prop::Clock, Value::Bool(true), None).unwrap();
        for i in 1..=10 {
            scene.advance(start + Duration::from_millis(30 + 20 * i), 1.0);
        }
        let steps = scene.draw_list(1.0).shaders[0].sim.as_ref().unwrap().steps;
        assert!((18..=21).contains(&steps), "{steps} steps in 0.2 s");
        // A file with a mistake is said over the node, and nothing runs.
        scene.set(water, Prop::Compute, Value::Text("genoma:nothing".into()), None).unwrap();
        let list = scene.draw_list(1.0);
        assert!(list.shaders.is_empty() && !list.texts.is_empty());
    }

    #[test]
    fn a_cached_node_is_one_picture_kept_while_it_rests() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let card = scene.create(Kind::Capsule, Role::None);
        scene.append(root, card);
        for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 20.0)] {
            scene.set(card, p, Value::Number(v), None).unwrap();
        }
        scene.set(card, Prop::Fill, Value::Color([1.0, 0.0, 0.0, 1.0]), None).unwrap();
        scene.set(card, Prop::Cache, Value::Bool(true), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let list = scene.draw_list(1.0);
        assert_eq!((list.rects.len(), list.groups.len(), list.images.len()), (0, 1, 1));
        assert!(list.groups[0].passes.is_empty());
        assert_eq!(scene.draw_list(1.0).groups[0].key, list.groups[0].key);
        scene.set(card, Prop::Fill, Value::Color([0.0, 1.0, 0.0, 1.0]), None).unwrap();
        assert_ne!(scene.draw_list(1.0).groups[0].key, list.groups[0].key);
    }

    #[test]
    fn a_blended_node_is_one_image_and_overlay_reads_what_is_under() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let under = scene.create(Kind::Capsule, Role::None);
        let top = scene.create(Kind::Capsule, Role::None);
        scene.append(root, under);
        scene.append(root, top);
        for node in [under, top] {
            for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 20.0)] {
                scene.set(node, p, Value::Number(v), None).unwrap();
            }
            scene.set(node, Prop::Fill, Value::Color([0.5, 0.5, 0.5, 1.0]), None).unwrap();
        }
        scene.set(top, Prop::Overlay, Value::Bool(true), None).unwrap();
        // Under all of the surface, so the overlaid node lies on it.
        scene.set(under, Prop::Width, Value::Number(200.0), None).unwrap();
        scene.set(under, Prop::Height, Value::Number(100.0), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        scene.set(top, Prop::Blend, Value::Text("multiply".into()), None).unwrap();
        let list = scene.draw_list(1.0);
        assert_eq!((list.rects.len(), list.groups.len(), list.images.len()), (1, 1, 1));
        assert_eq!((list.images[0].blend, list.images[0].mask), (1, None));
        scene.set(top, Prop::Blend, Value::Text("overlay".into()), None).unwrap();
        let list = scene.draw_list(1.0);
        assert_eq!(list.groups.len(), 2, "what is under, and the node");
        let image = &list.images[0];
        assert_eq!(image.blend, genoma_render::Image::OVERLAY);
        assert_eq!(image.mask, Some(list.groups[0].key));
        scene.set(top, Prop::Blend, Value::Text("normal".into()), None).unwrap();
        assert!(scene.draw_list(1.0).groups.is_empty());
    }

    #[test]
    fn frosted_glass_blurs_what_is_under_it_inside_its_shape() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let well = scene.create(Kind::Box, Role::None);
        scene.append(root, well);
        let card = |scene: &mut Scene, x: f32, fill: [f32; 4]| {
            let n = scene.create(Kind::Capsule, Role::None);
            scene.append(well, n);
            for (p, v) in [(Prop::Width, 60.0), (Prop::Height, 40.0), (Prop::OffsetX, x), (Prop::Radius, 8.0)] {
                scene.set(n, p, Value::Number(v), None).unwrap();
            }
            scene.set(n, Prop::Overlay, Value::Bool(true), None).unwrap();
            scene.set(n, Prop::Attach, Value::Text("corner".into()), None).unwrap();
            scene.set(n, Prop::Fill, Value::Color(fill), None).unwrap();
            n
        };
        card(&mut scene, 0.0, [1.0, 0.0, 0.0, 1.0]);
        let glass = card(&mut scene, 30.0, [1.0, 1.0, 1.0, 0.1]);
        scene.layout(Some(200.0), Some(100.0));
        assert!(scene.draw_list(1.0).groups.is_empty());
        scene.set(glass, Prop::BackdropBlur, Value::Number(5.0), None).unwrap();
        let list = scene.draw_list(1.0);
        assert_eq!(list.groups.len(), 1);
        let g = &list.groups[0];
        assert_eq!(g.sigma, 5.0);
        assert_eq!(g.list.rects.len(), 1, "the red card under it, not the glass itself");
        // The blurred picture laid in the glass's rectangle, its corners.
        let i = list.images.iter().find(|i| i.key == g.key).expect("the frosted image");
        assert_eq!((i.origin, i.size, i.radius), ([30.0, 0.0], [60.0, 40.0], 8.0));
        assert_eq!(list.rects.len(), 2, "both cards still drawn, the glass over its frost");
        // At rest, the same picture.
        assert_eq!(scene.draw_list(1.0).groups[0].key, g.key);
    }

    #[test]
    fn a_shader_reads_what_is_drawn_under_it() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let well = scene.create(Kind::Box, Role::None);
        scene.append(root, well);
        let under = scene.create(Kind::Capsule, Role::None);
        scene.append(well, under);
        for (p, v) in [(Prop::Width, 100.0), (Prop::Height, 60.0)] {
            scene.set(under, p, Value::Number(v), None).unwrap();
        }
        scene.set(under, Prop::Fill, Value::Color([0.0, 0.0, 1.0, 1.0]), None).unwrap();
        let pane = scene.create(Kind::Shader, Role::None);
        scene.append(well, pane);
        for (p, v) in [(Prop::Width, 40.0), (Prop::Height, 30.0), (Prop::OffsetX, 30.0), (Prop::OffsetY, 10.0)] {
            scene.set(pane, p, Value::Number(v), None).unwrap();
        }
        scene.set(pane, Prop::Overlay, Value::Bool(true), None).unwrap();
        scene.set(pane, Prop::Attach, Value::Text("corner".into()), None).unwrap();
        scene.set(pane, Prop::Src, Value::Text("genoma:glass".into()), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        assert!(scene.draw_list(1.0).shaders[0].backdrop.is_none(), "not asked, nothing read");
        scene.set(pane, Prop::Backdrop, Value::Bool(true), None).unwrap();
        let list = scene.draw_list(1.0);
        assert_eq!(list.groups.len(), 1);
        let g = &list.groups[0];
        assert_eq!((g.sigma, g.list.rects.len()), (0.0, 1), "the blue card, unblurred");
        let (key, at) = list.shaders[0].backdrop.expect("read");
        assert_eq!(key, g.key);
        // The texture reaches 24 beyond the pane, within the surface: from
        // the pane's corner, (-24, -10), and as wide as both reaches allow.
        assert_eq!(at, [-24.0, -10.0, 88.0, 64.0]);
    }

    fn spring_scene() -> (Scene, NodeId, Timing) {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let n = scene.create(Kind::Capsule, Role::None);
        scene.append(root, n);
        scene.set(n, Prop::OffsetX, Value::Number(0.0), None).unwrap();
        for prop in [Prop::Width, Prop::Height] {
            scene.set(n, prop, Value::Number(20.0), None).unwrap();
        }
        // Drawn once, as a node on screen is: what is never seen waits.
        scene.layout(Some(200.0), Some(100.0));
        let _ = scene.draw_list(1.0);
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
    fn a_node_is_found_by_its_id_while_shown() {
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let row = scene.create(Kind::Box, Role::None);
        let tray = scene.create(Kind::Capsule, Role::None);
        scene.append(root, row);
        scene.append(row, tray);
        scene.set_id(tray, Some("tray".into()));
        assert_eq!(scene.find("tray"), Some(tray));
        assert_eq!(scene.find("clock"), None);
        // Inside something absent, it is not there to open beside.
        scene.set(row, Prop::Present, Value::Bool(false), None).unwrap();
        assert_eq!(scene.find("tray"), None);
    }

    #[test]
    fn what_is_not_seen_asks_for_no_frames_and_catches_up() {
        let (mut scene, n, _) = spring_scene();
        let root = scene.root();
        // The node inside a box that goes absent: a closed cell's panel.
        let panel = scene.create(Kind::Box, Role::None);
        scene.append(root, panel);
        let dot = scene.create(Kind::Capsule, Role::None);
        scene.append(panel, dot);
        for prop in [Prop::Width, Prop::Height] {
            scene.set(dot, prop, Value::Number(10.0), None).unwrap();
        }
        scene.set(panel, Prop::Present, Value::Bool(false), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let _ = scene.draw_list(1.0);
        let t0 = Instant::now();
        scene.advance(t0, 1.0);
        let fade = Timing { duration: 0.2, curve: genoma_core::timing::Curve::LINEAR, delay: 0.0, spring: None };
        scene.set(dot, Prop::Opacity, Value::Number(0.0), Some(fade)).unwrap();
        // A pulse on it too: a mover that would beat for ever.
        scene.set(dot, Prop::Pulse, Value::Number(60.0), None).unwrap();
        let motion = scene.advance(t0 + Duration::from_millis(16), 1.0);
        assert!(!motion.moving && motion.wake.is_none(), "nothing seen moves: {motion:?}");
        // Seen again, after its time: it is where it should be by now.
        scene.set(panel, Prop::Present, Value::Bool(true), None).unwrap();
        scene.layout(Some(200.0), Some(100.0));
        let _ = scene.draw_list(1.0);
        scene.advance(t0 + Duration::from_millis(500), 1.0);
        assert_eq!(scene.value(dot, Prop::Opacity), Some(Value::Number(0.0)));
        // A seen node still moves.
        let motion = scene.advance(t0 + Duration::from_millis(520), 1.0);
        assert!(motion.moving || motion.wake.is_some(), "the pulse beats once seen");
        let _ = n;
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

    #[test]
    fn a_centred_shape_stays_centred_on_a_fractional_screen() {
        // `compact` on a 1.5 screen: a 34 × 0.85 band, a margin of 1.7, and
        // a cell of an odd 22.1 in it, after a sibling that leaves the band
        // starting between pixels.
        let fonts = Arc::new(std::sync::Mutex::new(ct::FontSystem::new_with_fonts(std::iter::empty())));
        let mut scene = Scene::new(fonts);
        let root = scene.root();
        let before = scene.create(Kind::Box, Role::None);
        scene.append(root, before);
        scene.set(before, Prop::Width, Value::Number(3.3), None).unwrap();
        let band = scene.create(Kind::Capsule, Role::None);
        scene.append(root, band);
        for (p, v) in [(Prop::Width, 37.4), (Prop::Height, 37.4)] {
            scene.set(band, p, Value::Number(v), None).unwrap();
        }
        scene.set(band, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
        let cell = scene.create(Kind::Capsule, Role::None);
        scene.append(band, cell);
        for (p, v) in [(Prop::Width, 22.1), (Prop::Height, 22.1)] {
            scene.set(cell, p, Value::Number(v), None).unwrap();
        }
        scene.set(cell, Prop::Overlay, Value::Bool(true), None).unwrap();
        scene.set(cell, Prop::Fill, Value::Color([1.0; 4]), None).unwrap();
        scene.layout(None, None);
        let list = scene.draw_list(1.5);
        assert_eq!(list.rects.len(), 2);
        let (outer, inner) = (&list.rects[0], &list.rects[1]);
        for r in [outer, inner] {
            for v in [r.origin[0], r.origin[1], r.size[0], r.size[1]] {
                assert_eq!(v, v.round(), "on whole pixels: {:?} {:?}", r.origin, r.size);
            }
        }
        for i in 0..2 {
            let before = inner.origin[i] - outer.origin[i];
            let after = outer.origin[i] + outer.size[i] - inner.origin[i] - inner.size[i];
            assert_eq!(before, after, "centred on axis {i}: {before} and {after}");
            assert!((inner.size[i] - 22.1 * 1.5).abs() <= 1.0, "within a pixel of its size: {}", inner.size[i]);
        }
    }
}
