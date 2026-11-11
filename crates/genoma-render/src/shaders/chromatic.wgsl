// genoma:chromatic, a pass (`pass = true`): red and blue parted from green
// outward from the middle, as a cheap lens does, the most at the corners.
// values: 0 how far they part at a corner, logical units.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let half = shader.size * 0.5;
    let shift = (p - half) / max(length(half), 1.0) * value(0);
    let r = g_backdrop(p + shift);
    let gr = g_backdrop(p);
    let b = g_backdrop(p - shift);
    return vec4<f32>(r.r, gr.g, b.b, max(max(r.a, gr.a), b.a));
}
