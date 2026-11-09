// genoma:dissolve: a colour that breaks up into noise as `progress` goes
// from 0 (whole) to 1 (gone), the breaking edge in a colour of its own.
// values: 0–3 the colour; 4 the progress; 5 the edge's width, 0…1;
// 6–9 the edge's colour; 10 the grain's size, logical units.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let n = clamp(0.5 + 0.5 * g_fbm(p / max(value(10), 1.0), 4), 0.0, 1.0);
    let edge = max(value(5), 0.0);
    // From two edges below the lowest noise (whole, no edge showing) to
    // its highest (gone).
    let cut = value(4) * (1.0 + 2.0 * edge) - 2.0 * edge;
    let aw = max(fwidth(n), 1e-4);
    let shown = clamp((n - cut) / aw + 0.5, 0.0, 1.0);
    let inner = clamp((n - cut - edge) / aw + 0.5, 0.0, 1.0);
    let c = mix(g_rgba(6), g_rgba(0), inner);
    return g_premul(vec4<f32>(c.rgb, c.a * shown));
}
