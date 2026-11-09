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
    Marks,
    Runs,
    MarkColor,
    ColorBottom,
    Shadow,
    ShadowBlur,
    ShadowY,
    TextAlign,
    Ellipsize,
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
    FillStops,
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
    FadeBottom,
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
    Halo,
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
    FillAcross,
    FillAngle,
    FillShape,
    Raised,
    Cursor,
    InhibitShortcuts,
    DecodeWidth,
    DecodeHeight,
    Soften,
    Follow,
    Clock,
    Placeholder,
    PlaceholderColor,
    SelectionColor,
    CaretColor,
    Focused,
    Secret,
    D,
    Points,
    Join,
    Cap,
    EvenOdd,
    CombineWith,
    Combine,
    CombineSoft,
    Navigate,
    FocusColor,
    BlurRadius,
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
    /// Stretches of a text with a style of their own (`TextRun`).
    Runs,
    /// A gradient's stops, `{ { number, string } }`.
    Stops,
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

/// The pointer shapes a node can ask for; the first is the compositor's
/// default, which a node names nothing to get.
pub const CURSORS: [&str; 7] = ["default", "pointer", "crosshair", "text", "grab", "grabbing", "not-allowed"];

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
    p("marks", Prop::Marks, Ty::Numbers, false, "Characters of the text, counted from 1, drawn in `mark_color`: the letters a search matched."),
    p("mark_color", Prop::MarkColor, Ty::Color, false, "The colour of the characters `marks` names."),
    p("runs", Prop::Runs, Ty::Runs, false, "Stretches of the text set apart, characters counted from 1, `to` included: each may set its own weight, italic, font, size (on the text's lines), colour, or strike it through. The words a line makes bold, italic or code."),
    p("ellipsize", Prop::Ellipsize, Ty::Choice(&["end", "middle", "start"]), false, "Where a text cut by its `max_width` loses its letters: at the end, the default, or in the middle, so a file name keeps its beginning and its extension."),
    p("text_align", Prop::TextAlign, Ty::Choice(&["start", "center", "end"]), false, "Where each line of a text sits within its `max_width`: a paragraph centred under a title."),
    p("color", Prop::Color, Ty::Color, true, "Text colour."),
    p("shadow", Prop::Shadow, Ty::Color, true, "A text's shadow: its glyphs blurred in this colour, under it, so a name reads over whatever moves behind it."),
    p("shadow_blur", Prop::ShadowBlur, Ty::Number, false, "The shadow's radius."),
    p("shadow_y", Prop::ShadowY, Ty::Number, false, "How far below the text the shadow lies."),
    p("color_bottom", Prop::ColorBottom, Ty::Color, true, "With it, the text is lit from `color` at the top of its box to this at the bottom: a gradient the glyphs reveal, the same for a short figure and a long one."),
    p("present", Prop::Present, Ty::Bool, false, "Whether the node exists; false takes it out of the layout."),
    p("pulse", Prop::Pulse, Ty::Number, false, "Beats a minute: the node swells and settles, then waits. 0 is still."),
    p("orbit", Prop::Orbit, Ty::Number, false, "Turns a second around its place. 0 is parked at twelve."),
    p("orbit_radius", Prop::OrbitRadius, Ty::Number, false, "Radius of the orbit, logical units."),
    p("icon", Prop::Icon, Ty::Text, false, "An icon's name in the icon directories, or a file's absolute path."),
    p("src", Prop::Src, Ty::Text, false, "An image's file: PNG, JPEG, WebP or SVG, an absolute path. Decoded off the frame; nothing is drawn until it is ready. A shader's WGSL file, read when set."),
    p("values", Prop::Values, Ty::Numbers, false, "A shader's numbers, up to 1024: what its `value(i)` reads."),
    p("follow", Prop::Follow, Ty::Number, false, "Seconds a shader's numbers take to ease most of the way to new values (an exponential's time constant); 0 is at once."),
    p("clock", Prop::Clock, Ty::Bool, false, "While true a shader's `shader.time` runs, a frame every refresh; it also runs while its numbers ease. Bind it to the value that moves it."),
    p("fit", Prop::Fit, Ty::Choice(&["cover", "contain", "fill"]), false, "How an image fills its box: cover crops it about its centre, contain shows it whole, fill stretches it."),
    p("gradient_top", Prop::GradientTop, Ty::Color, false, "Paints an icon in a 165° gradient from this colour…"),
    p("gradient_bottom", Prop::GradientBottom, Ty::Color, false, "…to this one."),
    p("stroke", Prop::Stroke, Ty::Number, false, "Rim width in logical units, crisped to whole pixels, for an icon-like outline; without it the rim is 1 logical unit on the physical pixel, never under 2 physical pixels."),
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
    p("fill_stops", Prop::FillStops, Ty::Stops, false, "The fill as a gradient of up to six stops, `{ position, colour }` with positions from 0 at the top to 1 at the bottom (across, with `fill_across`), in order. It takes the place of `fill` and `fill_bottom`."),
    p("fill_bottom", Prop::FillBottom, Ty::Color, true, "With it, the fill runs from `fill` at the top to this at the bottom."),
    p("fill_angle", Prop::FillAngle, Ty::Number, true, "Turns the fill's gradient (`fill` to `fill_bottom`, or `fill_stops`) to this angle, degrees clockwise as in CSS: 0 runs from the bottom up, 90 from left to right, 180 from the top down. The line spans the box corner to corner. For a conic fill, where the sweep starts, clockwise from twelve."),
    p("fill_shape", Prop::FillShape, Ty::Choice(&["linear", "radial", "conic"]), false, "The fill gradient's shape: along a line (the default), outward from the centre to the box's sides, or round the centre clockwise."),
    p("fill_across", Prop::FillAcross, Ty::Bool, false, "The fill's gradient runs from `fill` at the left edge to `fill_bottom` at the right: a progress line."),
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
    p("fade_bottom", Prop::FadeBottom, Ty::Number, false, "With `clip`: what is inside fades out over this many units above the foot, so a page longer than its well runs to the edge and goes, rather than stopping at a cut."),
    p("scroll_y", Prop::ScrollY, Ty::Number, true, "How far the children are scrolled up, logical units."),
    p("dash", Prop::Dash, Ty::Number, false, "A round rim drawn in dashes this long, logical units; 0 is solid."),
    p("dash_gap", Prop::DashGap, Ty::Number, false, "The space between dashes, logical units."),
    p("from_x", Prop::FromX, Ty::Number, true, "A thread's start, logical units inside its box."),
    p("from_y", Prop::FromY, Ty::Number, true, "A thread's start, logical units inside its box."),
    p("to_x", Prop::ToX, Ty::Number, true, "A thread's end."),
    p("to_y", Prop::ToY, Ty::Number, true, "A thread's end."),
    p("bend_x", Prop::BendX, Ty::Number, true, "A thread's control point: where the curve is pulled toward. A spring on it makes the thread bend and settle."),
    p("bend_y", Prop::BendY, Ty::Number, true, "A thread's control point."),
    p("halo", Prop::Halo, Ty::Number, true, "A capsule drawn as a halo instead of a shape, as Qt's RectangularShadow with this blur and a quarter of it as spread: its fill, half at the shape grown by the spread, gone this many logical units past it. To take the light down round something over a window (an auto-hiding membrane's tissues)."),
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
    p("tracking", Prop::Tracking, Ty::Number, false, "Letter spacing, in em (0.06 opens small upper-case labels)."),
    p("catch", Prop::Catch, Ty::Bool, false, "While true, the surface covers its output (a popup keeps its size) and takes the pointer and the keyboard: a click outside reaches on_outside, keys reach on_key."),
    p("decode_width", Prop::DecodeWidth, Ty::Number, false, "An image decoded at this width, logical units, rather than its box's: a box narrower shows the middle of it. For a box whose size moves."),
    p("decode_height", Prop::DecodeHeight, Ty::Number, false, "An image decoded at this height, logical units."),
    p("soften", Prop::Soften, Ty::Number, false, "An image blurred once, as it is decoded, by this radius in logical units: a backdrop behind niri's overview. Nothing is spent on it per frame; a decode smaller than the box blurs it further as it is drawn up."),
    p("raised", Prop::Raised, Ty::Bool, false, "Drawn after everything else on its surface, text included, with what lies under its shape erased first: a list a dropdown opened over the capsules beyond it."),
    p("cursor", Prop::Cursor, Ty::Choice(&CURSORS), false, "The pointer's shape over this node and what it holds, unless a node inside names its own."),
    p("inhibit_shortcuts", Prop::InhibitShortcuts, Ty::Bool, false, "While true on a catching surface, the compositor's own shortcuts stand aside and their keys reach on_key: a field recording a key combination."),
    p("placeholder", Prop::Placeholder, Ty::Text, false, "Shown in a field while it is empty."),
    p("placeholder_color", Prop::PlaceholderColor, Ty::Color, false, "The placeholder's colour; unset, the text's, faded."),
    p("selection_color", Prop::SelectionColor, Ty::Color, false, "What is selected in a field, drawn behind its text; unset, the text's colour at a quarter."),
    p("caret_color", Prop::CaretColor, Ty::Color, false, "A field's caret; unset, the text's colour."),
    p("focused", Prop::Focused, Ty::Bool, false, "While true, a field takes the keys and shows its caret: typing, the editing keys, Ctrl+A/C/X/V/Z/Y. Return reaches on_submit; Escape, Tab, Up and Down go on to on_key."),
    p("secret", Prop::Secret, Ty::Bool, false, "A field drawn as a dot a character, and never copied out: a password."),
    p("overlay", Prop::Overlay, Ty::Bool, false, "Out of the parent's flow: laid over its siblings at the parent's alignment."),
    p("d", Prop::D, Ty::Text, false, "A path's outline as SVG path data (M, L, C, Q, A, Z…), in logical units from the node's top-left corner."),
    p("points", Prop::Points, Ty::Numbers, false, "A path's polygon, closed: x and y of each corner in turn, logical units from the node's top-left corner. Used when `d` is not given."),
    p("join", Prop::Join, Ty::Choice(&["miter", "round", "bevel"]), false, "How a path's outline turns its corners."),
    p("cap", Prop::Cap, Ty::Choice(&["butt", "round", "square"]), false, "How a path's open ends and dashes end."),
    p("even_odd", Prop::EvenOdd, Ty::Bool, false, "A path filled by the even-odd rule: where it crosses itself, a hole."),
    p("combine_with", Prop::CombineWith, Ty::Text, false, "A capsule drawn as one shape with the capsule of this id, as `combine` says: metaballs, a bite out of a shape, a lens. Name one earlier in the tree, or the shape follows it a frame late."),
    p("combine", Prop::Combine, Ty::Choice(&["union", "subtract", "intersect"]), false, "How a capsule with `combine_with` meets the other: joined to it (both drawn, in this one's fill), with the other cut out, or only where both are. The other lends only its outline; give it no fill of its own unless it should be drawn too."),
    p("blur_radius", Prop::BlurRadius, Ty::Number, true, "The node and everything in it drawn blurred, after depth has turned it: the Gaussian's spread, logical units, as CSS's blur(). 0 (the default) costs nothing; a radius at rest is blurred once and kept, a moving one each frame."),
    p("navigate", Prop::Navigate, Ty::Bool, false, "Inside it the keyboard walks, once its surface has the keys: Tab and Shift+Tab, or the arrows, move the focus among its buttons and sliders, shown by a ring; a focused slider takes Left and Right; Return and space press the focused node. Other keys go to the focused node and on up to its ancestors' on_key."),
    p("focus_color", Prop::FocusColor, Ty::Color, false, "The ring round the focused node inside a `navigate` node."),
    p("combine_soft", Prop::CombineSoft, Ty::Number, true, "How softly `combine` meets the other shape, logical units: 0 is a sharp edge; a union this soft grows a neck between shapes that come close."),
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
         export type Role = \"group\" | \"label\" | \"button\" | \"slider\" | \"timer\" | \"status\"\n\n",
    );
    // Each and List repeat Node's fields rather than intersect with it: an
    // intersection makes every field of a table literal invariant, and a
    // `direction = "row"` would not match.
    let fields = node_fields();
    out.push_str(&format!("export type Node = {{\n{fields}}}\n\n"));
    out.push_str(&format!(
        "-- g.each: a box with a row per item, kept by key. `row` gets the item\n\
         -- and its place as functions, so a row whose key stays follows them.\n\
         export type Each<T> = {{\n{fields}{EACH}}}\n\n"
    ));
    out.push_str(&format!(
        "-- g.list: a keyed list that scrolls, mounting only the rows in view.\n\
         -- Every row is row_height tall; the list needs a height of its own.\n\
         export type List<T> = {{\n{fields}{EACH}{LIST}}}\n"
    ));
    out
}

const EACH: &str = "    items: () -> { T },\n    key: (T) -> string | number,\n    row: (item: () -> T, index: () -> number) -> Node,\n";
const LIST: &str = "    row_height: number,\n    -- A row's place, from 1: brought into view the least way.\n    reveal: (() -> number?)?,\n    -- Where it is scrolled to, and how far it can go.\n    on_scroll: ((offset: number, room: number) -> ())?,\n";

/// Every node's fields: the properties, then what every node takes besides.
fn node_fields() -> String {
    let mut out = String::new();
    for info in PROPS {
        let ty = match info.ty {
            Ty::Number => "number".to_string(),
            Ty::Color | Ty::Text => "string".to_string(),
            Ty::Bool => "boolean".to_string(),
            Ty::Numbers => "{ number }".to_string(),
            Ty::Stops => "{ { any } }".to_string(),
            Ty::Runs => "{ { from: number, to: number, weight: number?, italic: boolean?, font: string?, size: number?, color: string?, strike: boolean? } }".to_string(),
            Ty::Choice(options) => options.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(" | "),
        };
        let ty = if ty.contains('|') { format!("({ty})") } else { ty };
        out.push_str(&format!("    -- {}\n    {}: Binding<{ty}>?,\n", info.doc, info.name));
    }
    out.push_str(
        "    role: Role?,\n    name: string?,\n\
         \x20   -- What a g.popup's anchor calls the node by.\n\
         \x20   id: string?,\n\
         \x20   -- Property name to a timing set entry. Never a number.\n\
         \x20   transition: { [string]: string }?,\n\
         \x20   -- Input, heard by the node and passed up to the nearest one that\n\
         \x20   -- listens (not hover, size, focus, change or submit). Buttons are\n\
         \x20   -- 272 left, 273 right, 274 middle; positions are the node's own.\n\
         \x20   on_click: ((button: number, x: number, y: number) -> ())?,\n\
         \x20   on_press: ((button: number, x: number, y: number) -> ())?,\n\
         \x20   on_release: ((button: number, x: number, y: number) -> ())?,\n\
         \x20   -- How far the pointer is from where the press began.\n\
         \x20   on_drag: ((dx: number, dy: number) -> ())?,\n\
         \x20   on_move: ((x: number, y: number) -> ())?,\n\
         \x20   on_hover: ((entered: boolean) -> ())?,\n\
         \x20   -- Wheel steps (steps true) or touchpad distance in logical units.\n\
         \x20   on_wheel: ((dx: number, dy: number, steps: boolean) -> ())?,\n\
         \x20   -- The fingers left the touchpad moving: logical units a second.\n\
         \x20   on_fling: ((vx: number, vy: number) -> ())?,\n\
         \x20   -- A press outside the node that catches.\n\
         \x20   on_outside: (() -> ())?,\n\
         \x20   -- A key: its name (\"Escape\", \"a\"), the text it types, the modifiers.\n\
         \x20   on_key: ((name: string, text: string?, mods: { ctrl: boolean, shift: boolean, alt: boolean, logo: boolean, code: number }) -> ())?,\n\
         \x20   -- An image drawn for the first time.\n\
         \x20   on_ready: (() -> ())?,\n\
         \x20   on_size: ((width: number, height: number) -> ())?,\n\
         \x20   on_focus: ((focused: boolean) -> ())?,\n\
         \x20   on_focus_within: ((x: number, y: number, width: number, height: number) -> ())?,\n\
         \x20   -- A field's text after a change by the person; Return in it.\n\
         \x20   on_change: ((text: string) -> ())?,\n\
         \x20   on_submit: ((text: string) -> ())?,\n\
         \x20   [number]: Node,\n",
    );
    out
}
