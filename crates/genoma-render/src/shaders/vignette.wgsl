// genoma:vignette: the edges and corners darkened (or tinted), laid over
// what is under the node.
// values: 0–3 the colour; 4 the strength, 0…1; 5 where it begins, 0 (the
// centre) to 1 (a corner); 6 how softly, 0…1.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let half = shader.size * 0.5;
    let r = length(p - half) / max(length(half), 1.0);
    let start = value(5);
    let a = smoothstep(start, start + max(value(6), 0.001), r) * clamp(value(4), 0.0, 1.0);
    let c = g_rgba(0);
    return g_premul(vec4<f32>(c.rgb, c.a * a));
}
