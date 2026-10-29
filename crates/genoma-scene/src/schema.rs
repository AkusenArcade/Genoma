//! Every node property, described once (PRD §12, question 6).
//!
//! The Luau converter reads its types from this table, and `genoma types`
//! writes the `.d.luau` definitions from it, so the language server's idea
//! of a property and the runtime's cannot drift apart.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    Width,
    Height,
    MinWidth,
    MaxWidth,
    Grow,
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
    Level,
    Text,
    FontSize,
    Font,
    Tabular,
    Color,
    Present,
    Pulse,
    Orbit,
    OrbitRadius,
    Icon,
    GradientTop,
    GradientBottom,
    Stroke,
    Hollow,
    Blur,
    Wave,
    WavePeriod,
    WaveTime,
    LevelLeft,
    Src,
    Fit,
    Arc,
    Angle,
    Sway,
    SwayRange,
    Overlay,
    FillBottom,
    ArcRate,
    PaddingLeft,
    PaddingRight,
    OffsetX,
    OffsetY,
    Opacity,
    Attach,
    Catch,
    Growth,
    Reach,
    Clip,
    ScrollY,
    Dash,
    DashGap,
    Italic,
    Wrap,
    Weight,
    Tracking,
    Crisp,
    FromX,
    FromY,
    ToX,
    ToY,
    BendX,
    BendY,
    NodeSize,
    Bud,
    Rate,
    Lifetime,
    Speed,
    Spread,
    Heading,
    Gravity,
    ParticleSize,
    RotateX,
    RotateY,
    Depth,
    Perspective,
    PivotX,
    PivotY,
    Values,
    Follow,
    Clock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Number,
    /// `"#rrggbb"` or `"#rrggbbaa"`.
    Color,
    Text,
    Bool,
    /// A list of numbers, `{ number }`.
    Numbers,
    Choice(&'static [&'static str]),
}

pub struct PropInfo {
    pub name: &'static str,
    pub prop: Prop,
    pub ty: Ty,
    /// Moves over a timing when the node names one in `transition`.
    pub animatable: bool,
    pub doc: &'static str,
}

const fn p(name: &'static str, prop: Prop, ty: Ty, animatable: bool, doc: &'static str) -> PropInfo {
    PropInfo { name, prop, ty, animatable, doc }
}

pub const PROPS: &[PropInfo] = &[
    p("width", Prop::Width, Ty::Number, true, "Fixed width, logical units."),
    p("height", Prop::Height, Ty::Number, true, "Fixed height, logical units."),
    p("min_width", Prop::MinWidth, Ty::Number, false, "Never narrower than this."),
    p("max_width", Prop::MaxWidth, Ty::Number, false, "Never wider; text past it ends in an ellipsis."),
    p("grow", Prop::Grow, Ty::Number, false, "Share of the free space along the parent's direction."),
    p("padding", Prop::Padding, Ty::Number, false, "Inner space on every side."),
    p("padding_x", Prop::PaddingX, Ty::Number, false, "Inner space left and right."),
    p("padding_y", Prop::PaddingY, Ty::Number, false, "Inner space top and bottom."),
    p("gap", Prop::Gap, Ty::Number, false, "Space between children."),
    p("direction", Prop::Direction, Ty::Choice(&["row", "column"]), false, "How children are laid out."),
    p("align", Prop::Align, Ty::Choice(&["start", "center", "end", "stretch"]), false, "Cross-axis alignment of children."),
    p("justify", Prop::Justify, Ty::Choice(&["start", "center", "end", "between"]), false, "Main-axis distribution of children."),
    p("radius", Prop::Radius, Ty::Number, true, "Corner radius; a capsule without one is a pill."),
    p("fill", Prop::Fill, Ty::Color, true, "Capsule fill."),
    p("rim_top", Prop::RimTop, Ty::Color, true, "The lit border at the top edge."),
    p("rim_bottom", Prop::RimBottom, Ty::Color, true, "The lit border from mid-height down."),
    p("level", Prop::Level, Ty::Number, true, "Fill height, 0 to 1: a liquid rising from the bottom."),
    p("text", Prop::Text, Ty::Text, false, "What a text node says."),
    p("size", Prop::FontSize, Ty::Number, false, "Font size, logical units."),
    p("font", Prop::Font, Ty::Text, false, "Font family."),
    p("tabular", Prop::Tabular, Ty::Bool, false, "Figures of equal width, where the font has them."),
    p("color", Prop::Color, Ty::Color, true, "Text colour."),
    p("present", Prop::Present, Ty::Bool, false, "Whether the node exists; false takes it out of the layout."),
    p("pulse", Prop::Pulse, Ty::Number, false, "Beats a minute: the node swells and settles, then waits. 0 is still."),
    p("orbit", Prop::Orbit, Ty::Number, false, "Turns a second around its place. 0 is parked at twelve."),
    p("orbit_radius", Prop::OrbitRadius, Ty::Number, false, "Radius of the orbit, logical units."),
    p("icon", Prop::Icon, Ty::Text, false, "An icon's name in the icon directories, or a file's absolute path."),
    p("src", Prop::Src, Ty::Text, false, "An image's file: PNG, JPEG, WebP or SVG, an absolute path. Decoded off the frame; nothing is drawn until it is ready. A shader's WGSL file, read when set."),
    p("values", Prop::Values, Ty::Numbers, false, "A shader's numbers, up to 128: what its `value(i)` reads."),
    p("follow", Prop::Follow, Ty::Number, false, "Seconds a shader's numbers take to ease most of the way to new values (an exponential's time constant); 0 is at once."),
    p("clock", Prop::Clock, Ty::Bool, false, "While true a shader's `shader.time` runs, a frame every refresh; it also runs while its numbers ease. Bind it to the value that moves it."),
    p("fit", Prop::Fit, Ty::Choice(&["cover", "contain", "fill"]), false, "How an image fills its box: cover crops it about its centre, contain shows it whole, fill stretches it."),
    p("gradient_top", Prop::GradientTop, Ty::Color, false, "Paints an icon in a 165° gradient from this colour…"),
    p("gradient_bottom", Prop::GradientBottom, Ty::Color, false, "…to this one."),
    p("stroke", Prop::Stroke, Ty::Number, false, "Rim width in logical units, crisped to whole pixels, for an icon-like outline; the default is Bioma's rim."),
    p("hollow", Prop::Hollow, Ty::Bool, false, "The fill is drawn only outside the child capsules: a tissue's band."),
    p("blur", Prop::Blur, Ty::Bool, false, "The compositor blurs what is behind this shape."),
    p("wave", Prop::Wave, Ty::Number, false, "A liquid's surface rises and falls by this much, logical units."),
    p("wave_period", Prop::WavePeriod, Ty::Number, false, "The wave's length, logical units."),
    p("wave_time", Prop::WaveTime, Ty::Number, false, "Seconds for the wave to travel one length. 0 is still."),
    p("level_left", Prop::LevelLeft, Ty::Bool, false, "The level fills from the left rather than the bottom: a battery's charge, never to be read as a liquid."),
    p("arc", Prop::Arc, Ty::Number, true, "The part drawn, in turns clockwise from twelve: a sector of a disc, an arc of a ring."),
    p("angle", Prop::Angle, Ty::Number, false, "Turns clockwise from twelve: the node's place on its orbit radius."),
    p("sway", Prop::Sway, Ty::Number, false, "Seconds for one sway, there and back. 0 is still."),
    p("sway_range", Prop::SwayRange, Ty::Number, false, "How far the sway travels, logical units."),
    p("fill_bottom", Prop::FillBottom, Ty::Color, true, "With it, the fill runs from `fill` at the top to this at the bottom."),
    p("arc_rate", Prop::ArcRate, Ty::Number, false, "Turns a second the arc grows by between the values it is set to: a trail that keeps up with a continuous hand."),
    p("padding_left", Prop::PaddingLeft, Ty::Number, false, "Inner space on the left."),
    p("padding_right", Prop::PaddingRight, Ty::Number, false, "Inner space on the right."),
    p("offset_x", Prop::OffsetX, Ty::Number, true, "Moves what the node draws, not its place in the layout."),
    p("offset_y", Prop::OffsetY, Ty::Number, true, "Moves what the node draws, not its place in the layout."),
    p("opacity", Prop::Opacity, Ty::Number, true, "The whole subtree's opacity, 0 to 1: every alpha in it is multiplied."),
    p("attach", Prop::Attach, Ty::Choice(&["center", "below", "above", "below-center", "above-center", "left", "right", "corner"]), false, "Where an overlay sits: centred in its parent; under or over it, left-aligned or centred; beside it, centred; or at its top-left corner, offsets then measured from there."),
    p("growth", Prop::Growth, Ty::Number, true, "0 to 1: the shape grows from the middle of its parent's edge that faces it (where it is attached) to its own rectangle, its near edge held; content is not scaled."),
    p("reach", Prop::Reach, Ty::Number, true, "0 to 1: how far down a thread is drawn."),
    p("clip", Prop::Clip, Ty::Bool, false, "Children are drawn only inside this node's rectangle: a well that scrolls. Never on a cell, whose rim it would eat."),
    p("scroll_y", Prop::ScrollY, Ty::Number, true, "How far the children are scrolled up, logical units."),
    p("dash", Prop::Dash, Ty::Number, false, "A round rim drawn in dashes this long, logical units; 0 is solid."),
    p("dash_gap", Prop::DashGap, Ty::Number, false, "The space between dashes, logical units."),
    p("from_x", Prop::FromX, Ty::Number, true, "A thread's start, logical units inside its box."),
    p("from_y", Prop::FromY, Ty::Number, true, "A thread's start, logical units inside its box."),
    p("to_x", Prop::ToX, Ty::Number, true, "A thread's end."),
    p("to_y", Prop::ToY, Ty::Number, true, "A thread's end."),
    p("bend_x", Prop::BendX, Ty::Number, true, "A thread's control point: where the curve is pulled toward. A spring on it makes the thread bend and settle."),
    p("bend_y", Prop::BendY, Ty::Number, true, "A thread's control point."),
    p("bud", Prop::Bud, Ty::Number, true, "Joined to the nearest capsule above by a smooth union this soft, logical units: a neck that narrows as it falls to 0 and parts (budding, and reabsorption back)."),
    p("rate", Prop::Rate, Ty::Number, false, "An emitter's particles a second: bind it to the value it shows. At 0 the last ones finish their lives and then nothing runs."),
    p("lifetime", Prop::Lifetime, Ty::Number, false, "Seconds a particle lives."),
    p("speed", Prop::Speed, Ty::Number, false, "Logical units a second, for ballistic particles."),
    p("spread", Prop::Spread, Ty::Number, false, "Degrees around the heading, ballistic; or how far carried particles sway."),
    p("heading", Prop::Heading, Ty::Number, false, "Degrees clockwise from up: where ballistic particles go."),
    p("gravity", Prop::Gravity, Ty::Number, false, "Logical units a second squared, downward."),
    p("particle_size", Prop::ParticleSize, Ty::Number, false, "A particle's diameter, logical units."),
    p("rotate_x", Prop::RotateX, Ty::Number, true, "Degrees about the horizontal axis through the pivot: a panel unfolding from its top edge. Applies to the subtree; text stays flat for now."),
    p("rotate_y", Prop::RotateY, Ty::Number, true, "Degrees about the vertical axis through the pivot."),
    p("depth", Prop::Depth, Ty::Number, true, "Logical units toward the viewer (positive) or away (negative), through the perspective."),
    p("perspective", Prop::Perspective, Ty::Number, false, "Distance of the eye, logical units: smaller is stronger. 800 by default."),
    p("pivot_x", Prop::PivotX, Ty::Number, false, "Where the rotations turn, 0 to 1 across the node; 0.5 by default."),
    p("pivot_y", Prop::PivotY, Ty::Number, false, "Where the rotations turn, 0 to 1 down the node; 0.5 by default."),
    p("node", Prop::NodeSize, Ty::Number, false, "A thread's nodes: discs this wide at its start and at its tip, in `color`. 0 is none."),
    p("crisp", Prop::Crisp, Ty::Bool, false, "A hairline: position and size rounded to whole physical pixels, never under one (Metrics.crisp)."),
    p("italic", Prop::Italic, Ty::Bool, false, "Text in the font's italic."),
    p("wrap", Prop::Wrap, Ty::Bool, false, "Text past its max_width goes on to the next line, at a word, instead of ending in an ellipsis: a sentence, not a name."),
    p("weight", Prop::Weight, Ty::Number, false, "Font weight, 100 to 900 (400 regular, 500 medium, 700 bold)."),
    p("tracking", Prop::Tracking, Ty::Number, false, "Letter spacing, in em (Bioma's labels: 0.06)."),
    p("catch", Prop::Catch, Ty::Bool, false, "While true, the surface covers its output and takes the pointer and the keyboard: a click outside reaches on_outside, keys reach on_key."),
    p("overlay", Prop::Overlay, Ty::Bool, false, "Out of the parent's flow: laid over its siblings at the parent's alignment."),
];

impl Prop {
    pub fn info(self) -> &'static PropInfo {
        PROPS.iter().find(|i| i.prop == self).expect("every prop is in the table")
    }

    pub fn from_name(name: &str) -> Option<Prop> {
        PROPS.iter().find(|i| i.name == name).map(|i| i.prop)
    }
}

/// Luau definitions for the node constructors, for `luau-lsp`.
pub fn luau_definitions() -> String {
    let mut out = String::from(
        "-- Generated by `genoma types` from genoma-scene's property table. Do not edit.\n\n\
         export type Binding<T> = T | () -> T\n\n\
         export type Role = \"group\" | \"label\" | \"button\" | \"slider\" | \"timer\" | \"status\"\n\n\
         export type Node = {\n",
    );
    for info in PROPS {
        let ty = match info.ty {
            Ty::Number => "number".to_string(),
            Ty::Color | Ty::Text => "string".to_string(),
            Ty::Bool => "boolean".to_string(),
            Ty::Numbers => "{ number }".to_string(),
            Ty::Choice(options) => options.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(" | "),
        };
        let ty = if ty.contains('|') { format!("({ty})") } else { ty };
        out.push_str(&format!("    -- {}\n    {}: Binding<{ty}>?,\n", info.doc, info.name));
    }
    out.push_str(
        "    role: Role?,\n    name: string?,\n    id: string?,\n\
         \x20   -- Property name to a timing set entry. Never a number.\n\
         \x20   transition: { [string]: string }?,\n\
         \x20   [number]: Node,\n}\n\n\
         -- g.each: a box with a row per item, kept by key. `row` gets the item\n\
         -- and its place as functions, so a row whose key stays follows them.\n\
         export type Each<T> = Node & {\n\
         \x20   items: () -> { T },\n\
         \x20   key: (T) -> string | number,\n\
         \x20   row: (item: () -> T, index: () -> number) -> Node,\n}\n",
    );
    out
}
