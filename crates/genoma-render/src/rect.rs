//! The `rect` primitive (PRD §5.2): a rounded rectangle with a fill and the
//! lit border, both from one distance field, every instance in one draw call.

use bytemuck::{Pod, Zeroable};

/// One rounded rectangle, in physical pixels of its target.
///
/// The rim is Bioma's lit border: a band `rim` pixels wide inside the edge,
/// running from `rim_top` at the top to `rim_bottom` at mid-height and staying
/// there below. Colours are straight (not premultiplied) linear RGBA as the
/// compositor blends them.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Rect {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub radius: f32,
    pub rim: f32,
    /// How much of the height the fill covers, from the bottom: 1 is all of
    /// it, a lower value is a liquid's surface. The rim is never cut.
    pub level: f32,
    /// What the shape is: 0 a rounded rectangle; 1 a cut, which erases what
    /// is under it (a tissue's band where its cells are, so glass never
    /// sits on glass); 2 a thread along `path`; 3 a halo, the rectangle
    /// 1.25 × `rim` in from the quad's edge, grown by a quarter of `rim` and
    /// blurred over `rim` on both sides of that edge.
    pub cut: f32,
    pub fill: [f32; 4],
    pub rim_top: [f32; 4],
    pub rim_bottom: [f32; 4],
    /// The fill's surface as a wave: amplitude and wavelength in pixels, and
    /// the phase (0…1) it has travelled. Zero amplitude is a flat level. The
    /// last is flags: 1 for a level that fills from the left, 2 for a fill
    /// gradient that runs from the left edge to the right one.
    pub wave: [f32; 4],
    /// Only this part of the shape is drawn: start and sweep in turns,
    /// clockwise from twelve. A sweep of 1 or more is the whole shape; on a
    /// disc a smaller one is a sector, on a ring an arc. The last two are a
    /// dash and its gap along the rim, physical pixels; 0 is a solid rim.
    pub arc: [f32; 4],
    /// The fill's colour at the bottom: with an alpha above zero the fill
    /// runs from `fill` at the top to this. Transparent is a flat fill.
    pub fill_to: [f32; 4],
    /// Drawn only inside this rectangle, physical pixels: x0, y0, x1, y1.
    pub clip: [f32; 4],
    /// A thread's quadratic Bézier, physical pixels from the rectangle's
    /// centre: start x, y, control x, y; then end x, y, width, and how much
    /// of it is drawn (0…1).
    pub path: [f32; 4],
    pub path_end: [f32; 4],
    /// Faded toward a clip's foot: the bottom edge where nothing is left
    /// and how far above it the fade begins, physical pixels. A zero span
    /// is no fade. The third is the GPU's own (the fill's stops, by index);
    /// the fourth is the fill gradient's shape, kind × 1000 + degrees (1
    /// linear at an angle, 2 radial, 3 conic), 0 for the vertical one.
    pub fade: [f32; 4],
    /// Budding (PRD §8.2): the parent's box, its centre relative to this
    /// one's and its half size, physical pixels; then the parent's radius,
    /// and the smoothness of the union (0 is no bud). The shape drawn is the
    /// smooth union of the two, without the parent's own interior.
    pub bud: [f32; 4],
    pub bud_shape: [f32; 4],
    /// The fill as a gradient of up to six stops, along the same direction
    /// as `fill_to`: the first two rows are the positions (0…1, in order),
    /// the last of the eight slots the count; then each stop's colour. A
    /// count of 0 is no stops. Not an attribute: the stops go to the GPU
    /// once each, as the matrices do.
    pub stops: [[f32; 4]; 8],
    /// Depth (PRD §5.5): a 4×4 matrix, by rows, from the surface's physical
    /// pixels to projected ones with w for the perspective. The identity is
    /// a flat shape.
    pub transform: [[f32; 4]; 4],
}

/// A `Rect` as the GPU reads it: everything but the matrix, which would take
/// four of the sixteen vertex attributes a device guarantees. The matrices
/// go in a storage buffer, each distinct one once, and the instance carries
/// its index in `bud_shape.z`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuRect([f32; GpuRect::LEN]);

impl GpuRect {
    const LEN: usize = std::mem::size_of::<Rect>() / 4 - 16 - 32;
    /// `bud_shape.z`, the last vector before the matrix.
    const TRANSFORM: usize = Self::LEN - 2;

    /// `fade.z`.
    const STOPS: usize = std::mem::offset_of!(Rect, fade) / 4 + 2;

    fn new(rect: &Rect, transform: u32, stops: u32) -> GpuRect {
        let all: &[f32] = bytemuck::cast_slice(std::slice::from_ref(rect));
        let mut body = [0.0; Self::LEN];
        body.copy_from_slice(&all[..Self::LEN]);
        body[Self::TRANSFORM] = transform as f32;
        body[Self::STOPS] = stops as f32;
        GpuRect(body)
    }
}

impl Rect {
    pub const WHOLE: [f32; 4] = [0.0, 1.0, 0.0, 0.0];
    pub const IDENTITY: [[f32; 4]; 4] =
        [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    pub const NO_CLIP: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];
    pub const NO_STOPS: [[f32; 4]; 8] = [[0.0; 4]; 8];

    /// Up to six stops, (position, colour), positions in order.
    pub fn stops(list: &[(f32, [f32; 4])]) -> [[f32; 4]; 8] {
        let mut out = Self::NO_STOPS;
        let n = list.len().min(6);
        for (i, (at, colour)) in list.iter().take(n).enumerate() {
            out[i / 4][i % 4] = *at;
            out[2 + i] = *colour;
        }
        out[1][3] = n as f32;
        out
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    cutter: wgpu::RenderPipeline,
    runs: Vec<(bool, std::ops::Range<u32>)>,
    layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    transforms: wgpu::Buffer,
    transform_capacity: usize,
    gradients: wgpu::Buffer,
    gradient_capacity: usize,
    last_gradients: Vec<[[f32; 4]; 8]>,
    stop_sets: Vec<[[f32; 4]; 8]>,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
    /// What the buffers hold now: a write is made only for bytes that
    /// changed, because each one costs a staging buffer.
    last_viewport: [f32; 2],
    last_rects: Vec<u8>,
    last_transforms: Vec<[[f32; 4]; 4]>,
    /// Scratch, kept to reuse the allocations.
    matrices: Vec<[[f32; 4]; 4]>,
    gpu_rects: Vec<GpuRect>,
}

const SHADER: &str = r#"
struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
// By rows on the CPU, so as WGSL's columns each is the transpose: `p * m`
// is the matrix applied to p.
@group(0) @binding(1) var<storage, read> transforms: array<mat4x4<f32>>;
// Each distinct set of fill stops once, the first none (Rect::stops).
struct Stops { at: array<vec4<f32>, 2>, colour: array<vec4<f32>, 6> };
@group(0) @binding(2) var<storage, read> gradients: array<Stops>;

fn along_stops(s: Stops, t: f32) -> vec4<f32> {
    let n = i32(s.at[1].w + 0.5);
    var p = s.at[0].x;
    if (t <= p) { return s.colour[0]; }
    for (var i = 1; i < n; i++) {
        let q = s.at[i / 4][i % 4];
        if (t <= q) {
            return mix(s.colour[i - 1], s.colour[i], (t - p) / max(q - p, 1e-5));
        }
        p = q;
    }
    return s.colour[max(n - 1, 0)];
}

struct Instance {
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) shape: vec4<f32>,      // radius, rim, level, cut
    @location(3) fill: vec4<f32>,
    @location(4) rim_top: vec4<f32>,
    @location(5) rim_bottom: vec4<f32>,
    @location(6) wave: vec4<f32>,
    @location(7) arc: vec4<f32>,
    @location(8) fill_to: vec4<f32>,
    @location(9) clip: vec4<f32>,
    @location(10) path: vec4<f32>,
    @location(11) path_end: vec4<f32>,
    @location(14) fade: vec4<f32>,
    @location(12) bud: vec4<f32>,
    @location(13) bud_shape: vec4<f32>,
};

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,      // pixels from the rectangle's centre
    @location(1) half: vec2<f32>,
    @location(2) shape: vec4<f32>,
    @location(3) fill: vec4<f32>,
    @location(4) rim_top: vec4<f32>,
    @location(5) rim_bottom: vec4<f32>,
    @location(6) wave: vec4<f32>,
    @location(7) arc: vec4<f32>,
    @location(8) fill_to: vec4<f32>,
    @location(9) clip: vec4<f32>,
    @location(10) path: vec4<f32>,
    @location(11) path_end: vec4<f32>,
    @location(12) bud: vec4<f32>,
    @location(13) bud_shape: vec4<f32>,
    @location(14) fade: vec4<f32>,
    // The pixel before the node's matrix: where the clip and the fade are
    // measured, so what a tilted shape holds is cut by its turned parent,
    // not by where that parent lay flat. The same as the position when flat.
    @location(15) unturned: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vertex: u32, instance: Instance) -> Varyings {
    // Two triangles over the rectangle, one pixel larger on every side so the
    // antialiased edge is never cut by the quad; a bud covers its parent's
    // box too, where the bridge is drawn.
    let corner = vec2<f32>(f32(vertex & 1u), f32((vertex >> 1u) & 1u));
    var lo = instance.origin;
    var hi = instance.origin + instance.size;
    if (instance.bud_shape.y > 0.0) {
        let centre = instance.origin + instance.size * 0.5 + instance.bud.xy;
        lo = min(lo, centre - instance.bud.zw);
        hi = max(hi, centre + instance.bud.zw);
    }
    let pixel = lo - vec2<f32>(1.0) + corner * (hi - lo + vec2<f32>(2.0));
    // Depth: the corner through the node's matrix. Its w carries the
    // perspective, so the GPU interpolates the local coordinates correctly
    // and the distance field stays exact across a tilted shape.
    let p = vec4<f32>(pixel, 0.0, 1.0);
    let q = p * transforms[u32(instance.bud_shape.z)];
    let screen = q.xy / q.w;
    let clip = screen / globals.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: Varyings;
    out.position = vec4<f32>(clip * q.w, 0.0, q.w);
    out.half = instance.size * 0.5;
    out.local = pixel - instance.origin - out.half;
    out.shape = instance.shape;
    out.fill = instance.fill;
    out.rim_top = instance.rim_top;
    out.rim_bottom = instance.rim_bottom;
    out.wave = instance.wave;
    out.arc = instance.arc;
    out.fill_to = instance.fill_to;
    out.clip = instance.clip;
    out.path = instance.path;
    out.path_end = instance.path_end;
    out.bud = instance.bud;
    out.bud_shape = instance.bud_shape;
    out.fade = instance.fade;
    out.unturned = pixel;
    return out;
}

fn dot2(v: vec2<f32>) -> f32 { return dot(v, v); }

// Distance to a quadratic Bézier from a to c with control b, drawn only up to
// parameter g (Inigo Quilez's closed form, the roots clamped to [0, g]).
fn bezier(pos: vec2<f32>, a: vec2<f32>, b0: vec2<f32>, c: vec2<f32>, g: f32) -> f32 {
    let aa = b0 - a;
    let bb = a - 2.0 * b0 + c;
    if (dot(bb, bb) < 1e-4) {
        // Straight: the segment's own distance.
        let ab = c - a;
        let t = clamp(dot(pos - a, ab) / max(dot(ab, ab), 1e-6), 0.0, g);
        return length(pos - (a + ab * t));
    }
    let cc = aa * 2.0;
    let d = a - pos;
    let kk = 1.0 / dot(bb, bb);
    let kx = kk * dot(aa, bb);
    let ky = kk * (2.0 * dot(aa, aa) + dot(d, bb)) / 3.0;
    let kz = kk * dot(d, aa);
    let p = ky - kx * kx;
    let q = kx * (2.0 * kx * kx - 3.0 * ky) + kz;
    var h = q * q + 4.0 * p * p * p;
    var res = 0.0;
    if (h >= 0.0) {
        h = sqrt(h);
        let x = (vec2<f32>(h, -h) - q) / 2.0;
        let uv = sign(x) * pow(abs(x), vec2<f32>(1.0 / 3.0));
        let t = clamp(uv.x + uv.y - kx, 0.0, g);
        res = dot2(d + (cc + bb * t) * t);
    } else {
        let z = sqrt(-p);
        let v = acos(q / (p * z * 2.0)) / 3.0;
        let m = cos(v);
        let n = sin(v) * 1.732050808;
        let t = clamp(vec3<f32>(m + m, -n - m, n - m) * z - kx, vec3<f32>(0.0), vec3<f32>(g));
        res = min(dot2(d + (cc + bb * t.x) * t.x), dot2(d + (cc + bb * t.y) * t.y));
    }
    // The end at g is a round cap: the closest point may be the end itself.
    let tip = a + (cc + bb * g) * g;
    return sqrt(min(res, dot2(pos - tip)));
}

// Polynomial smooth minimum: the two fields joined, with a bridge as wide as
// k where they come close.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

// Signed distance to a rounded box: negative inside, in pixels.
// How far round a rounded rectangle's outline the point nearest `p` is,
// from the middle of the top edge clockwise, and the outline's length. A
// circle and a pill are its cases: dashes measured by the angle about the
// centre stretched along a pill's long sides.
fn perimeter_at(p: vec2<f32>, half: vec2<f32>, radius: f32) -> vec2<f32> {
    let r = min(radius, min(half.x, half.y));
    let a = max(half - vec2<f32>(r), vec2<f32>(0.0));
    let q = r * 1.5707963;
    let total = 4.0 * a.x + 4.0 * a.y + 4.0 * q;
    let c = clamp(p, -a, a);
    var d = p - c;
    // Inside the straight part's reach: the nearest side decides.
    if (abs(d.x) < 1e-4 && abs(d.y) < 1e-4) {
        if (a.x - abs(p.x) < a.y - abs(p.y)) { d = vec2<f32>(sign(p.x), 0.0); } else { d = vec2<f32>(0.0, sign(p.y)); }
    }
    var s = 0.0;
    if (abs(d.x) < 1e-4) {
        if (d.y < 0.0) {
            s = p.x;
            if (s < 0.0) { s = s + total; }
        } else {
            s = a.x + 2.0 * q + 2.0 * a.y + (a.x - p.x);
        }
    } else if (abs(d.y) < 1e-4) {
        if (d.x > 0.0) { s = a.x + q + (p.y + a.y); } else { s = 3.0 * a.x + 3.0 * q + 2.0 * a.y + (a.y - p.y); }
    } else if (d.x > 0.0 && d.y < 0.0) {
        s = a.x + r * atan2(d.x, -d.y);
    } else if (d.x > 0.0) {
        s = a.x + q + 2.0 * a.y + r * atan2(d.y, d.x);
    } else if (d.y > 0.0) {
        s = 3.0 * a.x + 2.0 * q + 2.0 * a.y + r * atan2(-d.x, d.y);
    } else {
        s = 3.0 * a.x + 3.0 * q + 4.0 * a.y + r * atan2(-d.y, -d.x);
    }
    return vec2<f32>(s, max(total, 1.0));
}

fn rounded_box(p: vec2<f32>, half: vec2<f32>, radius: f32) -> f32 {
    let r = min(radius, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn body(in: Varyings) -> vec4<f32> {
    // How many local pixels one screen pixel spans: 1 when flat, more or
    // less under depth. Taken before any branch, where derivatives are
    // defined.
    let aa = max(length(fwidth(in.local)) * 0.70710678, 1e-3);
    let p = in.unturned;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }
    // A halo, as Qt's RectangularShadow draws one (Membrane.qml: blur the
    // reach, spread a quarter of it): the shape grown by the spread is half
    // covered at its edge, whole a blur inside it and gone a blur outside.
    // The quad is the shape grown by spread and blur. Whole up to a quarter
    // of the reach and gone at it was twice as dark round every tissue
    // (Akusen, 2026-11-03).
    if (in.shape.w > 2.5) {
        let blur = max(in.shape.y, 1.0);
        let spread = blur * 0.25;
        let shape = max(in.half - vec2<f32>(blur + spread), vec2<f32>(0.0));
        let d = rounded_box(in.local, shape, in.shape.x) - spread;
        // Qt blurs in two dimensions, and round a band as thin as its blur
        // the tail falls faster than one edge's would: measured against
        // Bioma's Quickshell halo, the coverage to the power 1.25.
        let cover = pow(1.0 - smoothstep(-blur, blur, d), 1.25) * in.fill.a;
        return vec4<f32>(in.fill.rgb * cover, cover);
    }
    // A thread: a band along its Bézier, as far as it has grown.
    if (in.shape.w > 1.5) {
        let reach = in.path_end.w;
        if (reach <= 0.0) { discard; }
        let dist = bezier(in.local, in.path.xy, in.path.zw, in.path_end.xy, reach) - in.path_end.z * 0.5;
        let cover = clamp(0.5 - dist, 0.0, 1.0) * in.fill.a;
        return vec4<f32>(in.fill.rgb * cover, cover);
    }
    var d = rounded_box(in.local, in.half, in.shape.x);
    // A bud: joined to its parent by a smooth union, drawn only outside the
    // parent, so the parent's glass is never covered twice.
    var outside_parent = 1.0;
    if (in.bud_shape.y > 0.0) {
        let parent = rounded_box(in.local - in.bud.xy, in.bud.zw, in.bud_shape.x);
        d = smin(d, parent, in.bud_shape.y);
        outside_parent = clamp(parent + 0.5, 0.0, 1.0);
    }
    let rim = in.shape.y;

    // Coverage over one screen pixel of distance: the edge is analytic at any
    // scale and any tilt.
    let outer = clamp(0.5 - d / aa, 0.0, 1.0);
    // Inset by the rim width: the inner edge is the same field offset, so its
    // radius is concentric by construction.
    let inner = clamp(0.5 - (d + rim) / aa, 0.0, 1.0);

    // The light reaches the bottom colour at mid-height and stays there.
    let height = in.local.y + in.half.y;
    let t = clamp(height / max(in.half.y, 1.0), 0.0, 1.0);
    let rim_colour = mix(in.rim_top, in.rim_bottom, t);

    // The liquid's surface, antialiased like every other edge.
    let from_bottom = in.half.y - in.local.y;
    var level = in.shape.z * in.half.y * 2.0;
    if (in.wave.x > 0.0) {
        let x = in.local.x + in.half.x;
        // The wave travels to the right, as Vital.qml's translation does.
        level = level + in.wave.x * sin(6.2831853 * (x / max(in.wave.y, 1.0) - in.wave.z));
    }
    var surface = clamp(level - from_bottom + 0.5, 0.0, 1.0);
    let flags = u32(in.wave.w + 0.5);
    if ((flags & 1u) != 0u) {
        surface = clamp(in.shape.z * in.half.x * 2.0 - (in.local.x + in.half.x) + 0.5, 0.0, 1.0);
    }

    // The part of the shape that is drawn: an angle clockwise from twelve,
    // antialiased across one pixel at both of its edges.
    var part = 1.0;
    if (in.arc.y < 1.0) {
        let turn = fract(atan2(in.local.x, -in.local.y) / 6.2831853 - in.arc.x + 1.0);
        let radius = max(length(in.local), 1.0);
        let into = min(turn, in.arc.y - turn) * 6.2831853 * radius;
        part = clamp(into + 0.5, 0.0, 1.0);
        if (in.arc.y <= 0.0) { part = 0.0; }
    }

    var fill = in.fill;
    var along_fill = height / max(in.half.y * 2.0, 1.0);
    if ((flags & 2u) != 0u) {
        along_fill = (in.local.x + in.half.x) / max(in.half.x * 2.0, 1.0);
    }
    if (in.fade.w > 0.5) {
        along_fill = along_shape(in.local, in.half, in.fade.w);
    }
    if (in.fade.z > 0.5) {
        fill = along_stops(gradients[u32(in.fade.z + 0.5)], clamp(along_fill, 0.0, 1.0));
    } else if (in.fill_to.a > 0.0) {
        fill = mix(in.fill, in.fill_to, clamp(along_fill, 0.0, 1.0));
    }
    // Dashes along the rim: the distance travelled round the outline from
    // twelve o'clock, clockwise, with the pattern stretched to fit it a whole
    // number of times, so the last dash never meets the first.
    var dashed = 1.0;
    if (in.arc.z > 0.0) {
        let walk = perimeter_at(in.local, in.half, in.shape.x);
        let period = in.arc.z + in.arc.w;
        let count = max(1.0, round(walk.y / period));
        let fitted = walk.y / count;
        let dash = in.arc.z * fitted / period;
        let at = walk.x - floor(walk.x / fitted) * fitted;
        dashed = clamp(min(at, dash - at) + 0.5, 0.0, 1.0);
    }
    let rim_alpha = rim_colour.a * (outer - inner) * part * dashed * outside_parent;
    let fill_alpha = fill.a * inner * surface * part * outside_parent;
    if (in.shape.w > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, outer);
    }
    // Premultiplied out: the surface is composited as premultiplied alpha.
    return vec4<f32>(rim_colour.rgb * rim_alpha + fill.rgb * fill_alpha, rim_alpha + fill_alpha);
}

// Where a point lies along a turned, radial or conic fill (`fade.w`, kind ×
// 1000 + degrees). Linear follows CSS: 0° runs bottom to top, 90° left to
// right, and the line is as long as the box's projection on it, so the
// corners sit at 0 and 1. Radial reaches 1 at the middle of each side.
// Conic goes round clockwise from the angle, twelve at 0°.
fn along_shape(local: vec2<f32>, half: vec2<f32>, code: f32) -> f32 {
    let kind = floor(code / 1000.0 + 0.0005);
    let radians = (code - kind * 1000.0) * 0.017453293;
    if (kind < 1.5) {
        let direction = vec2<f32>(sin(radians), -cos(radians));
        let reach = abs(half.x * direction.x) + abs(half.y * direction.y);
        return dot(local, direction) / max(reach * 2.0, 1.0) + 0.5;
    }
    if (kind < 2.5) {
        return length(local / max(half, vec2<f32>(1.0)));
    }
    return fract(atan2(local.x, -local.y) / 6.2831853 - radians / 6.2831853 + 1.0);
}

// Toward a clip's foot, everything fades: alpha and colour, premultiplied.
fn faded(colour: vec4<f32>, y: f32, fade: vec4<f32>) -> vec4<f32> {
    if (fade.y <= 0.0) { return colour; }
    return colour * clamp((fade.x - y) / fade.y, 0.0, 1.0);
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    return faded(body(in), in.unturned.y, in.fade);
}
"#;

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rect globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let transform_capacity = 4;
        let transforms = Self::transform_buffer(device, transform_capacity);
        let gradient_capacity = 2;
        let gradients = Self::gradient_buffer(device, gradient_capacity);
        let bind_group = Self::bind_group(device, &layout, &globals, &transforms, &gradients);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let make = |blend: wgpu::BlendState| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuRect>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x4,
                        3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
                        6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4,
                        10 => Float32x4, 11 => Float32x4, 14 => Float32x4, 12 => Float32x4, 13 => Float32x4,
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let pipeline = make(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        // Destination-out: what is drawn is the shape's coverage taken away.
        let erase = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let cutter = make(wgpu::BlendState { color: erase, alpha: erase });
        let capacity = 16;
        let instances = Self::instance_buffer(device, capacity);
        Pipeline {
            pipeline,
            cutter,
            runs: Vec::new(),
            layout,
            globals,
            transforms,
            transform_capacity,
            gradients,
            gradient_capacity,
            last_gradients: Vec::new(),
            stop_sets: Vec::new(),
            bind_group,
            instances,
            capacity,
            count: 0,
            last_viewport: [0.0; 2],
            last_rects: Vec::new(),
            last_transforms: Vec::new(),
            matrices: Vec::new(),
            gpu_rects: Vec::new(),
        }
    }

    fn transform_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect transforms"),
            size: (capacity * std::mem::size_of::<[[f32; 4]; 4]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn gradient_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect gradients"),
            size: (capacity * std::mem::size_of::<[[f32; 4]; 8]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        globals: &wgpu::Buffer,
        transforms: &wgpu::Buffer,
        gradients: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rect globals"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: transforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: gradients.as_entire_binding() },
            ],
        })
    }

    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect instances"),
            size: (capacity * std::mem::size_of::<GpuRect>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        rects: &[Rect],
    ) {
        if rects.len() > self.capacity {
            self.capacity = rects.len().next_power_of_two();
            self.instances = Self::instance_buffer(device, self.capacity);
            self.last_rects.clear();
        }
        if viewport != self.last_viewport {
            queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&Globals { viewport, _pad: [0.0; 2] }));
            self.last_viewport = viewport;
        }
        // Each distinct matrix once, the identity first: a scene has a few,
        // one per tilted subtree, so a linear search is the cheap one.
        self.matrices.clear();
        self.matrices.push(Rect::IDENTITY);
        // And each distinct set of stops, none first.
        self.stop_sets.clear();
        self.stop_sets.push(Rect::NO_STOPS);
        self.gpu_rects.clear();
        for r in rects {
            let index = match self.matrices.iter().position(|m| *m == r.transform) {
                Some(i) => i,
                None => {
                    self.matrices.push(r.transform);
                    self.matrices.len() - 1
                }
            };
            let stops = if r.stops[1][3] < 0.5 {
                0
            } else {
                match self.stop_sets.iter().position(|s| *s == r.stops) {
                    Some(i) => i,
                    None => {
                        self.stop_sets.push(r.stops);
                        self.stop_sets.len() - 1
                    }
                }
            };
            self.gpu_rects.push(GpuRect::new(r, index as u32, stops as u32));
        }
        let mut rebind = false;
        if self.matrices.len() > self.transform_capacity {
            self.transform_capacity = self.matrices.len().next_power_of_two();
            self.transforms = Self::transform_buffer(device, self.transform_capacity);
            self.last_transforms.clear();
            rebind = true;
        }
        if self.stop_sets.len() > self.gradient_capacity {
            self.gradient_capacity = self.stop_sets.len().next_power_of_two();
            self.gradients = Self::gradient_buffer(device, self.gradient_capacity);
            self.last_gradients.clear();
            rebind = true;
        }
        if rebind {
            self.bind_group = Self::bind_group(device, &self.layout, &self.globals, &self.transforms, &self.gradients);
        }
        if self.stop_sets != self.last_gradients {
            queue.write_buffer(&self.gradients, 0, bytemuck::cast_slice(&self.stop_sets));
            self.last_gradients.clone_from(&self.stop_sets);
        }
        if self.matrices != self.last_transforms {
            queue.write_buffer(&self.transforms, 0, bytemuck::cast_slice(&self.matrices));
            self.last_transforms.clone_from(&self.matrices);
        }
        let bytes: &[u8] = bytemuck::cast_slice(&self.gpu_rects);
        if bytes != self.last_rects.as_slice() {
            // Only the span that differs: a moving satellite rewrites its own
            // instance, not the membrane's.
            let first = bytes.iter().zip(&self.last_rects).position(|(a, b)| a != b).unwrap_or(self.last_rects.len().min(bytes.len()));
            let size = std::mem::size_of::<GpuRect>();
            let start = first / size * size;
            let end = if bytes.len() == self.last_rects.len() {
                let last = bytes.iter().zip(&self.last_rects).rposition(|(a, b)| a != b).unwrap_or(first);
                (last / size + 1) * size
            } else {
                bytes.len()
            };
            if end > start {
                queue.write_buffer(&self.instances, start as u64, &bytes[start..end]);
            }
            self.last_rects.clear();
            self.last_rects.extend_from_slice(bytes);
        }
        self.count = rects.len() as u32;
        // Consecutive rects of one kind share a draw call.
        self.runs.clear();
        for (i, r) in rects.iter().enumerate() {
            let cut = r.cut > 0.5 && r.cut < 1.5;
            match self.runs.last_mut() {
                Some((c, range)) if *c == cut => range.end = i as u32 + 1,
                _ => self.runs.push((cut, i as u32..i as u32 + 1)),
            }
        }
    }

    /// The rects in `only` (indices into the list uploaded): one layer.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, only: std::ops::Range<u32>) {
        if self.count == 0 || only.is_empty() {
            return;
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (cut, range) in &self.runs {
            let range = range.start.max(only.start)..range.end.min(only.end);
            if range.is_empty() {
                continue;
            }
            pass.set_pipeline(if *cut { &self.cutter } else { &self.pipeline });
            pass.draw(0..4, range);
        }
    }
}
