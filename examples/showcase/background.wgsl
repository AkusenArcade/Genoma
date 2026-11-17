// The showcase's ground: a near-black with two slow lights in it, the
// accent and the second accent, drifting through a field of noise, a
// vignette, and a fine grain so the dark never bands on a recording.
// values: 0–3 the ground, 4–7 the accent, 8–11 the second accent, 12 how
// far the first light sits across (0…1), 13 the lights' strength.
fn shade(p: vec2<f32>) -> vec4<f32> {
    let size = shader.size;
    let uv = p / size;
    let aspect = vec2<f32>(size.x / size.y, 1.0);
    let t = shader.time * 0.05;

    let ground = g_rgba(0).rgb;
    let a = g_rgba(4).rgb;
    let b = g_rgba(8).rgb;
    let strength = value(13);

    // Two lights, each a soft body whose edge the noise frays.
    let ca = vec2<f32>(value(12) + 0.05 * sin(t * 2.1), 0.42 + 0.06 * cos(t * 1.7));
    let cb = vec2<f32>(0.78 + 0.07 * cos(t * 1.3), 0.78 + 0.05 * sin(t * 1.9));
    let warp = vec2<f32>(g_fbm(uv * 2.2 + vec2<f32>(t, -t), 4), g_fbm(uv * 2.2 + vec2<f32>(-t, t * 0.7) + 7.3, 4)) * 0.08;
    let da = length((uv + warp - ca) * aspect);
    let db = length((uv + warp - cb) * aspect);
    let la = exp(-pow(da / 0.42, 2.0)) * 0.30 * strength;
    let lb = exp(-pow(db / 0.36, 2.0)) * 0.16 * strength;

    var c = ground + a * la + b * lb;
    // A faint weave in the dark: the noise itself, barely lit.
    c = c + a * (0.012 * (0.5 + 0.5 * g_fbm(uv * 6.0 + vec2<f32>(t * 0.6, 0.0), 3)));

    // The corners fall away.
    let v = length((uv - 0.5) * vec2<f32>(1.0, 0.9));
    c = c * (1.0 - smoothstep(0.35, 0.95, v) * 0.55);

    // Grain: a dither under a step of the 8-bit ramp.
    let n = g_hash(p + fract(shader.time) * 97.0) - 0.5;
    c = c + vec3<f32>(n / 255.0 * 1.5);

    return vec4<f32>(c, 1.0);
}
