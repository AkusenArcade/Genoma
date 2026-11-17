// A shader of one's own: noise warped by noise (Inigo Quilez's domain
// warping), coloured along a ramp of the theme's two accents and a warm
// third, lit where the field folds. values: 0–3 the first colour, 4–7
// the second, 8–11 the third.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let uv = p / shader.size.y * 2.2;
    let t = shader.time * 0.12;
    let q = vec2<f32>(g_fbm(uv + vec2<f32>(0.0, t), 5), g_fbm(uv + vec2<f32>(5.2, 1.3 - t), 5));
    let r = vec2<f32>(
        g_fbm(uv + 3.0 * q + vec2<f32>(1.7, 9.2) + 0.6 * t, 5),
        g_fbm(uv + 3.0 * q + vec2<f32>(8.3, 2.8) - 0.4 * t, 5),
    );
    let f = 0.5 + 0.5 * g_fbm(uv + 3.0 * r, 5);

    let a = g_rgba(0).rgb;
    let b = g_rgba(4).rgb;
    let c = g_rgba(8).rgb;
    var col = mix(vec3<f32>(0.02, 0.03, 0.04), a, clamp(f * f * 2.2, 0.0, 1.0));
    col = mix(col, b, clamp(length(q) * 0.9, 0.0, 1.0));
    col = mix(col, c, clamp(abs(r.x) * 0.9, 0.0, 1.0) * 0.6);
    // Light where the field folds hardest.
    col = col * (0.55 + 0.9 * f * f);
    return vec4<f32>(col, 1.0);
}
