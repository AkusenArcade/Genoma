// genoma:glass: a pane that bends what is under it (the node needs
// `backdrop = true`): drawn toward the middle near its rim, as a lens's
// edge does, the colours parting there, a little frosted, tinted, its top
// edge catching the light.
// values: 0 the corners' radius, logical units; 1 the bend, how far the rim
// pulls, logical units; 2 the colours' parting at the rim, logical units;
// 3 the frost, logical units; 4–7 the tint (straight RGBA, its alpha how
// much); 8 the rim's light, 0…1.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let half = shader.size * 0.5;
    let radius = min(value(0), min(half.x, half.y));
    let d = g_sd_rounded_box(p - half, half, radius);
    let cover = g_cover(d);
    if (cover <= 0.0) {
        return vec4<f32>(0.0);
    }
    // 1 at the rim, 0 a few bends in: where the lens is curved.
    let bend = max(value(1), 0.0);
    let rim = clamp(1.0 + d / max(bend * 4.0, 1.0), 0.0, 1.0);
    let toward = normalize(p - half + vec2<f32>(1e-4, 1e-4));
    let pull = toward * bend * rim * rim;
    let part = toward * max(value(2), 0.0) * rim;
    let frost = max(value(3), 0.0);
    var seen = vec4<f32>(0.0);
    let taps = array<vec2<f32>, 5>(vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.4), vec2<f32>(-1.0, -0.4), vec2<f32>(-0.4, 1.0), vec2<f32>(0.4, -1.0));
    for (var i = 0; i < 5; i = i + 1) {
        let at = p - pull + taps[i] * frost;
        let r = g_backdrop(at - part);
        let gb = g_backdrop(at);
        let b = g_backdrop(at + part);
        seen = seen + vec4<f32>(r.r, gb.g, b.b, max(max(r.a, gb.a), b.a));
    }
    seen = seen / 5.0;
    let tint = g_rgba(4);
    var glass = seen * (1.0 - tint.a) + g_premul(vec4<f32>(tint.rgb, 1.0)) * tint.a;
    // The top edge lit, fading down the rim.
    let light = clamp(value(8), 0.0, 1.0) * rim * clamp(1.0 - p.y / max(shader.size.y * 0.5, 1.0), 0.0, 1.0);
    glass = vec4<f32>(glass.rgb + vec3<f32>(light), max(glass.a, light));
    return glass * cover;
}
