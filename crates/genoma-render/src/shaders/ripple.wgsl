// genoma:ripple: rings spreading from a point, fading as they go.
// values: 0–3 the colour; 4, 5 the centre, 0…1 of the width and height;
// 6 the distance between rings, logical units; 7 the speed, rings a
// second; 8 how far they reach, logical units; 9 a ring's width, 0…1 of
// the distance between them.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let centre = vec2<f32>(value(4), value(5)) * shader.size;
    let r = length(p - centre);
    let spacing = max(value(6), 1.0);
    let phase = fract(r / spacing - shader.time * value(7));
    let width = clamp(value(9), 0.02, 1.0);
    let d = (abs(phase - 0.5) - width * 0.5) * spacing;
    let fade = clamp(1.0 - r / max(value(8), 1.0), 0.0, 1.0);
    let c = g_rgba(0);
    return g_premul(vec4<f32>(c.rgb, c.a * g_cover(d) * fade));
}
