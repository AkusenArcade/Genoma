// A pass of one's own: what the node drew, read with g_backdrop, darkened
// every other row and lit at a line that sweeps down, as an old screen.
// values: 0 the rows' height, logical units; 1 the sweep, rows a second.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let seen = g_backdrop(p);
    let row = max(value(0), 1.0);
    let dark = select(1.0, 0.65, fract(p.y / (row * 2.0)) >= 0.5);
    let sweep = fract(shader.time * value(1) / max(shader.size.y / row, 1.0)) * shader.size.y;
    let lit = 1.0 + 0.35 * exp(-abs(p.y - sweep) / (row * 3.0));
    return vec4<f32>(seen.rgb * dark * lit, seen.a);
}
